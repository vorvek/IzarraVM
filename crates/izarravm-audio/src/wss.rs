// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! AD1848K (Windows Sound System) SoundPort stereo codec clean-room core:
//! the indirect register file, the direct-register (Index/Data/Status/PIO)
//! interface, the MCE / autocalibrate (ACI) handshake, the fixed sample-rate
//! table, format decode (8-bit unsigned PCM, mu-law, A-law, 16-bit signed LE),
//! byte-wide DMA playback render, and the base-count -> INT/IRQ + auto-reload.
//!
//! Scope is PLAYBACK ONLY: no capture/ADC, no CS4231 mode-2 / dual-DMA. The
//! companding decoders and linear converters live in `pcm.rs` (shared with the
//! Sound Blaster DSP); everything else here is independent of `SbDsp`.
//!
//! Built from the AD1848K datasheet:
//! - R0 Index Address (INIT/MCE/TRD/IXA3:0) -- datasheet "Index Register".
//! - R2 Status (INT sticky bit, cleared by any write) -- "Status Register".
//! - I8 Clock and Data Format (FMT/L/C/S/M/CFS2:0/CSS) -- IXA3:0 = 8.
//! - I9 Interface Configuration (ACAL/SDC/CEN/PEN) -- IXA3:0 = 9.
//! - I10 Pin Control (IEN gates the external INT pin) -- IXA3:0 = 10.
//! - I11 ACI (autocalibrate-in-progress, read-only bit5) -- IXA3:0 = 11.
//! - I12 Revision ID (K-grade = "1010") -- IXA3:0 = 12.
//! - I14/I15 Upper/Lower Base Count -- IXA3:0 = 14 & 15.

use std::collections::VecDeque;
use std::sync::LazyLock;

use crate::pcm::{push_frame_capped, sample_alaw, sample_i16, sample_u8, sample_ulaw};

/// Bounded length of the rendered-frame ring, in stereo frames. Mirrors the SB
/// R0 (Index Address) bit masks. `INIT` is read-only and `MCE`/`TRD` latch with
/// the 4-bit index on a write.
#[allow(
    dead_code,
    reason = "INIT (busy) state is never modeled; kept to document bit7 and assert it reads clear in tests"
)]
const R0_INIT: u8 = 0x80;
const R0_MCE: u8 = 0x40;
const R0_TRD: u8 = 0x20;
const R0_INDEX_MASK: u8 = 0x0F;

/// Index portion of the Index Address register at reset: index 0. The datasheet
/// specifies the full register reads "0100 0000 (40h)" once the codec leaves
/// INIT, i.e. MCE=1 at power-on (modeled via `mce: true` in `new`); INIT (bit7)
/// is folded in dynamically on read.
const R0_INDEX_IDLE: u8 = 0x00;

/// R2 (Status) INT bit (bit0). Sticky; cleared by any host write to R2.
const R2_INT: u8 = 0x01;
/// R2 initial state after reset, "1100 1100" per the datasheet, with INT clear.
const R2_RESET: u8 = 0xCC;

/// I8 (Clock and Data Format) bit masks.
const I8_FMT: u8 = 0x40; // bit6: 0 = 8-bit / mu-law, 1 = 16-bit / A-law
const I8_LC: u8 = 0x20; // bit5: 0 = linear PCM, 1 = companded
const I8_SM: u8 = 0x10; // bit4: 0 = mono, 1 = stereo
const I8_CFS_MASK: u8 = 0x0E; // bits3:1: clock-frequency-divide select
const I8_CFS_SHIFT: u8 = 1;
const I8_CSS: u8 = 0x01; // bit0: 0 = XTAL1 (24.576), 1 = XTAL2 (16.9344)

/// I9 (Interface Configuration) bit masks.
const I9_ACAL: u8 = 0x08;
#[allow(
    dead_code,
    reason = "SDC stored for round-trip; single-DMA is inert in playback-only scope"
)]
const I9_SDC: u8 = 0x04;
#[allow(
    dead_code,
    reason = "CEN stored for round-trip; capture is out of scope"
)]
const I9_CEN: u8 = 0x02;
const I9_PEN: u8 = 0x01;

/// I10 (Pin Control) Interrupt Enable (IEN, bit1). Gates the external INT pin
/// only: the sticky Status INT bit is set on underflow regardless, but the pin
/// (and thus the PIC forward) goes active only when IEN is set (datasheet:
/// "the internal INT bit will become one on counter underflow even if the
/// external interrupt pin is not enabled, i.e., IEN is zero").
const I10_IEN: u8 = 0x02;

