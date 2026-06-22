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
//! Built from the AD1848K datasheet (`dev_docs/reference/wss/ad1848.txt`):
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

use crate::pcm::{sample_alaw, sample_i16, sample_u8, sample_ulaw};

/// Bounded length of the rendered-frame ring, in stereo frames. Mirrors the SB
/// DSP ring: a rate-match buffer between the per-output-frame producer and the
/// host drainer; on push when full it drops the oldest frame so the block
/// counter and IRQ timing stay correct even if audio fidelity glitches.
const DSP_RING_CAP: usize = 8192;

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
// ponytail: fixed ~128-sample autocal window
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
    /// Rendered stereo frames, drained by the host audio path. See DSP_RING_CAP.
    rendered: VecDeque<(i16, i16)>,
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
        }
    }

    /// Set the IRQ/DMA jumper readback (the machine wires this from the core
    /// `WssConfig` via `Ad1848Config`).
    pub fn set_config(&mut self, config: Ad1848Config) {
        self.config = config;
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
                // ponytail: PIO playback not modeled; DOS WSS drivers use DMA.
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
            0..=3 => {
                // Config/ID region: resource selection is JUMPER-ONLY in this
                // model. The board exposes a fixed IRQ/DMA readback (see
                // `read_config`) and codec-aware detection keys solely off the I12
                // revision, so a guest driver that expects to *select* IRQ/DMA by
                // writing this region (the writable-config-register variant some
                // real WSS board glue offers) has its writes intentionally dropped.
                // There is no datasheet for this region (per the WSS README), so
                // modelling it as writable would mean inventing an encoding; the
                // read-back jumper model is the deliberate fidelity tradeoff.
            }
            4 => self.write_index(value),
            5 => self.write_indexed_data(value),
            6 => {
                // Any write to the Status register clears the sticky INT bit.
                self.status &= !R2_INT;
            }
            7 => {
                // ponytail: PIO playback not modeled; DOS WSS drivers use DMA
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
            }
            self.playing = true;
        } else {
            self.playing = false;
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
    fn is_stereo(&self) -> bool {
        self.regs[IDX_FORMAT] & I8_SM != 0
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

    /// CPU clocks until the next terminal-count IRQ, or `None` when nothing can
    /// raise one (idle, IEN clear, or an invalid `rate_hz`). Mirrors
    /// `SbDsp::clocks_until_next_irq`: it lets a halted CPU fast-forward to the
    /// codec's next interrupt instead of single-stepping the HLT.
    ///
    /// The AD1848 Current Count drains one sample period per output frame, and
    /// the underflow that latches the IRQ happens the period *after* the count
    /// reaches zero -- so a count of `current_count` reaches the interrupt in
    /// `current_count + 1` output frames (the same N+1 cadence `advance_count`
    /// enforces). At `rate_hz` frames per second over a `clock_hz` CPU, that is
    /// `(current_count + 1) * clock_hz / rate_hz` clocks, rounded up and clamped
    /// to at least one so the run loop always advances.
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
    pub fn clocks_until_next_irq(&self, rate_hz: u32, clock_hz: u64) -> Option<u64> {
        if !self.playing || rate_hz == 0 {
            return None;
        }
        if self.regs[IDX_PIN_CONTROL] & I10_IEN == 0 {
            return None;
        }
        if self.trd && (self.status & R2_INT) != 0 {
            return None;
        }
        let frames = u64::from(self.current_count) + 1;
        Some((frames * clock_hz).div_ceil(u64::from(rate_hz)).max(1))
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
    /// for the caller to forward via `take_irq`.
    pub fn tick_sample<B: FnMut() -> Option<u8>>(&mut self, fetch: B) {
        if let Some(frame) = self.render_frame(fetch) {
            if self.rendered.len() >= DSP_RING_CAP {
                self.rendered.pop_front();
            }
            self.rendered.push_back(frame);
        }
    }

    /// Pop the oldest rendered stereo frame for the host audio path, or `None`
    /// when the ring is empty.
    pub fn drain_frame(&mut self) -> Option<(i16, i16)> {
        self.rendered.pop_front()
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
mod tests {
    use super::*;

    // Direct-port offsets (codec = base+4, so config=0..3, codec regs=4..7).
    const R0_INDEX: u16 = 4;
    const R1_DATA: u16 = 5;
    const R2_STATUS: u16 = 6;

    /// Write an indirect register via R0 (index) + R1 (data).
    fn write_indirect(dev: &mut Ad1848, index: u8, value: u8) {
        dev.write_port(R0_INDEX, index);
        dev.write_port(R1_DATA, value);
    }

    /// Read an indirect register via R0 + R1.
    fn read_indirect(dev: &mut Ad1848, index: u8) -> u8 {
        dev.write_port(R0_INDEX, index);
        dev.read_port(R1_DATA)
    }

    #[test]
    fn r0_latches_index_and_mce_trd_bits() {
        let mut dev = Ad1848::default();
        // MCE | TRD | index 8.
        dev.write_port(R0_INDEX, R0_MCE | R0_TRD | 0x08);
        let v = dev.read_port(R0_INDEX);
        assert_eq!(v & R0_INDEX_MASK, 0x08, "index latched");
        assert_ne!(v & R0_MCE, 0, "MCE latched");
        assert_ne!(v & R0_TRD, 0, "TRD latched");
        assert_eq!(v & R0_INIT, 0, "INIT reads clear (always ready)");
    }

    #[test]
    fn indirect_register_round_trips_via_r0_r1() {
        let mut dev = Ad1848::default();
        // I0 (Left Input Control) is a plain stored register.
        write_indirect(&mut dev, 0, 0x5A);
        assert_eq!(read_indirect(&mut dev, 0), 0x5A);
        // I13 (Digital Mix) also round-trips.
        write_indirect(&mut dev, 13, 0xA5);
        assert_eq!(read_indirect(&mut dev, 13), 0xA5);
    }

    #[test]
    fn i8_write_is_gated_by_mce() {
        let mut dev = Ad1848::default();
        // Without MCE the format write is ignored.
        write_indirect(&mut dev, IDX_FORMAT as u8, 0x40);
        assert_eq!(
            read_indirect(&mut dev, IDX_FORMAT as u8),
            0x00,
            "I8 inert without MCE"
        );
        // Set MCE via R0, then the write sticks.
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, 0x40);
        assert_eq!(
            dev.read_port(R1_DATA) & I8_FMT,
            I8_FMT,
            "I8 honored under MCE"
        );
    }

    #[test]
    fn clearing_mce_asserts_then_clears_aci_across_autocal_window() {
        let mut dev = Ad1848::default();
        // Enter MCE, change I8, then clear MCE.
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, 0x40);
        // Clear MCE (index stays 11 to poll ACI).
        dev.write_port(R0_INDEX, IDX_TEST_INIT as u8);
        // ACI asserts immediately on MCE exit.
        assert_ne!(dev.read_port(R1_DATA) & I11_ACI, 0, "ACI set on MCE exit");
        // Drive the autocal countdown directly. ACI counts output sample periods;
        // the per-frame coupling (advance_autocal alongside tick_sample) is
        // verified at the machine-integration layer, out of scope for this core.
        for _ in 0..(AUTOCAL_SAMPLES - 1) {
            dev.advance_autocal();
        }
        assert_ne!(
            dev.read_port(R1_DATA) & I11_ACI,
            0,
            "ACI still set just before window end"
        );
        dev.advance_autocal();
        assert_eq!(
            dev.read_port(R1_DATA) & I11_ACI,
            0,
            "ACI clears after the autocal window"
        );
    }

    #[test]
    fn rate_table_decodes_representative_freq_crystal_combos() {
        let mut dev = Ad1848::default();
        let set_i8 = |dev: &mut Ad1848, cfs: u8, css: u8| {
            let v = ((cfs & 0x07) << I8_CFS_SHIFT) | (css & 1);
            dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
            dev.write_port(R1_DATA, v);
        };
        // XTAL1 (CSS=0): CFS0 -> 8000, CFS6 -> 48000, CFS7 -> 9600.
        set_i8(&mut dev, 0, 0);
        assert_eq!(dev.rate_hz(), 8000);
        set_i8(&mut dev, 6, 0);
        assert_eq!(dev.rate_hz(), 48000);
        set_i8(&mut dev, 7, 0);
        assert_eq!(dev.rate_hz(), 9600);
        // XTAL2 (CSS=1): CFS0 -> 5512, CFS5 -> 44100, CFS3 -> 22050.
        set_i8(&mut dev, 0, 1);
        assert_eq!(dev.rate_hz(), 5512);
        set_i8(&mut dev, 5, 1);
        assert_eq!(dev.rate_hz(), 44100);
        set_i8(&mut dev, 3, 1);
        assert_eq!(dev.rate_hz(), 22050);
        // XTAL1 CFS4/CFS5 are "Not Supported" -> 0.
        set_i8(&mut dev, 4, 0);
        assert_eq!(dev.rate_hz(), 0, "XTAL1 CFS4 unsupported");
    }

    /// Arm playback: 8-bit mono, base count `count`, PEN set.
    fn arm_8bit_mono(dev: &mut Ad1848, count: u16) {
        // I8 = 8-bit unsigned PCM, mono (all format bits clear), needs MCE.
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, 0x00);
        dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
        // Enable the external INT pin (IEN) so terminal-count IRQs forward.
        write_indirect(dev, IDX_PIN_CONTROL as u8, I10_IEN);
        write_indirect(dev, IDX_LOWER_COUNT as u8, (count & 0xFF) as u8);
        write_indirect(dev, IDX_UPPER_COUNT as u8, (count >> 8) as u8);
        write_indirect(dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        // Unmute both DACs at 0 dB so the render values pass through.
        write_indirect(dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(dev, IDX_RIGHT_DAC as u8, 0x00);
    }

    #[test]
    fn format_8bit_unsigned_decodes_and_duplicates_mono() {
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 4);
        // 0x80 -> silence on both channels.
        let f = dev.render_frame(|| Some(0x80));
        assert_eq!(f, Some((0, 0)));
        let f = dev.render_frame(|| Some(0xFF));
        assert_eq!(
            f,
            Some((32_512, 32_512)),
            "0xFF near full positive, mono dup"
        );
    }

    #[test]
    fn format_mulaw_and_alaw_known_points() {
        // mu-law: arm 8-bit companded mu-law (L/C set, FMT clear), mono.
        let mut dev = Ad1848::default();
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, I8_LC); // companded + mu-law
        dev.write_port(R0_INDEX, IDX_FORMAT as u8);
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0x10);
        write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0x00);
        write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
        // mu-law 0xFF is digital silence.
        assert_eq!(
            dev.render_frame(|| Some(0xFF)),
            Some((0, 0)),
            "mu-law 0xFF -> 0"
        );

        // A-law: FMT set + L/C set.
        let mut dev = Ad1848::default();
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, I8_LC | I8_FMT);
        dev.write_port(R0_INDEX, IDX_FORMAT as u8);
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0x10);
        write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0x00);
        write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
        // A-law full-scale positive = 0xAA.
        assert_eq!(
            dev.render_frame(|| Some(0xAA)),
            Some((32_256, 32_256)),
            "A-law 0xAA full scale"
        );
    }

    #[test]
    fn format_16bit_assembles_two_bytes_le_and_orders_stereo() {
        let mut dev = Ad1848::default();
        // I8 = 16-bit linear PCM (FMT set), stereo (S/M set).
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, I8_FMT | I8_SM);
        dev.write_port(R0_INDEX, IDX_FORMAT as u8);
        // base count 8 bytes = 2 stereo frames (4 bytes each).
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 8);
        write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
        write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
        // Stream: L = 0x0001 (lo=01, hi=00), R = 0xFFFE (lo=FE, hi=FF).
        let bytes = [0x01u8, 0x00, 0xFE, 0xFF];
        let mut i = 0;
        let f = dev.render_frame(|| {
            let b = bytes[i % bytes.len()];
            i += 1;
            Some(b)
        });
        assert_eq!(f, Some((1, -2)), "LE assembly + left-before-right");
    }

    #[test]
    fn base_count_terminal_sets_int_raises_irq_and_auto_reloads() {
        let mut dev = Ad1848::default();
        // 8-bit mono, base count 4. Datasheet: the counter decrements each sample
        // period until zero, and the NEXT period after zero underflows and fires
        // the interrupt -> INT after N+1 = 5 frames, then every 5 thereafter.
        arm_8bit_mono(&mut dev, 4);
        let mut irqs = Vec::new();
        for i in 1..=10 {
            let _ = dev.render_frame(|| Some(0x80));
            if dev.take_irq() {
                irqs.push(i);
            }
        }
        // Underflow at frame 5 (INT + reload), again at frame 10 (5 + 5).
        assert_eq!(irqs, vec![5, 10], "INT/IRQ at each underflow (N+1 cadence)");
        assert_ne!(dev.status() & R2_INT, 0, "Status INT sticky after TC");
        assert!(dev.is_playing(), "auto-reload keeps playback armed");
        // Frame 10 reloaded count to base (4); no further decrement this loop.
        assert_eq!(dev.current_count(), 4, "count reloaded from base");
    }

    #[test]
    fn writing_status_clears_int() {
        let mut dev = Ad1848::default();
        // Base count 1 -> underflow after N+1 = 2 sample periods.
        arm_8bit_mono(&mut dev, 1);
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0, no INT yet
        assert_eq!(dev.status() & R2_INT, 0, "no INT before underflow");
        let _ = dev.render_frame(|| Some(0x80)); // underflow -> INT set
        assert_ne!(dev.status() & R2_INT, 0, "INT set at TC");
        dev.write_port(R2_STATUS, 0x00); // any write to R2 acks INT
        assert_eq!(dev.status() & R2_INT, 0, "INT cleared by Status write");
    }

    #[test]
    fn i12_revision_reads_k_grade_pattern() {
        let mut dev = Ad1848::default();
        let rev = read_indirect(&mut dev, IDX_MISC_INFO as u8);
        assert_eq!(rev & 0x0F, 0b1010, "I12 ID3:0 = K-grade 1010");
    }

    #[test]
    fn config_region_reports_id_version_and_irq_dma_jumpers() {
        let mut dev = Ad1848::new(Ad1848Config { irq: 7, dma: 0 });
        assert_eq!(dev.read_port(0), 0x04, "config region board/version ID");
        // High nibble IRQ, low nibble DMA (IRQ7, DMA0 -> 0x70).
        assert_eq!(dev.read_port(1), 0x70, "IRQ/DMA jumper readback");
        dev.set_config(Ad1848Config { irq: 9, dma: 3 });
        assert_eq!(dev.read_port(1), (9 << 4) | 3, "config setter reflected");
    }

    #[test]
    fn pen_arms_only_with_nonzero_base_count() {
        let mut dev = Ad1848::default();
        // PEN set but base count still zero -> not armed.
        write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        assert!(!dev.is_playing(), "PEN without count does not arm");
        // Now load a count; arming re-evaluates on the upper-byte write.
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 4);
        write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
        assert!(dev.is_playing(), "count + PEN arms playback");
        assert_eq!(dev.current_count(), 4);
    }

    #[test]
    fn dac_mute_silences_and_attenuation_scales() {
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 8);
        // Mute the right DAC; left at 0 dB.
        write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(&mut dev, IDX_RIGHT_DAC as u8, DAC_MUTE);
        let f = dev.render_frame(|| Some(0xFF)).unwrap();
        assert_eq!(f.0, 32_512, "left passes at 0 dB");
        assert_eq!(f.1, 0, "right muted");
    }

    /// Arm playback in an arbitrary I8 format byte (MCE-gated write), base count,
    /// PEN set, both DACs unmuted at 0 dB.
    fn arm_format(dev: &mut Ad1848, i8_format: u8, count: u16) {
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, i8_format);
        dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
        // Enable the external INT pin (IEN) so terminal-count IRQs forward.
        write_indirect(dev, IDX_PIN_CONTROL as u8, I10_IEN);
        write_indirect(dev, IDX_LOWER_COUNT as u8, (count & 0xFF) as u8);
        write_indirect(dev, IDX_UPPER_COUNT as u8, (count >> 8) as u8);
        write_indirect(dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        write_indirect(dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(dev, IDX_RIGHT_DAC as u8, 0x00);
    }

    #[test]
    fn count_is_in_sample_periods_not_bytes_for_16bit_stereo() {
        // 16-bit stereo consumes 4 bytes per frame but the Current Count is in
        // sample periods: base count N must fire after N+1 *frames*, independent
        // of width/channels. Base count 3 -> underflow at frame 4, then 8.
        let mut dev = Ad1848::default();
        arm_format(&mut dev, I8_FMT | I8_SM, 3);
        let mut irqs = Vec::new();
        for i in 1..=8 {
            // Each frame pulls 4 bytes (L lo/hi, R lo/hi); value is irrelevant.
            let _ = dev.render_frame(|| Some(0x00));
            if dev.take_irq() {
                irqs.push(i);
            }
        }
        assert_eq!(
            irqs,
            vec![4, 8],
            "16-bit stereo: INT counts sample periods (N+1=4), not bytes"
        );
    }

    #[test]
    fn count_terminal_even_and_odd_base_16bit() {
        // Odd base count cannot overshoot zero now that the count decrements by
        // one sample period per frame. Even base behaves identically.
        for base in [4u16, 5u16] {
            let mut dev = Ad1848::default();
            arm_format(&mut dev, I8_FMT, base); // 16-bit mono (2 bytes/frame)
            let mut first_irq = None;
            for i in 1..=(2 * (base as u32 + 1)) {
                let _ = dev.render_frame(|| Some(0x00));
                if dev.take_irq() && first_irq.is_none() {
                    first_irq = Some(i);
                }
            }
            assert_eq!(
                first_irq,
                Some(base as u32 + 1),
                "16-bit base {base}: INT at frame N+1 exactly"
            );
            assert_eq!(
                dev.current_count(),
                base as u32,
                "post-reload count equals base {base}"
            );
        }
    }

    #[test]
    fn stereo_8bit_orders_left_before_right_and_counts_one_period() {
        // 8-bit stereo: distinct L/R bytes confirm channel order and that one
        // sample period (not two bytes) is consumed per frame.
        let mut dev = Ad1848::default();
        arm_format(&mut dev, I8_SM, 4); // 8-bit linear, stereo
        let before = dev.current_count();
        // L = 0xFF (near +full), R = 0x00 (full negative).
        let bytes = [0xFFu8, 0x00];
        let mut i = 0;
        let f = dev.render_frame(|| {
            let b = bytes[i % bytes.len()];
            i += 1;
            Some(b)
        });
        assert_eq!(f, Some((32_512, -32_768)), "8-bit stereo: left then right");
        assert_eq!(
            before - dev.current_count(),
            1,
            "one sample period consumed per stereo frame"
        );
    }

    #[test]
    fn rate_table_decodes_every_cfs_css_cell() {
        // Data-driven over all 16 (cfs, css) combinations so every table cell --
        // including both XTAL1 "Not Supported" codes (CFS4 and CFS5) -- is pinned.
        const XTAL1: [u32; 8] = [8000, 16000, 27429, 32000, 0, 0, 48000, 9600];
        const XTAL2: [u32; 8] = [5512, 11025, 18900, 22050, 37800, 44100, 33075, 6615];
        let mut dev = Ad1848::default();
        for css in 0u8..=1 {
            for cfs in 0u8..=7 {
                let v = (cfs << I8_CFS_SHIFT) | css;
                dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
                dev.write_port(R1_DATA, v);
                let expected = if css == 0 {
                    XTAL1[cfs as usize]
                } else {
                    XTAL2[cfs as usize]
                };
                assert_eq!(dev.rate_hz(), expected, "rate cell cfs={cfs} css={css}");
            }
        }
    }

    #[test]
    fn attenuation_follows_log_curve_with_sign_mask_and_mute() {
        // apply_atten selects a -1.5 dB-per-step logarithmic gain (10^(-1.5n/20)).
        // Step 0 is unity, so the input passes through unchanged.
        assert_eq!(apply_atten(1000, 0), 1000, "step 0 is unity gain");
        // Step 10 -> 10^(-15/20) = 0.17783: 1000 * 0.17783 ~= 178 (round).
        let n10 = apply_atten(1000, 10);
        let expected_10 = (1000.0 * 10f32.powf(-15.0 / 20.0)).round() as i16;
        assert_eq!(n10, expected_10, "step 10 matches the log law");
        assert!((n10 - 178).abs() <= 1, "step 10 ~= input * 0.1778 ({n10})");
        // The curve decreases monotonically across the 64 steps.
        let mut prev = apply_atten(30_000, 0);
        for n in 1u8..64 {
            let cur = apply_atten(30_000, n);
            assert!(cur <= prev, "step {n} must not be louder than {}", n - 1);
            prev = cur;
        }
        // Negative input keeps its sign under attenuation.
        assert_eq!(apply_atten(-1000, 10), -n10, "negative input keeps sign");
        // Mask: only the low 6 bits select attenuation; bit6 (0x40) is ignored.
        assert_eq!(
            apply_atten(1000, 0x40 | 10),
            apply_atten(1000, 10),
            "atten field is masked to 6 bits"
        );
        // Mute (bit7) silences the channel regardless of the attenuate field.
        assert_eq!(apply_atten(1000, DAC_MUTE | 10), 0, "mute -> 0");
    }

    #[test]
    fn i9_nonmce_write_passes_pen_but_preserves_acal_sdc() {
        let mut dev = Ad1848::default();
        // Set ACAL (and SDC) under MCE so they are latched in I9.
        dev.write_port(R0_INDEX, R0_MCE | IDX_IFACE_CONFIG as u8);
        dev.write_port(R1_DATA, I9_ACAL | I9_SDC);
        // Now without MCE, write I9 with PEN set and ACAL/SDC clear in the value.
        dev.write_port(R0_INDEX, IDX_IFACE_CONFIG as u8); // clears MCE
        dev.write_port(R1_DATA, I9_PEN);
        let i9 = read_indirect(&mut dev, IDX_IFACE_CONFIG as u8);
        assert_ne!(i9 & I9_ACAL, 0, "ACAL preserved across non-MCE I9 write");
        assert_ne!(i9 & I9_SDC, 0, "SDC preserved across non-MCE I9 write");
        assert_ne!(i9 & I9_PEN, 0, "PEN took effect on-the-fly");
    }

    #[test]
    fn r0_reads_40h_mce_set_after_reset() {
        // Datasheet: R0 reads "0100 0000 (40h)" once the codec leaves INIT.
        let dev = Ad1848::default();
        assert_eq!(dev.read_index(), R0_MCE, "post-reset R0 = 0x40 (MCE set)");
    }

    #[test]
    fn drain_frame_pops_pushed_frames() {
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 8);
        dev.tick_sample(|| Some(0xFF));
        dev.tick_sample(|| Some(0x80));
        assert_eq!(dev.drain_frame(), Some((32_512, 32_512)));
        assert_eq!(dev.drain_frame(), Some((0, 0)));
        assert_eq!(dev.drain_frame(), None, "ring drained");
    }

    #[test]
    fn ien_clear_sets_status_int_but_does_not_forward_irq_pin() {
        // Datasheet: the internal INT status bit becomes one on underflow even
        // when IEN is zero, but the external INT pin (the PIC forward) stays
        // inactive. Arm playback WITHOUT setting I10 IEN.
        let mut dev = Ad1848::default();
        dev.write_port(R0_INDEX, R0_MCE | IDX_FORMAT as u8);
        dev.write_port(R1_DATA, 0x00); // 8-bit mono
        dev.write_port(R0_INDEX, IDX_FORMAT as u8); // clear MCE
        // Deliberately leave I10 (Pin Control) IEN clear.
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 1);
        write_indirect(&mut dev, IDX_UPPER_COUNT as u8, 0);
        write_indirect(&mut dev, IDX_IFACE_CONFIG as u8, I9_ACAL | I9_PEN);
        write_indirect(&mut dev, IDX_LEFT_DAC as u8, 0x00);
        write_indirect(&mut dev, IDX_RIGHT_DAC as u8, 0x00);
        // Base count 1 -> underflow after N+1 = 2 sample periods.
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
        let _ = dev.render_frame(|| Some(0x80)); // underflow
        assert_ne!(
            dev.status() & R2_INT,
            0,
            "internal INT status set on underflow regardless of IEN"
        );
        assert!(
            !dev.take_irq(),
            "external INT pin not forwarded while IEN clear"
        );
    }

    #[test]
    fn trd_holds_count_until_int_acked() {
        // Datasheet: the Current Count Register does not decrement while both TRD
        // and the sticky INT bit are set. Arm with TRD set; after the underflow
        // sets INT the count must hold (no back-to-back re-underflow) until the
        // host acks INT via an R2 write.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 1);
        // Latch TRD via a final R0 write (TRD is set with the index on R0 writes;
        // the subsequent renders/reads never touch R0, so the latch persists).
        dev.write_port(R0_INDEX, R0_TRD);
        // base 1 -> underflow on the 2nd frame, count then reloads to 1.
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
        let _ = dev.render_frame(|| Some(0x80)); // underflow: INT set, reload 1
        assert!(dev.take_irq(), "first underflow forwards the IRQ edge");
        let held = dev.current_count();
        // Further frames must NOT decrement or re-underflow while TRD+INT hold.
        for _ in 0..5 {
            let _ = dev.render_frame(|| Some(0x80));
            assert_eq!(dev.current_count(), held, "count holds while TRD && INT");
            assert!(!dev.take_irq(), "no further IRQ while count is held");
        }
        // Ack INT (R2 write) -> transfers resume, count decrements again.
        dev.write_port(R2_STATUS, 0x00);
        let _ = dev.render_frame(|| Some(0x80));
        assert_eq!(
            dev.current_count(),
            held - 1,
            "count resumes decrementing once INT is acked"
        );
    }

    #[test]
    fn dma_underrun_midframe_does_not_advance_count_or_set_int() {
        // 16-bit mono: a frame pulls lo then hi. A fetch that yields the lo byte
        // then None must drop the frame WITHOUT advancing the count, setting INT,
        // or latching an IRQ.
        let mut dev = Ad1848::default();
        arm_format(&mut dev, I8_FMT, 4); // 16-bit mono
        let before = dev.current_count();
        let mut calls = 0;
        let frame = dev.render_frame(|| {
            calls += 1;
            if calls == 1 { Some(0x34) } else { None }
        });
        assert_eq!(frame, None, "partial 16-bit frame dropped");
        assert_eq!(
            dev.current_count(),
            before,
            "count unchanged on mid-frame underrun"
        );
        assert_eq!(dev.status() & R2_INT, 0, "no INT on underrun");
        assert!(!dev.take_irq(), "no IRQ on underrun");

        // 8-bit mono: the very first fetch returns None.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 4);
        let before = dev.current_count();
        let frame = dev.render_frame(|| None);
        assert_eq!(frame, None, "8-bit dry fetch yields no frame");
        assert_eq!(dev.current_count(), before, "count unchanged on dry fetch");
        assert_eq!(dev.status() & R2_INT, 0, "no INT on dry fetch");
        assert!(!dev.take_irq(), "no IRQ on dry fetch");
    }

    #[test]
    fn take_irq_is_one_shot_edge_independent_of_sticky_status() {
        // The sticky Status INT bit (acked by an R2 write) and the irq_pending
        // edge (consumed by take_irq for the PIC forward) are independent.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 1); // underflow after 2 frames
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
        let _ = dev.render_frame(|| Some(0x80)); // underflow
        assert_ne!(dev.status() & R2_INT, 0, "Status INT set at underflow");
        // take_irq is a one-shot edge: true once, then false, while INT stays set.
        assert!(dev.take_irq(), "first take_irq returns the edge");
        assert!(!dev.take_irq(), "edge is one-shot");
        assert_ne!(
            dev.status() & R2_INT,
            0,
            "Status INT still sticky after take_irq"
        );
        // Acking via an R2 write clears Status INT but does not by itself fire a
        // new edge; a fresh underflow latches a new independent edge.
        dev.write_port(R2_STATUS, 0x00);
        assert_eq!(dev.status() & R2_INT, 0, "R2 write clears Status INT");
        assert!(!dev.take_irq(), "no edge from a bare Status ack");
        // Drive to the next underflow (count reloaded to 1 -> 2 more frames).
        let _ = dev.render_frame(|| Some(0x80));
        let _ = dev.render_frame(|| Some(0x80));
        assert!(dev.take_irq(), "fresh underflow latches a new edge");
    }

    #[test]
    fn auto_reload_disarms_when_base_count_rewritten_to_zero() {
        // advance_count's else-branch: an underflow whose base count is now zero
        // disarms playback instead of re-arming.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 1); // base 1: arms with current_count = 1
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0, still armed
        assert!(dev.is_playing(), "still armed after first frame");
        // Zero the base by writing only the LOWER byte (upper was already 0):
        // base() now reads 0, but current_count stays 0 (no upper-byte reload).
        write_indirect(&mut dev, IDX_LOWER_COUNT as u8, 0);
        // current_count == 0, base == 0, still playing -> the next frame enters
        // the underflow period and the zero-base reload branch disarms playback.
        let _ = dev.render_frame(|| Some(0x80));
        assert!(!dev.is_playing(), "zero-base reload disarms playback");
        assert_ne!(dev.status() & R2_INT, 0, "INT still set at the underflow");
    }

    #[test]
    fn i11_and_i12_writes_are_ignored_read_only() {
        let mut dev = Ad1848::default();
        // I12 revision is read-only: a garbage write must not change the read.
        write_indirect(&mut dev, IDX_MISC_INFO as u8, 0xFF);
        assert_eq!(
            read_indirect(&mut dev, IDX_MISC_INFO as u8) & 0x0F,
            0b1010,
            "I12 revision unchanged by write"
        );
        // I11 ACI (bit5) cannot be forced on via a register write while no
        // autocal window is active. Select index 11 first (this clears the
        // power-on MCE and opens the ~128-sample autocal window), then exhaust
        // the window so aci_remaining is None before attempting the spoof write.
        let mut dev = Ad1848::default();
        dev.write_port(R0_INDEX, IDX_TEST_INIT as u8); // clears MCE -> ACI window
        for _ in 0..AUTOCAL_SAMPLES {
            dev.advance_autocal();
        }
        assert_eq!(
            dev.read_port(R1_DATA) & I11_ACI,
            0,
            "autocal window elapsed: ACI clear"
        );
        // Index is still 11 and MCE already clear, so this write neither
        // re-opens the window nor stores into the read-only ACI bit.
        dev.write_port(R1_DATA, I11_ACI);
        assert_eq!(
            dev.read_port(R1_DATA) & I11_ACI,
            0,
            "ACI cannot be spoofed on via an I11 write"
        );
    }

    #[test]
    fn format_dispatch_is_format_sensitive_with_sign_bearing_codes() {
        // The same input byte must decode differently under mu-law vs A-law,
        // proving format() routes to distinct decoders (not one swapped for the
        // other). Also pin a negative-polarity code for each.
        let render_one = |i8_format: u8, byte: u8| -> (i16, i16) {
            let mut dev = Ad1848::default();
            arm_format(&mut dev, i8_format, 16);
            dev.render_frame(move || Some(byte)).unwrap()
        };
        // Non-extreme byte under mu-law vs A-law -> different decoded values.
        let mu = render_one(I8_LC, 0x40);
        let al = render_one(I8_LC | I8_FMT, 0x40);
        assert_ne!(
            mu, al,
            "mu-law and A-law decode the same byte differently (dispatch is format-sensitive)"
        );
        // Negative-polarity codes: mu-law 0x70 (high bit clear -> negative),
        // A-law 0x2A (toggled sign clear -> negative).
        assert!(render_one(I8_LC, 0x70).0 < 0, "mu-law 0x70 is negative");
        assert!(
            render_one(I8_LC | I8_FMT, 0x2A).0 < 0,
            "A-law 0x2A is negative"
        );
    }

    #[test]
    fn clocks_until_next_irq_tracks_count_and_gates() {
        // Idle codec: nothing can wake the CPU.
        let mut dev = Ad1848::default();
        assert_eq!(dev.clocks_until_next_irq(48_000, 1_000_000), None);

        // Armed 8-bit mono with IEN set, base count 4 -> N+1 = 5 frames to the
        // first underflow. At rate == clock_hz that is exactly 5 clocks.
        arm_8bit_mono(&mut dev, 4);
        assert_eq!(dev.clocks_until_next_irq(1_000, 1_000), Some(5));
        // div_ceil: 5 frames at 1000 Hz over a 10_000 Hz clock -> 50 clocks.
        assert_eq!(dev.clocks_until_next_irq(1_000, 10_000), Some(50));
        // Invalid rate (the unsupported XTAL1 cells decode to 0) -> None.
        assert_eq!(dev.clocks_until_next_irq(0, 1_000), None);

        // Same arming but with IEN clear: the external pin never forwards, so no
        // wake. arm_format leaves IEN set, so write I10 back to 0 explicitly.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 4);
        write_indirect(&mut dev, IDX_PIN_CONTROL as u8, 0);
        assert_eq!(
            dev.clocks_until_next_irq(1_000, 1_000),
            None,
            "IEN clear cannot wake the CPU"
        );

        // TRD count-gate: once TRD is set AND the sticky INT bit is pending,
        // advance_count freezes the count, so no further underflow is generated
        // until the host acks INT. The estimator must mirror that and return None,
        // not a finite estimate the producer will never honor.
        let mut dev = Ad1848::default();
        arm_8bit_mono(&mut dev, 1);
        dev.write_port(R0_INDEX, R0_TRD); // latch TRD
        let _ = dev.render_frame(|| Some(0x80)); // count 1 -> 0
        let _ = dev.render_frame(|| Some(0x80)); // underflow: INT set, count held
        assert!(dev.take_irq(), "the underflow forwarded the first edge");
        assert_eq!(
            dev.clocks_until_next_irq(1_000, 1_000),
            None,
            "TRD + sticky INT freezes the count, so no further wake is generated"
        );
        // Acking INT (R2 write) clears the gate; the estimator returns finite again.
        dev.write_port(R2_STATUS, 0x00);
        assert!(
            dev.clocks_until_next_irq(1_000, 1_000).is_some(),
            "acking INT releases the TRD gate so the codec can wake again"
        );
    }
}
