// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{TimedMidiMessage, embedded_soundfont_path};
use izarravm_core::{MASTER_CLOCK_HZ, MidiBackend, MidiConfig, MidiPortId, MidiStatus};
use izarravm_native_synth::{FluidSynth, MuntSynth, SAMPLE_RATE_HZ};
use midir::{MidiOutput, MidiOutputConnection};
use std::collections::VecDeque;
use tracing::warn;

const ALL_NOTES_OFF: u8 = 123;
const MAX_PENDING_MESSAGES: usize = 4_096;

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
    rendered_frames: u64,
    scratch: Vec<i16>,
    role: MidiRole,
    config: MidiConfig,
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
            rendered_frames: 0,
            scratch: Vec::new(),
            role,
            config: config.clone(),
        };
        engine.configure(config);
        engine
    }

    pub const fn status(&self) -> MidiStatus {
        self.status
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

    /// Render native synthesis into an existing 44.1 kHz stereo mix.
    ///
    /// Guest timestamps map to absolute mixer frames. Splitting the same render
    /// span into smaller calls therefore leaves event offsets unchanged.
    pub fn render(&mut self, output: &mut [(i16, i16)]) {
        if output.is_empty() {
            return;
        }

        let sample_count = output.len().saturating_mul(2);
        self.scratch.resize(sample_count, 0);
        self.scratch.fill(0);

        let start = self.rendered_frames;
        let end = start.saturating_add(output.len() as u64);
        let mut cursor = 0usize;
        while let Some(message) = self.pending.front() {
            let event_frame = sample_frame_for_tick(message.guest_tick);
            if event_frame > end {
                break;
            }
            let offset = event_frame.saturating_sub(start).min(output.len() as u64) as usize;
            if !render_adapter(&mut self.adapter, &mut self.scratch[cursor * 2..offset * 2]) {
                self.fail_native();
                break;
            }
            cursor = offset;
            let message = self.pending.pop_front().expect("front message exists");
            if !send_native(&mut self.adapter, &message.bytes) {
                self.fail_native();
                break;
            }
        }
        if !matches!(self.adapter, MidiAdapter::Silent)
            && !render_adapter(&mut self.adapter, &mut self.scratch[cursor * 2..])
        {
            self.fail_native();
        }

        for (frame, samples) in output.iter_mut().zip(self.scratch.chunks_exact(2)) {
            frame.0 = frame.0.saturating_add(samples[0]);
            frame.1 = frame.1.saturating_add(samples[1]);
        }
        self.rendered_frames = end;
    }

    pub fn reconfigure(&mut self, config: &MidiConfig) {
        if !self.role.settings_changed(&self.config, config) {
            return;
        }
        self.close();
        self.config = config.clone();
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
    if !control.is_file() || !pcm.is_file() {
        return (MidiAdapter::Silent, MidiStatus::MissingRoms);
    }
    match MuntSynth::new(control, pcm) {
        Ok(synth) => (MidiAdapter::Munt(synth), MidiStatus::Ready),
        Err(error) => {
            warn!(%error, "could not initialize Munt with the selected ROMs");
            (MidiAdapter::Silent, MidiStatus::InitializationFailed)
        }
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

fn send_native(adapter: &mut MidiAdapter, bytes: &[u8]) -> bool {
    let result = match adapter {
        MidiAdapter::Fluid(synth) => synth.send(bytes),
        MidiAdapter::Munt(synth) => synth.send(bytes),
        MidiAdapter::Off | MidiAdapter::External(_) | MidiAdapter::Silent => return true,
    };
    if let Err(error) = result {
        warn!(%error, "native MIDI synthesis rejected a guest message");
        false
    } else {
        true
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