/// I11 (Test and Initialization) ACI bit (bit5), read-only.
const I11_ACI: u8 = 0x20;

/// Indirect register indices used by the playback path.
const IDX_LEFT_DAC: usize = 6;
const IDX_RIGHT_DAC: usize = 7;
const IDX_FORMAT: usize = 8;
const IDX_IFACE_CONFIG: usize = 9;
const IDX_PIN_CONTROL: usize = 10;
const IDX_TEST_INIT: usize = 11;
const IDX_MISC_INFO: usize = 12;
const IDX_UPPER_COUNT: usize = 14;
const IDX_LOWER_COUNT: usize = 15;

/// 6-bit DAC attenuate field mask (I6/I7 LDA5:0 / RDA5:0). LSB = -1.5 dB.
const DAC_ATTEN_MASK: u8 = 0x3F;
/// I6/I7 mute bit (bit7).
const DAC_MUTE: u8 = 0x80;

/// AD1848K K-grade revision ID (I12 ID3:0 = "1010").
const REVISION_K_GRADE: u8 = 0b1010;

/// Config/ID region board/version ID byte (offset 0). A board-integration value,
/// not codec-defined: the WSS standard has no single canonical board ID, so this
/// is a plausible static stand-in. Codec-aware detection keys off the I12
/// revision, so this region just needs to be present.
const WSS_BOARD_ID: u8 = 0x04;

/// Length of the post-MCE autocalibrate window, in output sample periods. The
/// datasheet specifies "approximately 128 sample cycles" during which ACI is
/// held high; system software polls ACI rather than counting cycles.
// Limit: fixed ~128-sample autocal window
const AUTOCAL_SAMPLES: u32 = 128;

/// AD1848 board config/ID region (4 ports at the card base, codec sits at base+4).
/// Carries the IRQ/DMA jumper readback so codec-aware detection can confirm the
/// resources. Defaults match the design (IRQ7, DMA0). This is the device-init
/// config; the user-facing `WssConfig` lives in `izarravm-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ad1848Config {
    pub irq: u8,
    pub dma: u8,
}

impl Default for Ad1848Config {
    fn default() -> Self {
        // base 0x530, IRQ7, DMA0 -- chosen to avoid the SB16 defaults (IRQ5/DMA1).
        Self { irq: 7, dma: 0 }
    }
}

/// Audio sample format decoded from I8 (FMT + L/C bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Pcm8,
    MuLaw,
    ALaw,
    Pcm16,
}

/// One AD1848K codec. Models the indirect register file, the direct-register
/// state machine, the MCE/ACI handshake, and the byte-wide DMA playback engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Ad1848 {
    /// Indirect register file I0..I15.
    regs: [u8; 16],
    /// Latched 4-bit indirect register index (from R0 writes).
    index: u8,
    /// MCE (Mode Change Enable) latch from R0.
    mce: bool,
    /// TRD (Transfer Request Disable) latch from R0.
    trd: bool,
    /// Status (R2) register; INT is the only sticky bit.
    status: u8,
    /// Autocalibrate-in-progress countdown, in output sample periods. `Some(n)`
    /// means ACI is asserted with `n` ticks remaining; `None` means settled.
    aci_remaining: Option<u32>,
    /// IRQ/DMA jumper readback for the config region.
    config: Ad1848Config,
    /// Pending IRQ edge from a terminal-count underflow; taken by `take_irq`.
    irq_pending: bool,
    /// Current DMA count, in sample periods (decrements by one per output frame,
    /// width- and channel-independent per the datasheet). Loaded from I14/I15
    /// when playback arms and auto-reloaded one period after underflow.
    current_count: u32,
    /// Whether the count has been armed (PEN set with a non-zero base count).
    playing: bool,
    /// Rendered stereo frames, drained by the host audio path; capped by
    /// `pcm::push_frame_capped` (drop-oldest rate-match buffer).
    rendered: VecDeque<(i16, i16)>,
    // HLE block data prefetched for batched WSS DMA.
    block_buffer: Option<Vec<u8>>,
    block_buffer_pos: usize,
}

impl Default for Ad1848 {
    fn default() -> Self {
        Self::new(Ad1848Config::default())
    }
}

