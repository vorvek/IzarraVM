// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Sound Blaster 16 CT1745 mixer chip: the index/data register file at I/O
//! `0x224`/`0x225` that selects the card's IRQ line and DMA channels and sets
//! the output volume. Clean-room derived from the Creative "Sound Blaster 16
//! Hardware Programming Guide`. See `docs/clean-room-audio.md` for the
//! derivation sources.
//!
//! The mixer models IRQ/DMA routing (`0x80`/`0x81`), the read-only Interrupt
//! Status register (`0x82`), and the volume registers that attenuate host
//! audio: master (`0x30`/`0x31`), voice (`0x32`/`0x33`), FM (`0x34`/`0x35`),
//! CD (`0x36`/`0x37`), PC speaker (`0x3B`), output gain (`0x41`/`0x42`) and the
//! ReSonique 2 wavetable extension (`0x50`/`0x51`). Other source, tone, and AGC
//! registers retain guest writes but have no audio effect because their signal
//! sources are not modeled (there is no line or microphone input).

use std::sync::LazyLock;

/// The SB16 base I/O address (fixed for the Resonique 2).
pub const MIXER_INDEX_PORT: u16 = 0x224;
pub const MIXER_DATA_PORT: u16 = 0x225;

/// Linear gain per level of a 5-bit volume register (`0x30`/`0x31`/`0x32`/`0x33`
/// and friends). The Guide gives the scale as `0..=31 => -62..0 dB` in 2 dB
/// steps; `gain = 10**(dB/20)`. Level 0 is forced to exactly 0.0 so a "0" write
/// is a hard mute rather than the ~-62 dB floor.
static VOL5_STEPS: LazyLock<[f32; 32]> = LazyLock::new(|| {
    let mut steps = [0f32; 32];
    for level in 1u32..32 {
        let db = -62.0 + 2.0 * level as f32;
        steps[level as usize] = 10f32.powf(db / 20.0);
    }
    steps
});

/// Linear gain per level of a 2-bit output-gain register (`0x41`/`0x42`). The
/// Guide gives `0..=3 => 0..+18 dB` in 6 dB steps.
static OUTGAIN_STEPS: LazyLock<[f32; 4]> = LazyLock::new(|| {
    let mut steps = [0f32; 4];
    for level in 0u32..4 {
        steps[level as usize] = 10f32.powf(6.0 * level as f32 / 20.0);
    }
    steps
});

/// Linear gain per level of the 2-bit PC-speaker volume register (`0x3B`).
///
/// The CT1745 gives this leg two bits, not five: the motherboard beeper feeds
/// the card's PC-SPK input and the card offers four coarse positions for it.
/// 86Box's `sb_att_7dbstep_2bits` table is `{164, 6537, 14637, 32767}` out of
/// 32768, i.e. -46.0, -14.0, -7.0 and 0 dB; the spacing is ~7 dB, which is why
/// the table carries that name. The emulator-side attenuation is taken from
/// those dB figures rather than from a 2-bit shift, so the leg is finer than
/// its control -- but the CONTROL stays as coarse as the hardware's, because a
/// guest that reads `0x3B` back must see the same four positions a real card
/// offers. SNDMIXER.COM's PC-SPEAKER fader therefore has four stops, not ten.
///
/// Level 0 is forced to a hard mute rather than 86Box's -46 dB floor, the same
/// deviation (and for the same reason) as [`VOL5_STEPS`] level 0.
static SPK2_STEPS: LazyLock<[f32; 4]> = LazyLock::new(|| {
    let mut steps = [0f32; 4];
    for (level, db) in [(1usize, -14.0f32), (2, -7.0), (3, 0.0)] {
        steps[level] = 10f32.powf(db / 20.0);
    }
    steps
});

