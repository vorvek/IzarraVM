//! Sound Blaster 16 CT1745 mixer chip: the index/data register file at I/O
//! `0x224`/`0x225` that selects the card's IRQ line and DMA channels and sets
//! the output volume. Clean-room derived from the Creative "Sound Blaster 16
//! Hardware Programming Guide" cached at `dev_docs/reference/sb16/` (the
//! greppable MIT-mirrored txt). See `docs/clean-room-audio.md` for the
//! derivation pointers.
//!
//! Scope this slice implements: the IRQ/DMA routing registers (`0x80`/`0x81`),
//! the read-only Interrupt Status register (`0x82`) with its producer-set /
//! guest-ack lifecycle, and the volume registers that actually attenuate the
//! host audio output (master `0x30`/`0x31`, voice `0x32`/`0x33`, output gain
//! `0x41`/`0x42`, plus the CT1345-compatible `0x22`/`0x04` aliases). The other
//! source/tone/AGC registers are stored and returned at their datasheet
//! defaults so a setup utility's read-modify-write round-trips preserve guest
//! writes, but have no audio effect this slice (their sources are not modeled).

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
    outgain_l: u8, // 0x41, 2-bit
    outgain_r: u8, // 0x42, 2-bit
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
    /// the 8-bit DMA / SB-MIDI bit; D1 (0x02) is the 16-bit DMA bit. MPU-401
    /// (D2) is never set this slice (MIDI is out of scope).
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
    /// mix. The CT1345-compatible alias at `0x28` mirrors these registers, so a
    /// guest that programs either path attenuates the same source.
    pub fn cd_gain(&self) -> (f32, f32) {
        (
            VOL5_STEPS[(self.inert[0x36] & 0x1F) as usize],
            VOL5_STEPS[(self.inert[0x37] & 0x1F) as usize],
        )
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

    fn read_register(&self, index: u8) -> u8 {
        match index {
            0x00 => 0x00,                        // Reset Mixer reads 0x00.
            0x04 => self.voice_compat_packed(),  // CT1345 voice alias of 0x32/0x33
            0x22 => self.master_compat_packed(), // CT1345 master alias of 0x30/0x31
            0x30 => self.master_l,
            0x31 => self.master_r,
            0x32 => self.voice_l,
            0x33 => self.voice_r,
            0x41 => self.outgain_l,
            0x42 => self.outgain_r,
            0x80 => self.irq_setup,
            0x81 => self.dma_setup,
            0x82 => self.irq_status,
            _ => self.inert[index as usize],
        }
    }

    fn write_register(&mut self, index: u8, value: u8) {
        match index {
            0x00 => self.reset(),
            // CT1345-compatible 4-bit/channel volume: high nibble = L, low = R,
            // mapped to the 5-bit registers as level<<1 (Guide: these are
            // "mapped to the new volume control registers").
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
            // CT1345-compatible CD volume alias: like 0x04/0x22, it maps into the
            // 5-bit CD registers (0x36/0x37) so cd_gain() sees either path. The
            // compat byte is also kept so a read of 0x28 round-trips.
            0x28 => {
                let (l, r) = unpack_compat(value);
                self.inert[0x36] = l;
                self.inert[0x37] = r;
                self.inert[0x28] = value;
            }
            0x30 => self.master_l = value & 0x1F,
            0x31 => self.master_r = value & 0x1F,
            0x32 => self.voice_l = value & 0x1F,
            0x33 => self.voice_r = value & 0x1F,
            0x41 => self.outgain_l = value & 0x03,
            0x42 => self.outgain_r = value & 0x03,
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

    /// Restore every register to its hardware default (the power-on state the
    /// Guide specifies): IRQ5 / DMA1|DMA5, master and voice 24/24 (-14 dB),
    /// output gain 0/0 (0 dB), and the documented inert defaults.
    fn reset(&mut self) {
        self.latched_index = 0;
        self.irq_setup = 0x02; // IRQ5
        self.dma_setup = 0x22; // DMA1 | DMA5
        self.irq_status = 0;
        self.master_l = 24;
        self.master_r = 24;
        self.voice_l = 24;
        self.voice_r = 24;
        self.outgain_l = 0;
        self.outgain_r = 0;
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
            outgain_l: 0,
            outgain_r: 0,
            inert: [0; 256],
        };
        mixer.reset();
        mixer
    }
}

/// Split a CT1345-compatible packed volume byte into (L, R) 5-bit levels: high
/// nibble = L, low nibble = R, each `<<1` into the 5-bit scale and capped to 31.
fn unpack_compat(value: u8) -> (u8, u8) {
    let l = (((value >> 4) & 0x0F) << 1).min(31);
    let r = ((value & 0x0F) << 1).min(31);
    (l, r)
}

/// Pack two 5-bit levels into a CT1345-compatible byte: each level `>>1` into
/// the 4-bit scale, high nibble = L, low nibble = R.
fn pack_compat(left: u8, right: u8) -> u8 {
    (((left >> 1) & 0x0F) << 4) | ((right >> 1) & 0x0F)
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

/// The stored-but-inert register defaults (Guide, Figure 4-3 and the per-
/// register notes). These have no audio effect this slice but are returned so a
/// setup utility's read-modify-write round-trips preserve guest writes.
fn default_inert() -> [u8; 256] {
    let mut regs = [0u8; 256];
    regs[0x0A] = 0x00; // Mic volume (3-bit), default 0
    regs[0x26] = 0xCC; // MIDI volume (CT1345-compat 4x2), default 12|12
    regs[0x28] = 0x00; // CD volume (CT1345-compat), default 0
    regs[0x2E] = 0x00; // Line volume (CT1345-compat), default 0
    regs[0x34] = 24; // MIDI volume (5-bit), default 24 => -14 dB
    regs[0x35] = 24;
    regs[0x36] = 0; // CD volume (5-bit), default 0
    regs[0x37] = 0;
    regs[0x38] = 0; // Line volume (5-bit), default 0
    regs[0x39] = 0;
    regs[0x3A] = 0; // Mic volume (5-bit), default 0
    regs[0x3B] = 0; // PC Speaker volume (2-bit), default 0
    regs[0x3C] = 0x1F; // Output mixer switches, default all closed
    regs[0x3D] = 0x15; // Input mixer L switches default
    regs[0x3E] = 0x0B; // Input mixer R switches default
    regs[0x3F] = 0; // Input gain L (2-bit), default 0 => 0 dB
    regs[0x40] = 0; // Input gain R (2-bit), default 0 => 0 dB
    regs[0x43] = 0; // Mic AGC, bit0=0 => AGC on (default)
    regs[0x44] = 8; // Treble L (4-bit), default 8 => 0 dB
    regs[0x45] = 8; // Treble R
    regs[0x46] = 8; // Bass L
    regs[0x47] = 8; // Bass R
    regs
}

#[cfg(test)]
#[path = "mixer_test.rs"]
mod tests;