impl Ad1848 {
    /// Build a codec with the given IRQ/DMA jumper config. Registers come up in
    /// their datasheet reset states for the bits the playback path observes; the
    /// rest are zeroed (round-trip only). I12 carries the K-grade revision.
    pub fn new(config: Ad1848Config) -> Self {
        let mut regs = [0u8; 16];
        // I6/I7 DAC controls power up muted ("1x00 0000").
        regs[IDX_LEFT_DAC] = DAC_MUTE;
        regs[IDX_RIGHT_DAC] = DAC_MUTE;
        // I9 Interface Config reset = "00xx 1000": ACAL set, PEN/CEN clear.
        regs[IDX_IFACE_CONFIG] = I9_ACAL;
        // I12 is not stored: `read_indexed_data` returns REVISION_K_GRADE directly
        // for that index, so a stored byte here would be dead state.
        Self {
            regs,
            index: R0_INDEX_IDLE,
            // Datasheet: R0 reads "0100 0000 (40h)" after reset -- MCE set.
            mce: true,
            trd: false,
            status: R2_RESET,
            aci_remaining: None,
            config,
            irq_pending: false,
            current_count: 0,
            playing: false,
            rendered: VecDeque::new(),
            block_buffer: None,
            block_buffer_pos: 0,
        }
    }

    /// Set the selected IRQ/DMA resources (the machine wires this from the core
    /// `WssConfig` via `Ad1848Config`, and from the CMOS block SNDCTRL.COM
    /// owns). The guest can change the same selection through the config
    /// register; see `write_port`.
    pub fn set_config(&mut self, config: Ad1848Config) {
        self.config = config;
    }

    /// The PIC line the codec's terminal-count interrupt currently forwards to.
    ///
    /// Read this at the point of use rather than caching it: the config
    /// register is writable, so the selection moves at runtime, and a stale
    /// copy would leave the codec reporting one line while interrupting on
    /// another.
    pub fn irq(&self) -> u8 {
        self.config.irq
    }

    /// The 8237 channel the codec currently pulls playback bytes from. Same
    /// caching caveat as [`Ad1848::irq`].
    pub fn dma(&self) -> usize {
        usize::from(self.config.dma)
    }

    /// One indexed register as the guest last left it, without moving the
    /// codec's own index latch. The host-side counterpart to a guest `IN` on
    /// R1, for a test that wants to check what a setup tool programmed without
    /// becoming a second writer. Indices past the file read back as 0.
    pub fn peek_register(&self, index: usize) -> u8 {
        self.regs.get(index).copied().unwrap_or(0)
    }

    // ---- Direct register (port) interface ---------------------------------

    /// Read one of the 8 device ports by `offset`:
    /// - 0..=3: WSS config/ID region (board ID/version + IRQ/DMA jumper readback).
    /// - 4: R0 Index Address (INIT/MCE/TRD/index).
    /// - 5: R1 Indexed Data (the selected indirect register, with read-only
    ///   bits resolved -- e.g. I11 ACI, I12 revision).
    /// - 6: R2 Status. Only the INT bit (bit0) is dynamic; PRDY/SOUR/PL-R/PU-L
    ///   are static reset-value stubs in this DMA-only playback scope.
    /// - 7: R3 PIO Data (stub).
    pub fn read_port(&mut self, offset: u16) -> u8 {
        match offset {
            0..=3 => self.read_config(offset),
            4 => self.read_index(),
            5 => self.read_indexed_data(),
            6 => self.status,
            7 => {
                // Limit: PIO playback not modeled; DOS WSS drivers use DMA.
                // The datasheet's PIO/Capture Data Register reads "1000 0000"
                // when idle, so return 0x80 (the DMA path is the modeled one).
                0x80
            }
            _ => 0xFF,
        }
    }

