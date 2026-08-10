// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{TimedMidiMessage, embedded_soundfont_path};
use izarravm_core::{MASTER_CLOCK_HZ, MidiBackend, MidiConfig, MidiPortId, MidiStatus};
use izarravm_native_synth::{Error as SynthError, FluidSynth, MuntSynth, RomKind, SAMPLE_RATE_HZ};
use midir::{MidiOutput, MidiOutputConnection};
use std::collections::VecDeque;
use tracing::warn;

const ALL_NOTES_OFF: u8 = 123;
const MAX_PENDING_MESSAGES: usize = 4_096;
const MAX_STAGED_FRAMES: usize = SAMPLE_RATE_HZ as usize / 10;

enum MidiAdapter {
    Off,
    External(MidiOutputConnection),
    Fluid(FluidSynth),
    Munt(MuntSynth),
    Silent,
}

#[derive(Clone, Copy)]
enum MidiRole {
    Wavetable,
    Receiver,
}

impl MidiRole {
    fn settings_changed(self, old: &MidiConfig, new: &MidiConfig) -> bool {
        match self {
            Self::Wavetable => old.soundfont != new.soundfont,
            Self::Receiver => {
                old.backend != new.backend
                    || old.external_port != new.external_port
                    || old.mt32_control_rom != new.mt32_control_rom
                    || old.mt32_pcm_rom != new.mt32_pcm_rom
            }
        }
    }
}

pub struct MidiEngine {
    adapter: MidiAdapter,
    status: MidiStatus,
    pending: VecDeque<TimedMidiMessage>,
    staged: VecDeque<(i16, i16)>,
    guest_frame_cursor: u64,
    scratch: Vec<i16>,
    role: MidiRole,
    config: MidiConfig,
    /// (Left, Right) linear gain applied as this engine adds itself to the mix,
    /// from the card's wavetable volume registers (`0x50`/`0x51`). Unity until
    /// the caller sets it, so an engine driven without a machine is unchanged.
    gain: (f32, f32),
    /// Guest messages the synth would not accept, dropped rather than fatal.
    /// See [`MidiEngine::rejected_messages`].
    rejected: u64,
}

impl MidiEngine {
    pub fn open_wavetable(config: &MidiConfig) -> Self {
        Self::open(config, MidiRole::Wavetable)
    }

    pub fn open_receiver(config: &MidiConfig) -> Self {
        Self::open(config, MidiRole::Receiver)
    }

    fn open(config: &MidiConfig, role: MidiRole) -> Self {
        let mut engine = Self {
            adapter: MidiAdapter::Off,
            status: MidiStatus::Ready,
            pending: VecDeque::new(),
            staged: VecDeque::with_capacity(MAX_STAGED_FRAMES),
            guest_frame_cursor: 0,
            scratch: Vec::new(),
            role,
            config: config.clone(),
            gain: (1.0, 1.0),
            rejected: 0,
        };
        engine.configure(config);
        engine
    }

    pub const fn status(&self) -> MidiStatus {
        self.status
    }

    /// The (Left, Right) gain [`render`](Self::render) will apply as this engine
    /// adds itself to the mix.
    ///
    /// Exposed because this is STAGING: it leaves no trace in the samples of a
    /// silent engine, so a frontend that forgets to push the card's wavetable
    /// level here fails invisibly, and only once something is playing. Reading
    /// it back is what lets `pump_audio` be tested at all.
    pub const fn gain(&self) -> (f32, f32) {
        self.gain
    }

    /// How many guest messages this engine handed to the synth and the synth
    /// did not take -- malformed, or offered while its input queue was full.
    ///
    /// A refusal is NOT a failure of the engine: the MPU-401 hands us whatever
    /// the guest wrote, a DOS driver is free to write a byte no synthesiser
    /// accepts, and a synth that is momentarily full is still a working synth.
    /// Counted so a stuck note has a number behind it.
    pub const fn rejected_messages(&self) -> u64 {
        self.rejected
    }