/// The CT1745 mixer. The index register (`0x224`) latches which register the
/// next data access (`0x225`) hits; the register file holds the routing and
/// volume state plus the inert store for round-trip-only registers.
#[derive(Debug, Clone, PartialEq)]
pub struct SbMixer {
    latched_index: u8,
    // Routing.
    irq_setup: u8,  // register 0x80
    dma_setup: u8,  // register 0x81
    irq_status: u8, // register 0x82 (read-only, producer-set / guest-ack-cleared)
    // Volume (the registers that attenuate host output this slice).
    master_l: u8,  // 0x30, 5-bit
    master_r: u8,  // 0x31, 5-bit
    voice_l: u8,   // 0x32, 5-bit
    voice_r: u8,   // 0x33, 5-bit
    fm_l: u8,      // 0x34, 5-bit (the FM/MIDI synthesiser bus, i.e. the OPL3)
    fm_r: u8,      // 0x35, 5-bit
    outgain_l: u8, // 0x41, 2-bit
    outgain_r: u8, // 0x42, 2-bit
    wt_l: u8,      // 0x50, 5-bit (ReSonique 2 extension: the wavetable MIDI leg)
    wt_r: u8,      // 0x51, 5-bit
    // Stored-but-inert registers at their datasheet defaults (round-trip only).
    inert: [u8; 256],
}

impl SbMixer {
    /// Build a mixer whose power-on routing matches the given IRQ line and DMA
    /// channels. A guest mixer reset (write `0x00`) still restores the
    /// hardware factory defaults (IRQ5 / DMA1 / DMA5); the host config is
    /// applied once at boot like `SBCONFIG`.
    pub fn with_power_on(irq: u8, dma8: usize, dma16: usize) -> Self {
        Self {
            irq_setup: encode_irq(irq),
            dma_setup: encode_dma(dma8, dma16),
            ..Self::default()
        }
    }

    /// Re-point the card's IRQ line and DMA channels after construction, as a
    /// guest write to mixer registers `0x80`/`0x81` would. Used to apply the
    /// resource assignment SNDCTRL.COM persisted in CMOS, which the host has to
    /// re-apply on every boot because the mixer is built before that NVRAM is
    /// read back.
    pub fn set_routing(&mut self, irq: u8, dma8: usize, dma16: usize) {
        self.irq_setup = encode_irq(irq);
        self.dma_setup = encode_dma(dma8, dma16);
    }

    /// Decode the selected IRQ line from register `0x80`. Bit layout (Guide,
    /// "Configuring DMA and Interrupt Settings"): D0=IRQ2, D1=IRQ5, D2=IRQ7,
    /// D3=IRQ10. Only one bit is meaningful; if several are set the lowest set
    /// bit wins. If no valid bit is set the card keeps the hardware default
    /// line (IRQ5) so audio never silently loses its interrupt.
    pub fn selected_irq(&self) -> u8 {
        let bits = self.irq_setup;
        if bits & 0x01 != 0 {
            2
        } else if bits & 0x02 != 0 {
            5
        } else if bits & 0x04 != 0 {
            7
        } else if bits & 0x08 != 0 {
            10
        } else {
            5
        }
    }

    /// Decode the selected 8-bit DMA channel from register `0x81` low bits.
    /// D0=DMA0, D1=DMA1, D3=DMA3; lowest set bit wins; defaults to DMA1.
    pub fn selected_dma_8(&self) -> usize {
        let bits = self.dma_setup;
        if bits & 0x01 != 0 {
            0
        } else if bits & 0x02 != 0 {
            1
        } else if bits & 0x08 != 0 {
            3
        } else {
            1
        }
    }

    /// Decode the selected 16-bit DMA channel from register `0x81` high bits.
    /// D5=DMA5, D6=DMA6, D7=DMA7; lowest set bit wins. If no 16-bit bit is set
    /// the DSP 4.x "16-bit sound over an 8-bit channel" mode applies: the armed
    /// `0xBx` command draws words from the selected 8-bit channel.
    pub fn selected_dma_16(&self) -> usize {
        let bits = self.dma_setup;
        if bits & 0x20 != 0 {
            5
        } else if bits & 0x40 != 0 {
            6
        } else if bits & 0x80 != 0 {
            7
        } else {
            self.selected_dma_8()
        }
    }

    /// Set the Interrupt Status register (`0x82`) source bit for the armed DMA
    /// mode right when the producer forwards the IRQ to the PIC. D0 (0x01) is
    /// the 8-bit DMA / SB-MIDI bit and D1 (0x02) is the 16-bit DMA bit. The HLE
    /// MPU-401 uses its own port state and does not drive this mixer register.
    pub fn set_irq_status(&mut self, is_16bit: bool) {
        self.irq_status = if is_16bit { 0x02 } else { 0x01 };
    }