    /// Write one of the 8 device ports by `offset` (see `read_port`).
    pub fn write_port(&mut self, offset: u16, value: u8) {
        match offset {
            // Config/ID region. Offset 1 SELECTS the codec's resources, in the
            // same nibble encoding `read_config` reports them: high nibble IRQ,
            // low nibble DMA. ReSonique II is the writable-config-register
            // variant of the WSS board glue, which is how real WSS/SB combo
            // chips behaved -- the OPTi 82C930 pairs WSS with SB Pro and lets a
            // driver move between them at runtime, no power cycle.
            //
            // This used to drop the write and expose jumper-only resources, on
            // the grounds that the region has no datasheet and a writable model
            // would mean inventing an encoding. That reasoning does not survive
            // contact with the read side: the readback encoding is already
            // invented, so honouring the identical encoding on write invents
            // nothing further -- and without it the codec is the one device on
            // the card that cannot be re-pointed without restarting the machine,
            // which is what SNDCTRL.COM would otherwise have to tell the user.
            //
            // Selecting resources RE-INITIALISES the codec, because a transfer
            // left running would keep driving the channel the board no longer
            // owns: playback stops, the count disarms, and the sticky INT and
            // pending edge are cleared, exactly as a driver re-programming the
            // board expects before it re-arms.
            1 => {
                self.config.irq = (value >> 4) & 0x0F;
                self.config.dma = value & 0x0F;
                self.playing = false;
                self.current_count = 0;
                self.irq_pending = false;
                self.status &= !R2_INT;
                self.regs[IDX_IFACE_CONFIG] &= !(I9_PEN | I9_CEN);
            }
            0 | 2 | 3 => {
                // Board ID and the mirror offsets stay read-only.
            }
            4 => self.write_index(value),
            5 => self.write_indexed_data(value),
            6 => {
                // Any write to the Status register clears the sticky INT bit.
                self.status &= !R2_INT;
            }
            7 => {
                // Limit: PIO playback not modeled; DOS WSS drivers use DMA
            }
            _ => {}
        }
    }

    /// Config/ID region read. Offset 0 returns a board ID/version byte; offset 1
    /// returns the IRQ/DMA jumper readback (high nibble IRQ, low nibble DMA);
    /// the rest mirror them. The exact ID byte is a plausible static value (the
    /// WSS standard has no single canonical board ID; codec-aware detection
    /// keys off the I12 revision, this region just needs to be present).
    fn read_config(&self, offset: u16) -> u8 {
        match offset {
            0 => WSS_BOARD_ID, // board/version ID
            1 => ((self.config.irq & 0x0F) << 4) | (self.config.dma & 0x0F),
            _ => 0x00,
        }
    }

    /// R0 Index Address read. INIT reflects ongoing initialization (we never
    /// model a busy INIT state, so it reads clear), MCE/TRD reflect the latches,
    /// and the low nibble is the latched index.
    fn read_index(&self) -> u8 {
        let mut v = self.index & R0_INDEX_MASK;
        if self.mce {
            v |= R0_MCE;
        }
        if self.trd {
            v |= R0_TRD;
        }
        // INIT (bit7) stays clear: the codec is always ready in this model, and
        // it is never set, so no explicit masking is needed here.
        v
    }

    /// R0 Index Address write: latch INIT(ignored, read-only)/MCE/TRD + index.
    /// Clearing MCE triggers the autocalibrate handshake.
    fn write_index(&mut self, value: u8) {
        let was_mce = self.mce;
        self.mce = value & R0_MCE != 0;
        self.trd = value & R0_TRD != 0;
        self.index = value & R0_INDEX_MASK;
        if was_mce && !self.mce {
            // Exiting MCE always asserts ACI for the autocal window, regardless
            // of ACAL (datasheet: "ACI will be set on exit from MCE state
            // regardless of whether or not ACAL was set").
            self.aci_remaining = Some(AUTOCAL_SAMPLES);
        }
    }

    /// R1 Indexed Data read. Most registers return their stored byte; the
    /// read-only status bits are injected live: I11 ACI and I12 revision.
    fn read_indexed_data(&self) -> u8 {
        let idx = self.index as usize;
        match idx {
            IDX_TEST_INIT => {
                let mut v = self.regs[idx] & !I11_ACI;
                if self.aci_remaining.is_some() {
                    v |= I11_ACI;
                }
                v
            }
            IDX_MISC_INFO => {
                // ID3:0 = revision; upper bits are reserved/read-only.
                REVISION_K_GRADE
            }
            _ => self.regs[idx],
        }
    }