    /// List output ports by stable name and same-name ordinal.
    pub fn external_ports() -> Vec<MidiPortId> {
        let Ok(output) = MidiOutput::new("IzarraVM port scan") else {
            return Vec::new();
        };
        let ports = output.ports();
        port_ids(
            ports
                .iter()
                .map(|port| output.port_name(port).ok())
                .collect::<Vec<_>>()
                .iter()
                .map(|name| name.as_deref()),
        )
    }

    pub fn send(&mut self, message: &TimedMidiMessage) {
        match &mut self.adapter {
            MidiAdapter::External(connection) => {
                let result = connection.send(&message.bytes);
                if let Err(error) = self.record_external_send_result(result) {
                    warn!(%error, "external MIDI port was disconnected");
                }
            }
            MidiAdapter::Fluid(_) | MidiAdapter::Munt(_) => self.queue(message.clone()),
            MidiAdapter::Off | MidiAdapter::Silent => {}
        }
    }

    /// Render guest-timed native synthesis into an existing 44.1 kHz stereo mix.
    ///
    /// Native PCM advances only as far as `guest_tick`, then waits in a bounded
    /// 100 ms staging queue for the wall-time mixer. A full queue keeps its oldest
    /// audio and resumes synthesis after the mixer drains space.
    pub fn render(&mut self, output: &mut [(i16, i16)], guest_tick: u64) {
        if output.is_empty() {
            return;
        }

        let target = sample_frame_for_tick(guest_tick).max(self.guest_frame_cursor);
        if matches!(self.adapter, MidiAdapter::Fluid(_) | MidiAdapter::Munt(_)) {
            self.stage_until(target);
        } else {
            self.guest_frame_cursor = target;
        }

        let (gain_l, gain_r) = self.gain;
        for frame in output {
            let Some(samples) = self.staged.pop_front() else {
                break;
            };
            frame.0 = frame.0.saturating_add(scale(samples.0, gain_l));
            frame.1 = frame.1.saturating_add(scale(samples.1, gain_r));
        }
    }

    /// Set the (Left, Right) linear gain this engine applies as it adds itself
    /// to the mix. The frontend takes it from `Machine::midi_gain`, so the
    /// guest's own mixer register drives it; nothing is stored on the host that
    /// the guest cannot read back.
    pub fn set_gain(&mut self, gain: (f32, f32)) {
        self.gain = gain;
    }

    fn stage_until(&mut self, target: u64) {
        let available = MAX_STAGED_FRAMES.saturating_sub(self.staged.len());
        let start = self.guest_frame_cursor;
        let end = start.saturating_add(available as u64).min(target);
        let frame_count = end.saturating_sub(start) as usize;
        self.scratch.resize(frame_count.saturating_mul(2), 0);
        self.scratch.fill(0);

        let mut cursor = 0usize;
        while let Some(message) = self.pending.front() {
            let event_frame = sample_frame_for_tick(message.guest_tick);
            if event_frame > end {
                break;
            }
            let offset = event_frame.saturating_sub(start).min(frame_count as u64) as usize;
            if !render_adapter(&mut self.adapter, &mut self.scratch[cursor * 2..offset * 2]) {
                self.fail_native();
                return;
            }
            cursor = offset;
            let message = self.pending.pop_front().expect("front message exists");
            match send_native(&mut self.adapter, &message.bytes) {
                NativeSend::Delivered => {}
                // The synth did not take this message -- a byte it will not
                // accept, or a queue that is full right now. Drop THAT
                // MESSAGE and keep playing. Failing the engine here is what
                // made a single stray `0xF7`/`0xF4`/`0xF9` -- all of which the
                // MPU-401 parser forwards verbatim as one-byte messages -- kill
                // the P300 for the rest of the session, silently and with no way
                // back short of a restart.
                NativeSend::Rejected => {
                    self.rejected = self.rejected.saturating_add(1);
                    if self.rejected == 1 {
                        warn!(
                            bytes = ?message.bytes,
                            "the synth did not take a guest MIDI message; dropping it and continuing"
                        );
                    }
                }
                NativeSend::Failed => {
                    self.fail_native();
                    return;
                }
            }
        }
        if !matches!(self.adapter, MidiAdapter::Silent)
            && !render_adapter(&mut self.adapter, &mut self.scratch[cursor * 2..])
        {
            self.fail_native();
            return;
        }

        for samples in self.scratch.chunks_exact(2) {
            self.staged.push_back((samples[0], samples[1]));
        }
        self.guest_frame_cursor = end;
    }

