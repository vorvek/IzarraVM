// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! VEGA video-card ownership and guest-visible routing.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::Arc;

use izarravm_bus::{BusWidth, Memory};
use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};
use izarravm_video::{
    CGA_FB_SIZE, DAC_ENTRIES, Distira, HGC_FB_SIZE, MARGO_FRAME_HZ, MARGO_MMIO_SIZE,
    MARGO_VRAM_SIZE, Margo, MargoDisplay, MargoScanTiming, TextFrame, VGA_MODE13H_BASE,
    VGA_MONO_TEXT_BASE, VGA_PLANAR_WINDOW_SIZE, VGA_TEXT_MEMORY_SIZE, Vga, VgaRaster, VideoMode,
};

use crate::video_params::{
    DISTIRA_PCI_BAR_SIZE, DISTIRA_PCI_DEVICE_ID, DISTIRA_PCI_LFB_OFFSET, DISTIRA_PCI_REVISION,
    DISTIRA_PCI_TEX_OFFSET, DISTIRA_PCI_VENDOR_ID,
};
use crate::{
    ActiveDisplay, DISTIRA_MMIO_BASE, MARGO_LFB_BASE, MARGO_MMIO_BASE, PresentedFrameUpdate,
    VideoHostMetricsSnapshot,
};

/// Margo's legacy extension index/data pair. Both sit in the VGA port block but
/// are undefined on plain VGA, the same slot real SuperVGA chips took for their
/// own extensions; the VGA core leaves them unclaimed, so `Vega` decodes them
/// before delegating.
///
/// KEEP IN SYNC with `roms/izbios-vbepm.inc`, which hardcodes these numbers and
/// the register indices below into the protected-mode stub. `vega_margo_ext_test`
/// executes the assembled stub against this decode, so a drift between the two
/// fails a test rather than silently producing a stub that writes nowhere.
pub(crate) const MARGO_EXT_INDEX: u16 = 0x03cb;
pub(crate) const MARGO_EXT_DATA: u16 = 0x03cd;

pub(crate) const MARGO_EXT_SEGSEL_LO: u8 = 0x00;
pub(crate) const MARGO_EXT_SEGSEL_HI: u8 = 0x01;
pub(crate) const MARGO_EXT_DISPX_LO: u8 = 0x02;
pub(crate) const MARGO_EXT_DISPX_HI: u8 = 0x03;
pub(crate) const MARGO_EXT_DISPY_LO: u8 = 0x04;
pub(crate) const MARGO_EXT_DISPY_HI: u8 = 0x05;
/// Write bit 0 to latch (DISPX, DISPY) into the display start.
pub(crate) const MARGO_EXT_DISPCTL: u8 = 0x06;

/// What a video-aperture byte write did, reported to `MachineBus::write_memory`.
///
/// `ArmedBlit` comes ONLY from a write that MOVED the Margo blitter's modeled
/// busy time -- never from a write that merely happened while the engine was
/// already busy. That distinction is load-bearing: a blit can outlast a batch (a
/// 640x480 FILL models ~1.54 ms of busy time against a 1 ms cap), and a guest
/// overlapping CPU rendering with a long blit must not re-stamp the origin on
/// every framebuffer store. Every aperture other than Margo's MMIO window
/// returns `Accepted` without even loading the busy counter, so the planar /
/// chain-4 / LFB / text write paths pay nothing for this.
///
/// WHAT THE CALLER OWES IT: the busy time is measured from the instant of this
/// write, but the machine drains Margo once per batch, with the whole batch's
/// nanoseconds. So the bus answers `ArmedBlit` by crediting the in-batch offset
/// (`Margo::credit_busy_ns`); see `MachineBus::write_memory_byte_recorded`. The
/// EDGE is tested in both directions -- a RESET that lowers busy time while an
/// earlier blit drains is also a new origin -- which is why this says "moved"
/// and not "increased".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoWrite {
    /// No aperture claimed the address; the caller falls back to RAM.
    Unclaimed,
    /// An aperture took the byte.
    Accepted,
    /// An aperture took the byte and it moved the Margo blitter's busy time.
    ArmedBlit,
}

/// The video host counters, one `Cell` per number.
///
/// Deliberately NOT a `Cell<VideoHostMetricsSnapshot>`. Every legacy VGA direct
/// access and every aperture hit passes through `record_direct_access`, and a
/// snapshot cell makes each of those a read-modify-write of the whole 80-byte
/// struct to bump one counter. The snapshot is assembled once, in
/// [`Vega::host_metrics`], which only the profiler calls.
#[derive(Debug, Default)]
struct VideoHostCounters {
    lfb_direct_read_bytes: Cell<u64>,
    lfb_direct_write_bytes: Cell<u64>,
    lfb_slow_read_bytes: Cell<u64>,
    lfb_slow_write_bytes: Cell<u64>,
    banked_direct_read_bytes: Cell<u64>,
    banked_direct_write_bytes: Cell<u64>,
    banked_slow_read_bytes: Cell<u64>,
    banked_slow_write_bytes: Cell<u64>,
    scanout_rows_converted: Cell<u64>,
    scanout_pixels_converted: Cell<u64>,
}

impl VideoHostCounters {
    #[inline]
    fn add(counter: &Cell<u64>, count: u64) {
        counter.set(counter.get().saturating_add(count));
    }

    fn snapshot(&self) -> VideoHostMetricsSnapshot {
        VideoHostMetricsSnapshot {
            margo_lfb_direct_read_bytes: self.lfb_direct_read_bytes.get(),
            margo_lfb_direct_write_bytes: self.lfb_direct_write_bytes.get(),
            margo_lfb_slow_read_bytes: self.lfb_slow_read_bytes.get(),
            margo_lfb_slow_write_bytes: self.lfb_slow_write_bytes.get(),
            margo_banked_direct_read_bytes: self.banked_direct_read_bytes.get(),
            margo_banked_direct_write_bytes: self.banked_direct_write_bytes.get(),
            margo_banked_slow_read_bytes: self.banked_slow_read_bytes.get(),
            margo_banked_slow_write_bytes: self.banked_slow_write_bytes.get(),
            margo_scanout_rows_converted: self.scanout_rows_converted.get(),
            margo_scanout_pixels_converted: self.scanout_pixels_converted.get(),
        }
    }
}

#[derive(Debug)]
struct PresentedArgbCache {
    words: Arc<Vec<u32>>,
    palette: [u32; DAC_ENTRIES],
    width: usize,
    height: usize,
    margo_generation: u64,
    owner: Option<ActiveDisplay>,
    valid: bool,
}