    /// R1 Indexed Data write. I8 (format/rate) and I9 (interface config) are
    /// MCE-gated, except PEN/CEN in I9 which may be written any time. I11/I12 are
    /// read-only. Other registers store for round-trip.
    fn write_indexed_data(&mut self, value: u8) {
        let idx = self.index as usize;
        match idx {
            IDX_FORMAT => {
                // I8 honored only while MCE is set (DAC muted during MCE).
                if self.mce {
                    self.regs[idx] = value;
                }
            }
            IDX_IFACE_CONFIG => {
                if self.mce {
                    self.regs[idx] = value;
                } else {
                    // PEN/CEN are the on-the-fly exceptions; preserve the rest.
                    let keep = self.regs[idx] & !(I9_PEN | I9_CEN);
                    self.regs[idx] = keep | (value & (I9_PEN | I9_CEN));
                }
                self.update_playback_arm();
            }
            IDX_LOWER_COUNT => self.regs[idx] = value,
            IDX_UPPER_COUNT => {
                // Writing the upper byte loads both into the current count.
                self.regs[idx] = value;
                self.current_count = self.base_count();
                self.update_playback_arm();
            }
            IDX_TEST_INIT | IDX_MISC_INFO => {
                // Read-only (ACI / revision); writes ignored.
            }
            _ => self.regs[idx] = value,
        }
    }

    /// (Re)evaluate whether playback is armed: PEN set and a non-zero base count.
    fn update_playback_arm(&mut self) {
        let pen = self.regs[IDX_IFACE_CONFIG] & I9_PEN != 0;
        if pen && self.base_count() > 0 {
            if !self.playing {
                self.current_count = self.base_count();
                self.block_buffer = None;
                self.block_buffer_pos = 0;
            }
            self.playing = true;
        } else {
            self.playing = false;
            self.block_buffer = None;
            self.block_buffer_pos = 0;
        }
    }

    /// 16-bit base count from I14 (upper) / I15 (lower).
    fn base_count(&self) -> u32 {
        (u32::from(self.regs[IDX_UPPER_COUNT]) << 8) | u32::from(self.regs[IDX_LOWER_COUNT])
    }

    // ---- Format / rate decode ---------------------------------------------

    /// Decode the current audio format from I8 (FMT + L/C bits).
    fn format(&self) -> Format {
        let i8v = self.regs[IDX_FORMAT];
        let companded = i8v & I8_LC != 0;
        let fmt = i8v & I8_FMT != 0;
        match (companded, fmt) {
            (false, false) => Format::Pcm8,
            (false, true) => Format::Pcm16,
            (true, false) => Format::MuLaw,
            (true, true) => Format::ALaw,
        }
    }

    /// True when I8 selects stereo (S/M bit set).
    pub fn is_stereo(&self) -> bool {
        self.regs[IDX_FORMAT] & I8_SM != 0
    }

    pub fn is_16bit(&self) -> bool {
        self.format() == Format::Pcm16
    }

    /// Number of DMA bytes consumed per output frame, used to size HLE blocks.
    pub fn bytes_per_frame(&self) -> usize {
        let per_sample = if self.is_16bit() { 2 } else { 1 };
        if self.is_stereo() {
            per_sample * 2
        } else {
            per_sample
        }
    }

    /// Output sample rate in Hz, decoded from I8's CFS2:0 + CSS bits via the
    /// fixed divide table.
    ///
    /// Returns `0` for the two XTAL1 "Not Supported" combinations (CFS4/CFS5 with
    /// CSS=0), matching the datasheet's "Not Supported" table entries — real
    /// hardware in those combos has no defined sample clock. **`0` means "invalid
    /// rate": callers MUST NOT use it as a divisor.** The machine integration
    /// guards with `.max(1)` (mirroring the SB DSP resampler path) before
    /// deriving any sample-period divisor.
    pub fn rate_hz(&self) -> u32 {
        let i8v = self.regs[IDX_FORMAT];
        let cfs = ((i8v & I8_CFS_MASK) >> I8_CFS_SHIFT) as usize;
        let xtal2 = i8v & I8_CSS != 0; // CSS=1 -> XTAL2 (16.9344 MHz)
        // Datasheet Clock Frequency Divide Select table (CFS index 0..7):
        //   col 0 = XTAL1 (24.576 MHz), col 1 = XTAL2 (16.9344 MHz).
        // 0 -> 8000 / 5512 (5.5125k)        4 -> n/a / 37800
        // 1 -> 16000 / 11025               5 -> n/a / 44100
        // 2 -> 27429 (27.42857k) / 18900   6 -> 48000 / 33075
        // 3 -> 32000 / 22050               7 -> 9600 / 6615
        const XTAL1: [u32; 8] = [8000, 16000, 27429, 32000, 0, 0, 48000, 9600];
        const XTAL2: [u32; 8] = [5512, 11025, 18900, 22050, 37800, 44100, 33075, 6615];
        if xtal2 { XTAL2[cfs] } else { XTAL1[cfs] }
    }