    /// Clear the Interrupt Status source bit. Called when the guest ISR
    /// acknowledges the DSP interrupt by reading `0x22E` (8-bit) or `0x22F`
    /// (16-bit).
    pub fn clear_irq_status(&mut self) {
        self.irq_status = 0;
    }

    /// (Left, Right) linear voice gain from registers `0x32`/`0x33`, applied to
    /// the DSP/DAC voice path at drain time.
    pub fn voice_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.voice_l & 0x1F) as usize],
            VOL5_STEPS[(self.voice_r & 0x1F) as usize],
        )
    }

    /// (Left, Right) linear FM gain from registers `0x34`/`0x35`. This is the
    /// synthesiser bus: on a CT1745 it attenuates the OPL3, and it is the only
    /// control a title has to balance its music against its digital effects.
    /// Duke Nukem 3D sets it from `MusicVolume` and its voice level from
    /// `FXVolume`; leaving it inert put the music 12 dB above where the card
    /// would have placed it and buried the sound effects underneath.
    pub fn fm_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.fm_l & 0x1F) as usize],
            VOL5_STEPS[(self.fm_r & 0x1F) as usize],
        )
    }

    /// (Left, Right) linear master gain from registers `0x30`/`0x31`, applied
    /// to the summed output alongside the output gain.
    pub fn master_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.master_l & 0x1F) as usize],
            VOL5_STEPS[(self.master_r & 0x1F) as usize],
        )
    }

    /// (Left, Right) linear output gain from registers `0x41`/`0x42`.
    pub fn outgain_gain(&self) -> (f32, f32) {
        (
            OUTGAIN_STEPS[(self.outgain_l & 0x03) as usize],
            OUTGAIN_STEPS[(self.outgain_r & 0x03) as usize],
        )
    }

    /// SB Pro output mode (register `0x0E`) bit1: stereo when set, mono when
    /// clear. The DSP samples this to interleave two bytes per 8-bit frame. The
    /// output-filter bit (bit5) is cosmetic and ignored. Register `0x0E` is an
    /// inert store, so this read round-trips a guest's write. Reset leaves it 0
    /// (mono): `default_inert` does not set `0x0E`.
    pub fn sbpro_stereo(&self) -> bool {
        self.inert[0x0E] & 0x02 != 0
    }

    /// (Left, Right) linear CD-Audio gain from registers `0x36`/`0x37` (the 5-bit
    /// CD volume), applied to the Red Book stream the ATAPI drive streams into the
    /// mix. The compat aliases at `0x28` (CT1345) and `0x08` (SB1/2) mirror these
    /// registers, so a guest that programs any path attenuates the same source.
    pub fn cd_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.inert[0x36] & 0x1F) as usize],
            VOL5_STEPS[(self.inert[0x37] & 0x1F) as usize],
        )
    }

    /// Linear PC-speaker gain from register `0x3B` (2-bit, D7-D6).
    ///
    /// The beeper is motherboard hardware, but its output is wired to the
    /// card's PC-SPK mixer input, so it is a card leg like any other: this
    /// attenuation, then the master, then the summing node. Mono, because the
    /// input is.
    pub fn speaker_gain(&self) -> f32 {
        SPK2_STEPS[(self.inert[0x3B] >> 6) as usize]
    }

    /// Raw 2-bit PC-speaker level from register `0x3B` (D7-D6).
    pub fn speaker_level(&self) -> u8 {
        self.inert[0x3B] >> 6
    }

    /// (Left, Right) linear wavetable-MIDI gain from registers `0x50`/`0x51`.
    ///
    /// This pair is a ReSonique 2 extension, not a CT1745 register: a real
    /// CT1745 leaves `0x50`/`0x51` undecoded, and its `0x34`/`0x35` "MIDI" pair
    /// is the FM synthesiser bus (which is why [`fm_gain`](Self::fm_gain) owns
    /// the OPL3). The Izarra 3000's card also carries a wavetable MPU whose
    /// synthesis is mixed on-card, the way an AWE32 mixes its EMU8000, and that
    /// leg had no level control at all. It gets one here, on the card's own
    /// register file, at the card's own 5-bit level scale, so a guest programs
    /// it with exactly the sequence it already uses for every other leg.
    pub fn wavetable_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.wt_l & 0x1F) as usize],
            VOL5_STEPS[(self.wt_r & 0x1F) as usize],
        )
    }

    /// Raw (Left, Right) CD-Audio levels from registers `0x36`/`0x37`.
    pub fn cd_levels(&self) -> (u8, u8) {
        (self.inert[0x36] & 0x1F, self.inert[0x37] & 0x1F)
    }

    /// Set the raw CD-Audio levels without touching the guest's selected mixer
    /// register or interrupt status. This is the host-control seam for the GUI;
    /// guest reads through either the SB16 registers or the CT1345/SB1 aliases
    /// see the same levels, because those reads are derived from this store.
    pub fn set_cd_levels(&mut self, left: u8, right: u8) {
        self.inert[0x36] = left.min(31);
        self.inert[0x37] = right.min(31);
    }

    /// Log every CT1745 mixer register write (`IZARRAVM_SB_CMD_TRACE`, the same
    /// gate the DSP command and 8237 mode traces use, so one run shows the whole
    /// setup: routing, volumes and transfer). Mixer writes happen a handful of
    /// times per title, so the env lookup behind a `OnceLock` costs nothing that
    /// matters. Routing registers are decoded inline because "which line did the
    /// card end up on" is the question this trace exists to answer.
    fn trace_write(&self, index: u8, value: u8) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_SB_CMD_TRACE").is_some()) {
            return;
        }
        let note = match index {
            0x00 => " (RESET MIXER -> factory IRQ5/DMA1|5)".to_owned(),
            0x80 | 0x81 => format!(
                " -> irq={} dma8={} dma16={}",
                self.selected_irq(),
                self.selected_dma_8(),
                self.selected_dma_16()
            ),
            _ => String::new(),
        };
        eprintln!("[SBMIX] reg={index:#04x} value={value:#04x}{note}");
    }

    /// Decode the `0x224`/`0x225` port pair. Returns `true` if the port belongs
    /// to the mixer.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            MIXER_INDEX_PORT => {
                self.latched_index = value;
                true
            }
            MIXER_DATA_PORT => {
                self.write_register(self.latched_index, value);
                self.trace_write(self.latched_index, value);
                true
            }
            _ => false,
        }
    }

    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            // A read of the index port is undefined on hardware; returning the
            // latched index is harmless and helps a probing routine.
            MIXER_INDEX_PORT => Some(self.latched_index),
            MIXER_DATA_PORT => Some(self.read_register(self.latched_index)),
            _ => None,
        }
    }

    /// Read a register WITHOUT touching the latched index, for a host that
    /// wants to see what the guest programmed. The guest's own path is
    /// [`read_port`](Self::read_port); this is the same decode with no side
    /// effect, so a test can look at the register file between guest writes
    /// without becoming one of them.
    pub fn peek_register(&self, index: u8) -> u8 {
        self.read_register(index)
    }

    fn read_register(&self, index: u8) -> u8 {
        match index {
            0x00 => 0x00, // Reset Mixer reads 0x00.
            // SB1/2 aliases: one 4-bit level for both channels. Read back the
            // left channel's nibble, so a read-modify-write on the alias sees
            // the level the alias last set (or that 0x30/0x34/0x36 set since).
            0x02 => compat_nibble(self.master_l),
            0x06 => compat_nibble(self.fm_l),
            0x08 => compat_nibble(self.inert[0x36]),
            0x0A => self.inert[0x3A] >> 5, // mic: derived from the 5-bit register
            0x04 => self.voice_compat_packed(), // CT1345 voice alias of 0x32/0x33
            0x22 => self.master_compat_packed(), // CT1345 master alias of 0x30/0x31
            0x26 => pack_compat(self.fm_l, self.fm_r), // SB Pro FM alias of 0x34/0x35
            0x28 => pack_compat(self.inert[0x36], self.inert[0x37]),
            // The 5-bit level registers carry the level in D7-D3 (D2-D0
            // reserved); the 2-bit gain registers carry it in D7-D6. See the
            // note on `write_register`.
            0x30 => self.master_l << 3,
            0x31 => self.master_r << 3,
            0x32 => self.voice_l << 3,
            0x33 => self.voice_r << 3,
            0x34 => self.fm_l << 3,
            0x35 => self.fm_r << 3,
            0x36 => self.inert[0x36] << 3,
            0x37 => self.inert[0x37] << 3,
            0x41 => self.outgain_l << 6,
            0x42 => self.outgain_r << 6,
            0x50 => self.wt_l << 3,
            0x51 => self.wt_r << 3,
            0x80 => self.irq_setup,
            0x81 => self.dma_setup,
            0x82 => self.irq_status,
            _ => self.inert[index as usize],
        }
    }

    fn write_register(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.reset(),
            // CT1345/SB Pro-compatible 4-bit-per-channel volume: high nibble = L,
            // low = R, mapped into the 5-bit registers (Guide: these are "mapped
            // to the new volume control registers"; 86Box `sb_ct1745_mixer_write`
            // writes `(regs[n] & 0xf0) | 0x8` into the left 5-bit register and
            // `((regs[n] & 0xf) << 4) | 0x8` into the right).
            0x04 => {
                let (l, r) = unpack_compat(value);
                self.voice_l = l;
                self.voice_r = r;
            }
            0x22 => {
                let (l, r) = unpack_compat(value);
                self.master_l = l;
                self.master_r = r;
            }
            // The FM/MIDI bus has the same alias pair as master and voice, and it
            // is the one an SB Pro-era title actually uses -- the 0x34/0x35 SB16
            // registers did not exist on the CT1345. Leaving 0x26 in the inert
            // store meant such a title got NO music attenuation at all (stuck at
            // the 0 dB power-on level), the same defect 0x34/0x35 had.
            0x26 => {
                let (l, r) = unpack_compat(value);
                self.fm_l = l;
                self.fm_r = r;
            }
            // CT1345-compatible CD volume alias: like 0x04/0x22, it maps into the
            // 5-bit CD registers (0x36/0x37) so cd_gain() sees either path.
            0x28 => {
                let (l, r) = unpack_compat(value);
                self.set_cd_levels(l, r);
            }
            // SB1/2 aliases: a single 4-bit level driving BOTH channels (86Box
            // cases 0x02/0x06/0x08 write the same `((regs[n] & 0xf) << 4) | 0x8`
            // byte to the left and right 5-bit registers).
            0x02 => {
                let level = compat_level(value);
                self.master_l = level;
                self.master_r = level;
            }
            0x06 => {
                let level = compat_level(value);
                self.fm_l = level;
                self.fm_r = level;
            }
            0x08 => {
                let level = compat_level(value);
                self.set_cd_levels(level, level);
            }
            // Mic: the SB Pro 3-bit register maps into the SB16 5-bit one with the
            // 86Box shape `(regs[0x0a] << 5) | 0x18`. Both are inert for audio (no
            // mic source is modeled), but they are one control, so 0x3A is the
            // single store and a read of 0x0A is derived from it.
            0x0A => self.inert[0x3A] = ((value & 0x07) << 5) | 0x18,
            // 5-bit level registers: the level is LEFT-aligned in D7-D3 and
            // D2-D0 are reserved (Guide, Figure 4-3; DOSBox-X `CTMIXER_Write`
            // uses `val>>3`, 86Box `sb_ct1745_mixer_write` uses `regs[n]>>3`).
            // Reading the low five bits instead is not an off-by-a-few-dB
            // rounding error: every level that is a multiple of four writes a
            // byte whose low five bits are zero, so `& 0x1F` HARD-MUTED the
            // channel. Duke Nukem 3D writes `FXVolume * 31 / 255 << 3` -- 228
            // becomes level 27, byte 0xD8, which the old decode read as 24.
            0x30 => self.master_l = value >> 3,
            0x31 => self.master_r = value >> 3,
            0x32 => self.voice_l = value >> 3,
            0x33 => self.voice_r = value >> 3,
            0x34 => self.fm_l = value >> 3,
            0x35 => self.fm_r = value >> 3,
            0x36 => self.set_cd_levels(value >> 3, self.inert[0x37]),
            0x37 => self.set_cd_levels(self.inert[0x36], value >> 3),
            // 2-bit gain registers: level in D7-D6 (86Box `regs[0x41]>>6`).
            0x41 => self.outgain_l = value >> 6,
            0x42 => self.outgain_r = value >> 6,
            // The ReSonique 2 wavetable leg (see `wavetable_gain`). Same 5-bit
            // D7-D3 encoding as 0x30-0x37, so the same `>> 3` decode.
            0x50 => self.wt_l = value >> 3,
            0x51 => self.wt_r = value >> 3,
            0x80 => self.irq_setup = value,
            0x81 => self.dma_setup = value,
            0x82 => { /* Interrupt Status is read-only; writes are ignored. */ }
            _ => self.inert[index as usize] = value,
        }
    }

    fn voice_compat_packed(&self) -> u8 {
        pack_compat(self.voice_l, self.voice_r)
    }

    fn master_compat_packed(&self) -> u8 {
        pack_compat(self.master_l, self.master_r)
    }

    /// Restore every register to its hardware default: IRQ5 / DMA1|DMA5, output
    /// gain 0/0 (0 dB), and the documented inert defaults.
    ///
    /// Master, voice, FM and CD power on at level 31 (0 dB), NOT the level 24
    /// (-14 dB) the Guide documents. This is a deliberate, and not a novel,
    /// deviation: DOSBox-X's `CTMIXER_Reset` sets `master/dac/fm/cda = 31` and
    /// 86Box's reset block carries the comment "Changed defaults from -14dB to
    /// 0dB". The reason is that a DOS title which never touches the mixer -- the
    /// common case, since BLASTER tells it nothing about volume -- would
    /// otherwise play 14 dB down, and a title that sets only its own voice level
    /// stacks a second 14 dB on top of that. The card's analog output stage
    /// (`output_gain`) is the host's volume control; the mixer should start out of
    /// the way.
    ///
    /// The CD level (`0x36`/`0x37`, in the inert store) follows the same rule and
    /// for the same reason: DOSBox-X's `CTMIXER_Reset` sets `cda` to 31 and 86Box
    /// resets `0x36`/`0x37` to `0xF8`. It used to power on hard-muted, which meant
    /// a CD-audio title that never programmed the mixer -- the usual case, since
    /// the drive plays Red Book through the card's CD-in with no guest involvement
    /// -- got silence from a working drive. The GUI front panel and `set_cd_levels`
    /// remain the host's control over this line.
    ///
    /// The wavetable extension (`0x50`/`0x51`) powers on at level 31 for the
    /// same reason as the rest: it is a new control over a leg that until now
    /// had none, so its power-on position has to be the level that leaves the
    /// leg exactly where it already was.
    fn reset(&mut self) {
        self.latched_index = 0;
        self.irq_setup = 0x02; // IRQ5
        self.dma_setup = 0x22; // DMA1 | DMA5
        self.irq_status = 0;
        self.master_l = 31;
        self.master_r = 31;
        self.voice_l = 31;
        self.voice_r = 31;
        self.fm_l = 31;
        self.fm_r = 31;
        self.outgain_l = 0;
        self.outgain_r = 0;
        self.wt_l = 31;
        self.wt_r = 31;
        self.inert = default_inert();
    }
}