impl Default for PresentedArgbCache {
    fn default() -> Self {
        Self {
            words: Arc::new(Vec::new()),
            palette: [0; DAC_ENTRIES],
            width: 0,
            height: 0,
            margo_generation: 0,
            owner: None,
            valid: false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Vega {
    vga: Box<Vga>,
    margo: Margo,
    distira: Distira,
    margo_active: bool,
    margo_linear: bool,
    margo_bank: u16,
    // Margo's legacy extension register file, addressed through the index/data
    // pair at MARGO_EXT_INDEX/MARGO_EXT_DATA. It exists so the VBE 2.0
    // protected-mode stub can be real code: a client that far-calls SetWindow
    // has no INT 10h available, so the bank has to be reachable over I/O. The
    // registers ALIAS the state the INT 10h path already owns rather than
    // shadowing it, so a guest that sets the mode through INT 10h and then banks
    // through the stub cannot observe two different windows.
    margo_ext_index: u8,
    margo_ext_disp_x: u16,
    margo_ext_disp_y: u16,
    // Recon instrument (2026-08-12): how many accepted 4F02 mode sets asked for
    // the LINEAR framebuffer (request bit 0x4000) versus the banked 64 KB
    // window. Diagnostic only -- deliberately OUTSIDE `canonical_projection`,
    // like every other counter, so a capture cannot differ over it.
    //
    // WHY IT IS HERE: the LFB and the banked window are served by two different
    // (and both currently slow) bus paths, so which one a fixture uses decides
    // which of them is worth making fast. NASCAR was measured banked (2,325
    // 4F05 window calls per 4G cycles); GP2 was unmeasured, and GP2 is the other
    // VESA fixture. `IZARRAVM_VBE_TRACE=1` prints each mode set as it happens so
    // a SHORT run answers the question without waiting for a full schedule.
    vbe_mode_sets_linear: u32,
    vbe_mode_sets_banked: u32,
    presented_argb_cache: RefCell<PresentedArgbCache>,
    host_counters: VideoHostCounters,
    distira_command: u16,
    distira_mem_base: u32,
    distira_init_enable: u32,
    /// The two halves of the device-window classification, counted separately
    /// so a test can tell which of them a change removed.
    ///
    /// `questions` counts every time the BUS asks whether an address is a device
    /// window (`MachineBus::is_device_window` and `memory_wait_states_device`).
    /// `gauntlet_entries` counts the subset that got past the extended-RAM
    /// screen and actually walked `rom_offset` plus Vega's ten predicates. The
    /// fetch fast path removes QUESTIONS; the screen removes GAUNTLET ENTRIES.
    /// One counter could not separate those, and a test that cannot separate
    /// them passes for the wrong reason.
    ///
    /// Test-only, and deliberately NOT counted inside `owns_memory`: the
    /// debug-only `claims_no_byte_in` assertion also calls `owns_memory`, so a
    /// counter there would move for a reason that has nothing to do with the
    /// path under test.
    ///
    /// They exist because what they pin is a mechanism no value comparison can
    /// see: an extended-RAM address classifies as "not a device window" either
    /// way, and only the cost of reaching that answer changes.
    #[cfg(test)]
    device_window_questions: Cell<u64>,
    #[cfg(test)]
    device_window_gauntlet_entries: Cell<u64>,
}

impl Default for Vega {
    fn default() -> Self {
        Self {
            vga: Box::new(Vga::default()),
            margo: Margo::default(),
            distira: Distira::new(),
            margo_active: false,
            margo_linear: false,
            margo_bank: 0,
            margo_ext_index: 0,
            margo_ext_disp_x: 0,
            margo_ext_disp_y: 0,
            vbe_mode_sets_linear: 0,
            vbe_mode_sets_banked: 0,
            presented_argb_cache: RefCell::new(PresentedArgbCache::default()),
            host_counters: VideoHostCounters::default(),
            // Izarra has no PCI BIOS yet, so Distira powers on with its fixed
            // BAR decoded. Guest drivers may still rewrite command and BAR0.
            distira_command: 0x0002,
            distira_mem_base: DISTIRA_MMIO_BASE & !(DISTIRA_PCI_BAR_SIZE - 1),
            distira_init_enable: 0,
            #[cfg(test)]
            device_window_questions: Cell::new(0),
            #[cfg(test)]
            device_window_gauntlet_entries: Cell::new(0),
        }
    }
}

impl Vega {
    pub(crate) fn legacy(&self) -> &Vga {
        &self.vga
    }

    pub(crate) fn legacy_mut(&mut self) -> &mut Vga {
        &mut self.vga
    }

    #[cfg(test)]
    pub(crate) fn margo(&self) -> &Margo {
        &self.margo
    }

    #[cfg(test)]
    pub(crate) fn margo_mut(&mut self) -> &mut Margo {
        &mut self.margo
    }

    #[cfg(test)]
    pub(crate) fn distira_mut(&mut self) -> &mut Distira {
        &mut self.distira
    }

    pub(crate) fn select_legacy(&mut self) {
        self.margo_active = false;
        self.distira.disable_display();
    }

    pub(crate) fn set_vbe_mode(&mut self, request: u16) -> bool {
        let mode = request & 0x01ff;
        if !self.margo.set_mode(mode) {
            return false;
        }
        // VBE 4F02, BX bit 15: display memory is cleared on a mode set unless
        // the caller asks to keep it. Without this, whatever the previous
        // owner left in VRAM (the graphical POST frame, most visibly) scans
        // out through the new mode's palette and pitch as stale garbage.
        if request & 0x8000 == 0 {
            self.margo.vram_mut_noted().fill(0);
        }
        self.margo_active = true;
        self.margo_linear = request & 0x4000 != 0;
        self.margo_bank = 0;
        if self.margo_linear {
            self.vbe_mode_sets_linear = self.vbe_mode_sets_linear.saturating_add(1);
        } else {
            self.vbe_mode_sets_banked = self.vbe_mode_sets_banked.saturating_add(1);
        }
        trace_vbe_mode_set(
            request,
            self.vbe_mode_sets_linear,
            self.vbe_mode_sets_banked,
        );
        true
    }

    /// Accepted 4F02 mode sets so far, as (linear, banked). Recon instrument;
    /// see the field comments. Test-only: a fixture run reads the same numbers
    /// off `IZARRAVM_VBE_TRACE`, which is the form that answers the question
    /// without a build that carries a reporting path nobody else calls.
    #[cfg(test)]
    pub(crate) fn vbe_mode_set_window_counts(&self) -> (u32, u32) {
        (self.vbe_mode_sets_linear, self.vbe_mode_sets_banked)
    }

    pub(crate) fn current_vbe_mode(&self) -> Option<u16> {
        self.margo_active
            .then(|| self.margo.display().mode | if self.margo_linear { 0x4000 } else { 0 })
    }

    pub(crate) fn vbe_window_control(&mut self, bx: u16, bank: u16) -> Result<u16, u16> {
        if !self.margo_active {
            return Err(0x014f);
        }
        if self.margo_linear {
            return Err(0x034f);
        }
        if bx as u8 != 0 {
            return Err(0x014f);
        }
        match (bx >> 8) as u8 {
            0x00 => {
                self.margo_bank = bank;
                Ok(bank)
            }
            0x01 => Ok(self.margo_bank),
            _ => Err(0x014f),
        }
    }

    /// Compute a display start from a pixel coordinate and latch it. Shared by
    /// `INT 10h 4F07h` and the extension register file's `DISPCTL` so the two
    /// entry points cannot drift; the pitch and depth come from the active Margo
    /// mode, which is why the arithmetic lives on the device and not in the
    /// protected-mode stub.
    pub(crate) fn program_display_start_xy(&mut self, x: u16, y: u16) -> bool {
        let display = self.margo.display();
        if u32::from(x) >= display.width {
            return false;
        }
        let depth = izarravm_video::bytes_per_pixel(display.bpp);
        let start = u64::from(y)
            .saturating_mul(u64::from(display.pitch))
            .saturating_add(u64::from(x).saturating_mul(u64::from(depth)));
        if start > u64::from(u32::MAX) {
            return false;
        }
        self.program_display_start(start as u32)
    }

    fn read_margo_ext(&self) -> u8 {
        match self.margo_ext_index {
            MARGO_EXT_SEGSEL_LO => self.margo_bank as u8,
            MARGO_EXT_SEGSEL_HI => (self.margo_bank >> 8) as u8,
            MARGO_EXT_DISPX_LO => self.margo_ext_disp_x as u8,
            MARGO_EXT_DISPX_HI => (self.margo_ext_disp_x >> 8) as u8,
            MARGO_EXT_DISPY_LO => self.margo_ext_disp_y as u8,
            MARGO_EXT_DISPY_HI => (self.margo_ext_disp_y >> 8) as u8,
            // DISPCTL is a strobe: the latch it fires has no state to read back,
            // and the pending flag belongs to the frame boundary, not the guest.
            _ => 0,
        }
    }

    fn write_margo_ext(&mut self, value: u8) {
        let wide = |half: u16, byte: u8, high: bool| {
            if high {
                (half & 0x00ff) | (u16::from(byte) << 8)
            } else {
                (half & 0xff00) | u16::from(byte)
            }
        };
        match self.margo_ext_index {
            MARGO_EXT_SEGSEL_LO => self.margo_bank = wide(self.margo_bank, value, false),
            MARGO_EXT_SEGSEL_HI => self.margo_bank = wide(self.margo_bank, value, true),
            MARGO_EXT_DISPX_LO => self.margo_ext_disp_x = wide(self.margo_ext_disp_x, value, false),
            MARGO_EXT_DISPX_HI => self.margo_ext_disp_x = wide(self.margo_ext_disp_x, value, true),
            MARGO_EXT_DISPY_LO => self.margo_ext_disp_y = wide(self.margo_ext_disp_y, value, false),
            MARGO_EXT_DISPY_HI => self.margo_ext_disp_y = wide(self.margo_ext_disp_y, value, true),
            MARGO_EXT_DISPCTL if value & 0x01 != 0 => {
                // The latch applies at the next frame either way, so bit 7 (the
                // caller's "wait for retrace" request) needs nothing here: real
                // hardware does not stall the CPU for it, and neither does this.
                let (x, y) = (self.margo_ext_disp_x, self.margo_ext_disp_y);
                let _ = self.program_display_start_xy(x, y);
            }
            _ => {}
        }
    }

    pub(crate) fn set_margo_mode_640x480x8(&mut self) {
        self.margo.set_mode_640x480x8();
        self.margo_active = true;
        self.margo_linear = true;
        self.margo_bank = 0;
        self.distira.disable_display();
    }

    pub(crate) fn load_margo_test_pattern(&mut self) {
        self.set_margo_mode_640x480x8();
        let display = self.margo.display();
        let width = display.width as usize;
        let height = display.height as usize;
        let pitch = display.pitch as usize;
        // Straight into the store, so the damage tracker is told up front; the
        // mode set already marked everything, but that is its business, not
        // this pattern writer's.
        let vram = self.margo.vram_mut_noted();
        for y in 0..height {
            for x in 0..width {
                vram[y * pitch + x] = ((x + y) & 0xff) as u8;
            }
        }
    }

    /// Borrows the six outer routing/configuration latches for canonical
    /// comparison. Vga, Margo, and Distira internals belong to their own
    /// future owners.
    pub(crate) fn canonical_projection(&self) -> CanonicalVega<'_> {
        CanonicalVega { vega: self }
    }

    /// The Vega-owned init-enable latch and the mirror Distira keeps of it.
    /// Capture rejects a snapshot when the two have drifted apart.
    pub(crate) fn distira_init_enable_mirror(&self) -> (u32, u32) {
        (self.distira_init_enable, self.distira.init_enable())
    }

    pub(crate) fn active_display(&self) -> ActiveDisplay {
        if self.distira.display_enabled() {
            ActiveDisplay::Distira
        } else if self.margo_active {
            ActiveDisplay::MargoLfb
        } else {
            ActiveDisplay::VgaRaster
        }
    }

    pub(crate) fn margo_display(&self) -> MargoDisplay {
        self.margo.display()
    }

    pub(crate) fn margo_active(&self) -> bool {
        self.margo_active
    }

    pub(crate) fn host_metrics(&self) -> VideoHostMetricsSnapshot {
        self.host_counters.snapshot()
    }

    /// Attribute one admitted direct access to whichever Margo aperture owns it.
    ///
    /// Every legacy VGA direct access also arrives here, so the ownership
    /// question is answered BEFORE any counter is touched and the not-Margo
    /// answer costs a range compare plus one short-circuited `margo_active`
    /// test. The counters themselves are separate cells so the bump is a single
    /// word, not a whole-snapshot read-modify-write.
    #[inline]
    pub(crate) fn record_direct_access(
        &self,
        address: u32,
        bytes: usize,
        kind: izarravm_bus::BusAccessKind,
    ) {
        use izarravm_bus::BusAccessKind::{DataRead, DataWrite};
        let counters = &self.host_counters;
        let counter = if self.margo_banked_window_at(address) {
            match kind {
                DataRead => &counters.banked_direct_read_bytes,
                DataWrite => &counters.banked_direct_write_bytes,
                _ => return,
            }
        } else if self.margo_active
            && self.margo_linear
            && margo_lfb_offset(address, bytes).is_some()
        {
            match kind {
                DataRead => &counters.lfb_direct_read_bytes,
                DataWrite => &counters.lfb_direct_write_bytes,
                _ => return,
            }
        } else {
            return;
        };
        VideoHostCounters::add(counter, bytes as u64);
    }

    pub(crate) fn margo_lfb_direct_bytes(&self, address: u32, bytes: usize) -> Option<&[u8]> {
        if !self.margo_active || !self.margo_linear {
            return None;
        }
        let start = margo_lfb_offset(address, bytes)?;
        Some(&self.margo.vram()[start..start + bytes])
    }

    pub(crate) fn margo_lfb_direct_bytes_mut(
        &mut self,
        address: u32,
        bytes: usize,
    ) -> Option<&mut [u8]> {
        if !self.margo_active || !self.margo_linear {
            return None;
        }
        let start = margo_lfb_offset(address, bytes)?;
        self.margo.note_vram_write(start, bytes);
        Some(&mut self.margo.vram_mut()[start..start + bytes])
    }

    pub(crate) fn active_video_mode(&self) -> VideoMode {
        self.vga.active_mode()
    }

    pub(crate) fn mode13_direct_page_available(&self) -> bool {
        self.margo_banked_window_key().is_some()
            || (!self.margo_banked_window_at(VGA_MODE13H_BASE)
                && self.vga.mode13h_direct_page_available())
    }

    pub(crate) fn mode13_direct_page(&mut self, physical_page: u32) -> Option<*mut u8> {
        // Read side of the banked window, for the same reason as the write side:
        // `mode13_direct_page_available` is forced false while Margo is banked.
        if let Some(ptr) = self.margo_banked_direct_page(physical_page) {
            return Some(ptr);
        }
        // STRUCTURAL, read side; see `direct_write_page` for why this is a line
        // rather than a comment about caller behaviour.
        if self.margo_banked_window_at(VGA_MODE13H_BASE) {
            return None;
        }
        if !self.mode13_direct_page_available() {
            return None;
        }
        let offset = physical_page.checked_sub(VGA_MODE13H_BASE)? as usize;
        if offset & 0x0fff != 0 {
            return None;
        }
        self.vga.mode13h_direct_page_ptr(offset)
    }

    /// Non-zero when a host pointer for the legacy aperture is available.
    ///
    /// PROBE/COPY AGREEMENT: this is what `direct_vga_bytes` consults to decide
    /// `bulk_direct`, while `direct_write_page` produces the pointer. If this said
    /// yes where that says no, every bulk attempt would be a wasted 4 KiB source
    /// read followed by a fallback -- and because the REP loop re-enters at L-1,
    /// that waste repeats at L, L-1, ..., 1. Quadratic, not incidental. The banked
    /// arm below is what keeps the two in step; `T6` pins it.
    pub(crate) fn direct_write_token(&self) -> u8 {
        if self.margo_banked_window_key().is_some() {
            // Distinct from the VGA's own tokens so a transition between the two
            // owners always MOVES the token, never merely re-uses a value.
            return 0xff;
        }
        if self.margo_banked_window_at(VGA_MODE13H_BASE) {
            0
        } else {
            self.vga.direct_write_token()
        }
    }

    /// Diagnostic only, for the wipe census: the index register in force for `port`.
    pub(crate) fn port_index_selector(&self, port: u16) -> u8 {
        self.vga.port_index_selector(port)
    }

    pub(crate) fn direct_write_page(&mut self, physical_page: u32) -> Option<*mut u8> {
        // Margo's banked window first: while it owns the aperture the VGA token is
        // 0 by construction, so without this arm the function refuses exactly the
        // pages the slice exists to serve.
        if let Some(ptr) = self.margo_banked_direct_page(physical_page) {
            return Some(ptr);
        }
        // STRUCTURAL: while Margo owns the window the VGA never serves it, even if
        // the Margo arm above declined (an out-of-store bank, say). Without this
        // line that case falls through to the VGA, which is unreachable today only
        // because every caller pre-validates -- a guarantee downgraded to a caller
        // contract. One line keeps it a guarantee.
        if self.margo_banked_window_at(VGA_MODE13H_BASE) {
            return None;
        }
        if self.direct_write_token() == 0 {
            return None;
        }
        let offset = physical_page.checked_sub(VGA_MODE13H_BASE)? as usize;
        if offset & 0x0fff != 0 {
            return None;
        }
        self.vga.direct_write_page_ptr(offset)
    }

    /// Dirty-notify the engine that OWNS the address, not whichever one the
    /// address range historically belonged to.
    ///
    /// `Vga::note_direct_write` opens with `debug_assert_ne!(direct_write_token(),
    /// 0)`, and that token is 0 by construction while Margo is banked -- so once
    /// the banked window is admitted, routing every aperture write into the VGA
    /// fires that assert on the FIRST write in any debug build. In release it is
    /// worse: legacy mode13 dirty state gets mutated from Margo offsets, and if the
    /// guest was in mode 13h before the 4F02 the token is 1, the assert passes, and
    /// `mode13_linear_authoritative` is set from Margo offsets -- silent corruption.
    ///
    /// While Margo owns the window its writes are Margo's business. Nothing is
    /// forwarded to the VGA, which is also what keeps `arm_graphics_settle` off
    /// this path.
    pub(crate) fn note_direct_write(&mut self, address: u32, bytes: usize) {
        if self.margo_banked_window_at(address) {
            if let Some(offset) = self.margo_banked_window_offset(address) {
                self.margo.note_vram_write(offset, bytes);
            }
            return;
        }
        let Some(offset) = address.checked_sub(VGA_MODE13H_BASE) else {
            return;
        };
        self.vga.note_direct_write(offset as usize, bytes);
    }

    /// The page-granular twin. The JIT reports dirty PAGES with no address, so the
    /// owner is decided by who owns the window itself.
    pub(crate) fn note_direct_write_pages(&mut self, dirty_pages: u16) {
        if self.margo_banked_window_at(VGA_MODE13H_BASE) {
            if let Some(base) = self.margo_banked_window_key() {
                for page in 0..16 {
                    if dirty_pages & (1 << page) != 0 {
                        self.margo
                            .note_vram_write(base as usize + page * 0x1000, 0x1000);
                    }
                }
            }
            return;
        }
        self.vga.note_direct_write_pages(dirty_pages);
    }

    pub(crate) fn finish_direct_write_batch(&mut self) {
        self.vga.finish_direct_write_batch();
    }

    pub(crate) fn frame_sequence(&self) -> u64 {
        self.vga.frames_completed()
    }

    pub(crate) fn program_display_start(&mut self, start: u32) -> bool {
        self.margo.program_display_start(start)
    }

    pub(crate) fn display_start_pending(&self) -> bool {
        self.margo.display_start_pending()
    }

    /// Whether the Margo blitter still has modeled busy time to drain.
    ///
    /// This is the TIME-DRAINING half of STATUS.BUSY only. An armed but unfed
    /// color-expand stream also reads BUSY, and it is deliberately excluded: it
    /// waits on guest MONO_DATA writes, not on elapsed time, so it would never
    /// clear on its own.
    pub(crate) fn blitter_busy_ns(&self) -> u64 {
        self.margo.busy_ns()
    }

    /// Whether the DMA pusher is enabled and still has commands to consume.
    ///
    /// This is the ONE case where a batch boundary is guest-observable through
    /// Margo rather than merely convenient: `pump_pusher` runs at batch end and
    /// stalls on `busy_ns() == 0`, so it consumes at most one COMMAND per batch.
    /// Section 7.9 makes `PUSH_GET` a readable register and section 9 promises
    /// that "the pusher's PUSH_GET advances through the ring as it consumes
    /// commands" and that software feeding it "as a producer to its consumer
    /// behaves as it would on the real part". Draining at batch cadence instead
    /// of at the modeled completion cadence would stretch a ~740 ns glyph expand
    /// to a whole batch cap -- up to ~1350x -- and that is visible in PUSH_GET.
    /// So `Machine::vega_edge_ticks` keeps the busy deadline exactly while this
    /// is true, and drops it otherwise.
    pub(crate) fn pusher_has_queued_work(&self) -> bool {
        let p = self.margo.pusher();
        p.enabled && p.size != 0 && p.get != p.put
    }

    /// Tell Margo the write that just moved its busy time landed `elapsed_ns`
    /// into the current batch. Only `MachineBus::write_memory_byte_recorded`
    /// calls this, and only on `VideoWrite::ArmedBlit`; `pump_pusher` must NOT,
    /// since it runs at batch end after the drain, i.e. at offset 0 of the next
    /// batch.
    pub(crate) fn credit_blit_arm(&mut self, elapsed_ns: u64) {
        self.margo.credit_busy_ns(elapsed_ns);
    }

    pub(crate) fn port_disabled(&self, port: u16) -> bool {
        !self.vga.video_subsystem_enabled() && port != 0x3c3 && (0x3b0..=0x3df).contains(&port)
    }

    pub(crate) fn port_enabled(&self, port: u16) -> bool {
        self.vga.video_subsystem_enabled() || port == 0x3c3
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            MARGO_EXT_INDEX => return Some(self.margo_ext_index),
            MARGO_EXT_DATA => return Some(self.read_margo_ext()),
            _ => {}
        }
        self.vga.read_port(port)
    }

    /// Lazy-path read of 0x3DA / 0x3BA / 0x3C2, with `beam` in the dot unit of
    /// whichever engine owns the display (see `margo_scanout`).
    ///
    /// While Margo is scanning out, the two halves come from different owners on
    /// purpose. The SIDE EFFECTS stay the VGA core's: this is one chip, a 0x3DA
    /// read still resets the attribute-controller flip-flop and still catches the
    /// legacy raster up, and which alias is decoded (0x3DA versus 0x3BA) is still
    /// the Misc Output color/mono bit's business. The BITS come from Margo,
    /// because Margo is what the monitor is showing. An inactive alias returns
    /// `None` with no side effects at all, exactly as the VGA path does.
    pub(crate) fn read_status_port_lazy(&mut self, port: u16, beam: u64) -> Option<u8> {
        let Some(scan) = self.margo_scanout() else {
            return self.vga.read_status_port_lazy(port, beam);
        };
        match port {
            0x3C2 => {
                self.vga.catch_up();
                Some(self.vga.status0_switch_sense_bits() | scan.status0_vretrace_bits(beam))
            }
            // Hercules cannot be the personality driving a Margo VBE mode (the
            // HGC path is a mono-only legacy personality), but if some state
            // ever put the two together, the HGC status register is the VGA
            // core's own and stays with it rather than being answered from a
            // frame clock it does not run on.
            port if self.vga.status1_port_active(port) && self.vga.is_hercules_personality() => {
                self.vga.read_status_port_lazy(port, beam)
            }
            port if self.vga.status1_port_active(port) => {
                self.vga.status1_side_effects();
                Some(scan.status1_bits(beam))
            }
            _ => None,
        }
    }

    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            MARGO_EXT_INDEX => {
                self.margo_ext_index = value;
                return true;
            }
            MARGO_EXT_DATA => {
                self.write_margo_ext(value);
                return true;
            }
            _ => {}
        }
        self.vga.write_port(port, value)
    }

    /// The legacy VGA raster's beam. NOT the scanout beam while a VBE mode is on
    /// screen -- that one lives in the timeline's Margo frame phase, not in a
    /// device, and reaches this module as the `beam` argument. See
    /// `Machine::scanout_beam_dots`.
    pub(crate) fn beam_dots(&self) -> u64 {
        self.vga.beam_dots()
    }

    /// Margo's scanout timing, when Margo owns the display.
    ///
    /// `None` for the VGA raster AND for Distira: Distira has its own 60 Hz
    /// scanout and its own status path, and nothing about this slice claims to
    /// have fixed 0x3DA for a 3D mode. Whatever the answer there is, it stays
    /// what it is today.
    pub(crate) fn margo_scanout(&self) -> Option<MargoScanTiming> {
        (self.active_display() == ActiveDisplay::MargoLfb)
            .then(|| MargoScanTiming::for_display(self.margo.display()))
    }

    pub(crate) fn dot_clock_hz(&self) -> u64 {
        self.vga.dot_clock_hz()
    }

    pub(crate) fn frame_dots(&self) -> u64 {
        self.vga.frame_dots()
    }

    /// Dots to the next vertical-retrace start edge from `beam`, in the unit of
    /// whichever engine owns the display.
    ///
    /// The VGA arm ignores `beam` and reads its own live raster beam instead.
    /// That is only correct because every caller sources `beam` from
    /// `Machine::scanout_beam_dots`, whose non-Margo arm returns exactly that
    /// same value -- a silent coupling between two functions in different
    /// modules, so the debug assertion below states it rather than leaving the
    /// next reader to rediscover it.
    pub(crate) fn dots_until_vretrace_start(&self, beam: u64) -> Option<u64> {
        match self.margo_scanout() {
            Some(scan) => scan.dots_until_vretrace_start(beam),
            None => {
                debug_assert_eq!(
                    beam,
                    self.vga.beam_dots(),
                    "the VGA arm answers from its own beam, so a caller passing \
                     anything else is asking a question this cannot answer"
                );
                self.vga.dots_until_vretrace_start()
            }
        }
    }

    #[cfg(feature = "jit")]
    pub(crate) fn poll_skip_status1_port_active(&self) -> bool {
        self.port_enabled(0x3da)
            && self.vga.color_status1_port_active()
            && !self.vga.is_hercules_personality()
    }

    #[cfg(feature = "jit")]
    pub(crate) fn status1_bits(&self, beam: u64) -> u8 {
        match self.margo_scanout() {
            Some(scan) => scan.status1_bits(beam),
            None => self.vga.status1_bits(beam),
        }
    }

    #[cfg(feature = "jit")]
    pub(crate) fn status1_side_effects(&mut self) {
        self.vga.status1_side_effects();
    }

    #[cfg(feature = "jit")]
    pub(crate) fn dots_until_status1_bit_change_from(
        &self,
        beam: u64,
        bit: u8,
        target: bool,
    ) -> Option<u64> {
        match self.margo_scanout() {
            Some(scan) => scan.dots_until_status1_bit_change_from(beam, bit, target),
            None => self
                .vga
                .dots_until_status1_bit_change_from(beam, bit, target),
        }
    }

    pub(crate) fn advance(
        &mut self,
        margo_nanoseconds: u64,
        margo_frames: u64,
        distira_lines: u64,
        vga_dots: u64,
    ) {
        self.margo.advance_busy(margo_nanoseconds);
        self.margo.advance_frames(margo_frames);
        self.distira.advance_frame_phase(distira_lines);
        self.vga.advance(vga_dots);
    }

    pub(crate) fn screen_text(&self) -> TextFrame {
        self.vga.frame()
    }

    pub(crate) fn is_graphics_mode(&self) -> bool {
        match self.active_display() {
            ActiveDisplay::MargoLfb | ActiveDisplay::Distira => true,
            ActiveDisplay::VgaRaster => matches!(
                self.vga.active_mode(),
                VideoMode::Mode13h
                    | VideoMode::Planar
                    | VideoMode::ModeX
                    | VideoMode::Cga
                    | VideoMode::Hercules
            ),
        }
    }

    pub(crate) fn display_refresh_hz(&self) -> f64 {
        let hz = match self.active_display() {
            ActiveDisplay::VgaRaster => match self.vga.frame_dots() {
                0 => 60.0,
                dots => self.vga.dot_clock_hz() as f64 / dots as f64,
            },
            // Margo scans out at exactly MARGO_FRAME_HZ in every mode, which is
            // the same constant its frame phase (and now its 0x3DA retrace)
            // runs at -- not a coincidence and not a second number. Distira's
            // own 60 Hz scanout is unrelated and unchanged.
            ActiveDisplay::MargoLfb => MARGO_FRAME_HZ as f64,
            ActiveDisplay::Distira => 60.0,
        };
        hz.clamp(50.0, 120.0)
    }

    pub(crate) fn vga_raster(&self) -> Option<VgaRaster> {
        self.vga.last_presented().cloned()
    }

    pub(crate) fn palette_argb(&self) -> [u32; DAC_ENTRIES] {
        self.vga.palette_argb()
    }

    /// The whole beam raster, borders and all, for CRC comparison.
    ///
    /// This keeps the one-pixel stand-in that
    /// [`Self::presented_frame_argb`] gave up, and the difference is deliberate:
    /// nothing that records observations reads this. Its callers are the unit
    /// tester's `frame_crc32` and in-tree tests, all of which set a mode before
    /// looking, so the stand-in is unreachable rather than merely unlikely. Do
    /// not route an observer through here — use `presented_frame_argb`, which
    /// says `None` when there is no frame.
    pub(crate) fn frame_argb(&self) -> (Vec<u32>, usize, usize) {
        match self.active_display() {
            ActiveDisplay::VgaRaster => {
                if let Some((words, width, height, _)) = self.vga.cached_mode13h_presented_argb() {
                    return (words, width, height);
                }
                let palette = self.palette_argb();
                match self.vga_raster() {
                    Some(raster) => {
                        let words = raster
                            .pixels
                            .iter()
                            .map(|&index| palette[usize::from(index)])
                            .collect();
                        (words, raster.width as usize, raster.height as usize)
                    }
                    None => (vec![0], 1, 1),
                }
            }
            ActiveDisplay::MargoLfb => {
                let display = self.margo.display();
                let (width, height) = (display.width as usize, display.height as usize);
                let palette = self.palette_argb();
                (self.margo.scanout_argb(&palette), width, height)
            }
            ActiveDisplay::Distira => {
                let display = self.distira.display();
                let (width, height) = (display.width as usize, display.height as usize);
                (self.distira.scanout_argb(), width, height)
            }
        }
    }

    /// The most recently completed display frame, or `None` when there is not
    /// one yet.
    ///
    /// **`None` is a real answer and callers must handle it.** Two moments have
    /// no completed frame: before the first raster of the run, and between a
    /// mode set and the first raster of the new mode — every mode set drops the
    /// presented frame on purpose, so nobody is handed a frame carrying the
    /// previous mode's geometry. The second window is up to a whole frame
    /// period, about 14 ms at 70 Hz, and a DOS title crosses it on every
    /// menu-to-gameplay transition.
    ///
    /// This used to answer both with a one-pixel black image. Stage 1 of the
    /// eXoDOS sweep archived 30 of them, one in each of 30 different games, and
    /// they read as data all the way through the classifier: a 1x1 frame is
    /// vacuously one solid colour, which is the blank-screen signature. A
    /// substitute frame is worse than no frame, because it looks like a
    /// measurement.
    pub(crate) fn presented_frame_argb(&self) -> Option<(Vec<u32>, usize, usize)> {
        if self.active_display() != ActiveDisplay::VgaRaster {
            return Some(self.frame_argb());
        }

        if let Some((mut words, width, _, display_height)) =
            self.vga.cached_mode13h_presented_argb()
        {
            words.truncate(width.saturating_mul(display_height));
            return Some((words, width, display_height));
        }

        let palette = self.palette_argb();
        let raster = self.vga.last_presented()?;
        let width = raster.width as usize;
        let height = if raster.display_height == 0 {
            raster.height as usize
        } else {
            raster.display_height as usize
        };
        let visible = &raster.pixels[..width.saturating_mul(height).min(raster.pixels.len())];
        let words: Vec<u32> = visible
            .iter()
            .map(|&index| palette[usize::from(index)])
            .collect();
        // A raster with no pixels is not a picture either. The un-programmed
        // CRTC reads back zero dimensions, and a zero-sized frame would reach a
        // consumer as an empty image rather than as the absence of one.
        if words.is_empty() {
            return None;
        }
        Some((words, width, height))
    }

    /// [`Self::presented_frame_argb`] plus the scanline runs that changed since
    /// the previous call.
    ///
    /// The pixels are the SAME pixels the contract path presents. That is the
    /// whole invariant here, and it is easy to lose: the two fast branches
    /// below re-derive a frame from a source of their own (Margo's row damage,
    /// the VGA index raster and DAC) instead of from `presented_frame_argb`, so
    /// each one is admitted only where it provably agrees.
    ///
    /// Margo has no second definition -- `presented_frame_argb` reaches
    /// `frame_argb`, which scans out exactly what `margo_frame_update` scans
    /// out, row by row.
    ///
    /// The VGA arm is narrower. Canonical Mode 13h presents from the VGA's own
    /// cached ARGB frame, not from the index raster, and it accepts a SHORT
    /// raster by truncating where a row-diff would have to reject it. Both of
    /// those go to the generic branch, which is `presented_frame_argb` verbatim
    /// with a row diff on top -- one presentation, two ways of costing it.
    pub(crate) fn presented_frame_update(&self) -> Option<PresentedFrameUpdate> {
        let owner = self.active_display();
        if owner == ActiveDisplay::MargoLfb {
            return self.margo_frame_update();
        }
        if owner == ActiveDisplay::VgaRaster
            && !self.vga.presents_cached_mode13h_argb()
            && let Some(update) = self.vga_frame_update()
        {
            return Some(update);
        }
        let (words, width, height) = self.presented_frame_argb()?;
        let mut cache = self.presented_argb_cache.borrow_mut();
        // `changed_frame_rows` indexes `row * width .. + width` for every row
        // below `height`, so a frame whose word count is not exactly
        // `width * height` cannot be diffed at all -- it must be published
        // whole. `presented_frame_argb` produces one on the truncating short-
        // raster path, and the cropping arms of `frame_argb` can too.
        let full = !cache.valid
            || cache.owner != Some(owner)
            || cache.width != width
            || cache.height != height
            || cache.words.len() != words.len()
            || words.len() != width.saturating_mul(height);
        let changed_rows = if full {
            std::iter::once(0..height).collect()
        } else {
            changed_frame_rows(&cache.words, &words, width, height)
        };
        if full {
            cache.words = Arc::new(words);
        } else if !changed_rows.is_empty() {
            let cached = Arc::make_mut(&mut cache.words);
            for rows in &changed_rows {
                let start = rows.start * width;
                let end = rows.end * width;
                cached[start..end].copy_from_slice(&words[start..end]);
            }
        }
        cache.width = width;
        cache.height = height;
        // One cache serves both branches, so record the palette the words were
        // built under even here: a later fast-branch call compares it, and a
        // stale one would have it re-convert a frame it already holds.
        cache.palette = self.palette_argb();
        cache.owner = Some(owner);
        cache.valid = true;
        Some(PresentedFrameUpdate {
            words: cache.words.clone(),
            changed_rows,
            width,
            height,
        })
    }

    /// The palette-indexed fast diff for the plain VGA raster: compare index
    /// bytes against the cached ARGB words through the DAC, so an unchanged row
    /// costs a compare and no conversion. Admitted only from
    /// [`Self::presented_frame_update`], and only where it agrees with
    /// `presented_frame_argb` -- see that function for the two exclusions.
    /// `None` means "not answerable here", never "no frame".
    fn vga_frame_update(&self) -> Option<PresentedFrameUpdate> {
        let raster = self.vga.last_presented()?;
        let width = raster.width as usize;
        let height = if raster.display_height == 0 {
            raster.height as usize
        } else {
            raster.display_height as usize
        };
        let pixels = &raster.pixels[..width.saturating_mul(height).min(raster.pixels.len())];
        if pixels.is_empty() || pixels.len() != width.saturating_mul(height) {
            return None;
        }
        let palette = self.palette_argb();
        let mut cache = self.presented_argb_cache.borrow_mut();
        let full = !cache.valid
            || cache.owner != Some(ActiveDisplay::VgaRaster)
            || cache.width != width
            || cache.height != height
            || cache.words.len() != pixels.len()
            || cache.palette != palette;
        if full {
            cache.words = Arc::new(vec![0; pixels.len()]);
        }
        let mut changed_rows = Vec::new();
        let mut first = None;
        for row in 0..height {
            let start = row * width;
            let end = start + width;
            let dirty = full
                || pixels[start..end]
                    .iter()
                    .zip(&cache.words[start..end])
                    .any(|(&index, &word)| palette[usize::from(index)] != word);
            if dirty {
                if first.is_none() {
                    first = Some(row);
                }
            } else if let Some(start) = first.take() {
                changed_rows.push(start..row);
            }
        }
        if let Some(start) = first {
            changed_rows.push(start..height);
        }
        if !changed_rows.is_empty() {
            let words = Arc::make_mut(&mut cache.words);
            for rows in &changed_rows {
                let start = rows.start * width;
                let end = rows.end * width;
                for (word, &index) in words[start..end].iter_mut().zip(&pixels[start..end]) {
                    *word = palette[usize::from(index)];
                }
            }
        }
        cache.palette = palette;
        cache.width = width;
        cache.height = height;
        cache.owner = Some(ActiveDisplay::VgaRaster);
        cache.valid = true;
        Some(PresentedFrameUpdate {
            words: cache.words.clone(),
            changed_rows,
            width,
            height,
        })
    }

    fn margo_frame_update(&self) -> Option<PresentedFrameUpdate> {
        let display = self.margo.display();
        let width = display.width as usize;
        let height = display.height as usize;
        if width == 0 || height == 0 {
            return None;
        }
        let palette = self.palette_argb();
        let generation = self.margo.content_generation();
        let mut cache = self.presented_argb_cache.borrow_mut();
        let full = !cache.valid
            || cache.owner != Some(ActiveDisplay::MargoLfb)
            || cache.width != width
            || cache.height != height
            || cache.words.len() != width.saturating_mul(height)
            || cache.palette != palette;
        let changed_rows = if full {
            std::iter::once(0..height).collect()
        } else {
            self.margo.changed_rows_since(cache.margo_generation)
        };
        if full {
            cache.words = Arc::new(vec![0; width.saturating_mul(height)]);
        }
        if !changed_rows.is_empty() {
            self.margo
                .scanout_argb_rows(&palette, &changed_rows, Arc::make_mut(&mut cache.words));
            let rows = changed_rows
                .iter()
                .map(|range| range.end.saturating_sub(range.start) as u64)
                .sum::<u64>();
            VideoHostCounters::add(&self.host_counters.scanout_rows_converted, rows);
            VideoHostCounters::add(
                &self.host_counters.scanout_pixels_converted,
                rows.saturating_mul(width as u64),
            );
        }
        cache.palette = palette;
        cache.width = width;
        cache.height = height;
        cache.margo_generation = generation;
        cache.owner = Some(ActiveDisplay::MargoLfb);
        cache.valid = true;
        Some(PresentedFrameUpdate {
            words: cache.words.clone(),
            changed_rows,
            width,
            height,
        })
    }

    pub(crate) fn capture_frame_argb(&mut self) -> (Vec<u32>, usize, usize) {
        if self.active_display() != ActiveDisplay::VgaRaster {
            return self.frame_argb();
        }

        let raster = self.vga.render_full_frame();
        let width = raster.width as usize;
        let height = raster.display_height.min(raster.height) as usize;
        let palette = self.palette_argb();
        let words = raster.pixels[..width * height]
            .iter()
            .map(|&index| palette[usize::from(index)])
            .collect();
        (words, width, height)
    }

    pub(crate) fn frame_generation(&self) -> Option<u64> {
        match self.active_display() {
            ActiveDisplay::MargoLfb => {
                let display = self.margo.display();
                let generation = self
                    .margo
                    .content_generation()
                    .wrapping_mul(0xd6e8_feb8_6659_fd93)
                    .wrapping_add(self.vga.content_gen());
                Some(Self::frame_generation_key(
                    generation,
                    display.width,
                    display.height,
                ))
            }
            ActiveDisplay::Distira => None,
            ActiveDisplay::VgaRaster if self.vga.is_text_mode() => None,
            ActiveDisplay::VgaRaster => Some(Self::frame_generation_key(
                self.vga.content_gen(),
                self.vga.raster_width(),
                self.vga.raster_height(),
            )),
        }
    }

    pub(crate) fn presented_frame_generation(&self) -> Option<u64> {
        if self.active_display() == ActiveDisplay::MargoLfb {
            return self.frame_generation();
        }
        if self.active_display() == ActiveDisplay::Distira || self.vga.is_text_mode() {
            return None;
        }
        let raster = self.vga.last_presented()?;
        Some(Self::frame_generation_key(
            raster.generation,
            raster.width,
            raster.height,
        ))
    }

    fn frame_generation_key(generation: u64, width: u32, height: u32) -> u64 {
        const K: u64 = 0x9e37_79b9_7f4a_7c15;
        generation
            .wrapping_mul(K)
            .wrapping_add(u64::from(width).wrapping_mul(0x0001_0000_0001))
            .wrapping_add(u64::from(height).wrapping_mul(0x1_0000_0001_0000))
    }

    pub(crate) fn screen_crc32(&mut self, x: u16, y: u16, w: u16, h: u16) -> u32 {
        let (words, frame_w, frame_h) = self.frame_argb();
        let x = usize::from(x);
        let y = usize::from(y);
        let x_end = x.saturating_add(usize::from(w)).min(frame_w);
        let y_end = y.saturating_add(usize::from(h)).min(frame_h);
        let mut bytes = Vec::new();
        for row in y..y_end {
            for col in x..x_end {
                bytes.extend_from_slice(&words[row * frame_w + col].to_le_bytes());
            }
        }
        crate::unittester::crc32(&bytes)
    }

    pub(crate) fn drain_distira_fifo(&mut self) {
        self.distira.drain_fifo();
    }

    pub(crate) fn pump_pusher(&mut self, memory: &Memory) {
        let p = self.margo.pusher();
        if !p.enabled || p.size == 0 {
            return;
        }
        let mut get = p.get;
        let mut budget = (p.size / 4) as u64;
        while self.margo.busy_ns() == 0 && get != p.put && budget > 0 {
            let header = read_ring_word(memory, p.base, p.size, get);
            let method = (header & 0xffff) as usize;
            let count = header >> 16;
            get = (get + 4) % p.size;
            budget -= 1;
            let mut i = 0u32;
            while i < count && get != p.put && budget > 0 {
                let data = read_ring_word(memory, p.base, p.size, get);
                for byte in 0..4 {
                    self.margo.write_mmio_u8(
                        method + (i as usize) * 4 + byte,
                        (data >> (8 * byte)) as u8,
                    );
                }
                get = (get + 4) % p.size;
                budget -= 1;
                i += 1;
            }
            self.margo.set_pusher_get(get);
        }
    }

    pub(crate) fn read_wide_memory(&mut self, address: u32, width: BusWidth) -> Option<u32> {
        let bytes = width.bytes() as usize;
        if let Some(offset) = self.distira_lfb_offset(address, bytes) {
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            return Some(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => u32::from(self.distira.read_lfb_u16(offset)),
                BusWidth::Dword => self.distira.read_lfb_u32(offset),
            });
        }
        if self.distira_texture_offset(address, bytes).is_some() {
            return Some(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => 0xffff,
                BusWidth::Dword => u32::MAX,
            });
        }
        None
    }

    pub(crate) fn write_wide_memory(&mut self, address: u32, width: BusWidth, value: u32) -> bool {
        let bytes = width.bytes() as usize;
        if let Some(offset) = self.distira_lfb_offset(address, bytes) {
            let offset = if width == BusWidth::Byte {
                offset
            } else {
                offset & !1
            };
            match width {
                BusWidth::Byte => {}
                BusWidth::Word => self.distira.write_lfb_u16(offset, value as u16),
                BusWidth::Dword => self.distira.write_lfb_u32(offset, value),
            }
            return true;
        }
        if let Some(offset) = self.distira_texture_offset(address, bytes) {
            if width == BusWidth::Dword {
                self.distira.write_texture_u32(offset, value);
            }
            return true;
        }
        false
    }

    /// `margo_elapsed_ns` is the in-batch offset the read is taken at, and is
    /// consumed by the Margo MMIO arm alone (STATUS.BUSY). The caller only has
    /// to compute it when `margo_mmio_at` says the address is in that window;
    /// every other aperture passes 0 and pays nothing.
    pub(crate) fn read_memory(
        &mut self,
        address: u32,
        out: &mut [u8],
        margo_elapsed_ns: u64,
    ) -> bool {
        let width = out.len();
        if width == 0 {
            return false;
        }

        if self.margo_banked_window_at(address) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = address
                    .checked_add(index as u32)
                    .and_then(|address| self.margo_banked_window_offset(address))
                    .map(|offset| self.margo.read_vram_u8(offset))
                    .unwrap_or(0xff);
            }
            VideoHostCounters::add(&self.host_counters.banked_slow_read_bytes, width as u64);
            return true;
        }

        if let Some(offset) = self.legacy_gfx_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = match self.vga.active_mode() {
                    VideoMode::Mode13h => self.vga.cpu_read_chain4(offset + index),
                    _ => self.vga.cpu_read(offset + index),
                };
            }
            return true;
        }

        if let Some(offset) = self.hercules_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.vga.hgc_read(offset + index);
            }
            return true;
        }

        if let Some(offset) = self.text_offset(address, width) {
            let cga_window = self.vga.is_cga_personality();
            let cga_graphics = self.vga.active_mode() == VideoMode::Cga;
            for (index, byte) in out.iter_mut().enumerate() {
                let byte_offset = if cga_window {
                    (offset + index) & (CGA_FB_SIZE - 1)
                } else {
                    offset + index
                };
                *byte = if cga_graphics {
                    self.vga.cga_read(byte_offset)
                } else {
                    self.vga.read_u8(byte_offset).unwrap_or(0xff)
                };
            }
            return true;
        }

        if self.vga.video_memory_enabled()
            && let Some(offset) = planar_offset(address, width)
        {
            match self.vga.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.vga.cpu_read(offset + index);
                    }
                    return true;
                }
                VideoMode::Mode13h => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.vga.cpu_read_chain4(offset + index);
                    }
                    return true;
                }
                VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
            }
        }

        if let Some(offset) = margo_lfb_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_vram_u8(offset + index);
            }
            VideoHostCounters::add(&self.host_counters.lfb_slow_read_bytes, width as u64);
            return true;
        }
        if let Some(offset) = margo_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.margo.read_mmio_u8_at(offset + index, margo_elapsed_ns);
            }
            return true;
        }
        if let Some(offset) = self.distira_lfb_offset(address, width) {
            match width {
                1 => out[0] = 0xff,
                2 => out.copy_from_slice(&self.distira.read_lfb_u16(offset & !1).to_le_bytes()),
                4 => out.copy_from_slice(&self.distira.read_lfb_u32(offset & !1).to_le_bytes()),
                _ => {
                    for (index, byte) in out.iter_mut().enumerate() {
                        *byte = self.distira.read_lfb_u8(offset + index);
                    }
                }
            }
            return true;
        }
        if self.distira_texture_offset(address, width).is_some() {
            out.fill(0xff);
            return true;
        }
        if let Some(offset) = self.distira_mmio_offset(address, width) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.distira.read_mmio_u8(offset + index);
            }
            return true;
        }
        false
    }

    pub(crate) fn write_memory_u8(&mut self, address: u32, value: u8) -> VideoWrite {
        if self.margo_banked_window_at(address) {
            if let Some(offset) = self.margo_banked_window_offset(address) {
                self.margo.write_vram_u8(offset, value);
            }
            VideoHostCounters::add(&self.host_counters.banked_slow_write_bytes, 1);
            return VideoWrite::Accepted;
        }
        if let Some(offset) = self.legacy_gfx_offset(address, 1) {
            match self.vga.active_mode() {
                VideoMode::Mode13h => self.vga.cpu_write_chain4(offset, value),
                _ => self.vga.cpu_write(offset, value),
            }
            return VideoWrite::Accepted;
        }
        if let Some(offset) = self.hercules_offset(address, 1) {
            self.vga.hgc_write(offset, value);
            return VideoWrite::Accepted;
        }
        if let Some(offset) = self.text_offset(address, 1) {
            let offset = if self.vga.is_cga_personality() {
                offset & (CGA_FB_SIZE - 1)
            } else {
                offset
            };
            if self.vga.active_mode() == VideoMode::Cga {
                self.vga.cga_write(offset, value);
            } else {
                let _ = self.vga.write_u8(offset, value);
            }
            return VideoWrite::Accepted;
        }
        if self.vga.video_memory_enabled()
            && let Some(offset) = planar_offset(address, 1)
        {
            match self.vga.active_mode() {
                VideoMode::Planar | VideoMode::ModeX => {
                    self.vga.cpu_write(offset, value);
                    return VideoWrite::Accepted;
                }
                VideoMode::Mode13h => {
                    self.vga.cpu_write_chain4(offset, value);
                    return VideoWrite::Accepted;
                }
                VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => {}
            }
        }
        if let Some(offset) = margo_lfb_offset(address, 1) {
            self.margo.write_vram_u8(offset, value);
            VideoHostCounters::add(&self.host_counters.lfb_slow_write_bytes, 1);
            return VideoWrite::Accepted;
        }
        if let Some(offset) = margo_mmio_offset(address, 1) {
            // The ONLY arming path in this function: COMMAND (and the final
            // MONO_DATA word of a color-expand stream) is what charges the
            // blitter modeled busy time; CONTROL.RESET is what drops it. Detect
            // the EDGE -- an operation was armed by THIS write -- rather than
            // the level, so a write landing while an earlier blit is still
            // draining is an ordinary accepted write and does not re-stamp the
            // origin.
            //
            // The edge is the arm STAMP, not the busy VALUE. A value comparison
            // is blind to the case that matters most: every setter in margo.rs
            // is an assign and nothing drains mid-batch, so a second operation
            // of identical modeled duration -- every glyph in izbios' `lfb_text`,
            // which fixes MG_DIM at 0x00080008 -- leaves `busy_ns` unchanged and
            // would be read as an ordinary write. The drain credit would then
            // still name the FIRST arm's instant and the second operation would
            // report idle for its entire length, breaking section 9 by a whole
            // operation rather than a rounding error.
            let stamp_before = self.margo.busy_stamp();
            self.margo.write_mmio_u8(offset, value);
            return if self.margo.busy_stamp() != stamp_before {
                VideoWrite::ArmedBlit
            } else {
                VideoWrite::Accepted
            };
        }
        if self.distira_lfb_offset(address, 1).is_some()
            || self.distira_texture_offset(address, 1).is_some()
        {
            return VideoWrite::Accepted;
        }
        if let Some(offset) = self.distira_mmio_offset(address, 1) {
            self.distira.write_mmio_u8(offset, value);
            return VideoWrite::Accepted;
        }
        VideoWrite::Unclaimed
    }

    /// Conservative SUPERSET of `owns_memory`: false here proves no aperture can claim the
    /// address, true only means the full chain has to run.
    ///
    /// Why it exists: `MachineBus::data_access_wait_states` and `code_fetch_wait_states` guard
    /// on `address >= 0xA0000`, which is not "inside a video aperture" but "not conventional
    /// RAM". A 32-bit protected-mode guest with its heap above 1 MB therefore ran all ten
    /// predicates below, built ten `Option`s and returned false on EVERY data access and code
    /// fetch. A RIP profile of Quake/586 put 3.37% of wall in `owns_memory` for exactly that.
    ///
    /// Every aperture except Distira's is at a compile-time constant. The legacy group
    /// (`margo_banked_window_at`, `legacy_gfx_offset`, `hercules_offset`, `text_offset`,
    /// `planar_offset`) is bounded by 0xA0000..0xC0000: the four `GfxAperture` selections are
    /// A0000+128K, A0000+64K, B0000+32K and B8000+32K; Hercules is VGA_MONO_TEXT_BASE plus two
    /// 32K pages, which ends exactly at 0xC0000; planar and the Margo banked window are
    /// VGA_MODE13H_BASE plus one 64K window. Margo's LFB and MMIO are adjacent constants. Only
    /// the Distira BAR moves, and it is read live here rather than cached, so there is no
    /// invalidation hook to forget when the BAR is reprogrammed.
    ///
    /// The predicates all require the WHOLE access inside their range (`address + width <=
    /// end`), so testing the start address alone cannot under-approximate.
    #[inline]
    fn may_own_memory(&self, address: u32) -> bool {
        const LEGACY_END: u32 = 0x000C_0000;
        const MARGO_END: u32 = MARGO_MMIO_BASE + MARGO_MMIO_SIZE as u32;
        if (VGA_MODE13H_BASE..LEGACY_END).contains(&address)
            || (MARGO_LFB_BASE..MARGO_END).contains(&address)
        {
            return true;
        }
        self.distira_memory_enabled()
            && (self.distira_mem_base..self.distira_mem_base.saturating_add(DISTIRA_PCI_BAR_SIZE))
                .contains(&address)
    }

    /// The lowest address at or above 1 MB that any aperture in this card can
    /// claim. Every address in `0x0010_0000 .. floor` is therefore plain memory
    /// as far as `owns_memory` is concerned, and the bus uses that to skip the
    /// gauntlet for the extended RAM a 32-bit game runs in.
    ///
    /// DERIVED FROM `may_own_memory`, which is the hand-written superset of every
    /// aperture: of its three regions, the legacy one ends at 0x000C_0000 (below
    /// 1 MB, so it cannot bound this), Margo's LFB+MMIO block starts at
    /// `MARGO_LFB_BASE`, and Distira's BAR starts wherever the guest last wrote
    /// BAR0. Any aperture added to `may_own_memory` above 1 MB MUST be added
    /// here as well; `may_own_memory_agrees_with_the_extended_floor` is the test
    /// that fails when one is not.
    ///
    /// WHAT WOULD ACTUALLY BREAK THIS, since "any aperture" is too broad to be
    /// useful. Because this returns the LOWEST claimant above 1 MB, a new
    /// claimant placed ABOVE `MARGO_LFB_BASE` costs nothing: the screen already
    /// declines everything up there and hands it to the gauntlet. That covers
    /// the top-of-4GB BIOS alias (0xFFFF_0000) and would equally cover an APIC
    /// at 0xFEC0_0000 or 0xFEE0_0000 if this machine ever modelled one -- it
    /// does not today (no SMP, PIC-only interrupts), and it would not matter
    /// here if it did. The ONLY dangerous addition is a claimant BETWEEN 1 MB
    /// and `MARGO_LFB_BASE`, which is exactly the window the sweep covers.
    /// Raised in review of PR #768 by the dynarec campaign; recorded because the
    /// reasoning is the useful part, not the answer.
    pub(crate) fn device_free_extended_floor(&self) -> u32 {
        if self.distira_memory_enabled() && self.distira_mem_base < MARGO_LFB_BASE {
            self.distira_mem_base
        } else {
            MARGO_LFB_BASE
        }
    }

    /// Called by the bus each time it ASKS whether an address is a device
    /// window, before the extended-RAM screen runs. See
    /// [`Vega::device_window_questions`].
    #[cfg(test)]
    pub(crate) fn note_device_window_question(&self) {
        self.device_window_questions
            .set(self.device_window_questions.get() + 1);
    }

    /// Called by the bus each time a question got past the screen and entered
    /// the gauntlet itself.
    #[cfg(test)]
    pub(crate) fn note_device_window_gauntlet_entry(&self) {
        self.device_window_gauntlet_entries
            .set(self.device_window_gauntlet_entries.get() + 1);
    }

    #[cfg(test)]
    pub(crate) fn device_window_questions(&self) -> u64 {
        self.device_window_questions.get()
    }

    #[cfg(test)]
    pub(crate) fn device_window_gauntlet_entries(&self) -> u64 {
        self.device_window_gauntlet_entries.get()
    }

    pub(crate) fn owns_memory(&self, address: u32, width: usize) -> bool {
        if !self.may_own_memory(address) {
            // The superset is a hand-derived claim about every constant above, so prove it on
            // every debug run rather than trusting the derivation: if this ever fires, an
            // aperture moved outside the bounds `may_own_memory` encodes.
            debug_assert!(
                !self.owns_memory_uncached(address, width),
                "may_own_memory missed an aperture claiming {address:#x} width {width}"
            );
            return false;
        }
        self.owns_memory_uncached(address, width)
    }

    /// True iff NO byte in `address .. address + bytes` is claimed by any Vega aperture.
    ///
    /// Asked byte by byte ON PURPOSE, and the reason is the whole point of the query. Every
    /// `*_offset` predicate above requires the WHOLE access to be in range --
    /// `distira_lfb_offset` tests `offset + width <= DISTIRA_PCI_TEX_OFFSET`,
    /// `distira_texture_offset` tests `offset + width <= DISTIRA_PCI_BAR_SIZE` -- so a Word
    /// straddling a window's end DECLINES WIDE while its base byte is still claimed. A wide
    /// "declines" is therefore strictly WEAKER than "claims nothing here", and would not pin the
    /// behaviour of a split loop that asks the question once per byte.
    ///
    /// `owns_memory` at width 1 asks exactly what that loop asks, and carries its own
    /// `may_own_memory` pre-filter assertion, so this is the right primitive to build on.
    ///
    /// NOT `#[cfg(debug_assertions)]`, even though its only caller is a `debug_assert!`:
    /// `debug_assert!` expands to `if cfg!(debug_assertions) { .. }`, so its argument is still
    /// TYPE-CHECKED and compiled in a release build. Gating the method made the release build fail
    /// to compile while every debug build and the whole test suite stayed green.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(crate) fn claims_no_byte_in(&self, address: u32, bytes: u32) -> bool {
        (0..bytes).all(|i| !self.owns_memory(address.wrapping_add(i), 1))
    }

    fn owns_memory_uncached(&self, address: u32, width: usize) -> bool {
        self.margo_banked_window_at(address)
            || self.legacy_gfx_offset(address, width).is_some()
            || self.hercules_offset(address, width).is_some()
            || self.text_offset(address, width).is_some()
            || (self.vga.video_memory_enabled() && planar_offset(address, width).is_some())
            || margo_lfb_offset(address, width).is_some()
            || margo_mmio_offset(address, width).is_some()
            || self.distira_lfb_offset(address, width).is_some()
            || self.distira_texture_offset(address, width).is_some()
            || self.distira_mmio_offset(address, width).is_some()
    }

    pub(crate) fn memory_decode_key(&self) -> Option<u32> {
        self.distira_memory_enabled()
            .then_some(self.distira_mem_base)
    }

    pub(crate) fn memory_bar_overlaps(&self, start: usize, end: usize) -> bool {
        if !self.distira_memory_enabled() {
            return false;
        }
        let bar_start = u64::from(self.distira_mem_base);
        let bar_end = bar_start + u64::from(DISTIRA_PCI_BAR_SIZE);
        (start as u64) < bar_end && bar_start < end as u64
    }

    pub(crate) fn pci_read_config_byte(&self, offset: u32) -> u8 {
        match offset {
            0x00 => (DISTIRA_PCI_VENDOR_ID & 0xff) as u8,
            0x01 => (DISTIRA_PCI_VENDOR_ID >> 8) as u8,
            0x02 => (DISTIRA_PCI_DEVICE_ID & 0xff) as u8,
            0x03 => (DISTIRA_PCI_DEVICE_ID >> 8) as u8,
            0x04 => self.distira_command as u8,
            0x05 => (self.distira_command >> 8) as u8,
            0x08 => DISTIRA_PCI_REVISION,
            0x09 => 0x00,
            0x0a => 0x00,
            0x0b => 0x04,
            0x0e => 0x00,
            0x10..=0x13 => ((self.distira_mem_base >> ((offset - 0x10) * 8)) & 0xff) as u8,
            0x40..=0x43 => ((self.distira_init_enable >> ((offset - 0x40) * 8)) & 0xff) as u8,
            _ => 0x00,
        }
    }

    pub(crate) fn pci_write_config_byte(&mut self, offset: u32, value: u8) {
        match offset {
            0x04 => {
                self.distira_command = (self.distira_command & !0x0002) | u16::from(value & 0x02);
            }
            0x10..=0x12 => {}
            0x13 => self.distira_mem_base = u32::from(value) << 24,
            0x40..=0x43 => {
                let shift = (offset - 0x40) * 8;
                self.distira_init_enable =
                    (self.distira_init_enable & !(0xff << shift)) | (u32::from(value) << shift);
            }
            _ => {}
        }
        self.distira.set_init_enable(self.distira_init_enable);
    }

    fn distira_memory_enabled(&self) -> bool {
        self.distira_command & 0x0002 != 0 && self.distira_mem_base != 0
    }

    pub(crate) fn distira_bar_offset(&self, address: u32, width: usize) -> Option<u32> {
        if !self.distira_memory_enabled() {
            return None;
        }
        let offset = address.checked_sub(self.distira_mem_base)?;
        let end = offset.checked_add(width as u32)?;
        (end <= DISTIRA_PCI_BAR_SIZE).then_some(offset)
    }

    pub(crate) fn distira_mmio_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        (offset < DISTIRA_PCI_LFB_OFFSET && offset + width as u32 <= DISTIRA_PCI_LFB_OFFSET)
            .then_some(offset as usize)
    }

    pub(crate) fn distira_lfb_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if (DISTIRA_PCI_LFB_OFFSET..DISTIRA_PCI_TEX_OFFSET).contains(&offset)
            && offset + width as u32 <= DISTIRA_PCI_TEX_OFFSET
        {
            Some((offset - DISTIRA_PCI_LFB_OFFSET) as usize)
        } else {
            None
        }
    }

    pub(crate) fn distira_texture_offset(&self, address: u32, width: usize) -> Option<usize> {
        let offset = self.distira_bar_offset(address, width)?;
        if offset >= DISTIRA_PCI_TEX_OFFSET && offset + width as u32 <= DISTIRA_PCI_BAR_SIZE {
            Some((offset - DISTIRA_PCI_TEX_OFFSET) as usize)
        } else {
            None
        }
    }

    fn legacy_gfx_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.vga.video_memory_enabled() {
            return None;
        }
        match self.vga.active_mode() {
            VideoMode::Planar | VideoMode::ModeX | VideoMode::Mode13h => {
                let aperture = self.vga.gfx_aperture();
                legacy_gfx_aperture_offset(aperture.base, aperture.length, address, width)
            }
            VideoMode::Text | VideoMode::Cga | VideoMode::Hercules => None,
        }
    }

    fn text_offset(&self, address: u32, width: usize) -> Option<usize> {
        if self.vga.is_hercules_personality() || !self.vga.video_memory_enabled() {
            return None;
        }
        text_offset(self.vga.text_memory_base(), address, width)
    }

    fn hercules_offset(&self, address: u32, width: usize) -> Option<usize> {
        if !self.vga.video_memory_enabled() || !self.vga.is_hercules_personality() {
            return None;
        }
        let end = VGA_MONO_TEXT_BASE + HGC_FB_SIZE as u32 * 2;
        if !(VGA_MONO_TEXT_BASE..end).contains(&address) || address + width as u32 > end {
            return None;
        }
        let offset = (address - VGA_MONO_TEXT_BASE) as usize;
        if offset >= HGC_FB_SIZE && !self.vga.hgc_page1_addressable() {
            return None;
        }
        Some(offset)
    }

    /// THE SHARED PREDICATE. Every question about the banked window -- "is a host
    /// pointer available", "which bytes does it mean", "has the mapping changed" --
    /// is answered from this one function, so the three cannot disagree.
    ///
    /// `Some(bank)` exactly when a banked Margo window owns the legacy aperture.
    /// `None` covers every way it can stop owning it: no VESA mode
    /// (`margo_active` false, which `select_legacy` clears and nothing else in the
    /// identity tracked), a linear-framebuffer mode, or a bank whose 64 KiB window
    /// does not lie wholly inside the frame store.
    ///
    /// THE INVARIANT THIS EXISTS FOR: anything that can change the return of
    /// `margo_banked_direct_page` changes the return of this function, and
    /// `direct_write_identity` is built from it -- so a mapping change cannot be
    /// invisible to the cached-pointer guard. That is provable here rather than by
    /// enumerating call sites, which is what the previous design got wrong.
    pub(crate) fn margo_banked_window_key(&self) -> Option<u32> {
        // `margo_banked_direct_page` derives a window base by multiplying the bank
        // by the window size and requires the result to lie in the frame store. It
        // is page-independent -- the same key answers for every page of the
        // window -- only because the store divides evenly by the window.
        const { assert!(MARGO_VRAM_SIZE.is_multiple_of(VGA_PLANAR_WINDOW_SIZE as usize)) };
        if !self.margo_active || self.margo_linear {
            return None;
        }
        let base = u32::from(self.margo_bank).checked_mul(VGA_PLANAR_WINDOW_SIZE)?;
        let end = base.checked_add(VGA_PLANAR_WINDOW_SIZE)?;
        (end as usize <= MARGO_VRAM_SIZE).then_some(base)
    }

    /// A host pointer to one 4 KiB page of the banked Margo window, or `None`.
    ///
    /// Written in terms of `margo_banked_window_key` rather than repeating its
    /// conditions, so the identity that guards cached copies of this pointer and
    /// the grant itself cannot drift apart.
    pub(crate) fn margo_banked_direct_page(&mut self, physical_page: u32) -> Option<*mut u8> {
        let base = self.margo_banked_window_key()?;
        let offset = physical_page.checked_sub(VGA_MODE13H_BASE)?;
        if offset & 0x0fff != 0 || offset >= VGA_PLANAR_WINDOW_SIZE {
            return None;
        }
        let start = base.checked_add(offset)? as usize;
        // Raw `vram_mut`, deliberately: writes through this pointer are reported
        // by `note_direct_write`/`note_direct_write_pages` at the end of the
        // block that made them, which is exactly the row set that moved. Marking
        // full damage here would repaint the screen for every banked page grant.
        let vram = self.margo.vram_mut();
        (start + 0x1000 <= vram.len()).then(|| vram[start..].as_mut_ptr())
    }

    /// What the cached-pointer guards compare across a potentially mapping-moving
    /// operation. The VGA's own token, plus the banked-window key.
    ///
    /// The key is what the previous `(token, bank, linear)` tuple was missing:
    /// `select_legacy` clears `margo_active` and touches nothing else, so across
    /// banked-VESA -> INT 10h mode 03h the old tuple was INVARIANT while the
    /// mapping flipped, leaving a live pointer into Margo VRAM serving legacy VGA.
    pub(crate) fn direct_write_identity(&self) -> (u8, Option<u32>) {
        (
            self.vga.direct_write_token(),
            self.margo_banked_window_key(),
        )
    }

    fn margo_banked_window_at(&self, address: u32) -> bool {
        self.margo_active
            && !self.margo_linear
            && (VGA_MODE13H_BASE..VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE).contains(&address)
    }

    fn margo_banked_window_offset(&self, address: u32) -> Option<usize> {
        if !self.margo_banked_window_at(address) {
            return None;
        }
        let bank = usize::from(self.margo_bank) * VGA_PLANAR_WINDOW_SIZE as usize;
        let offset = bank + (address - VGA_MODE13H_BASE) as usize;
        (offset < MARGO_VRAM_SIZE).then_some(offset)
    }
}