    /// Output frame rate (alias of `rate_hz`; one stereo frame per sample period).
    ///
    /// Inherits `rate_hz`'s `0`-means-invalid contract: the two unsupported XTAL1
    /// clock selects yield `0`, which callers MUST guard before using as a
    /// divisor (the integration path clamps with `.max(1)`).
    pub fn output_frame_rate(&self) -> u32 {
        self.rate_hz()
    }

    // ---- DMA render --------------------------------------------------------

    /// Whether playback is armed (PEN set + non-zero base count).
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Current DMA count used to size the next HLE block.
    pub fn current_dma_count(&self) -> u32 {
        self.current_count
    }

    /// Whether the post-MCE autocalibrate (ACI) window is still retiring. The
    /// integration loop must keep advancing the output-sample clock while this is
    /// true so the ~128-sample window drains even before playback arms; once it is
    /// false and playback is idle there is no per-frame work to do.
    pub fn autocal_active(&self) -> bool {
        self.aci_remaining.is_some()
    }

    /// Current DMA count remaining before terminal count.
    pub fn current_count(&self) -> u32 {
        self.current_count
    }

    /// Output frames until the next terminal-count IRQ, or `None` when nothing
    /// can raise one (idle or IEN clear). The machine timeline converts this
    /// device-domain count to the first causal master-tick deadline.
    ///
    /// The AD1848 Current Count drains one sample period per output frame, and
    /// the underflow that latches the IRQ happens the period *after* the count
    /// reaches zero -- so a count of `current_count` reaches the interrupt in
    /// `current_count + 1` output frames (the same N+1 cadence `advance_count`
    /// enforces).
    ///
    /// The external INT pin is gated by I10 IEN, so a codec armed with IEN clear
    /// sets only the sticky Status bit on underflow and never forwards the line;
    /// such a configuration cannot wake the CPU and returns `None`.
    ///
    /// The TRD count-gate (R0 bit5) is also honored: while TRD is set and the
    /// sticky INT bit is still pending the host's ack, `advance_count` freezes the
    /// Current Count (datasheet: "the DMA Current Counter will not decrement while
    /// both the TRD bit is set and the INT bit is a one"), so no further underflow
    /// -- hence no new IRQ -- is generated until the host acks. The estimator must
    /// mirror that gate and return `None`, or the run loop would fast-forward a
    /// halted CPU to a wake the producer never actually generates.
    pub fn frames_until_next_irq(&self) -> Option<u64> {
        if !self.playing {
            return None;
        }
        if self.regs[IDX_PIN_CONTROL] & I10_IEN == 0 {
            return None;
        }
        if self.trd && (self.status & R2_INT) != 0 {
            return None;
        }
        Some(u64::from(self.current_count) + 1)
    }

    /// Decode one mono sample of the current format from the byte-wide DMA. 8-bit
    /// formats pull one byte; 16-bit pulls two (little-endian: low then high).
    /// Returns `None` if the DMA runs dry mid-sample. Byte fetching is the DMA
    /// buffer addressing concern only; the sample-period counter (I14/I15) is
    /// width-independent, so this returns no byte count.
    fn fetch_sample<B: FnMut() -> Option<u8>>(&mut self, fetch: &mut B) -> Option<i16> {
        match self.format() {
            Format::Pcm8 => Some(sample_u8(fetch()?)),
            Format::MuLaw => Some(sample_ulaw(fetch()?)),
            Format::ALaw => Some(sample_alaw(fetch()?)),
            Format::Pcm16 => {
                let lo = fetch()?;
                let hi = fetch()?;
                let word = u16::from(lo) | (u16::from(hi) << 8);
                Some(sample_i16(word))
            }
        }
    }