    /// Apply new settings, and re-open an engine that is not currently working.
    ///
    /// The second half is not a nicety. `fail_native` is a LATCH: it drops the
    /// adapter to `Silent` and leaves the status at `InitializationFailed`, and
    /// the only thing that used to clear it was a settings change this engine's
    /// role cares about -- for the wavetable, a different SoundFont, and nothing
    /// else. So a P300 that died mid-session stayed dead, and the config panel
    /// kept showing "The MIDI output could not be initialized." no matter what
    /// the user did to it, including switching the P330 receiver off. Anything
    /// short of Ready is retried here, so re-accepting the panel is a retry.
    pub fn reconfigure(&mut self, config: &MidiConfig) {
        if !self.role.settings_changed(&self.config, config) && self.status == MidiStatus::Ready {
            return;
        }
        self.close();
        self.config = config.clone();
        self.rejected = 0;
        self.configure(config);
    }

    fn configure(&mut self, config: &MidiConfig) {
        let (adapter, status) = match self.role {
            MidiRole::Wavetable => open_fluidsynth(config),
            MidiRole::Receiver => match config.backend {
                MidiBackend::Off => (MidiAdapter::Off, MidiStatus::Ready),
                MidiBackend::External => self.open_external(config.external_port.as_ref()),
                MidiBackend::Munt => open_munt(config),
            },
        };
        self.adapter = adapter;
        self.status = status;
    }

    fn open_external(&self, requested: Option<&MidiPortId>) -> (MidiAdapter, MidiStatus) {
        let Some(requested) = requested else {
            return (MidiAdapter::Silent, MidiStatus::MissingPort);
        };
        let Ok(output) = MidiOutput::new("IzarraVM") else {
            return (MidiAdapter::Silent, MidiStatus::InitializationFailed);
        };
        let ports = output.ports();
        let names: Vec<_> = ports
            .iter()
            .map(|port| output.port_name(port).ok())
            .collect();
        let Some(index) = matching_port_index(names.iter().map(|name| name.as_deref()), requested)
        else {
            return (MidiAdapter::Silent, MidiStatus::MissingPort);
        };
        match output.connect(&ports[index], "IzarraVM P330 MIDI") {
            Ok(connection) => (MidiAdapter::External(connection), MidiStatus::Ready),
            Err(error) => {
                warn!(%error, "could not open the selected external MIDI port");
                (MidiAdapter::Silent, MidiStatus::InitializationFailed)
            }
        }
    }

    fn queue(&mut self, message: TimedMidiMessage) {
        if self.pending.len() == MAX_PENDING_MESSAGES {
            return;
        }
        let index = self
            .pending
            .iter()
            .position(|queued| queued.guest_tick > message.guest_tick)
            .unwrap_or(self.pending.len());
        self.pending.insert(index, message);
    }

    fn fail_native(&mut self) {
        self.adapter = MidiAdapter::Silent;
        self.pending.clear();
        self.staged.clear();
        self.status = MidiStatus::InitializationFailed;
    }

    fn record_external_send_result<E>(&mut self, result: Result<(), E>) -> Result<(), E> {
        if result.is_err() {
            self.adapter = MidiAdapter::Silent;
            self.status = MidiStatus::MissingPort;
        }
        result
    }

    fn close(&mut self) {
        silence(&mut self.adapter);
        self.adapter = MidiAdapter::Off;
        self.pending.clear();
        self.staged.clear();
    }
}