fn changed_frame_rows(old: &[u32], new: &[u32], width: usize, height: usize) -> Vec<Range<usize>> {
    let mut changed = Vec::new();
    let mut first = None;
    for row in 0..height {
        let start = row * width;
        let end = start + width;
        let dirty = old[start..end] != new[start..end];
        match (first, dirty) {
            (None, true) => first = Some(row),
            (Some(start), false) => {
                changed.push(start..row);
                first = None;
            }
            _ => {}
        }
    }
    if let Some(start) = first {
        changed.push(start..height);
    }
    changed
}

/// One line per accepted 4F02 mode set, under `IZARRAVM_VBE_TRACE`.
///
/// Gated at the CALL SITE by nothing at all on purpose: an accepted VBE mode set
/// happens a handful of times in a whole run (GP2 sets one mode; NASCAR sets
/// one), so this is not a hot path and the `OnceLock` read is already more
/// machinery than the site needs. The env var is read once per process.
fn trace_vbe_mode_set(request: u16, linear_total: u32, banked_total: u32) {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_VBE_TRACE").is_some()) {
        return;
    }
    let window = if request & 0x4000 != 0 {
        "LINEAR"
    } else {
        "banked"
    };
    eprintln!(
        "[VBE] 4F02 request={request:#06x} mode={:#05x} window={window} dont_clear={} \
         totals: linear={linear_total} banked={banked_total}",
        request & 0x01ff,
        request & 0x8000 != 0,
    );
}

