// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_audio::{Resampler, SbDsp, SbMixer};
use izarravm_bus::Memory;
use izarravm_core::SoundBlasterConfig;
use std::collections::VecDeque;

use crate::dma::DmaController;
use crate::timeline::RatePhase;
use crate::{DAC_HZ, DAC_PENDING_FRAME_CAP, OPL_NATIVE_HZ};

/// Per-second Sound Blaster diagnostics, enabled by `IZARRAVM_SB_DEBUG`.
///
/// The fields are chosen so the known failure modes are distinguishable from
/// each other: whether the guest ever programmed the DSP at all, whether the
/// producer and consumer accumulators drift, whether the render ring overflows,
/// and whether the resampler is being rebuilt mid-stream.
#[derive(Debug, Clone, Default)]
struct SbTrace {
    micros: u64,
    ticked_frames: u64,
    rendered_frames: u64,
    truncated_frames: u64,
    padded_frames: u64,
    irqs: u64,
    resampler_rebuilds: u64,
    last_dropped: u64,
    pending_depth: usize,
}

impl SbTrace {
    /// Whether the trace is enabled. Read once at construction: an env lookup
    /// per sample would itself distort what is being measured.
    fn enabled() -> bool {
        std::env::var_os("IZARRAVM_SB_DEBUG").is_some()
    }

    fn report(&mut self, dsp: &mut SbDsp, dma8: usize, dma16: usize) {
        let dropped = dsp.dropped_frames();
        let peak = dsp.take_peak_abs();
        eprintln!(
            "[SB] playing={} rate={} out_rate={} bits={} stereo={} auto_init={} dma={} block={} remaining={} ticked/s={} rendered/s={} truncated/s={} padded/s={} irqs/s={} ring_drops/s={} resampler_rebuilds/s={} pending={} peak={}",
            dsp.is_playing(),
            dsp.rate_hz(),
            dsp.output_frame_rate(),
            if dsp.is_16bit() { 16 } else { 8 },
            dsp.is_stereo(),
            dsp.is_auto_init(),
            if dsp.is_16bit() { dma16 } else { dma8 },
            dsp.block_size(),
            dsp.block_remaining(),
            self.ticked_frames,
            self.rendered_frames,
            self.truncated_frames,
            self.padded_frames,
            self.irqs,
            dropped.saturating_sub(self.last_dropped),
            self.resampler_rebuilds,
            self.pending_depth,
            peak,
        );
        self.last_dropped = dropped;
        self.micros = 0;
        self.ticked_frames = 0;
        self.rendered_frames = 0;
        self.truncated_frames = 0;
        self.padded_frames = 0;
        self.irqs = 0;
        self.resampler_rebuilds = 0;
    }
}

#[derive(Debug, Clone)]
struct ActiveSb16Path {
    dsp: SbDsp,
    mixer: SbMixer,
    resampler: Resampler,
    resampler_rate_hz: u32,
    render_phase: RatePhase,
    held_dac_frame: (i32, i32),
    /// Resampled DAC-rate frames produced but not yet claimed by a render
    /// window. See `render_voice` for why this has to persist across calls.
    pending: VecDeque<(i32, i32)>,
    /// None when `IZARRAVM_SB_DEBUG` is unset, so every trace site is a null
    /// check on an Option rather than an env lookup or an argument build.
    trace: Option<SbTrace>,
}

impl ActiveSb16Path {
    fn sample_stereo(&mut self) {
        self.dsp.set_sbpro_stereo(self.mixer.sbpro_stereo());
    }