    /// Produce one stereo output frame from byte-wide DMA, or `None` if idle /
    /// the DMA underran. Stereo pulls left then right (the AD1848K orders left
    /// before right); mono duplicates its single sample to both channels. The
    /// Current Count decrements by exactly one sample period per output frame
    /// (datasheet: I14/I15 count sample periods, width- and channel-independent);
    /// at terminal count the Status INT bit is set, an IRQ is latched, and the
    /// count auto-reloads (WSS playback is inherently auto-init).
    ///
    /// Underrun contract: `fetch` advances the DMA read pointer as a side effect,
    /// so it MUST supply a *whole* frame atomically -- all 1/2/4 bytes for the
    /// current format/channel selection -- or none of them. If it returns `Some`
    /// for the first byte(s) of a frame and `None` partway through (e.g. the
    /// 16-bit low byte present but the high byte absent), those already-fetched
    /// bytes are consumed and the partial frame is dropped (`None` returned, the
    /// count not advanced, no INT/IRQ), which on the next call desyncs the stream
    /// by the consumed byte(s). The integration layer guarantees whole-frame
    /// availability before calling (mirroring real hardware, which advances the
    /// period counter every period and substitutes midscale on a true underrun
    /// rather than swallowing partial bytes); within this playback-only core that
    /// guarantee is the caller's responsibility.
    pub fn render_frame<B: FnMut() -> Option<u8>>(&mut self, mut fetch: B) -> Option<(i16, i16)> {
        if !self.playing {
            return None;
        }
        let left = self.fetch_sample(&mut fetch)?;
        let right = if self.is_stereo() {
            self.fetch_sample(&mut fetch)?
        } else {
            left
        };
        self.advance_count();
        Some(self.attenuate((left, right)))
    }

    /// Per-output-frame producer entry point: render one frame and push it onto
    /// the rendered ring (drop-oldest on overflow). A `None` frame (idle or DMA
    /// dry) is not pushed. The IRQ raised inside `render_frame` is left pending
    /// for the caller to forward via `take_irq`. Returns whether a frame was
    /// produced.
    pub fn tick_sample<B: FnMut() -> Option<u8>>(&mut self, fetch: B) -> bool {
        if let Some(frame) = self.render_frame(fetch) {
            // The WSS DAC has no drop counter yet; only the SB path is being
            // instrumented in this batch.
            let _ = push_frame_capped(&mut self.rendered, frame);
            true
        } else {
            false
        }
    }

    /// Produce up to `n` frames, stopping on a dry source and returning the
    /// number produced.
    pub fn tick_n_samples<B: FnMut() -> Option<u8>>(&mut self, n: usize, mut fetch: B) -> usize {
        let mut produced = 0;
        while produced < n && self.tick_sample(&mut fetch) {
            produced += 1;
        }
        produced
    }

    /// Pop the oldest rendered stereo frame for the host audio path, or `None`
    /// when the ring is empty.
    pub fn drain_frame(&mut self) -> Option<(i16, i16)> {
        self.rendered.pop_front()
    }

    // HLE block-buffer accessors.
    pub fn take_block_buffer(&mut self) -> Option<Vec<u8>> {
        self.block_buffer.take()
    }

    pub fn set_block_buffer(&mut self, buf: Vec<u8>) {
        self.block_buffer = Some(buf);
        self.block_buffer_pos = 0;
    }

    pub fn block_buffer_pos(&self) -> usize {
        self.block_buffer_pos
    }

    pub fn advance_block_buffer(&mut self, bytes: usize) {
        self.block_buffer_pos += bytes;
    }

    pub fn block_buffer_len(&self) -> usize {
        self.block_buffer.as_ref().map_or(0, |b| b.len())
    }

    pub fn block_buffer(&self) -> Option<&Vec<u8>> {
        self.block_buffer.as_ref()
    }

