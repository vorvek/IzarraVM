// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_audio::{Resampler, SbDsp, SbMixer};
use izarravm_bus::Memory;
use izarravm_core::SoundBlasterConfig;

use crate::dma::DmaController;
use crate::timeline::RatePhase;
use crate::{DAC_HZ, OPL_NATIVE_HZ};

#[derive(Debug, Clone)]
struct ActiveSb16Path {
    dsp: SbDsp,
    mixer: SbMixer,
    resampler: Resampler,
    resampler_rate_hz: u32,
    render_phase: RatePhase,
    held_dac_frame: (i32, i32),
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
    voice_bus_l: f32,
    voice_bus_r: f32,
    cd_l: f32,
    cd_r: f32,
}

impl Ct1745Mix {
    const fn bypass() -> Self {
        Self {
            active: false,
            voice_bus_l: 1.0,
            voice_bus_r: 1.0,
            cd_l: 0.0,
            cd_r: 0.0,
        }
    }

    pub(crate) fn mix_opl_voice(self, opl: (i32, i32), voice: (i32, i32)) -> (i32, i32) {
        if !self.active {
            return opl;
        }
        (
            ((opl.0 + voice.0) as f32 * self.voice_bus_l) as i32,
            ((opl.1 + voice.1) as f32 * self.voice_bus_r) as i32,
        )
    }

    pub(crate) fn mix_cd(self, cd: (i32, i32)) -> (i32, i32) {
        (
            (cd.0 as f32 * self.cd_l) as i32,
            (cd.1 as f32 * self.cd_r) as i32,
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
        };
        active.sample_stereo();
        Self {
            active: Some(active),
        }
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
            if is_16bit {
                active.dsp.tick_n_samples(
                    due_frames as usize,
                    || None,
                    || dma.read_word(channel, memory),
                );
            } else {
                active.dsp.tick_n_samples(
                    due_frames as usize,
                    || dma.read_byte(channel, memory),
                    || None,
                );
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
        let mut voice = active.resampler.process(&native);
        let mut held = if active.dsp.needs_output_tick() {
            active.held_dac_frame
        } else {
            (0, 0)
        };
        voice.truncate(window.output_frames);
        if let Some(frame) = voice.last().copied() {
            held = frame;
        }
        voice.resize(window.output_frames, held);
        active.held_dac_frame = held;
        voice
    }

    pub(crate) fn mix_snapshot(&self) -> Ct1745Mix {
        let Some(active) = self.active.as_ref() else {
            return Ct1745Mix::bypass();
        };
        let (master_l, master_r) = active.mixer.master_gain();
        let (outgain_l, outgain_r) = active.mixer.outgain_gain();
        let (cd_l, cd_r) = active.mixer.cd_gain();
        Ct1745Mix {
            active: true,
            voice_bus_l: master_l * outgain_l,
            voice_bus_r: master_r * outgain_r,
            cd_l,
            cd_r,
        }
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