    fn sync_resampler(&mut self) {
        self.sample_stereo();
        let rate = self.dsp.output_frame_rate().max(1);
        if rate != self.resampler_rate_hz {
            self.resampler = Resampler::new(rate, DAC_HZ);
            self.resampler_rate_hz = rate;
            if let Some(trace) = &mut self.trace {
                trace.resampler_rebuilds += 1;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Sb16Path {
    active: Option<ActiveSb16Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sb16IrqDeadline {
    line: u8,
    frames: u64,
    rate_hz: u64,
}

impl Sb16IrqDeadline {
    pub(crate) const fn line(self) -> u8 {
        self.line
    }

    pub(crate) const fn frames(self) -> u64 {
        self.frames
    }

    pub(crate) const fn rate_hz(self) -> u64 {
        self.rate_hz
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sb16Irq {
    line: u8,
}

impl Sb16Irq {
    pub(crate) const fn line(self) -> u8 {
        self.line
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sb16RenderWindow {
    pub(crate) elapsed_master_ticks: u64,
    pub(crate) fallback_opl_samples: usize,
    pub(crate) output_frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ct1745Mix {
    active: bool,
    /// FM (OPL3) bus attenuation, mixer registers `0x34`/`0x35`.
    fm_l: f32,
    fm_r: f32,
    voice_bus_l: f32,
    voice_bus_r: f32,
    cd_l: f32,
    cd_r: f32,
    /// PC-speaker input attenuation, mixer register `0x3B` (mono, 2-bit).
    speaker: f32,
}

impl Ct1745Mix {
    const fn bypass() -> Self {
        Self {
            active: false,
            fm_l: 1.0,
            fm_r: 1.0,
            voice_bus_l: 1.0,
            voice_bus_r: 1.0,
            cd_l: 0.0,
            cd_r: 0.0,
            speaker: 1.0,
        }
    }

    /// Sum the two card-internal buses the CT1745 controls: the FM synthesiser
    /// (registers `0x34`/`0x35`) and the DSP voice (`0x32`/`0x33`, already
    /// applied at drain time in `render_voice`), then take the master and output
    /// gain. Keeping FM on its own leg is what lets a title balance music
    /// against effects -- the two share only the master.
    pub(crate) fn mix_opl_voice(self, opl: (i32, i32), voice: (i32, i32)) -> (i32, i32) {
        if !self.active {
            return opl;
        }
        (
            ((opl.0 as f32 * self.fm_l + voice.0 as f32) * self.voice_bus_l) as i32,
            ((opl.1 as f32 * self.fm_r + voice.1 as f32) * self.voice_bus_r) as i32,
        )
    }

    pub(crate) fn mix_cd(self, cd: (i32, i32)) -> (i32, i32) {
        (
            (cd.0 as f32 * self.cd_l) as i32,
            (cd.1 as f32 * self.cd_r) as i32,
        )
    }

    /// Take the PC-speaker leg through the card: the 2-bit PC-SPK level
    /// (`0x3B`) and then the master, exactly as 86Box's
    /// `sb16_awe32_filter_pc_speaker` does (`buffer * speaker * master`). The
    /// beeper is a mono source, so one sample fans out to the stereo master.
    ///
    /// A card that is not installed has no PC-SPK input to route the beeper
    /// through, so the leg passes at unity rather than vanishing: the
    /// motherboard speaker still works on a machine with no sound card.
    pub(crate) fn mix_speaker(self, spk: i32) -> (i32, i32) {
        if !self.active {
            return (spk, spk);
        }
        (
            (spk as f32 * self.speaker * self.voice_bus_l) as i32,
            (spk as f32 * self.speaker * self.voice_bus_r) as i32,
        )
    }
}

impl Sb16Path {
    pub(crate) fn new(config: &SoundBlasterConfig) -> Self {
        if !config.enabled {
            return Self { active: None };
        }

        let mut active = ActiveSb16Path {
            dsp: SbDsp::default(),
            mixer: SbMixer::with_power_on(
                config.irq.line(),
                config.dma.channel(),
                config.high_dma.channel(),
            ),
            resampler: Resampler::new(OPL_NATIVE_HZ, DAC_HZ),
            resampler_rate_hz: 0,
            render_phase: RatePhase::default(),
            held_dac_frame: (0, 0),
            pending: VecDeque::new(),
            trace: SbTrace::enabled().then(SbTrace::default),
        };
        active.sample_stereo();
        Self {
            active: Some(active),
        }
    }

    /// Re-point the card's IRQ/DMA routing. No-op when the SB16 path is
    /// disabled, since there is no mixer to configure.
    pub(crate) fn set_routing(&mut self, irq: u8, dma8: usize, dma16: usize) {
        if let Some(active) = self.active.as_mut() {
            active.mixer.set_routing(irq, dma8, dma16);
        }
    }

    /// The routing the mixer currently answers on, or `None` when the path is
    /// disabled. Read from the mixer registers rather than from any cached
    /// copy, so it reflects a guest write to `0x80`/`0x81`.
    pub(crate) fn routing(&self) -> Option<(u8, usize, usize)> {
        let active = self.active.as_ref()?;
        Some((
            active.mixer.selected_irq(),
            active.mixer.selected_dma_8(),
            active.mixer.selected_dma_16(),
        ))
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        let active = self.active.as_mut()?;
        if let Some(value) = active.mixer.read_port(port) {
            return Some(value);
        }
        let value = active.dsp.read_port(port)?;
        if matches!(port, 0x22E | 0x22F) {
            active.mixer.clear_irq_status();
        }
        Some(value)
    }

    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.mixer.write_port(port, value) {
            active.sample_stereo();
            return true;
        }
        active.dsp.write_port(port, value)
    }

    pub(crate) fn timeline_rate_hz(&mut self) -> u64 {
        let Some(active) = self.active.as_mut() else {
            return 0;
        };
        active.sample_stereo();
        if active.dsp.needs_output_tick() {
            u64::from(active.dsp.output_frame_rate())
        } else {
            0
        }
    }

    /// Whether the DSP output clock still has PCM to produce this batch, i.e.
    /// DMA playback is armed or ADPCM decode residue is draining. Cheap enough
    /// for the per-batch cap gate: one Option test and one bool.
    pub(crate) fn is_producing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.dsp.needs_output_tick())
    }

    pub(crate) fn irq_deadline(&self) -> Option<Sb16IrqDeadline> {
        let active = self.active.as_ref()?;
        Some(Sb16IrqDeadline {
            line: active.mixer.selected_irq(),
            frames: active.dsp.frames_until_next_irq()?,
            rate_hz: u64::from(active.dsp.output_frame_rate()),
        })
    }

    pub(crate) fn advance(
        &mut self,
        micros: u64,
        due_frames: u64,
        dma: &mut DmaController,
        memory: &mut Memory,
    ) -> Option<Sb16Irq> {
        let active = self.active.as_mut()?;
        active.dsp.advance_micros(micros);
        active.sample_stereo();

        let rate = active.dsp.output_frame_rate();
        let irq_line = active.mixer.selected_irq();
        let dma8 = active.mixer.selected_dma_8();
        let dma16 = active.mixer.selected_dma_16();
        let mut irq = None;
        if active.dsp.needs_output_tick() && rate > 0 {
            let is_16bit = active.dsp.is_16bit();
            let channel = if is_16bit { dma16 } else { dma8 };
            let ticked = if is_16bit {
                active.dsp.tick_n_samples(
                    due_frames as usize,
                    || None,
                    || dma.read_word(channel, memory),
                )
            } else {
                active.dsp.tick_n_samples(
                    due_frames as usize,
                    || dma.read_byte(channel, memory),
                    || None,
                )
            };
            if let Some(trace) = &mut active.trace {
                trace.ticked_frames += ticked as u64;
            }
            if active.dsp.take_irq() {
                active.mixer.set_irq_status(active.dsp.is_16bit());
                irq = Some(Sb16Irq { line: irq_line });
            }
        }
        if active.dsp.take_irq() {
            active.mixer.set_irq_status(active.dsp.is_16bit());
            irq = Some(Sb16Irq { line: irq_line });
        }
        if let Some(trace) = &mut active.trace {
            if irq.is_some() {
                trace.irqs += 1;
            }
            trace.micros += micros;
            if trace.micros >= 1_000_000 {
                trace.report(&mut active.dsp, dma8, dma16);
            }
        }
        irq
    }

    pub(crate) fn render_voice(&mut self, window: Sb16RenderWindow) -> Vec<(i32, i32)> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        active.sync_resampler();

        let rate = active.dsp.output_frame_rate();
        let native_frames = if window.elapsed_master_ticks > 0 {
            active
                .render_phase
                .advance(window.elapsed_master_ticks, u64::from(rate)) as usize
        } else {
            (window.fallback_opl_samples as f64 * rate as f64 / OPL_NATIVE_HZ as f64).round()
                as usize
        };

        let (voice_l, voice_r) = active.mixer.voice_gain();
        let mut native = Vec::with_capacity(native_frames);
        for _ in 0..native_frames {
            let Some((left, right)) = active.dsp.drain_frame() else {
                break;
            };
            let left = clamp_i16((i32::from(left) as f32 * voice_l) as i32);
            let right = clamp_i16((i32::from(right) as f32 * voice_r) as i32);
            native.push((i32::from(left), i32::from(right)));
        }
        let produced = active.resampler.process(&native);
        let produced_len = produced.len();

        // Carry the resampler's output across windows instead of forcing each
        // window to consume exactly what it produced.
        //
        // `native_frames` is derived from elapsed guest master ticks, which
        // arrive in bursts as the emulation thread runs; `window.output_frames`
        // is the OPL resampler's count for the same window, driven by the
        // smooth host pacing. The two never agree frame-for-frame, so a window
        // where the guest ran long overproduces and one where it ran short
        // underproduces -- even though they match on average. Discarding the
        // surplus and repeating a frame to cover the shortfall turned that
        // ordinary jitter into a torn stream: measured on a real Quake capture,
        // ~14k frames discarded and ~14k repeated per second against 44.1k
        // rendered, which is the crackle heard on every DSP title.
        //
        // Queuing the surplus turns the disagreement into a few frames of
        // standing latency, and leaves padding for a genuine underrun.
        let overflow = (active.pending.len() + produced_len).saturating_sub(DAC_PENDING_FRAME_CAP);
        for _ in 0..overflow {
            active.pending.pop_front();
        }
        active.pending.extend(produced);

        let take = window.output_frames.min(active.pending.len());
        let mut voice: Vec<(i32, i32)> = active.pending.drain(..take).collect();

        let mut held = if active.dsp.needs_output_tick() {
            active.held_dac_frame
        } else {
            (0, 0)
        };
        if let Some(frame) = voice.last().copied() {
            held = frame;
        }
        let short = window.output_frames - voice.len();
        voice.resize(window.output_frames, held);
        active.held_dac_frame = held;
        if let Some(trace) = &mut active.trace {
            trace.rendered_frames += produced_len as u64;
            // `truncated` now counts only frames lost to the carry-over cap --
            // a real overrun, not per-window jitter. `padded` counts a genuine
            // underrun: the queue ran dry and the last frame was held.
            trace.truncated_frames += overflow as u64;
            trace.padded_frames += short as u64;
            trace.pending_depth = active.pending.len();
        }
        voice
    }

    pub(crate) fn mix_snapshot(&self) -> Ct1745Mix {
        let Some(active) = self.active.as_ref() else {
            return Ct1745Mix::bypass();
        };
        let (fm_l, fm_r) = active.mixer.fm_gain();
        let (master_l, master_r) = active.mixer.master_gain();
        let (outgain_l, outgain_r) = active.mixer.outgain_gain();
        let (cd_l, cd_r) = active.mixer.cd_gain();
        Ct1745Mix {
            active: true,
            fm_l,
            fm_r,
            voice_bus_l: master_l * outgain_l,
            voice_bus_r: master_r * outgain_r,
            cd_l,
            cd_r,
            speaker: active.mixer.speaker_gain(),
        }
    }

    /// (Left, Right) linear gain for the wavetable MIDI leg, mixer registers
    /// `0x50`/`0x51`. A card that is not installed carries no wavetable leg,
    /// but the synth still exists as a host device, so it passes at unity.
    pub(crate) fn wavetable_gain(&self) -> (f32, f32) {
        self.active
            .as_ref()
            .map_or((1.0, 1.0), |active| active.mixer.wavetable_gain())
    }

    pub(crate) fn peek_mixer_register(&self, index: u8) -> Option<u8> {
        self.active
            .as_ref()
            .map(|active| active.mixer.peek_register(index))
    }

    pub(crate) fn cd_levels(&self) -> (u8, u8) {
        self.active
            .as_ref()
            .map_or((0, 0), |active| active.mixer.cd_levels())
    }

    pub(crate) fn set_linked_cd_level(&mut self, level: u8) {
        if let Some(active) = self.active.as_mut() {
            active.mixer.set_cd_levels(level, level);
        }
    }

    #[cfg(test)]
    pub(super) fn test_render_dsp_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        let (voice_l, voice_r) = active.mixer.voice_gain();
        let mut out = Vec::with_capacity(native_samples);
        for _ in 0..native_samples {
            let Some((left, right)) = active.dsp.drain_frame() else {
                break;
            };
            let left = clamp_i16((i32::from(left) as f32 * voice_l) as i32);
            let right = clamp_i16((i32::from(right) as f32 * voice_r) as i32);
            out.push((left, right));
        }
        out
    }

    #[cfg(test)]
    pub(super) fn test_output_frame_rate(&self) -> u32 {
        self.active
            .as_ref()
            .map_or(0, |active| active.dsp.output_frame_rate())
    }

    #[cfg(test)]
    pub(super) fn test_resampler_rate_hz(&self) -> u32 {
        self.active
            .as_ref()
            .map_or(0, |active| active.resampler_rate_hz)
    }

    #[cfg(test)]
    pub(super) fn test_block_remaining(&self) -> u32 {
        self.active
            .as_ref()
            .map_or(0, |active| active.dsp.block_remaining())
    }

    #[cfg(test)]
    pub(super) fn test_is_playing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.dsp.is_playing())
    }

    #[cfg(test)]
    pub(super) fn test_take_irq(&mut self) -> bool {
        self.active
            .as_mut()
            .is_some_and(|active| active.dsp.take_irq())
    }

    #[cfg(test)]
    pub(super) fn test_frames_until_next_irq(&self) -> Option<u64> {
        self.active
            .as_ref()
            .and_then(|active| active.dsp.frames_until_next_irq())
    }

    #[cfg(test)]
    pub(super) fn test_drain_frame(&mut self) -> Option<(i16, i16)> {
        self.active
            .as_mut()
            .and_then(|active| active.dsp.drain_frame())
    }

    #[cfg(test)]
    pub(super) fn test_dsp_matches(&self, other: &Self) -> bool {
        match (&self.active, &other.active) {
            (Some(left), Some(right)) => left.dsp == right.dsp,
            (None, None) => true,
            _ => false,
        }
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
#[path = "sb16_path_test.rs"]
mod tests;