impl Default for SbMixer {
    fn default() -> Self {
        let mut mixer = Self {
            latched_index: 0,
            irq_setup: 0,
            dma_setup: 0,
            irq_status: 0,
            master_l: 0,
            master_r: 0,
            voice_l: 0,
            voice_r: 0,
            fm_l: 0,
            fm_r: 0,
            outgain_l: 0,
            outgain_r: 0,
            wt_l: 0,
            wt_r: 0,
            inert: [0; 256],
        };
        mixer.reset();
        mixer
    }
}

/// Widen a compat 4-bit level to the 5-bit scale.
///
/// The hardware mapping sets the low bit: 86Box writes the 5-bit register as
/// `(nibble << 4) | 0x8`, i.e. level `(nibble << 1) | 1`. Dropping that `| 1`
/// (a plain `nibble << 1`) is not a rounding detail at the top of the scale --
/// it makes the loudest value a compat register can express level 30, which is
/// -2 dB, so an SB Pro-era title asking for FULL volume is quietly attenuated
/// and its read-back of the 5-bit register disagrees with what the same level
/// written natively would show (0xF0 vs 0xF8).
fn compat_level(nibble: u8) -> u8 {
    ((nibble & 0x0F) << 1) | 1
}

/// Narrow a 5-bit level back to the compat 4-bit scale (the inverse of
/// [`compat_level`], which round-trips because `((n << 1) | 1) >> 1 == n`).
fn compat_nibble(level: u8) -> u8 {
    (level >> 1) & 0x0F
}