fn read_ring_word(memory: &Memory, base: u32, size: u32, off: u32) -> u32 {
    let start = off as usize % size as usize;
    if start + 4 <= size as usize
        && let Some(slice) = memory
            .as_slice()
            .get(base as usize + start..base as usize + start + 4)
    {
        return u32::from_le_bytes(slice.try_into().unwrap());
    }
    let mut bytes = [0u8; 4];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let ring_offset = (off as usize + index) % size as usize;
        *byte = memory
            .as_slice()
            .get(base as usize + ring_offset)
            .copied()
            .unwrap_or(0);
    }
    u32::from_le_bytes(bytes)
}

fn text_offset(base: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + VGA_TEXT_MEMORY_SIZE as u32;
    if (base..end).contains(&address) && address + width as u32 <= end {
        Some((address - base) as usize)
    } else {
        None
    }
}

fn planar_offset(address: u32, width: usize) -> Option<usize> {
    let end = VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
    if (VGA_MODE13H_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - VGA_MODE13H_BASE) as usize)
    } else {
        None
    }
}

fn legacy_gfx_aperture_offset(base: u32, length: u32, address: u32, width: usize) -> Option<usize> {
    let end = base + length;
    if !(base..end).contains(&address) || address + width as u32 > end {
        return None;
    }
    let offset = ((address - base) % VGA_PLANAR_WINDOW_SIZE) as usize;
    (offset + width <= VGA_PLANAR_WINDOW_SIZE as usize).then_some(offset)
}