impl Drop for MidiEngine {
    fn drop(&mut self) {
        self.close();
    }
}

fn open_fluidsynth(config: &MidiConfig) -> (MidiAdapter, MidiStatus) {
    let mut fallback_status = MidiStatus::Ready;
    if let Some(path) = &config.soundfont {
        match FluidSynth::new(path) {
            Ok(synth) => return (MidiAdapter::Fluid(synth), MidiStatus::Ready),
            Err(error) => {
                warn!(%error, path = %path.display(), "custom SoundFont failed; using the embedded bank");
                fallback_status = MidiStatus::MissingSoundFont;
            }
        }
    }

    let path = match embedded_soundfont_path() {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, "could not cache the embedded SoundFont");
            return (MidiAdapter::Silent, MidiStatus::InitializationFailed);
        }
    };
    match FluidSynth::new(&path) {
        Ok(synth) => (MidiAdapter::Fluid(synth), fallback_status),
        Err(error) => {
            warn!(%error, path = %path.display(), "could not initialize FluidSynth");
            (MidiAdapter::Silent, MidiStatus::InitializationFailed)
        }
    }
}

fn open_munt(config: &MidiConfig) -> (MidiAdapter, MidiStatus) {
    let (Some(control), Some(pcm)) = (&config.mt32_control_rom, &config.mt32_pcm_rom) else {
        return (MidiAdapter::Silent, MidiStatus::MissingRoms);
    };
    match MuntSynth::new(control, pcm) {
        Ok(synth) => (MidiAdapter::Munt(synth), MidiStatus::Ready),
        Err(error) => {
            // The message names the file and the requirement; the status is
            // what the panel can colour. Both exist because "The MIDI output
            // could not be initialized." told a user with a real, complete ROM
            // set nothing whatsoever about why theirs was refused.
            warn!(
                %error,
                control = %control.display(),
                pcm = %pcm.display(),
                "could not initialize Munt with the selected ROMs"
            );
            (MidiAdapter::Silent, munt_rom_status(&error))
        }
    }
}

/// Map a ROM loader failure onto the status the config panel renders.
fn munt_rom_status(error: &SynthError) -> MidiStatus {
    match error {
        SynthError::MissingRom(_) => MidiStatus::RomPathMissing,
        SynthError::RomNotFound {
            kind: RomKind::Control,
            ..
        } => MidiStatus::RomControlMissing,
        SynthError::RomNotFound {
            kind: RomKind::Pcm, ..
        } => MidiStatus::RomPcmMissing,
        // One file, unidentified: the user pointed at something that is not a
        // ROM this library knows. Which of the two it was meant to be is not
        // knowable, so report the control image, the one that is looked for first.
        SynthError::InvalidRom(_) => MidiStatus::RomControlMissing,
        SynthError::MissingRoms => MidiStatus::RomsNotPairable,
        _ => MidiStatus::InitializationFailed,
    }
}

fn render_adapter(adapter: &mut MidiAdapter, output: &mut [i16]) -> bool {
    let result = match adapter {
        MidiAdapter::Fluid(synth) => synth.render_interleaved_i16(output),
        MidiAdapter::Munt(synth) => synth.render_interleaved_i16(output),
        MidiAdapter::Off | MidiAdapter::External(_) | MidiAdapter::Silent => return true,
    };
    if let Err(error) = result {
        warn!(%error, "native MIDI synthesis failed while rendering");
        false
    } else {
        true
    }
}

/// What one message did to the synth, which is not the same question as whether
/// the message was good.
enum NativeSend {
    Delivered,
    /// The message itself was not acceptable. The synth is untouched and healthy.
    Rejected,
    /// The synth failed. Nothing more can be played through it.
    Failed,
}