    /// Advance the Current Count by one sample period. Per the datasheet, the
    /// counter decrements each sample period until zero is reached; the *next*
    /// sample period after zero underflows, which is when the sticky Status INT
    /// is set, an IRQ is latched, and the count auto-reloads from I14/I15. So a
    /// base count of N produces the interrupt after N+1 sample periods. We detect
    /// the underflow as the period entered with the count already at zero, rather
    /// than firing the instant the count reaches zero (which would be one period
    /// early).
    ///
    /// Two datasheet gates apply:
    /// - TRD (R0 bit5): "The DMA Current Counter Register will not decrement
    ///   while both the TRD bit is set and the INT bit is a one." When the host
    ///   uses TRD to pause transfers until it acks INT (R2 write), the count
    ///   holds and no further underflow is generated until the ack.
    /// - IEN (I10 bit1): the sticky Status INT bit is set on underflow
    ///   regardless, but the external interrupt *pin* (the PIC forward latched in
    ///   `irq_pending`) goes active only when IEN is set ("the internal INT bit
    ///   will become one on counter underflow even if ... IEN is zero").
    fn advance_count(&mut self) {
        // TRD count-gate: hold the count (no decrement, no re-underflow) while
        // TRD is set and the sticky INT bit is still pending the host's ack.
        if self.trd && (self.status & R2_INT) != 0 {
            return;
        }
        if self.current_count == 0 {
            // Underflow period: the count was zero entering this sample period.
            // The internal INT *status* bit is sticky and set regardless of IEN.
            self.status |= R2_INT;
            // The external INT *pin* (PIC forward) is gated by IEN (I10 bit1).
            if self.regs[IDX_PIN_CONTROL] & I10_IEN != 0 {
                self.irq_pending = true;
            }
            // Auto-reload. If the base count is zero, leave playback disarmed.
            let base = self.base_count();
            if base > 0 {
                // Reload N; the next N decrements reach zero and the period after
                // underflows, repeating the N+1 sample-period cadence.
                self.current_count = base;
            } else {
                self.playing = false;
            }
        } else {
            self.current_count -= 1;
        }
    }

    /// Apply the I6/I7 DAC output attenuation (and mute) at drain time. The 6-bit
    /// field is -1.5 dB/step from 0 dB (0) to -94.5 dB (63); a set mute bit
    /// silences the channel. The per-step gain follows the AD1848's documented
    /// logarithmic law (`DAC_ATTEN_STEPS`), not a linear approximation.
    fn attenuate(&self, frame: (i16, i16)) -> (i16, i16) {
        let (l, r) = frame;
        (
            apply_atten(l, self.regs[IDX_LEFT_DAC]),
            apply_atten(r, self.regs[IDX_RIGHT_DAC]),
        )
    }

    /// Take and clear a pending terminal-count IRQ (the host ISR acks INT via a
    /// write to R2; this separately tracks the edge for the PIC forward).
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending;
        self.irq_pending = false;
        pending
    }

    /// Current Status register value (for tests / status polls).
    pub fn status(&self) -> u8 {
        self.status
    }

    /// Advance the autocalibrate countdown by one output sample period. The
    /// codec's converters run internally during the ~128-sample post-MCE window
    /// whether or not playback is armed, so the machine calls this once per
    /// output frame (alongside `tick_sample`) to retire the ACI window. When the
    /// countdown elapses, ACI clears.
    ///
    /// ACI-window retiring is coupled to a valid programmed sample rate (the
    /// integration loop ticks at `rate_hz`): a guest that clears MCE while an
    /// unsupported (rate-0) format is selected is non-physical -- real drivers
    /// select a valid rate before clearing MCE -- so that corner is intentionally
    /// not special-cased here.
    pub fn advance_autocal(&mut self) {
        if let Some(n) = self.aci_remaining {
            if n <= 1 {
                self.aci_remaining = None;
            } else {
                self.aci_remaining = Some(n - 1);
            }
        }
    }
}

/// Linear gain per step of the 6-bit I6/I7 DAC attenuate field. The AD1848
/// attenuates -1.5 dB per step from 0 dB (step 0) to -94.5 dB (step 63);
/// `gain = 10**(-1.5 * n / 20)`. Step 0 is exactly 1.0 (unity), and larger
/// steps are quieter. Built like `VOL5_STEPS` in `mixer.rs`.
static DAC_ATTEN_STEPS: LazyLock<[f32; 64]> = LazyLock::new(|| {
    let mut steps = [0f32; 64];
    for (n, step) in steps.iter_mut().enumerate() {
        *step = 10f32.powf(-1.5 * n as f32 / 20.0);
    }
    steps
});

/// Apply one channel's I6/I7 DAC attenuate/mute control to a sample. Mute (bit7)
/// zeroes the channel; otherwise the 6-bit attenuate field selects a -1.5 dB-per-
/// step logarithmic gain from `DAC_ATTEN_STEPS` (the AD1848's documented law).
fn apply_atten(sample: i16, ctrl: u8) -> i16 {
    if ctrl & DAC_MUTE != 0 {
        return 0;
    }
    (f32::from(sample) * DAC_ATTEN_STEPS[(ctrl & DAC_ATTEN_MASK) as usize]).round() as i16
}

#[cfg(test)]
#[path = "wss_test.rs"]
mod tests;