fn margo_lfb_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_LFB_BASE + MARGO_VRAM_SIZE as u32;
    if (MARGO_LFB_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_LFB_BASE) as usize)
    } else {
        None
    }
}

/// Whether `address` falls in the Margo MMIO register window. A constant-range
/// test the bus uses to decide whether a read has to compute the in-batch Margo
/// nanosecond offset at all, so the LFB / planar / text / chain-4 read paths do
/// not pay for a peek only STATUS consumes.
pub(crate) fn margo_mmio_at(address: u32) -> bool {
    (MARGO_MMIO_BASE..MARGO_MMIO_BASE + MARGO_MMIO_SIZE as u32).contains(&address)
}

fn margo_mmio_offset(address: u32, width: usize) -> Option<usize> {
    let end = MARGO_MMIO_BASE + MARGO_MMIO_SIZE as u32;
    if (MARGO_MMIO_BASE..end).contains(&address) && address + width as u32 <= end {
        Some((address - MARGO_MMIO_BASE) as usize)
    } else {
        None
    }
}

/// Borrowed outer routing/configuration latches for canonical comparison.
///
/// These six scalars are the authoritative guest-programmed routing state the
/// Vega owner holds directly: the Margo personality, aperture, and bank
/// latches plus the Distira PCI command, BAR0, and init-enable latches. The
/// Margo linear and bank latches deliberately retain stale values across a
/// legacy mode set (select_legacy clears only margo_active). Routing-outcome
/// parity for active_display() additionally requires Distira's own
/// display_enabled state, which belongs to the future Distira owner; this
/// section is necessary but not sufficient on its own.
pub(crate) struct CanonicalVega<'a> {
    vega: &'a Vega,
}

impl CanonicalVega<'_> {
    /// Writes version 1 of the fixed 14-byte Vega outer-routing payload.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        out.write_bool(self.vega.margo_active)?;
        out.write_bool(self.vega.margo_linear)?;
        out.write_u16(self.vega.margo_bank)?;
        out.write_u16(self.vega.distira_command)?;
        out.write_u32(self.vega.distira_mem_base)?;
        out.write_u32(self.vega.distira_init_enable)
    }
}