fn send_native(adapter: &mut MidiAdapter, bytes: &[u8]) -> NativeSend {
    let result = match adapter {
        MidiAdapter::Fluid(synth) => synth.send(bytes),
        MidiAdapter::Munt(synth) => synth.send(bytes),
        MidiAdapter::Off | MidiAdapter::External(_) | MidiAdapter::Silent => {
            return NativeSend::Delivered;
        }
    };
    match result {
        Ok(()) => NativeSend::Delivered,
        Err(error) => triage_send_error(&error),
    }
}

/// Decide whether a failed send costs the MESSAGE or the ENGINE.
///
/// Split out because this is the whole of the decision and it is not reachable
/// through `send_native` without a live synthesiser: `MuntSynth::send` can only
/// be made to return a full queue by filling one, and only a real ROM set opens
/// one at all.
///
/// `mt32emu_play_msg` and `mt32emu_play_sysex` return exactly two failures --
/// `MT32EMU_RC_QUEUE_FULL` when the synth is open and momentarily full, and
/// `MT32EMU_RC_NOT_OPENED` when there is no synth behind the context. Only the
/// second is terminal. Treating both as terminal (which the bare `NativeCall`
/// mapping did) turns one busy audio window into a P330 that is dead for the
/// session, exactly the way one stray `0xF7` used to.
fn triage_send_error(error: &SynthError) -> NativeSend {
    match error {
        SynthError::InvalidMidiMessage | SynthError::SynthQueueFull => NativeSend::Rejected,
        error => {
            warn!(%error, "native MIDI synthesis failed on a guest message");
            NativeSend::Failed
        }
    }
}

fn silence(adapter: &mut MidiAdapter) {
    match adapter {
        MidiAdapter::External(connection) => {
            for message in all_notes_off_messages() {
                let _ = connection.send(&message);
            }
        }
        MidiAdapter::Fluid(synth) => {
            if let Err(error) = synth.all_notes_off() {
                warn!(%error, "FluidSynth all-notes-off failed");
            }
        }
        MidiAdapter::Munt(synth) => {
            if let Err(error) = synth.all_notes_off() {
                warn!(%error, "Munt all-notes-off failed");
            }
        }
        MidiAdapter::Off | MidiAdapter::Silent => {}
    }
}

/// Attenuate one sample by a linear gain. Unity is an exact identity (the
/// multiply and the round both leave the value alone), so an engine whose gain
/// has not been set renders bit-for-bit what it rendered before there was one.
fn scale(sample: i16, gain: f32) -> i16 {
    if gain == 1.0 {
        return sample;
    }
    (f32::from(sample) * gain).round().clamp(-32768.0, 32767.0) as i16
}

fn sample_frame_for_tick(guest_tick: u64) -> u64 {
    let numerator = u128::from(guest_tick) * u128::from(SAMPLE_RATE_HZ);
    u64::try_from(numerator.div_ceil(u128::from(MASTER_CLOCK_HZ))).unwrap_or(u64::MAX)
}

fn matching_port_index<'a>(
    names: impl IntoIterator<Item = Option<&'a str>>,
    requested: &MidiPortId,
) -> Option<usize> {
    names
        .into_iter()
        .enumerate()
        .filter(|(_, name)| name.is_some_and(|name| name == requested.name))
        .nth(usize::from(requested.ordinal))
        .map(|(index, _)| index)
}

fn port_ids<'a>(names: impl IntoIterator<Item = Option<&'a str>>) -> Vec<MidiPortId> {
    let mut ports: Vec<MidiPortId> = Vec::new();
    for name in names.into_iter().flatten() {
        let ordinal = ports.iter().filter(|port| port.name == name).count();
        ports.push(MidiPortId {
            name: name.to_owned(),
            ordinal: u16::try_from(ordinal).unwrap_or(u16::MAX),
        });
    }
    ports
}

fn all_notes_off_messages() -> impl Iterator<Item = [u8; 3]> {
    (0..16).map(|channel| [0xb0 | channel, ALL_NOTES_OFF, 0])
}

#[cfg(test)]
#[path = "midi_test.rs"]
mod tests;