/// Split a CT1345-compatible packed volume byte into (L, R) 5-bit levels: high
/// nibble = L, low nibble = R.
fn unpack_compat(value: u8) -> (u8, u8) {
    (compat_level(value >> 4), compat_level(value))
}

/// Pack two 5-bit levels into a CT1345-compatible byte: high nibble = L, low
/// nibble = R.
fn pack_compat(left: u8, right: u8) -> u8 {
    (compat_nibble(left) << 4) | compat_nibble(right)
}

/// Encode an IRQ line number as the `0x80` Interrupt Setup byte.
fn encode_irq(irq: u8) -> u8 {
    match irq {
        2 => 0x01,
        5 => 0x02,
        7 => 0x04,
        10 => 0x08,
        _ => 0x02, // default IRQ5
    }
}

/// Encode (8-bit channel, 16-bit channel) as the `0x81` DMA Setup byte.
fn encode_dma(dma8: usize, dma16: usize) -> u8 {
    let low = match dma8 {
        0 => 0x01,
        1 => 0x02,
        3 => 0x08,
        _ => 0x02,
    };
    let high = match dma16 {
        5 => 0x20,
        6 => 0x40,
        7 => 0x80,
        _ => 0x20,
    };
    low | high
}

/// The stored-but-inert register defaults. These have no audio effect this slice
/// but are returned so a setup utility's read-modify-write round-trips preserve
/// guest writes.
///
/// The bytes are HARDWARE-ENCODED: each is what a read of that register returns
/// at power-on, with the level sitting in the field the register actually uses,
/// not the bare level. A 4-bit tone control lives in D7-D4, so its 0 dB centre
/// reads 0x80 and not 8; the 2-bit speaker volume reads 0x80; and the mic level
/// carries 86Box's `(reg << 5) | 0x18` shape. Storing bare levels here made the
/// inert file contradict the very convention the live registers above are
/// decoded with, and a guest reading a tone control back saw 0x08, a value the
/// card cannot produce. Reference: 86Box `sb_ct1745_mixer_write` reset block.
///
/// `0x36`/`0x37` (CD) are the exception in FORM, not in convention: they are
/// live registers whose store happens to live here, so they hold the decoded
/// LEVEL, and `read_register` left-aligns them on the way out exactly as it does
/// for `0x30`-`0x35`.
fn default_inert() -> [u8; 256] {
    let mut regs = [0u8; 256];
    // 0x02/0x06/0x08/0x0A/0x22/0x04/0x26/0x28 are aliases whose reads are derived
    // from the register they map into, so they have no default of their own.
    regs[0x2E] = 0x00; // Line volume (CT1345-compat), default 0
    regs[0x36] = 31; // CD volume (5-bit), 0 dB (level, not the D7-D3 byte)
    regs[0x37] = 31;
    regs[0x38] = 0x00; // Line volume (5-bit), default 0
    regs[0x39] = 0x00;
    regs[0x3A] = 0x18; // Mic volume, (0 << 5) | 0x18
    // PC Speaker volume, 2-bit field in D7-D6 (86Box: "steps of 64"). LIVE, not
    // inert: `speaker_gain` decodes it. The store stays here because the whole
    // byte round-trips -- D5-D0 are don't-care on the card and are neither
    // masked on the way in nor cleared on the way out, matching 86Box, so a
    // guest's read-modify-write sees its own byte back.
    regs[0x3B] = 0x80;
    regs[0x3C] = 0x1F; // Output mixer switches, default all closed
    regs[0x3D] = 0x15; // Input mixer L switches default
    regs[0x3E] = 0x0B; // Input mixer R switches default
    regs[0x3F] = 0x00; // Input gain L, 2-bit field in D7-D6 => 0 dB
    regs[0x40] = 0x00; // Input gain R
    regs[0x43] = 0x00; // Mic AGC, bit0=0 => AGC on (default)
    regs[0x44] = 0x80; // Treble L, 4-bit field in D7-D4, 8 => 0 dB
    regs[0x45] = 0x80; // Treble R
    regs[0x46] = 0x80; // Bass L
    regs[0x47] = 0x80; // Bass R
    regs
}

#[cfg(test)]
#[path = "mixer_test.rs"]
mod tests;
