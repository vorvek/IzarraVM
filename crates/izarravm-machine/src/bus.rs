// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(feature = "jit")]
use izarravm_cpu::PollLoop;

#[cfg(feature = "jit")]
#[derive(Debug, Clone, Copy)]
pub(super) struct PollBusCertificate {
    raw_clocks_per_iteration: u64,
}

#[cfg(all(test, feature = "jit"))]
impl PollBusCertificate {
    pub(super) fn raw_clocks_per_iteration(self) -> u64 {
        self.raw_clocks_per_iteration
    }
}

#[cfg(feature = "jit")]
fn ranges_overlap(start: u32, len: u32, observed_start: u32, observed_len: u32) -> bool {
    let Some(end) = start.checked_add(len) else {
        return true;
    };
    let Some(observed_end) = observed_start.checked_add(observed_len) else {
        return true;
    };
    start < observed_end && observed_start < end
}

/// Whether the Accurate (386) class also answers the time-derived poll ports
/// lazily, i.e. WITHOUT ending the CPU batch. `IZARRAVM_LAZY_PORT_386`.
///
/// DEFAULT OFF, and that is a fidelity decision, not caution: see
/// `MachineBus::lazy_ports_386` for the exact drift this changes. Read once
/// per process; the run loop reads the resolved bool, never the environment.
fn lazy_port_reads_386_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("IZARRAVM_LAZY_PORT_386")
            .map(|value| matches!(value.trim(), "1" | "on" | "true" | "yes"))
            .unwrap_or(false)
    });
    *ENABLED
}

/// Whether the bus screens extended RAM out of the device-window classification
/// instead of walking the aperture gauntlet for it. `IZARRAVM_EXTENDED_RAM_SCREEN`.
///
/// DEFAULT ON. The screen changes no charged wait-state and no guest-visible
/// value -- it only removes work -- so there is no fidelity reason to ship it
/// off. The knob exists for MEASUREMENT: the win it claims is a few percent of
/// wall, which is close enough to this box's noise floor that a two-binary
/// comparison cannot carry it (layout variance between two builds has been
/// measured at 3.7% here). One binary, two arms, interleaved, is the only way to
/// grade it honestly.
///
/// UNSET means ON, so a recorded run with the variable absent is the ON arm.
/// `0`, `off`, `false`, `no` and the empty string are the escape; every other
/// spelling is ON.
///
/// Read once per process; the bus stores the resolved bool, so the hot path
/// never touches the environment or a lazy static.
pub(super) fn extended_ram_screen_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("IZARRAVM_EXTENDED_RAM_SCREEN")
            .map(|value| !matches!(value.trim(), "0" | "off" | "false" | "no" | ""))
            .unwrap_or(true)
    });
    *ENABLED
}

/// Whether a guest port access to a legacy X-bus device is charged its real ISA
/// bus time (`isa_io_clocks`, ~1.00 us), in BOTH directions.
/// `IZARRAVM_ISA_IO_WAIT`.
///
/// **DEFAULT OFF**, and the OFF arm is byte-identical to the pre-slice tree by
/// construction: this predicate gates the single call to `charge_isa_io_wait` on
/// each of `read_io`/`write_io`, and the refusal term it adds to
/// `poll_bus_certificate_from`.
///
/// WHAT THE ON ARM BUYS, and why it is not a nicety. On the I586 persona one
/// 8-bit `IN` costs the guest 1.610 clocks today (`IN_PORT_CORE_CLOCKS` 12
/// scaled by `level_timing(I586) = (1,12)`, plus `clocks_for(Byte,
/// wait_states.io = 2)` = 4 raw bus clocks scaled by `bus_timing(I586) =
/// (16,105)`), and one `OUT` 1.443. 86Box charges `ISA_CYCLES(8)` = 160 CPU
/// cycles = 0.96 us on the PIT, on BOTH directions
/// (`.bench/86Box-source/src/pit_fast.c:423` write, `:563` read; a Pentium
/// machine always selects `PIT_8254_FAST`, `src/machine/machine.c:199-211`).
/// The IBM PC AT Technical Reference specifies 6 BCLK = 720-750 ns for an 8-bit
/// bus operation to an 8-bit device
/// (`dev_docs/reference/ibm-bios/6183355_PC_AT_Technical_Reference_Mar86.txt:477-484`),
/// and Abrash measured 780-813 ns for `OUT DX,AL` on a good 486/33 ISA board
/// (Black Book Table 45.1). We are two orders of magnitude under all three.
/// Full derivation and prior art: `dev_docs/isa-io-waitstate-research-2026-08-30.md`.
///
/// THE CONVENTION (mirroring `out_imm8_rows_armed` in `izarravm-cpu`): unset and
/// `""` name the SAME (default) arm, `0`/`off` state it, `1`/`on` name the
/// charge, and anything else panics rather than silently running the base -- a
/// mistyped ladder leg that quietly ran the default would be read as the arm it
/// did not run.
///
/// Read once per process; the bus stores the resolved bool at construction, so
/// the per-access hot path is one bool test, never an environment or lazy-static
/// touch.
pub(super) fn isa_io_wait_armed() -> bool {
    #[cfg(test)]
    if let Some(forced) = ISA_IO_WAIT_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| parse_isa_io_wait_arm(std::env::var("IZARRAVM_ISA_IO_WAIT")))
}

/// The `IZARRAVM_ISA_IO_WAIT` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `isa_io_wait_armed`.
fn parse_isa_io_wait_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset is the default. The default flipped ON on 2026-08-30, owner-directed, on the
        // three-row ladder slate (.bench/results/isa-io-wait-ladder-20260830): gp2-586 min-wall
        // 1.7818 with every round's effect ~30x its own within-arm null (armed legs rt
        // 2.11-2.17), descent2-3dfx-586 1.0840 min-wall / 1.1019 mean at 4/4 valid rounds under
        // a pre-declared 4% bar, wolf3d-586 0.9991 as the uncharged-row null control. The flip
        // is an ERA BOUNDARY for instruction-count and rt series; the four final_instructions
        // pins moved with it (dev_docs/isa-io-wait-results-2026-08-30.md).
        Err(std::env::VarError::NotPresent) => return true,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_ISA_IO_WAIT is set to a value that is not valid UTF-8; accepted \
                 spellings are unset (the shipped default since the 2026-08-30 flip: ON), `1` \
                 or `on` (the same arm, stated), and `0` or `off` (the pre-slice charging)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        // Empty names the SAME arm as unset -- the default.
        "" => true,
        "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_ISA_IO_WAIT={other:?} names no arm; accepted spellings are unset (the \
             shipped default since the 2026-08-30 flip: ON -- charge every legacy X-bus port \
             access, both directions, one ISA bus period), `1` or `on` (the same arm, stated) \
             and `0` or `off` (the pre-slice charging). Refusing to guess: a mistyped ladder \
             leg that silently ran the default would be read as the arm it did not run"
        ),
    }
}

// Per-THREAD, because the shipped knob is a process-wide `OnceLock` and the tests have to run
// both arms in one process. Mirrors `OUT_IMM8_ROWS_OVERRIDE` in `izarravm-cpu`.
#[cfg(test)]
thread_local! {
    static ISA_IO_WAIT_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force the `IZARRAVM_ISA_IO_WAIT` arm on this thread for the length of a test; `None` restores
/// the ambient reading.
#[cfg(test)]
pub(super) fn set_isa_io_wait_for_test(forced: Option<bool>) {
    ISA_IO_WAIT_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the tests; `isa_io_wait_armed` caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process.
#[cfg(test)]
pub(super) fn parse_isa_io_wait_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_isa_io_wait_arm(value)
}

/// Whether `port` decodes to a device 86Box charges ISA bus time for -- and
/// therefore, under `owner-ruling-86box-accuracy-parity`, the exact set this
/// charge may cover in either direction.
///
/// THE SET IS 86BOX'S, PORT FOR PORT. The research note's Shape C proposed the
/// whole legacy X-bus (PIC, keyboard, DMA, page registers, the x87 latches); an
/// adversarial read of every `ISA_CYCLES`/`cycles_sub` site in `.bench/86Box-source`
/// narrowed it to the three device families 86Box actually charges. Charging
/// beyond 86Box is charging where 86Box is accurate and we would be inventing --
/// outside the parity ruling, not sheltered by it -- and its largest term would
/// land on the V86 monitor's PIC OCW3 probes (prince/duke under TOKAEMM), which
/// no evidence asks for.
///
/// | ports | device | 86Box site | charged? |
/// |---|---|---|---|
/// | `0x40-0x43` | PIT (8254) | `src/pit_fast.c:423` write, `:563` read | **YES**, `ISA_CYCLES(8)` |
/// | `0x61`, `0x63` | PPI port B + its mirror | `src/device/port_6x.c:58`, `:91`, `:105` via `cycles_sub` | **YES**, `is486`-gated |
/// | `0x70-0x71` | RTC/CMOS (NMI mask shares 0x70) | `src/device/nvr_at.c:670`, `:722` | **YES** |
/// | `0x20-0x21`, `0xA0-0xA1` | PIC 1/2 | `src/device/pic.c` | NO -- no cycle charge anywhere |
/// | `0x60`, `0x64` | keyboard controller | `src/device/kbc_at.c` | NO |
/// | `0x00-0x0F`, `0xC0-0xDF` | DMA 1/2 | `src/dma.c` | NO |
/// | `0x80-0x8F` | DMA page + POST | `src/dma.c` | NO (only the generic unclaimed-port `io_delay`) |
/// | `0xF0-0xF1` | x87 latch strobes | — | NO |
///
/// A Pentium always selects the charging PIT (`PIT_8254_FAST`,
/// `src/machine/machine.c:199-211`), and `is486` is implied by `is586`, so both
/// gated charges are live on the persona this slice targets.
///
/// **WIDENING THIS SET NEEDS ITS OWN EVIDENCE AND ITS OWN LADDER.** The physical
/// argument (every one of those devices really does sit on the 8 MHz bus) is
/// sound and is NOT what was rejected; what was rejected is landing it here, on
/// the parity ruling's coat-tails, with no measurement of the rows it moves.
///
/// THE 4% DELTA, deliberately kept. 86Box charges `ISA_CYCLES(8)` = 8 BCLK = 160
/// CPU cycles = 0.960 us at 166 MHz; `isa_io_clocks` charges 1.000 us. Both sit
/// inside the 120-180 clock band the hardware documents support (research note
/// section 3), and sharing the constant with the OPL and SB16 DSP charges is
/// worth more than four percent of agreement with one of the two references --
/// a second, nearly equal constant would be a drift hazard, not a fidelity gain.
///
/// WHAT IS DELIBERATELY EXCLUDED, beyond the parity table above, and why each
/// exclusion is a decision rather than an oversight:
///
/// * **The VGA registers, including 0x3DA and the whole 0x3B0-0x3DF range.**
///   86Box charges these NOTHING: its per-card `vid_timing_*` costs feed
///   `svga_read_common` (the video MEMORY path,
///   `.bench/86Box-source/src/video/vid_svga.c:2039`), while `svga_in`'s
///   `case 0x3da:` (`:622-634`) adds no cycles at all. Under the
///   `owner-ruling-86box-accuracy-parity` ruling, being inaccurate the same way
///   86Box is inaccurate can merge -- so 0x3DA stays cheap here even though real
///   hardware and DOSBox-X both charge it. This is also where the largest counts
///   on the board live (gp2-586 reads 0x3DA 2.003 billion times in an 80 s run),
///   so repricing it is a separate slice with a separate risk budget, not a
///   side effect of this one.
/// * **The OPL ports and the SB16 DSP read ports.** They ALREADY take
///   `isa_io_clocks` on the read side (`read_io`'s OPL and DSP arms), default-on
///   in the Approximate class. Listing them here would charge those reads twice.
///   Their WRITE side stays uncharged: the research note's Shape C does not list
///   them, 86Box's charging of OPL writes is not established in the note, and
///   adding a write charge to a landed default-on mechanism is a different
///   change from arming a default-off one.
/// * **The Lotura chipset ports 0xE0-0xE7.** A host-native device of this
///   machine, not an ISA peripheral; nothing on the metal answers there.
/// * **IDE/ATA (0x1F0-0x1F7, 0x170-0x177) and PCI configuration (0xCF8-0xCFF).**
///   Explicitly left alone by the research note's Shape C. The ATA poll-skip
///   machinery prices its own iterations, and 0xCF8 is a PCI cycle, not an ISA
///   one.
/// * **Unclaimed ports.** 86Box charges a smaller `io_delay` (56 cycles at
///   166 MHz) when no handler answers (`src/io.c:384` and peers), and a real ISA
///   cycle does happen whether or not a device drives the data lines. That is a
///   real gap, but it is a different charge with a different magnitude, and
///   modelling it would move fixtures that touch no modelled device at all.
///   Left for a follow-up with its own ladder.
const fn port_is_legacy_isa_io(port: u16) -> bool {
    matches!(port, 0x0040..=0x0043 | 0x0061 | 0x0063 | 0x0070..=0x0071)
}

/// The one port the io-family poll skip can ever certify. Both entry points into
/// `poll_bus_certificate_from` reach it only after establishing the polled port
/// is the active colour status-1 register: the interpreter checks
/// `poll.resolved_port(cpu) != 0x03da` outright (`run.rs`), and the call-out seam
/// checks `vega.poll_skip_status1_port_active()`, which is `port_enabled(0x3da)`
/// plus the colour-personality tests. Named so the certificate's ISA-charge
/// refusal has a port to ask about without threading one through the seam's POD.
#[cfg(feature = "jit")]
pub(super) const POLL_SKIP_IO_PORT: u16 = 0x03da;

/// The whole composition rule for `MachineBus::lazy_ports_386`, split out from
/// the environment read so it is testable without touching process state.
///
/// The `!uses_approximate_timing()` term is the load-bearing half: it is what
/// makes the switch structurally unable to move 486/586, which already have the
/// 3DA and 0x61 arms from `lazy_port_reads`. The gameport is no longer one of
/// this switch's ports at all: its arm keys on the RC one-shots being idle, a
/// device-state question that is the same on every persona.
pub(super) const fn lazy_ports_386_composed(mode: GswMode, env_enabled: bool) -> bool {
    !mode.uses_approximate_timing() && env_enabled
}

/// `lazy_ports_386_composed` against the process environment. One call per bus
/// construction; the environment itself is read once per process.
pub(super) fn lazy_ports_386_for(mode: GswMode) -> bool {
    lazy_ports_386_composed(mode, lazy_port_reads_386_enabled())
}

impl Machine {
    pub(super) fn make_bus(&mut self) -> MachineBus<'_> {
        // Captured before the struct literal below since VEGA and trace are also
        // mutably borrowed by other fields in that same literal.
        let beam_at_batch_start = self.scanout_beam_dots();
        let margo_scanout_at_batch_start = self.vega.margo_scanout().is_some();
        let trace_elapsed_at_batch_start = self.trace.elapsed_clocks();
        // Read from the CPU, the same authoritative mode owner that scale_bus
        // uses. Machine's active_mode copy is kept for bus register readback.
        let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(self.cpu.level());
        // The per-persona I-cache fetch cost, snapshotted for the batch alongside the bus scale
        // (see `MachineBus::icache_fetch_clocks`). Captured here because `cache_model` is also
        // mutably borrowed by the literal below.
        let icache_fetch_clocks = u64::from(izarravm_bus::BusCycle::clocks_for(
            BusWidth::Byte,
            self.cache_model.code_fetch_wait_states(),
        ));
        MachineBus {
            memory: &mut self.memory,
            ram_lookup: &mut self.ram_lookup,
            vega: &mut self.vega,
            pci: &mut self.pci,
            rom: &self.rom,
            serial: &mut self.serial,
            serial2: &mut self.serial2,
            lpt: &mut self.lpt,
            lpt2: &mut self.lpt2,
            device_ports: &mut self.device_ports,
            open_bus: &mut self.open_bus,
            pic: &mut self.pic,
            pit: &mut self.pit,
            keyboard: &mut self.keyboard,
            gameport: &mut self.gameport,
            speaker: &mut self.speaker,
            rtc: &mut self.rtc,
            dma: &mut self.dma,
            fdc: &mut self.fdc,
            opl: &mut self.opl,
            sb16: &mut self.sb16,
            wavetable_mpu: &mut self.wavetable_mpu,
            midi_mpu: &mut self.midi_mpu,
            wss: &mut self.wss,
            wss_base: self.wss_base,
            wss_enabled: self.wss_enabled,
            ide: &mut self.ide,
            ata: &mut self.ata,
            bmide: &mut self.bmide,
            trace: &mut self.trace,
            pending_soft_int: &mut self.pending_soft_int,
            pending_bios32: &mut self.pending_bios32,
            last_int_vector: &mut self.last_int_vector,
            active_mode: self.active_mode,
            pending_mode: &mut self.pending_mode,
            fast_post: self.fast_post,
            booter_inert: self.booter_inert,
            program_runtime: self.program_runtime,
            pending_toka_service: &mut self.pending_toka_service,
            toka_service_status: self.toka_service_status,
            pending_cd_doorbell: &mut self.pending_cd_doorbell,
            cd_doorbell_status: &mut self.cd_doorbell_status,
            cd_redirector_armed: self.cd_redirector_dos_ds.is_some(),
            unittester: &mut self.unittester,
            wait_states: self.profile.wait_states,
            cache: &mut self.cache_model,
            icache_fetch_clocks,
            flat_data_cost: self.active_mode.uses_approximate_timing(),
            extended_ram_screen: extended_ram_screen_enabled(),
            lazy_port_reads: self.active_mode.uses_approximate_timing(),
            isa_io_wait: isa_io_wait_armed(),
            lazy_ports_386: lazy_ports_386_for(self.active_mode),
            io_touched: &mut self.io_touched,
            exempt_io_touched: &mut self.exempt_io_touched,
            ata_poll_skip_enabled: self.ata_poll_skip_enabled,
            ata_poll_skip_armed: &mut self.ata_poll_skip_armed,
            ata_poll_skip_slice_too_short: self.ata_poll_skip_slice_too_short,
            ata_poll_skip: &mut self.ata_poll_skip,
            isa_io_clocks: &mut self.isa_io_batch_clocks,
            pit_observer_fine_until: &mut self.pit_observer_fine_until,
            opl_probe: &mut self.opl_probe,
            device_wrote_memory: &mut self.device_wrote_memory,
            pending_device_memory_write_range: &mut self.pending_device_memory_write_range,
            direct_map_changed: &mut self.direct_map_changed,
            direct_data_map_changed: &mut self.direct_data_map_changed,
            aperture_content_changed: &mut self.aperture_content_changed,
            direct_mapping_epoch: &mut self.direct_mapping_epoch,
            vga_wipe_census: &mut self.vga_wipe_census,
            core_clocks_so_far: 0,
            prior_runs_core_clocks: 0,
            timeline_at_batch_start: self.timeline,
            master_ticks_at_batch_start: self.timeline.now_ticks(),
            beam_at_batch_start,
            margo_scanout_at_batch_start,
            trace_elapsed_at_batch_start,
            bus_rem_at_batch_start: self.bus_rem,
            bus_num_at_batch_start,
            bus_den_at_batch_start,
        }
    }

    /// A bus for CONSTRUCTION-TIME device programming -- the seed writes a machine
    /// performs before any guest instruction runs (`new_raw_program`'s PIT seed and
    /// 8259 pair setup in `dos.rs`). Identical to `make_bus` except that the ISA
    /// wait-state charge is disarmed.
    ///
    /// WHY IT HAS TO BE DISARMED, and why clearing the accrual afterwards would be
    /// the weaker fix. `isa_io_batch_clocks` is an accrual belonging to a live CPU
    /// batch: the run loop folds it into that batch's guest-clock step and zeroes it
    /// (`run_until_tick`), and `canonical_state_capture` refuses a machine holding a
    /// non-zero one precisely because an uncommitted accrual has no batch to belong
    /// to (`MachineCanonicalCaptureError::UncommittedBatchTiming`). A construction
    /// write happens outside every batch, so a charge taken there is not "pending",
    /// it is SPURIOUS -- it would be paid by whichever batch happened to run first,
    /// and until then the machine cannot be captured at all. The seed writes model
    /// what the BIOS POST already did before the guest was loaded; that time is not
    /// the guest's to pay.
    ///
    /// Found by adversarial review of the `IZARRAVM_ISA_IO_WAIT` slice: armed, a
    /// freshly constructed `new_raw_program` machine held three PIT periods of
    /// accrual and refused canonical capture before executing an instruction.
    pub(super) fn make_construction_bus(&mut self) -> MachineBus<'_> {
        let mut bus = self.make_bus();
        bus.isa_io_wait = false;
        bus
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let mut bus = self.make_bus();
        bus.read_phys_u8(address).unwrap_or(0)
    }

    /// The last fatal-fault line this machine reported, as printed to stderr.
    /// Kept so the reporting is assertable: the line itself goes to stderr,
    /// which a test cannot read.
    pub fn last_fault_line(&self) -> Option<&str> {
        self.last_fault_line.as_deref()
    }

    /// Read one byte at a LINEAR address, walking the guest's own page tables
    /// when paging is on. `None` means the address is not mapped.
    ///
    /// Host-side diagnostics only, and deliberately not the CPU's own
    /// `translate_linear`: that one is not a probe. It sets CR2 on a miss,
    /// issues charged page-walk bus reads, and writes accessed bits back into
    /// guest memory through a path that reaches `note_code_write`. A dump that
    /// mutates the state it is dumping is worse than no dump.
    ///
    /// Reading a linear address as if it were physical is the bug this exists
    /// to stop, and it is a quiet one: it returns plausible bytes rather than
    /// failing, so the reader believes them. It has already been made once,
    /// against Doom under JemmEx, which maps non-identity.
    ///
    /// Two limits remain, and callers should not paper over them. An unbacked
    /// but MAPPED address reads as 0xFF, because that is what the bus fills for
    /// open bus and for anything past installed RAM, so `None` distinguishes
    /// untranslatable and not unbacked. And a byte inside the VGA aperture is
    /// fetched through the normal read path, which loads the VGA read latches:
    /// dumping there is not free of guest-visible effect, on a machine the GUI
    /// can resume.
    pub fn read_linear_u8(&mut self, linear: u32) -> Option<u8> {
        let physical = self.translate_linear_probe(linear)?;
        Some(self.read_physical_u8(physical))
    }

    /// The linear-to-physical half of `read_linear_u8`, on its own. Same probe discipline: it
    /// walks the guest's own tables with uncharged byte reads, sets no CR2, writes no accessed
    /// bit, and returns `None` for an unmapped address.
    ///
    /// No longer diagnostics-only: the HLE BIOS block writers
    /// (`write_guest_linear_block` / `read_guest_linear_block`) deliver
    /// caller-visible data through this walk, because a BIOS service handed
    /// an ES:DI block must honor the caller's paging. The probe discipline
    /// stands regardless -- a BIOS writing a reply is not an instruction, so
    /// it must not set CR2, dirty a page, or check R/W and U/S. The known
    /// consequences (no accessed/dirty bits for a paging-to-disk host; no
    /// permission check) are recorded on those helpers.
    pub fn translate_linear_probe(&mut self, linear: u32) -> Option<u32> {
        if self.cpu.control.cr0 & 0x8000_0000 == 0 {
            return Some(linear);
        }
        let directory = self.cpu.control.cr3 & !0xfff;
        let pde = self.walk_entry(directory + (linear >> 22) * 4);
        if pde & 1 == 0 {
            return None;
        }
        let physical = if pde & 0x80 != 0 {
            // PSE: a 4 MB page maps its whole range from the directory entry,
            // with no page table to consult. Note this does not consult CR4.PSE,
            // so a guest that sets bit 7 on a machine without PSE is
            // mistranslated here. Inherited from the walker this replaced, and
            // left alone rather than silently diverging from it.
            (pde & 0xffc0_0000) | (linear & 0x003f_ffff)
        } else {
            let pte = self.walk_entry((pde & !0xfff) + ((linear >> 12) & 0x3ff) * 4);
            if pte & 1 == 0 {
                return None;
            }
            (pte & !0xfff) | (linear & 0xfff)
        };
        Some(physical)
    }

    /// `CpuBus::peek_direct_ram` against this machine's live bus, for the ONE thing a host-side
    /// test needs it for: asking whether a physical address is the aligned, page-local, A20-clean
    /// plain RAM that the call-out's phase P requires. Charges nothing, exactly as the trait
    /// method it forwards to. Not a data path -- see that method's doc.
    ///
    /// NAMED AND HIDDEN so it cannot be mistaken for one. A `pub` cross-crate forwarder to an
    /// uncharged RAM read is exactly the hole the trait method's doc warns about: a caller that
    /// reaches for the short name and silently deletes its access from `elapsed_clocks` and from
    /// the bus trace, which are the currency every performance comparison in this project is
    /// measured in. The `_for_probe` suffix makes the misuse read wrong at the call site, and
    /// `#[doc(hidden)]` keeps it out of the crate's surface. The whole caller set is the section
    /// 6.0 TSS-geometry test.
    #[doc(hidden)]
    pub fn peek_direct_ram_for_probe(&mut self, physical: u32, width: BusWidth) -> Option<u32> {
        let bus = self.make_bus();
        bus.peek_direct_ram(physical, width)
    }

    /// One page-table entry, assembled from four byte reads.
    ///
    /// Not `read_physical_u32`, which goes through `read_memory` and charges bus
    /// clocks: `elapsed_clocks` is the currency every performance comparison in
    /// this project is measured in, and a diagnostic must not move it. Four byte
    /// reads take the uncharged path, which is also what the walker this
    /// replaced did. Reading a dword here would additionally A20-gate the entry
    /// fetch while the data byte below stays ungated, so the two halves of one
    /// translation would disagree about A20.
    fn walk_entry(&mut self, address: u32) -> u32 {
        u32::from_le_bytes([
            self.read_physical_u8(address),
            self.read_physical_u8(address.wrapping_add(1)),
            self.read_physical_u8(address.wrapping_add(2)),
            self.read_physical_u8(address.wrapping_add(3)),
        ])
    }

    pub fn read_physical_u16(&mut self, address: u32) -> u16 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Word, BusAccessKind::DataRead)
            .map(|value| value as u16)
            .unwrap_or(0)
    }

    pub fn read_physical_u32(&mut self, address: u32) -> u32 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap_or(0)
    }

    /// Last byte written to a passive I/O port (such as 0x80, the POST diagnostic
    /// port), or None if the port address is not in the passive port map. A
    /// decoded but never written port reads its default, not None.
    pub fn io_port(&self, port: u16) -> Option<u8> {
        self.device_ports.read_port(port)
    }

    pub fn write_physical_u8(&mut self, address: u32, value: u8) {
        let mut footprint = RamWriteFootprint::default();
        {
            let mut bus = self.make_bus();
            let _ = bus.write_memory_byte_recorded(address, value, &mut footprint);
        }
        footprint.notify(&mut self.cpu);
    }

    pub fn write_physical_u16(&mut self, address: u32, value: u16) {
        let mut footprint = RamWriteFootprint::default();
        {
            let mut bus = self.make_bus();
            let _ = bus.write_memory_recorded(
                address,
                BusWidth::Word,
                u32::from(value),
                BusAccessKind::DataWrite,
                &mut footprint,
            );
        }
        footprint.notify(&mut self.cpu);
    }

    pub fn write_physical_u32(&mut self, address: u32, value: u32) {
        let mut footprint = RamWriteFootprint::default();
        {
            let mut bus = self.make_bus();
            let _ = bus.write_memory_recorded(
                address,
                BusWidth::Dword,
                value,
                BusAccessKind::DataWrite,
                &mut footprint,
            );
        }
        footprint.notify(&mut self.cpu);
    }

    pub(super) fn write_guest_block(&mut self, address: u32, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut footprint = if u32::try_from(bytes.len()).is_ok() {
            RamWriteFootprint::default()
        } else {
            RamWriteFootprint::coarse()
        };
        for (offset, &value) in bytes.iter().enumerate() {
            let at = address.wrapping_add(offset as u32);
            // Keep the old per-byte bus lifetime. Dropping the bus completes a Vega direct-write
            // batch, so widening that lifetime here would change device-visible batching.
            let mut bus = self.make_bus();
            let _ = bus.write_memory_byte_recorded(at, value, &mut footprint);
        }
        footprint.notify(&mut self.cpu);
    }

    /// `write_guest_block` against a guest LINEAR address, walking the guest's
    /// own page tables for every page the block touches.
    ///
    /// An HLE BIOS service never issues the guest's memory writes, so nothing
    /// else applies the page tables on its behalf: whatever address the service
    /// computes from the caller's `ES:DI` goes straight onto the bus. That is
    /// correct only while linear equals physical. It stops being correct under
    /// a memory manager that maps upper memory from extended memory, which
    /// TOKAEMM does, and a DPMI transfer buffer allocated out of a UMB lands
    /// exactly there. The block is then deposited at the unbacked physical
    /// address of the same number and the caller reads back 0xFF, with no error
    /// reported anywhere. The same mistake was made once before, in the fault
    /// dump; `machine_fault_site_test.rs` records it.
    ///
    /// An unmapped page is not faulted on. A BIOS service is not an
    /// instruction: it has no faulting frame to deliver, and the guest's own
    /// read of the buffer will take the page fault in its place.
    ///
    /// The block STOPS at the first page that cannot be translated, and the
    /// return is how many bytes landed, which is `bytes.len()` when every page
    /// translated. A short delivery is not reported anywhere else, so a service
    /// that reports a transferred count -- AL for INT 13h AH=02h, the
    /// block-count field of a Disk Address Packet -- has to take that count
    /// from here or it reports bytes that never arrived. Stopping is what makes
    /// that count describe memory: resuming on the far side of a hole would
    /// leave bytes in the caller's buffer that the count says did not arrive.
    pub(super) fn write_guest_linear_block(&mut self, linear: u32, bytes: &[u8]) -> usize {
        for (offset, chunk) in Self::linear_pages(linear, bytes.len()) {
            let Some(physical) = self.translate_linear_probe(linear.wrapping_add(offset)) else {
                return offset as usize;
            };
            self.write_guest_block(physical, &bytes[offset as usize..][..chunk]);
        }
        bytes.len()
    }

    /// How many bytes from `linear`, up to `len`, lie in pages the guest's
    /// tables currently translate. `len` means the whole range is reachable.
    ///
    /// The read side of the same problem `write_guest_linear_block` reports:
    /// `read_guest_linear_block` fills an untranslatable page with 0xFF, and
    /// 0xFF is also what the bus returns for a page that IS mapped but has
    /// nothing behind it, so the fill cannot be told from real read-back. A
    /// service that takes guest bytes and COMMITS them somewhere persistent --
    /// a disk sector, a serial port -- must ask this first and shorten the
    /// transfer rather than commit bytes the guest never supplied.
    ///
    /// This detects only pages the guest's tables do not map. A mapped page
    /// with no memory behind it still reads 0xFF and is not reported here;
    /// nothing distinguishes that case from a guest that really stored 0xFF.
    pub(super) fn linear_present_prefix(&mut self, linear: u32, len: usize) -> usize {
        for (offset, _) in Self::linear_pages(linear, len) {
            if self
                .translate_linear_probe(linear.wrapping_add(offset))
                .is_none()
            {
                return offset as usize;
            }
        }
        len
    }

    /// `read_guest_block` against a guest LINEAR address. Same walk and the same
    /// reasons as `write_guest_linear_block`; an unmapped page reads as 0xFF,
    /// which is what the bus returns for unbacked memory anyway.
    pub(super) fn read_guest_linear_block(&mut self, linear: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0xffu8; len];
        for (offset, chunk) in Self::linear_pages(linear, len) {
            if let Some(physical) = self.translate_linear_probe(linear.wrapping_add(offset)) {
                for index in 0..chunk {
                    out[offset as usize + index] =
                        self.read_physical_u8(physical.wrapping_add(index as u32));
                }
            }
        }
        out
    }

    /// Split `[linear, linear + len)` into `(offset_from_linear, length)` runs
    /// that each stay inside one 4 KB page, so one translation covers each run.
    fn linear_pages(linear: u32, len: usize) -> Vec<(u32, usize)> {
        let mut runs = Vec::new();
        let mut offset = 0usize;
        while offset < len {
            let at = linear.wrapping_add(offset as u32);
            let to_page_end = (0x1000 - (at & 0xfff)) as usize;
            let chunk = to_page_end.min(len - offset);
            runs.push((offset as u32, chunk));
            offset += chunk;
        }
        runs
    }

    pub fn bus_trace(&self) -> &BusTrace {
        &self.trace
    }

    pub fn set_bus_trace_detailed(&mut self, detailed: bool) {
        self.trace.set_tracing_mode(if detailed {
            TracingMode::Full
        } else {
            TracingMode::Off
        });
    }
}

#[derive(Clone, Copy)]
enum ByteRoute {
    DirectRam(usize),
    Rom,
    OpenBus,
    DeviceOrFallbackRam,
}

trait RamWriteRecorder {
    fn record_ram_write(&mut self, physical: u32, width: u32);
}

struct IgnoreRamWrites;

impl RamWriteRecorder for IgnoreRamWrites {
    #[inline(always)]
    fn record_ram_write(&mut self, _physical: u32, _width: u32) {}
}

const RAM_WRITE_INLINE_SPANS: usize = 4;

struct RamWriteFootprint {
    spans: [(u32, u32); RAM_WRITE_INLINE_SPANS],
    span_count: usize,
    wrote_ram: bool,
    coarse: bool,
}

impl Default for RamWriteFootprint {
    fn default() -> Self {
        Self {
            spans: [(0, 0); RAM_WRITE_INLINE_SPANS],
            span_count: 0,
            wrote_ram: false,
            coarse: false,
        }
    }
}

impl RamWriteFootprint {
    // A scalar write can create at most four discontiguous byte footprints. Keeping those spans
    // inline avoids allocating on the public u8/u16/u32 paths. More fragmented bulk writes use a
    // conservative global code-cache reset instead of growing an unbounded footprint.
    fn coarse() -> Self {
        Self {
            coarse: true,
            ..Self::default()
        }
    }

    fn notify(self, cpu: &mut CpuGsw) {
        if !self.wrote_ram {
            return;
        }
        if self.coarse {
            cpu.note_device_memory_write();
            return;
        }
        for &(address, width) in &self.spans[..self.span_count] {
            cpu.note_device_memory_write_range(address, width);
        }
    }
}

impl RamWriteRecorder for RamWriteFootprint {
    fn record_ram_write(&mut self, physical: u32, width: u32) {
        debug_assert_ne!(width, 0);
        self.wrote_ram = true;
        if self.coarse {
            return;
        }
        if self.span_count != 0
            && let (start, len) = &mut self.spans[self.span_count - 1]
            && start.checked_add(*len) == Some(physical)
            && let Some(combined) = len.checked_add(width)
        {
            *len = combined;
            return;
        }
        if self.span_count == RAM_WRITE_INLINE_SPANS {
            self.span_count = 0;
            self.coarse = true;
            return;
        }
        self.spans[self.span_count] = (physical, width);
        self.span_count += 1;
    }
}

/// Flatten a `direct_write_identity` to the single byte the wipe census records.
///
/// The census exists to answer "did the aperture mapping move", so it must see the
/// COMPOSITE identity, not the raw VGA token: a 4F05 moves the mapping while the
/// raw token sits still. `lib.rs`'s recorder already logs the composite, and two
/// scales in one census would make its rows incomparable.
fn vega_census_token(identity: (u8, Option<u32>)) -> u8 {
    match identity.1 {
        Some(base) => 0x80 | ((base >> 16) as u8 & 0x3f),
        None => identity.0,
    }
}

impl MachineBus<'_> {
    /// The fetch-byte certification loop shared by every poll shape family:
    /// certify each slot's warm-RAM fetch range (rejecting a BIOS-stub overlay
    /// alias or any device-window byte) and sum the per-byte fetch cost.
    /// Callers add their own family-specific addend (the io wait-state read or
    /// the memory family's data-read cost) on top.
    /// The fetch-byte certification loop's real body, over a plain SLICE rather than a
    /// `PollLoop` -- so it can be shared by the interpreter's `poll_bus_certificate`
    /// (which has a real `PollLoop`) and the call-out-site seam's
    /// `poll_bus_certificate_from_fetches` (which only ever has the POD's `fetches` /
    /// `fetch_count`, because `PollLoop`'s fields are private to `izarravm-cpu` and
    /// cannot cross the seam -- GP2 poll-skip design, BLOCKER B). One derivation, two
    /// callers with two different shapes of input, so a mutant that certifies one
    /// wrong cannot leave the other looking right.
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    fn poll_fetch_certificate_raw_from(&self, fetches: &[(u32, u32, u8)]) -> Option<u64> {
        let mut raw = 0u64;
        for &(linear, physical, len) in fetches {
            let len = u32::from(len);
            if len == 0
                || ranges_overlap(linear, len, BIOS32_DIRECTORY_LINEAR, 1)
                || ranges_overlap(linear, len, BIOS32_PCI_LINEAR, 1)
                || ranges_overlap(linear, len, BIOS_LEGACY_IRET_LINEAR, BIOS_STUB_WINDOW_LEN)
            {
                return None;
            }
            let last = physical.checked_add(len - 1)?;
            let first_gated = self.apply_a20(physical);
            let last_gated = self.apply_a20(last);
            if last_gated != first_gated.checked_add(len - 1)?
                || usize::try_from(last_gated).ok()? >= self.memory.len()
            {
                return None;
            }
            for offset in 0..len {
                let address = first_gated.checked_add(offset)?;
                if self.is_device_window(address, BusWidth::Byte) {
                    return None;
                }
            }
            raw = raw.checked_add(u64::from(BusCycle::clocks_for(
                BusWidth::Byte,
                self.cache.code_fetch_wait_states(),
            )))?;
        }
        Some(raw)
    }

    /// `poll_fetch_certificate_raw_from` over a certified `PollLoop`'s own fetch set.
    /// Byte-identical to the pre-split behaviour: this just builds the slice
    /// `poll_fetch_certificate_raw_from` now certifies.
    #[cfg(feature = "jit")]
    fn poll_fetch_certificate_raw(&self, poll: PollLoop) -> Option<u64> {
        let mut fetches = [(0u32, 0u32, 0u8); 6];
        for (slot, entry) in fetches.iter_mut().zip(0..poll.fetch_count()) {
            *slot = poll.fetch(entry)?;
        }
        // M1: `poll.fetch_count()` is always <= 6 for every certified shape today, but the old
        // pre-split code degraded to `None` (via `fetch(entry)?` above) on an out-of-range
        // `fetch_count()` rather than panicking. `.min(6)` preserves that refusal shape instead of
        // turning a future shape's arity mistake into an index-out-of-range panic reachable from
        // guest execution.
        self.poll_fetch_certificate_raw_from(&fetches[..poll.fetch_count().min(6)])
    }

    /// Certify exact warm-RAM fetch and I/O costs for the classified io-family poll
    /// loop, from the POD's own `fetches`/`fetch_count` rather than a `PollLoop` --
    /// the call-out-site seam's entry point (BLOCKER B), sharing
    /// `poll_fetch_certificate_raw_from` with the interpreter's `poll_bus_certificate`
    /// below so the two can never certify the same shape differently.
    #[cfg(feature = "jit")]
    fn poll_bus_certificate_from(
        &self,
        fetches: &[(u32, u32, u8)],
        port: u16,
    ) -> Option<PollBusCertificate> {
        if self.trace.tracing_mode() != TracingMode::Off || !self.lazy_port_reads {
            return None;
        }
        // THE ISA-CHARGE REFUSAL (`IZARRAVM_ISA_IO_WAIT`). This certificate is
        // denominated in RAW BUS CLOCKS: `poll_project_scaled_bus_clocks` puts
        // the whole per-iteration figure through `scale_bus`, and `poll_commit_bus`
        // commits it into the bus trace. The ISA charge is deliberately NOT in that
        // unit -- it is unscaled CPU clocks accrued into `isa_io_batch_clocks`
        // and folded into the batch step outside `scale_bus` (see
        // `charge_isa_io_wait`) -- so it CANNOT be added to `raw` without being
        // divided by the persona's bus scaler, which is the whole thing the
        // charge exists to escape. A certificate that simply omitted it would
        // under-price every skipped iteration by ~1 us and let the skip machinery
        // fast-forward a span the guest could not have run, which is the one
        // correctness interaction the research note names as a blocker.
        //
        // So: on a charged port, REFUSE. Poll skip declines and the loop runs its
        // iterations for real, each paying the charge through `read_io`. This is
        // conservative rather than free, and today it is also inert: the io-family
        // poll skip only ever certifies `POLL_SKIP_IO_PORT` (0x3DA), which is
        // deliberately NOT in the charged set (86Box parity -- see
        // `port_is_legacy_isa_io`). The guard is here so that a future slice which
        // widens the charged set -- the DOSBox-X-style flat model over every port,
        // Shape A in the note -- disables the skip instead of silently
        // under-pricing it.
        if self.isa_io_wait && self.lazy_port_reads && port_is_legacy_isa_io(port) {
            return None;
        }
        let mut raw = self.poll_fetch_certificate_raw_from(fetches)?;
        raw = raw.checked_add(u64::from(BusCycle::clocks_for(
            BusWidth::Byte,
            self.wait_states.io,
        )))?;
        Some(PollBusCertificate {
            raw_clocks_per_iteration: raw,
        })
    }

    /// Certify exact warm-RAM fetch and I/O costs for the classified io-family
    /// poll loop. BYTE-IDENTICAL to the pre-memory-poll-shape behavior: this
    /// function is never called for a memory-family `PollLoop` (the executor
    /// dispatches on `family()` before certification), so its own logic and
    /// order are unchanged. Delegates to `poll_bus_certificate_from` (BLOCKER B).
    #[cfg(feature = "jit")]
    pub(super) fn poll_bus_certificate(
        &self,
        poll: PollLoop,
        port: u16,
    ) -> Option<PollBusCertificate> {
        let mut fetches = [(0u32, 0u32, 0u8); 6];
        for (slot, entry) in fetches.iter_mut().zip(0..poll.fetch_count()) {
            *slot = poll.fetch(entry)?;
        }
        // M1: see `poll_fetch_certificate_raw`'s matching comment.
        self.poll_bus_certificate_from(&fetches[..poll.fetch_count().min(6)], port)
    }

    /// Certify exact warm-RAM fetch and data-read costs for the classified
    /// memory-family poll loop. `data_physical` is the polled cell's physical
    /// address, already resolved through `CpuGsw::probe_linear_read_physical`
    /// (R2: a TLB-hit-only, non-mutating probe run by the caller before this
    /// certificate is built). This function then applies the SAME
    /// `apply_a20` + `is_device_window` + single-physical-page checks the
    /// fetch certificate already applies to every fetch byte, to the data
    /// address, and adds one flat dword data-access charge
    /// (`jit_data_cost_clocks`, the same model `charge_direct_memory`'s
    /// direct-page hit uses).
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    pub(super) fn poll_memory_bus_certificate(
        &self,
        poll: PollLoop,
        data_physical: u32,
    ) -> Option<PollBusCertificate> {
        if self.trace.tracing_mode() != TracingMode::Off {
            return None;
        }
        let mut raw = self.poll_fetch_certificate_raw(poll)?;
        let width = u32::from(poll.memory_cell_width()?);
        if width == 0 {
            return None;
        }
        // Single-physical-page requirement: the R2 probe translated only the
        // first byte's page, so a range crossing a 4 KiB boundary has an
        // unverified physical for its tail bytes. Reject it outright (the
        // interpreter handles the split access correctly on its own).
        if (data_physical & 0x0fff) + width > 0x1000 {
            return None;
        }
        let last = data_physical.checked_add(width - 1)?;
        let first_gated = self.apply_a20(data_physical);
        let last_gated = self.apply_a20(last);
        if last_gated != first_gated.checked_add(width - 1)?
            || usize::try_from(last_gated).ok()? >= self.memory.len()
        {
            return None;
        }
        for offset in 0..width {
            let address = first_gated.checked_add(offset)?;
            if self.is_device_window(address, BusWidth::Byte) {
                return None;
            }
        }
        raw = raw.checked_add(self.jit_data_cost_clocks(BusWidth::Dword))?;
        Some(PollBusCertificate {
            raw_clocks_per_iteration: raw,
        })
    }

    #[cfg(feature = "jit")]
    pub(super) fn poll_project_scaled_bus_clocks(
        &self,
        certificate: PollBusCertificate,
        iterations: u64,
    ) -> Option<u64> {
        let additional = certificate
            .raw_clocks_per_iteration
            .checked_mul(iterations)?;
        self.trace.elapsed_clocks().checked_add(additional)?;
        self.jit_projected_batch_scaled_bus_clocks(additional)
    }

    /// Commit the certified aggregate clocks, then replay the idempotent status
    /// read side effects once. `io_touched` remains false like the lazy 3DA path.
    #[cfg(feature = "jit")]
    pub(super) fn poll_commit_bus(&mut self, certificate: PollBusCertificate, iterations: u64) {
        let additional = certificate
            .raw_clocks_per_iteration
            .checked_mul(iterations)
            .expect("projected poll bus multiplication must succeed");
        self.trace
            .elapsed_clocks()
            .checked_add(additional)
            .expect("projected poll bus clock addition must succeed");
        self.trace.add_elapsed_clocks(additional);
        self.vega.status1_side_effects();
    }

    /// Commit the certified aggregate clocks for a memory-family span. Unlike
    /// `poll_commit_bus`, there is no port side effect to replay: the memory
    /// shape never touches a device port.
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    pub(super) fn poll_commit_memory_bus(
        &mut self,
        certificate: PollBusCertificate,
        iterations: u64,
    ) {
        let additional = certificate
            .raw_clocks_per_iteration
            .checked_mul(iterations)
            .expect("projected poll bus multiplication must succeed");
        self.trace
            .elapsed_clocks()
            .checked_add(additional)
            .expect("projected poll bus clock addition must succeed");
        self.trace.add_elapsed_clocks(additional);
    }

    /// Charge one 8-bit ISA bus cycle for a legacy X-bus port access, in either
    /// direction. `IZARRAVM_ISA_IO_WAIT`, DEFAULT OFF (`isa_io_wait_armed`).
    ///
    /// WHY THE CHARGE IS THE SAME IN BOTH DIRECTIONS, which is the one thing a
    /// reader is most likely to expect to differ. On the Pentium an I/O write is
    /// NOT posted: "only memory writes are buffered and I/O writes are not"
    /// (Pentium Family Developer's Manual Vol 1,
    /// `dev_docs/reference/Pentium-K6/241428-004_..._Jul95.txt:3332`), and `OUT`
    /// does not begin the next instruction "before ... the I/O write has
    /// completed" (`:3403-3416`). 86Box charges the PIT identically on both
    /// directions (`pit_fast.c:423` write, `:563` read). DOSBox-X's 3/4 discount
    /// on writes is the outlier and has no Pentium justification, so it is not
    /// copied.
    ///
    /// THE CHARGE IS ADDITIVE, replacing nothing. The CPU-side `IN`/`OUT` core
    /// clocks and the recorded bus cycle both stay exactly as they are; this is
    /// the device-side ISA cycle on top, matching 86Box's structure (a flat
    /// per-instruction cost plus a per-device `ISA_CYCLES` charge).
    ///
    /// THE UNIT. `isa_io_clocks` is CPU clocks at the LIVE mode's clock rate --
    /// 166 at 166 MHz, 100 at 100 MHz -- so what is invariant across personas is
    /// the microsecond, not the clock count, which is what a fixed-frequency bus
    /// means. It accrues into `isa_io_batch_clocks`, which `run_until_tick` folds
    /// into the batch's guest-clock step OUTSIDE `scale_bus` (see the comment
    /// above that site): expressing this as bus wait states instead would put it
    /// through the 586's 16/105 bus scaler and shrink a microsecond to 0.15 of
    /// one, which is exactly the shape the charge exists to escape.
    ///
    /// GATED ON `lazy_port_reads` (the Approximate 486/586 class), like both
    /// existing `isa_io_clocks` sites. The Accurate 386 class advances devices
    /// per instruction and is the class whose clock bit-identity is still a merge
    /// bar, so it keeps today's cadence whatever this knob says. A conservative
    /// fork: the physical argument for the charge applies to a 386 too, and
    /// arming it there is a separate decision with its own re-pin.
    fn charge_isa_io_wait(&mut self, port: u16) {
        if self.isa_io_wait && self.lazy_port_reads && port_is_legacy_isa_io(port) {
            *self.isa_io_clocks += isa_io_clocks(self.active_mode);
        }
    }

    fn record_pending_device_memory_write(&mut self, physical: u32, width: u32) {
        if width == 0 || *self.device_wrote_memory {
            return;
        }
        match self.pending_device_memory_write_range {
            Some((start, pending_width)) if *start == physical => {
                *pending_width = (*pending_width).max(width);
            }
            None => {
                *self.pending_device_memory_write_range = Some((physical, width));
            }
            Some(_) => {
                *self.pending_device_memory_write_range = None;
                *self.device_wrote_memory = true;
            }
        }
    }

    fn advance_direct_mapping_epoch(&mut self) {
        advance_direct_mapping_epoch(self.direct_mapping_epoch);
    }

    fn mark_direct_map_changed(&mut self) {
        self.advance_direct_mapping_epoch();
        *self.direct_map_changed = true;
    }

    /// The VGA direct-write aperture re-pointed. Deliberately does NOT advance the direct-mapping
    /// epoch: the epoch is the "every cached host pointer is void" signal, and this event voids
    /// exactly one range. `CpuGsw::note_direct_data_map_changed` invalidates that range by hand at
    /// the batch boundary, and it can only do so while the epoch still matches the entries it is
    /// keeping. See that function for why the range is the whole scope.
    fn mark_direct_data_map_changed(&mut self) {
        *self.direct_data_map_changed = true;
    }

    fn set_a20_gate(&mut self, enabled: bool) {
        if self.keyboard.a20_enabled() != enabled {
            self.keyboard.set_a20(enabled);
            self.advance_direct_mapping_epoch();
        }
    }

    pub(crate) fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        let core_clocks_so_far = self.core_clocks_so_far;
        <Self as CpuBus>::write_io(
            self,
            port,
            width,
            value,
            core_clocks_so_far,
            cpu_is_ring0_pm,
        )
    }
}

impl Drop for MachineBus<'_> {
    fn drop(&mut self) {
        self.vega.finish_direct_write_batch();
    }
}

/// The A20 gate clears address line 20 when it is closed. With the gate off, any
/// physical address with bit 20 set folds down by 0x100000, so a real-mode
/// program reaching 0x100000-0x10FFEF (the most a seg:off pair can address) wraps
/// back to 0x0-0xFFEF, the classic 1 MiB wraparound the HMA depends on. The
/// effect is intentionally global, matching A20M# on real hardware: bit 20 is
/// cleared on every physical address, so high ROM (0xFFFF0000) and the upper half
/// of the Margo LFB alias down too when the gate is closed. That is unreachable
/// in normal use, since A20 powers on enabled and stays so unless a guest
/// deliberately closes it.
const A20_MASK: u32 = !(1 << 20);

/// The port each byte of a wider-than-byte I/O cycle targets. The IDE/ATA 16-bit
/// data registers (primary `0x1F0`, secondary `0x170`) stream every byte through
/// the same port via their data FIFO, so a word/dword access repeats the port.
/// Every other (8-bit-decoded) port takes consecutive bytes at `port`, `port+1`,
/// ... - exactly the VGA index/data-pair behaviour a single 16-bit `OUT` to
/// `0x3C4`/`0x3CE`/`0x3D4` relies on to set an index and its datum at once.
impl MachineBus<'_> {
    /// The VGA status ports (3DA/3BA/3C2), lifted out of `read_io` unchanged so the hot 0x3DA
    /// fast path near the top of that function and the in-order arm run ONE body instead of two
    /// copies that could drift apart.
    ///
    /// `None` means decline and fall through, which is exactly what the inactive status1 alias
    /// does in the non-lazy class. The lazy class answers 0xFF itself rather than declining.
    fn read_vga_status_port(&mut self, port: u16, skip_io_touched: bool) -> Option<u32> {
        if self.lazy_port_reads || self.lazy_ports_386 {
            let beam = self.predicted_beam();
            if let Some(value) = self.vega.read_status_port_lazy(port, beam) {
                return Some(u32::from(value));
            }
            // Inactive alias (e.g. 3BA polled in a color setup): no side
            // effects, matching `Vga::read_port`'s existing
            // `status1_port_selected` gate, and -- since this arm's static
            // port set is disjoint from every other device's decoded ports
            // (grep-confirmed: nothing else claims 0x3B0..=0x3DF) -- the same
            // 0xFF the non-lazy path's fallthrough to `device_ports`'s passive
            // table would eventually produce. Returned directly, without
            // setting io_touched, so an inactive-alias poll stays lazy too
            // instead of silently falling back to the old behavior.
            return Some(0xff);
        }
        if !skip_io_touched {
            *self.io_touched = true;
        }
        // The BEAM peek is taken in BOTH timing classes, on exactly the
        // same grounds as the 0x61 OUT peek and the 0x40-0x42 counter
        // peek below: it changes only the VALUE, never whether the batch
        // ends. `io_touched` is already set above, so this arm keeps the
        // batch-ending behavior the Accurate class has always had; only
        // the bits reported change.
        //
        // Why the Accurate class needs it. Without the peek this arm
        // read the LIVE beam, which is the beam as of BATCH START, and
        // only then ended the batch -- so a retrace poll reported a
        // position up to a whole batch stale. That was bounded at a
        // DAC period while the fine fallback was unconditional, but
        // `fine_batch_grain_required` now gates it (and no term in that
        // gate covers a display poll: 3DA/3BA arm nothing), so an
        // otherwise-idle 386 guest polling retrace sits on the 1 ms
        // coarse cap and reads a beam up to 1 ms old. The deadline cache
        // does not bound it either: `vega_edge_ticks` carries the Margo
        // blit and DISPLAY_START terms, not a retrace edge.
        //
        // `read_status_port_lazy` is the same function the lazy arm
        // above calls and performs the identical guest-visible side
        // effects a `read_port` of these three ports would
        // (`status1_side_effects` / `catch_up`); it declines exactly
        // where `read_port` declines -- the inactive status1 alias --
        // so the fallthrough at the call site is unchanged.
        let beam = self.predicted_beam();
        self.vega.read_status_port_lazy(port, beam).map(u32::from)
    }
}

const fn io_word_sub_port(port: u16, index: u32) -> u16 {
    if port == ata::PRIMARY_CMD_BASE || port == ide::SECONDARY_CMD_BASE {
        port
    } else {
        port.wrapping_add(index as u16)
    }
}

impl MachineBus<'_> {
    /// The RAM-only tail of `charge_direct_memory`: the flat L1 cost in the Approximate class, or
    /// the full A20-plus-wait-state routing otherwise. Factored out so the interpreter's FastMap
    /// serve path (`CpuBus::charge_direct_ram_memory`) can charge a `PageKind::Ram` hit without
    /// repeating the video-aperture range compare that `charge_direct_memory` already ran once at
    /// FastMap population time. The Mode13 aperture keeps going through the full
    /// `charge_direct_memory`, unchanged, so `note_direct_write` and its persona wait states never
    /// get bypassed.
    #[inline]
    fn charge_ram_only(&mut self, address: u32, width: BusWidth, kind: BusAccessKind) {
        if self.flat_data_cost {
            self.trace.record(kind, address, width, self.cache.cost.l1);
            return;
        }
        let address = self.apply_a20(address);
        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);
    }

    /// The classification tail of `charge_physical_instruction_fetch_run`: A20 folding, the
    /// uniform-region test, the I-cache-vs-device-window split, and the exact per-byte fallback.
    /// Out of line ON PURPOSE: this body is why ThinLTO used to refuse to inline the whole
    /// method, leaving the warm below-aperture fast path paying a call frame per instruction.
    /// The head owns the `count != 0` and below-0xA0000 screens, so every caller arrives here
    /// with a run that genuinely needs classifying.
    #[cold]
    #[inline(never)]
    fn charge_classified_instruction_fetch_run(
        &mut self,
        physical_start: u32,
        count: u32,
    ) -> Result<(), BusError> {
        let first = self.apply_a20(physical_start);
        let last = self.apply_a20(physical_start.wrapping_add(count - 1));
        let first_ws = self.code_fetch_wait_states(first);
        // Uniform iff every byte lands in the same wait-state region with no A20 wrap
        // between the ends. apply_a20 already folded both ends, so equal wait-states on
        // contiguous post-A20 addresses means the whole run is one region. The endpoint-only
        // test relies on `count` being one instruction's length (at most 15 bytes), far smaller
        // than any wait-state region: a narrower region wholly contained between two matching
        // endpoints cannot exist at that scale. A caller passing a large `count` must not assume
        // this holds; the non-uniform branch's exact per-byte loop is the safe fallback regardless.
        let uniform =
            last == first.wrapping_add(count - 1) && first_ws == self.code_fetch_wait_states(last);
        if uniform {
            // I-cache model: an instruction whose bytes lie in cacheable RAM is
            // delivered by the I-cache in ONE bus access, not one per byte. The
            // per-byte bus cost (>= 2 clocks/byte) is a slow-bus artifact; on a part
            // with an instruction cache a hit returns the whole (pre-decoded) line in
            // a single fetch. Charging per byte here floors every mode's Dhrystone/
            // Sieve far below its era band (the floor is the same clocks in every
            // mode, so the fast modes can never separate). One access per instruction
            // makes the bands reachable for the slower modes and lifts the fast modes
            // toward (though not all the way to, see bench_reference.rs) their targets.
            //
            // ROM / device code (uncached) keeps the exact per-byte charge: those
            // windows are not I-cached, so `is_device_window` routes them to the
            // per-byte loop below to preserve firmware/POST and device-execution
            // timing unchanged.
            if first >= 0x000A_0000 && self.is_device_window(first, BusWidth::Byte) {
                self.trace
                    .record_instruction_fetch_run(first, count, first_ws);
            } else {
                // Single I-cache access for the whole instruction run.
                self.trace.record_instruction_fetch_run(first, 1, first_ws);
            }
        } else {
            for i in 0..count {
                self.charge_instruction_fetch(physical_start.wrapping_add(i))?;
            }
        }
        Ok(())
    }
}

/// The call-out-site poll skip's 32-bit return-lane bound (GP2 poll-skip design obligation 4),
/// factored out of `MachineBus::callout_poll_skip` so it is unit-testable on its own (Rank-4
/// killer, GP2 poll-skip revision review: "no fixture ever produces a span within four orders of
/// magnitude of the ceiling, so a future edit to `direct_poll_skip_max_raw` or to the clamp
/// expression would ship silently"). A hardcoded ceiling INDEPENDENT of `max_skipped_raw`: the
/// caller's clock lane takes the LOW 32 BITS of an `i64`, and bit 32 is the step-break status
/// bit, so nothing this may admit may let `raw_core_clocks * iterations` reach it -- whatever a
/// misconfigured `IZARRAVM_DIRECT_POLL_MAX_RAW` claims. `max_skipped_raw` can still tune the
/// bound DOWN (a smaller request always wins the `.min`); it can never tune it up past this
/// ceiling. The 64-clock margin comfortably covers `IN_PORT_CORE_CLOCKS` (12, `izarravm-cpu`,
/// not visible from this crate) plus any other small additive term the caller folds in on top of
/// this function's own return.
#[cfg(feature = "jit")]
pub(crate) const LANE_SAFETY_CEILING: u64 = u32::MAX as u64 - 64;

#[cfg(feature = "jit")]
pub(crate) fn poll_skip_upper_bound(
    cap: u64,
    spent: u64,
    max_skipped_raw: u64,
    raw_core_clocks: u64,
) -> u64 {
    cap.saturating_sub(spent)
        .min(u64::from(u32::MAX))
        .saturating_sub(1)
        .min(max_skipped_raw.min(LANE_SAFETY_CEILING) / raw_core_clocks.max(1))
}

impl CpuBus for MachineBus<'_> {
    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            let value = match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            };
            return Ok(DirectMemoryRead {
                value,
                direct: true,
            });
        }
        if self
            .direct_margo_lfb_bytes(address, width.bytes() as usize, width)
            .is_some()
        {
            let data = self
                .vega
                .margo_lfb_direct_bytes(address, width.bytes() as usize)
                .expect("the LFB probe and copy must agree");
            let value = match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            };
            self.record_direct_vga_accesses(address, width.bytes() as usize, width, kind);
            return Ok(DirectMemoryRead {
                value,
                direct: true,
            });
        }
        // A MISALIGNED but page-local access into plain RAM. Today this falls through to
        // `read_memory`, which declines wide at `vega`, hits `should_split`, and recurses one
        // byte at a time; every byte re-asks `vega` and then reads the same `self.memory` slice.
        //
        // VALUE EQUALITY, argued from L-RAM per byte and never from a width-parameterised `vega`
        // predicate. `direct_page_ram_bytes_unaligned` succeeds only if `direct_ram_bytes` does,
        // which requires this page's `page_bases` entry to be non-`RAM_LOOKUP_SLOW`, which
        // `ram_lookup_page_is_direct` grants only to a page with NO `memory_bar_overlaps` byte.
        // That classification is page-granular BY CONSTRUCTION -- `ram_lookup_page_base` is
        // evaluated per page over the whole `[start, end)` -- so no byte of this page is in the
        // Distira BAR, so EVERY per-byte `vega` consultation in today's loop declines, whatever
        // width the wide call would have used. The bytes then come from the same slice, assembled
        // little-endian identically.
        //
        // Proving `read_wide_memory(address, width) == None` would prove NOTHING here: every
        // `*_offset` predicate requires the whole access in range, so a wide decline is strictly
        // weaker than "claims nothing", and a Word straddling a window's end declines wide while
        // its base byte is still claimed.
        if width.misaligned_at(address)
            && let Some((address, start, end)) =
                self.direct_page_ram_bytes_unaligned(address, width.bytes() as usize)
        {
            debug_assert!(
                self.vega.claims_no_byte_in(address, width.bytes()),
                "a direct RAM page claimed by a Vega aperture reached the unaligned admission"
            );
            self.charge_direct_ram_split(address, width, kind)?;
            let data = &self.memory.as_slice()[start..end];
            let value = match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            };
            return Ok(DirectMemoryRead {
                value,
                direct: true,
            });
        }
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    /// The aligned arm of `read_memory_direct` above with the charge pair deleted, and built by
    /// calling the SAME `direct_page_ram_bytes` predicate rather than by a parallel
    /// reimplementation -- the two must agree byte for byte, because the one caller reads the same
    /// address twice (once here, once through the charged arm) and pins the values equal.
    ///
    /// See the trait's doc for why nothing else may call this.
    fn peek_direct_ram(&self, address: u32, width: BusWidth) -> Option<u32> {
        let (_, start, end) = self.direct_page_ram_bytes(address, width.bytes() as usize, width)?;
        let data = &self.memory.as_slice()[start..end];
        Some(match width {
            BusWidth::Byte => u32::from(data[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
            BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        if let Some((address, start, _)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8)?,
                BusWidth::Word => self.memory.write_u16(start, value as u16)?,
                BusWidth::Dword => self.memory.write_u32(start, value)?,
            }
            return Ok(DirectMemoryWrite { direct: true });
        }
        if self
            .direct_margo_lfb_bytes(address, width.bytes() as usize, width)
            .is_some()
        {
            let data = self
                .vega
                .margo_lfb_direct_bytes_mut(address, width.bytes() as usize)
                .expect("the LFB probe and copy must agree");
            data.copy_from_slice(&value.to_le_bytes()[..width.bytes() as usize]);
            self.record_direct_vga_accesses(address, width.bytes() as usize, width, kind);
            return Ok(DirectMemoryWrite { direct: true });
        }
        // The write twin of the unaligned admission in `read_memory_direct`; see that comment for
        // the L-RAM per-byte argument. `Memory::write_u16`/`write_u32` take a BYTE offset and have
        // no alignment requirement of their own, so the data path needs no change.
        //
        // Here the lemma is the SHARP one, and the assert is not decoration. `write_wide_memory`'s
        // LFB arm SWALLOWS byte writes (`BusWidth::Byte => {}` while still returning `true`), so a
        // byte-split write into the LFB is DROPPED where the wide write would have stored -- silent
        // data loss, not a timing difference. It cannot arise today, because no BAR page is
        // `direct_ram_bytes`-able; asserting L-RAM per byte is what stops a future BAR or decode
        // change from making it arise silently.
        if width.misaligned_at(address)
            && let Some((address, start, _)) =
                self.direct_page_ram_bytes_unaligned(address, width.bytes() as usize)
        {
            debug_assert!(
                self.vega.claims_no_byte_in(address, width.bytes()),
                "a direct RAM page claimed by a Vega aperture reached the unaligned admission"
            );
            self.charge_direct_ram_split(address, width, kind)?;
            match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8)?,
                BusWidth::Word => self.memory.write_u16(start, value as u16)?,
                BusWidth::Dword => self.memory.write_u32(start, value)?,
            }
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)
            .map(|()| DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataRead || out.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if !out.len().is_multiple_of(access) {
            return Ok(0);
        }
        if let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, out.len(), access_width)
        {
            self.record_direct_ram_accesses(address, out.len(), access_width, kind);
            out.copy_from_slice(&self.memory.as_slice()[start..end]);
            return Ok(out.len());
        }
        if self
            .direct_margo_lfb_bytes(address, out.len(), access_width)
            .is_some()
        {
            out.copy_from_slice(
                self.vega
                    .margo_lfb_direct_bytes(address, out.len())
                    .expect("the LFB probe and copy must agree"),
            );
            self.record_direct_vga_accesses(address, out.len(), access_width, kind);
            return Ok(out.len());
        }
        let Some((address, page_offset)) =
            self.direct_vga_bytes(address, out.len(), access_width, false)
        else {
            return Ok(0);
        };
        let page = address & !(RAM_LOOKUP_PAGE_MASK as u32);
        let Some(ptr) = self.vega.mode13_direct_page(page) else {
            return Ok(0);
        };
        unsafe { std::ptr::copy_nonoverlapping(ptr.add(page_offset), out.as_mut_ptr(), out.len()) };
        self.record_direct_vga_accesses(address, out.len(), access_width, kind);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataWrite || data.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if !data.len().is_multiple_of(access) {
            return Ok(0);
        }
        if let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, data.len(), access_width)
        {
            self.record_direct_ram_accesses(address, data.len(), access_width, kind);
            self.memory.as_mut_slice()[start..end].copy_from_slice(data);
            return Ok(data.len());
        }
        if self
            .direct_margo_lfb_bytes(address, data.len(), access_width)
            .is_some()
        {
            self.vega
                .margo_lfb_direct_bytes_mut(address, data.len())
                .expect("the LFB probe and copy must agree")
                .copy_from_slice(data);
            self.record_direct_vga_accesses(address, data.len(), access_width, kind);
            return Ok(data.len());
        }
        let Some((address, page_offset)) =
            self.direct_vga_bytes(address, data.len(), access_width, true)
        else {
            return Ok(0);
        };
        let page = address & !(RAM_LOOKUP_PAGE_MASK as u32);
        let Some(ptr) = self.vega.direct_write_page(page) else {
            return Ok(0);
        };
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(page_offset), data.len()) };
        self.record_direct_vga_accesses(address, data.len(), access_width, kind);
        self.vega.note_direct_write(address, data.len());
        Ok(data.len())
    }

    fn direct_memory_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> usize {
        if !matches!(kind, BusAccessKind::DataRead | BusAccessKind::DataWrite)
            || bytes == 0
            || !bytes.is_multiple_of(access_width.bytes() as usize)
        {
            return 0;
        }
        // First match wins, where this used to take `ram.max(vga)`. The `max`
        // was never choosing between two live answers: RAM and the video
        // apertures are disjoint address ranges, so at most one probe can
        // admit, and the loser contributed 0. The ladder now also asks Margo's
        // LFB, and evaluating all three to take a maximum would price two
        // certain misses on every RAM access.
        if let Some((_, start, end)) = self.direct_page_ram_bytes(address, bytes, access_width) {
            debug_assert!(
                self.direct_vga_bytes(
                    address,
                    bytes,
                    access_width,
                    kind == BusAccessKind::DataWrite
                )
                .is_none()
                    && self
                        .direct_margo_lfb_bytes(address, bytes, access_width)
                        .is_none(),
                "RAM and the video apertures must be mutually exclusive; \
                 taking the first admission would otherwise hide a shorter grant"
            );
            return end - start;
        }
        if match kind {
            BusAccessKind::DataRead => self.direct_vga_bytes(address, bytes, access_width, false),
            BusAccessKind::DataWrite => self.direct_vga_bytes(address, bytes, access_width, true),
            _ => None,
        }
        .is_some()
        {
            return bytes;
        }
        self.direct_margo_lfb_bytes(address, bytes, access_width)
            .map_or(0, |_| bytes)
    }

    #[inline]
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        let gated = self.apply_a20(address);
        if gated != address {
            return Ok(None);
        }
        let physical_page = gated & !(RAM_LOOKUP_PAGE_MASK as u32);
        let video_page = (izarravm_video::VGA_MODE13H_BASE
            ..izarravm_video::VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE)
            .contains(&physical_page);
        let video_ptr = match kind {
            BusAccessKind::DataRead if video_page => self.vega.mode13_direct_page(physical_page),
            BusAccessKind::DataWrite if video_page => self.vega.direct_write_page(physical_page),
            _ => None,
        };
        if let Some(ptr) = video_ptr {
            return Ok(Some(DirectPage {
                physical_page,
                ptr,
                len: RAM_LOOKUP_PAGE_SIZE,
                writable: kind == BusAccessKind::DataWrite,
                mapping_epoch: *self.direct_mapping_epoch,
            }));
        }
        let Some((start, end)) = self.direct_ram_bytes(physical_page, RAM_LOOKUP_PAGE_SIZE) else {
            return Ok(None);
        };
        if end - start != RAM_LOOKUP_PAGE_SIZE {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: RAM_LOOKUP_PAGE_SIZE,
            writable: matches!(kind, BusAccessKind::DataWrite),
            mapping_epoch: *self.direct_mapping_epoch,
        }))
    }

    fn begin_compiled_window(&mut self) -> Option<CompiledBusWindow> {
        if !self.flat_data_cost {
            return None;
        }
        let raw = self
            .trace
            .elapsed_clocks()
            .checked_sub(self.trace_elapsed_at_batch_start)?;
        CompiledBusWindow::certify(
            *self.direct_mapping_epoch,
            self.trace.tracing_mode(),
            self.jit_fetch_cost_clocks(),
            [
                self.jit_data_cost_clocks(BusWidth::Byte),
                self.jit_data_cost_clocks(BusWidth::Word),
                self.jit_data_cost_clocks(BusWidth::Dword),
            ],
            [
                self.jit_mode13_data_cost_clocks(BusWidth::Byte),
                self.jit_mode13_data_cost_clocks(BusWidth::Word),
                self.jit_mode13_data_cost_clocks(BusWidth::Dword),
            ],
            raw,
            self.bus_rem_at_batch_start,
            self.bus_num_at_batch_start,
            self.bus_den_at_batch_start,
        )
    }

    fn finish_compiled_window(&mut self, window: CompiledBusWindow, delta: CompiledBusDelta) {
        debug_assert_eq!(window.mapping_epoch(), *self.direct_mapping_epoch);
        debug_assert_eq!(
            window.batch_raw_clocks(),
            self.trace
                .elapsed_clocks()
                .saturating_sub(self.trace_elapsed_at_batch_start)
        );
        let writes = delta.vga_writes();
        debug_assert_eq!(writes.is_empty(), writes.dirty_pages == 0);
        if !writes.is_empty() {
            self.vega.note_direct_write_pages(writes.dirty_pages);
        }
        self.trace
            .add_elapsed_clocks(window.delta_raw_clocks(&delta));
    }

    #[inline]
    fn charge_direct_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        let video_end = izarravm_video::VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
        if address >= izarravm_video::VGA_MODE13H_BASE
            && address
                .checked_add(width.bytes())
                .is_some_and(|end| end <= video_end)
        {
            self.vega
                .record_direct_access(address, width.bytes() as usize, kind);
            if kind == BusAccessKind::DataWrite {
                self.vega.note_direct_write(address, width.bytes() as usize);
            }
            let ws = if self.active_mode.uses_approximate_timing() {
                video_wait_states_approx(self.active_mode.persona())
            } else {
                self.wait_states.video
            };
            self.trace.record(kind, address, width, ws);
            return Ok(());
        }
        // Only the CPU's DirectPageCache fast paths call this, and a live entry
        // guarantees cacheable RAM under the current A20 state: `direct_page`
        // installs a page only when `apply_a20` is the identity for it (an A20
        // toggle then invalidates the cache via note_a20_changed), and the
        // direct map never covers a device window. Conventional pages sit below
        // the 0xA0000 aperture; extended pages start at 1 MiB, exclude the
        // Distira BAR (whose decode changes rebuild the map AND invalidate the
        // cache), and system RAM ends below the Margo LFB/MMIO and high-ROM
        // bases. So in the Approximate class (`flat_data_cost`) the charge is
        // always the flat L1 cost: skip apply_a20 and the wait-state routing.
        // The Accurate class keeps the full path so its tag arrays stay warm.
        //
        // Accepted residue: a same-instruction REP OUTS that moves the Distira
        // BAR over its own source buffer keeps charging the stale entry's flat
        // cost until the post-instruction io_touched step break invalidates it.
        // That divergence is timing-only; functional behavior is identical.
        self.charge_ram_only(address, width, kind);
        Ok(())
    }

    /// The interpreter's FastMap serve path calls this for a hit already classified as
    /// `PageKind::Ram` at population time, so the video-aperture range compare `charge_direct_memory`
    /// runs on every call is redundant here and is skipped. See `charge_ram_only` for the shared
    /// tail; a Mode13 hit must NOT call this and instead calls `charge_direct_memory`, unchanged.
    #[inline]
    fn charge_direct_ram_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.charge_ram_only(address, width, kind);
        Ok(())
    }

    /// Charge a page-local misaligned direct-RAM access as `width.bytes()` BYTE cycles, which is
    /// exactly what `read_memory`'s `should_split` loop (and `write_memory_recorded`'s twin) does
    /// today. Bit-identical to that loop in all three accounting fields, and the two arms are
    /// bit-identical for DIFFERENT reasons, so both are argued.
    ///
    /// **Flat (`Approximate`) arm.** Today's loop recurses into `read_memory` per byte, and each
    /// recursion re-applies A20 to its own `address + offset` before reaching
    /// `data_access_wait_states(gated_i, Byte)`. Those N gatings collapse to ONE because `A20_MASK`
    /// clears only bit 20 and `RAM_LOOKUP_PAGE_BITS` is 12, so bit 20 is constant across every byte
    /// of one 4 KiB page -- the 1 MiB boundary is always a page boundary. Given page-locality,
    /// `apply_a20(base + i) == apply_a20(base) + i` for every `i < width.bytes()`. For a non-device
    /// address `data_access_wait_states` returns `cache.cost.l1`, never the `is_device_window` arm,
    /// which is the second premise the caller owes. `record_memory_run` then advances
    /// `elapsed_clocks` by `count * clocks_for(Byte, l1)`, bumps `access_count` by `count`, and in
    /// `Full` mode pushes cycles at `address + i` -- identical in all three fields to N `record`
    /// calls (its own doc, and the `record_instruction_fetch_run` precedent).
    ///
    /// The three `debug_assert`s pin exactly the three preconditions: A20 identity, page-locality,
    /// and per-byte non-device. The third is checked over ALL N bytes, not just the base, because
    /// a per-access question is strictly weaker than the per-byte one the split loop asks.
    ///
    /// **Accurate arm.** A literal per-byte transcription: same A20, same
    /// `data_access_wait_states` per byte, same `trace.record`.
    ///
    /// DO NOT fold into `record_memory_run`: `data_access_wait_states`'s non-flat arm MUTATES the
    /// modeled cache tag state, and those tags are CANONICAL STATE (`canonical_state.rs`, the
    /// cosmetic-cache round trip). Collapsing the loop would change the modeled tag sequence and
    /// therefore canonical state, on a persona whose whole point is that it does not approximate.
    /// The temptation to "just use the run here too" is obvious and is the one wrong thing in this
    /// function; the mutation test in `machine_bus_timing_test.rs` exists to catch it.
    fn charge_direct_ram_split(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        let count = width.bytes();
        // BOTH ARMS OWE THESE, so they are asserted before the branch rather than inside it.
        //
        // The Accurate arm gates A20 ONCE and then adds `i`, exactly as the flat arm collapses N
        // gatings into one, so it leans on L-A20 and page-locality just as hard. A violation there
        // is worse, not milder: it produces a wrong ADDRESS SEQUENCE, and this arm's addresses
        // feed `data_access_wait_states`, whose modeled cache-tag mutations are CANONICAL STATE.
        // Asserting only in the flat arm would have left the canonical-state arm -- the one whose
        // whole reason for existing is that its tags must not be disturbed -- unguarded.
        //
        // L-A20: `A20_MASK` clears only bit 20 and a page is 4 KiB, so the 1 MiB boundary is
        // always a page boundary; given page-locality, `apply_a20(base + i) == apply_a20(base) + i`
        // for every `i < width.bytes()`.
        debug_assert_eq!(self.apply_a20(address), address);
        debug_assert!(
            (address as usize & RAM_LOOKUP_PAGE_MASK) + count as usize <= RAM_LOOKUP_PAGE_SIZE
        );
        // L-RAM, checked over ALL N bytes rather than at the base. The flat arm NEEDS this (it
        // hardcodes `cache.cost.l1` instead of asking `data_access_wait_states`); the Accurate arm
        // would still be equal to the old loop without it, since that loop routed device bytes the
        // same way. It is asserted for both anyway because it is a CALLER precondition of this
        // method either way -- the aperture must never reach here.
        debug_assert!((0..count).all(|i| {
            let at = address.wrapping_add(i);
            at < 0x000A_0000 || !self.is_device_window(at, BusWidth::Byte)
        }));
        if self.flat_data_cost {
            self.trace
                .record_memory_run(kind, address, count, BusWidth::Byte, self.cache.cost.l1);
            return Ok(());
        }
        let base = self.apply_a20(address);
        for i in 0..count {
            let at = base.wrapping_add(i);
            let ws = self.data_access_wait_states(at, BusWidth::Byte);
            self.trace.record(kind, at, BusWidth::Byte, ws);
        }
        Ok(())
    }

    /// Per-CYCLE, not per-access: `BusCycle::clocks_for` ignores width, so this returns the same
    /// value for all three widths and a MISALIGNED access costs `width.bytes()` times it. See the
    /// trait's declaration for the full caveat and for why the multiplier is the caller's.
    fn jit_direct_memory_max_clocks(&self, width: BusWidth, _kind: BusAccessKind) -> Option<u64> {
        let ram_wait_states = if self.flat_data_cost {
            self.cache.cost.l1
        } else {
            self.cache
                .cost
                .l1
                .max(self.cache.cost.l2)
                .max(self.cache.cost.ram)
        };
        let ram = u64::from(BusCycle::clocks_for(width, ram_wait_states));
        Some(ram.max(self.jit_mode13_data_cost_clocks(width)))
    }

    fn jit_cached_fetch_run_clocks(&self, start: u32, count: u32) -> Option<u64> {
        if count == 0 {
            return Some(0);
        }
        let end = start.checked_add(count - 1)?;
        if end < 0x000A_0000 {
            return Some(2 + u64::from(self.cache.code_fetch_wait_states()));
        }
        let first = self.apply_a20(start);
        let last = self.apply_a20(end);
        let wait_states = self.code_fetch_wait_states(first);
        if last != first.wrapping_add(count - 1) || wait_states != self.code_fetch_wait_states(last)
        {
            return None;
        }
        let accesses = if first >= 0x000A_0000 && self.is_device_window(first, BusWidth::Byte) {
            count
        } else {
            1
        };
        Some((2 + u64::from(wait_states)) * u64::from(accesses))
    }

    fn jit_projected_batch_scaled_bus_clocks(&self, additional_raw: u64) -> Option<u64> {
        let raw = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        raw.checked_add(additional_raw)?
            .checked_mul(u64::from(self.bus_num_at_batch_start))?
            .checked_add(self.bus_rem_at_batch_start)
            .map(|scaled| scaled / u64::from(self.bus_den_at_batch_start))
    }

    /// One instruction-fetch access of cacheable RAM: `clocks_for(_, code_fetch_wait_states)` = 2 +
    /// the per-mode I-cache constant. Matches what `charge_physical_instruction_fetch_run`'s
    /// cacheable-RAM fast path records for one access. The JIT cost-fold folds this per slot.
    fn jit_fetch_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.code_fetch_wait_states())
    }

    /// Every JIT cost dial on this bus is a pure function of the active mode: the cache tier and
    /// code-fetch constants are rewritten only by `CacheModel::set_mode`, the mode 13h wait
    /// states come from `active_mode.persona()`, and the bus scale is `bus_timing(cpu.level())`.
    /// The video wait states are copied once at bus construction and never written afterwards.
    /// So the mode discriminant is an exact epoch, offset by one to stay clear of the trait
    /// default's 0.
    fn jit_cost_dial_epoch(&self) -> u64 {
        self.active_mode as u64 + 1
    }

    fn native_fetches_are_uniform(&self) -> bool {
        self.flat_data_cost
    }

    fn native_aggregate_accounting_allowed(&self) -> bool {
        self.trace.tracing_mode() == TracingMode::Off
    }

    fn charge_native_cached_fetches(
        &mut self,
        linear_start: u32,
        physical_start: u32,
        fetch_lens: &[u8],
        iterations: u64,
    ) -> bool {
        if linear_start & !0x0fff == (BIOS_LEGACY_IRET_LINEAR & !0x0fff) {
            for _ in 0..iterations {
                let mut linear = linear_start;
                for &len in fetch_lens {
                    self.note_code_fetch_linear(linear);
                    linear = linear.wrapping_add(u32::from(len));
                }
            }
        }
        let physical = self.apply_a20(physical_start);
        let fetch_cost = u64::from(izarravm_bus::BusCycle::clocks_for(
            BusWidth::Byte,
            self.code_fetch_wait_states(physical),
        ));
        let instruction_count = (fetch_lens.len() as u64).saturating_mul(iterations);
        self.trace
            .add_elapsed_clocks(fetch_cost.saturating_mul(instruction_count));
        true
    }

    /// One byte-wide direct data access: `clocks_for(Byte, cost.l1)` = 2 + the flat L1 wait-state,
    /// exactly what `charge_direct_memory` records for a direct-page hit in the Approximate class.
    fn jit_data_byte_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.cost.l1)
    }

    fn jit_data_cost_clocks(&self, width: BusWidth) -> u64 {
        u64::from(izarravm_bus::BusCycle::clocks_for(
            width,
            self.cache.cost.l1,
        ))
    }

    fn jit_mode13_data_cost_clocks(&self, width: BusWidth) -> u64 {
        let wait_states = if self.active_mode.uses_approximate_timing() {
            video_wait_states_approx(self.active_mode.persona())
        } else {
            self.wait_states.video
        };
        u64::from(izarravm_bus::BusCycle::clocks_for(width, wait_states))
    }

    /// What `read_io`/`write_io` record for one port access: `clocks_for(width, wait_states.io)`,
    /// the exact figure `self.trace.record` uses. `wait_states` is copied once at bus
    /// construction and never rewritten, so it is constant within a `jit_cost_dial_epoch`.
    fn jit_io_cost_clocks(&self, width: BusWidth) -> u64 {
        u64::from(izarravm_bus::BusCycle::clocks_for(
            width,
            self.wait_states.io,
        ))
    }

    fn jit_scale_bus_cost_upper(&self, raw_clocks: u64) -> u64 {
        raw_clocks
            .saturating_mul(u64::from(self.bus_num_at_batch_start))
            .saturating_add(u64::from(self.bus_den_at_batch_start) - 1)
            / u64::from(self.bus_den_at_batch_start)
    }

    fn rep_data_byte_cost_upper(&self) -> u64 {
        let ram = if self.flat_data_cost {
            self.cache.cost.l1
        } else {
            self.cache
                .cost
                .l1
                .max(self.cache.cost.l2)
                .max(self.cache.cost.ram)
        };
        let video = if self.flat_data_cost {
            video_wait_states_approx(self.active_mode.persona())
        } else {
            self.wait_states.video
        };
        let wait_states = ram
            .max(self.wait_states.ram)
            .max(self.wait_states.rom)
            .max(video);
        let raw = u64::from(izarravm_bus::BusCycle::clocks_for(
            BusWidth::Byte,
            wait_states,
        ));
        let (num, den) = bus_timing(self.active_mode.persona());
        raw.saturating_mul(u64::from(num))
            .saturating_add(u64::from(den) - 1)
            / u64::from(den)
    }

    fn rep_page_walk_cost_upper(&self) -> Option<u64> {
        let video = if self.active_mode.uses_approximate_timing() {
            video_wait_states_approx(self.active_mode.persona())
        } else {
            self.wait_states.video
        };
        let wait_states = self
            .cache
            .cost
            .l1
            .max(self.cache.cost.l2)
            .max(self.cache.cost.ram)
            .max(self.wait_states.ram)
            .max(self.wait_states.rom)
            .max(video);
        let raw = u64::from(BusCycle::clocks_for(BusWidth::Dword, wait_states)).saturating_mul(4);
        Some(self.jit_scale_bus_cost_upper(raw))
    }

    fn charge_native_vga_writes(&mut self, writes: NativeVgaWrites) {
        if writes.is_empty() {
            return;
        }
        self.vega.note_direct_write_pages(writes.dirty_pages);
        let clocks = self
            .jit_mode13_data_cost_clocks(BusWidth::Byte)
            .saturating_mul(writes.byte_writes)
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(writes.word_writes),
            )
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(writes.dword_writes),
            );
        self.trace.add_elapsed_clocks(clocks);
    }

    /// Flush the JIT cost-fold's accumulated bus clocks into the trace's running total in one op.
    fn charge_bus_clocks_bulk(&mut self, clocks: u64) {
        self.trace.add_elapsed_clocks(clocks);
    }

    /// See the trait doc: the straight-line run loop adds this figure's growth
    /// to its core total against the guest-clock run cap. The same scaled-bus
    /// accounting applies in every CPU mode so a bus-heavy run cannot cross an
    /// earlier master-timeline deadline unnoticed.
    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        let raw = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        (raw * u64::from(self.bus_num_at_batch_start) + self.bus_rem_at_batch_start)
            / u64::from(self.bus_den_at_batch_start)
    }

    /// The division-free form of the run-loop cap test. Exactly equivalent to
    /// `in_batch_scaled_bus_clocks() >= target` by `floor(A / den) >= target` iff
    /// `A >= target * den`, so the run boundaries and `brk_cap` counts are bit-identical.
    /// Widened to `u128` for the two products so no bound argument about `raw * num` or
    /// `target * den` is needed; a 128-bit multiply is a few cycles against a 64-bit divide's
    /// tens, and this runs once per retired instruction.
    #[inline]
    fn in_batch_scaled_bus_clocks_at_least(&self, target: u64) -> bool {
        let raw = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        let scaled = u128::from(raw) * u128::from(self.bus_num_at_batch_start)
            + u128::from(self.bus_rem_at_batch_start);
        scaled >= u128::from(target) * u128::from(self.bus_den_at_batch_start)
    }

    /// The `raw` that `in_batch_scaled_bus_clocks` scales. Monotone within a batch because
    /// `trace.elapsed_clocks()` only grows and the subtrahend is a batch-start snapshot.
    #[inline]
    fn in_batch_raw_bus_clocks(&self) -> u64 {
        self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start
    }

    /// `F = ceil(num / den)` over this batch's snapshotted scale. See the trait doc for why that
    /// is a valid bound on the scaled figure's growth per raw clock. `num` and `den` are `u32`
    /// and `den > 0` (every `bus_timing` pair is), so the ceiling cannot overflow and is never 0.
    /// A mode change never lands mid-batch — it is staged in `pending_mode` and applied after the
    /// batch — so this stays constant for the whole batch, which is what makes it safe for the
    /// run loop to read once per run.
    #[inline]
    fn in_batch_scaled_bus_clocks_screen_scale(&self) -> u64 {
        let num = u64::from(self.bus_num_at_batch_start);
        let den = u64::from(self.bus_den_at_batch_start);
        num.div_ceil(den).max(1)
    }

    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let address = self.apply_a20(address);
        let bytes = width.bytes() as usize;

        if let Some(value) = self.vega.read_wide_memory(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return Ok(value);
        }

        if should_split(address, width) {
            let mut value = 0u32;
            for offset in 0..width.bytes() {
                let byte = self.read_memory(address + offset, BusWidth::Byte, kind)?;
                value |= byte << (offset * 8);
            }
            return Ok(value);
        }

        if let Some((start, end)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            return Ok(match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            });
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        let mut data = [0u8; 4];
        self.read_phys(address, &mut data[..bytes])?;
        Ok(match width {
            BusWidth::Byte => u32::from(data[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
            BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.write_memory_recorded(address, width, value, kind, &mut IgnoreRamWrites)
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let address = self.apply_a20(address);
        // Fast path: a prefetch window entirely within conventional RAM (below
        // the 0xA0000 video aperture) is one bounded slice copy instead of a
        // gauntlet-walking read_phys_u8 per byte.
        let ram_end = address as usize + out.len();
        if ram_end <= 0x000A_0000 && ram_end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..ram_end]);
            return Ok(out.len());
        }
        let mut copied = 0;
        for (offset, byte) in out.iter_mut().enumerate() {
            match self.read_phys_u8(address + offset as u32) {
                Ok(value) => {
                    *byte = value;
                    copied += 1;
                }
                Err(BusError::UnmappedMemory { .. }) if copied > 0 => break,
                Err(err) => return Err(err),
            }
        }
        Ok(copied)
    }

    #[inline]
    fn note_code_fetch_linear(&mut self, linear: u32) {
        // ONE range compare for the whole body. Every interpreted instruction and every warm JIT
        // fetch calls this, and outside `FIRMWARE_FETCH_WINDOW` (the two BIOS32 entry points
        // through the end of the per-vector stub table) neither half can do anything: the BIOS32
        // arm's `_ => None` writes `None` back over an already-`None` slot, and the stub arm's own
        // window is contained in this one. So the guard is a pure hoist, not a behaviour change.
        if linear.wrapping_sub(FIRMWARE_FETCH_WINDOW_START) >= FIRMWARE_FETCH_WINDOW_LEN {
            return;
        }
        if self.pending_bios32.is_none() {
            *self.pending_bios32 = match linear {
                BIOS32_DIRECTORY_LINEAR => Some(Bios32Call::Directory),
                BIOS32_PCI_LINEAR => Some(Bios32Call::Pci),
                _ => None,
            };
        }
        // The legacy FF00:0000 target through the end of the per-vector stub table.
        if linear.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(linear);
        }
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        let address = self.apply_a20(address);
        let ws = self.code_fetch_wait_states(address);
        self.trace.record(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            ws,
        );
        Ok(())
    }

    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        if count != 0 && start.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(start);
        }
        self.charge_physical_instruction_fetch_run(start, count)
    }

    // Inlinable on purpose. The warm-fetch profile showed this call as a standing leaf (5.96%
    // SELF on JIT-off prince-486) not because the fast path is expensive but because ThinLTO
    // declined to inline a body that carries the whole classification tail. The head below is
    // small enough to inline into `charge_fetch_run`; the tail moves to the `#[inline(never)]`
    // helper so it cannot drag the head out of line again.
    #[inline]
    fn charge_physical_instruction_fetch_run(
        &mut self,
        physical_start: u32,
        count: u32,
    ) -> Result<(), BusError> {
        if count == 0 {
            return Ok(());
        }
        // Fast path: a run that lies entirely in conventional RAM (below the
        // 0xA0000 video aperture). Every address below 0x100000 has bit 20 clear,
        // so `apply_a20` is the identity there regardless of the gate state;
        // `code_fetch_wait_states` is the per-mode I-cache constant for any
        // address below 0xA0000 (the device-window gate only engages at or above
        // it); and a contiguous run below 0xA0000 is uniform by construction.
        // The classification would therefore always land in the uniform
        // cacheable-RAM arm and charge ONE I-cache access at the constant
        // wait-state, so charge exactly that in one step. ROM/device/A20-edge
        // runs keep the full classification, byte-for-byte.
        // The same argument, one region up. A 32-bit game's code lives in
        // extended RAM, so the screen above never fires for it and every fetch
        // run pays the `#[cold]` tail: two A20 folds, two wait-state lookups
        // through the device gauntlet, and the uniformity test. A run that lies
        // wholly inside `0x0010_0000 .. device_free_extended_floor()` reaches
        // exactly the same conclusion as that tail -- cacheable RAM, uniform,
        // one I-cache access -- because no window claims any byte of it (see
        // `is_device_free_extended_ram`) and a contiguous run inside one region
        // is uniform by construction.
        //
        // A20 has to be OPEN for this arm, and that is not the same requirement
        // the conventional arm carries. Below 1 MB the mask is the identity, so
        // that arm can ignore the gate; up here a masked gate folds the run down
        // into conventional memory, where a window may well claim it. Testing
        // the gate first also makes `physical_start` a post-A20 address, which
        // is what `is_device_free_extended_ram` asserts it is given.
        // Written as one short-circuiting condition on purpose: a run below the
        // video aperture must not pay for the extended test, so the gate read
        // and the floor comparison sit behind the `||`.
        if let Some(end) = physical_start.checked_add(count - 1)
            && (end < 0x000A_0000
                || (self.keyboard.a20_enabled()
                    && self.is_device_free_extended_ram(physical_start, count as usize)))
        {
            debug_assert_eq!(
                self.icache_fetch_clocks,
                u64::from(izarravm_bus::BusCycle::clocks_for(
                    BusWidth::Byte,
                    self.cache.code_fetch_wait_states()
                )),
                "icache_fetch_clocks is stale relative to the live cache model; a mode change \
                 landed without rebuilding the bus"
            );
            // The whole charge is one add of a per-batch constant. The wait-state itself is only
            // needed to DESCRIBE the cycle, so it is read (chasing `cache`) exclusively on the
            // tracing arm, which the default build never takes.
            if self.trace.tracing_mode() == TracingMode::Off {
                self.trace.add_elapsed_clocks(self.icache_fetch_clocks);
            } else {
                self.trace.record_instruction_fetch_run(
                    physical_start,
                    1,
                    self.cache.code_fetch_wait_states(),
                );
            }
            return Ok(());
        }
        self.charge_classified_instruction_fetch_run(physical_start, count)
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        // Publish the current run offset for lazy time-dependent reads.
        self.core_clocks_so_far = core_clocks_so_far;
        // Ring-0-monitor port-time exemption (V86 trap tax, Part 1): the TOKAEMM
        // monitor's own device pokes (remapped_pic_line's PIC OCW3 probe,
        // chiefly) are chipset-side bookkeeping done on the guest's behalf, not
        // guest-visible device activity in their own right. Ending the CPU batch
        // around them (the normal io_touched contract) triples the guest-visible
        // cost of every V86 trap for no fidelity gain: device time is still exact
        // because the batch still ends at the next event_batch_cap edge or the
        // next GUEST port access, and OCW3's read-select is pure register state
        // (see pic.rs -- `read_isr` is a mode bit, not time-derived), so deferring
        // exactly when it is consumed relative to batch-end timing is safe. Gated
        // on `lazy_port_reads` (Approximate class only, i.e. 486/586): the
        // Accurate 386 class keeps byte-identical batch semantics, matching
        // every other lazy-read gate in this function.
        let skip_io_touched = cpu_is_ring0_pm && self.lazy_port_reads;
        // The exemption above keeps the batch running across a device access, so
        // the device-edge deadline cache cannot rely on `io_touched` alone to know
        // that a schedule may have moved. Record the access separately; the run
        // loop drops the cache on either flag. Off the exempt path this costs
        // nothing, and on it, one store.
        if skip_io_touched {
            *self.exempt_io_touched = true;
        }
        // Snapshot for the lazy gameport arm far below, which is the one lazy
        // port that cannot be dispatched before the general `io_touched` set:
        // its decode position is load-bearing (a configured WSS/SB base could in
        // principle overlap 0x200-0x207, and today the earlier arm wins). Rather
        // than hoist the arm and silently change that precedence, the arm CLEARS
        // the flag again -- but only when this very read is what set it, which is
        // what this snapshot establishes. A wider-than-byte access re-enters
        // read_io per byte and so re-snapshots per byte.
        let io_touched_before_read = *self.io_touched;
        // Bus-clock trace recording stays unconditional for every port, both timing
        // classes: `predicted_beam`'s bus term scales exactly the clocks recorded
        // here, so a lazy read that skipped this would under-predict its own beam.
        self.trace.record(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        if let Some(value) = self.pci.read_io(port, width, self.vega) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(value);
        }

        if let Some(base) = self.pci.ide_bus_master_io_base()
            && bmide::BusMasterIde::owns_io(port, width, base)
        {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            if self.pci.ide_io_enabled() {
                return Ok(self.bmide.read_io(port, width, base));
            }
            return Ok(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => 0xffff,
                BusWidth::Dword => u32::MAX,
            });
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port access decomposes into byte cycles, the way the
            // ISA bus does for a port that is not 16-bit: the low byte comes from the
            // port and each higher byte from the next port (`io_word_sub_port` keeps
            // the IDE/ATA data registers on the same port). This is the canonical VGA
            // mode-set path - a single 16-bit `OUT 0x3C4`/`0x3CE`/`0x3D4` sets an
            // index and its datum - which used to halt the VM with WidthMismatch.
            // Per-byte io_touched/lazy dispatch happens in the recursive calls below
            // (each byte re-enters read_io), so nothing to set here directly.
            let mut value = 0u32;
            for i in 0..width.bytes() {
                let byte = self.read_io(
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    core_clocks_so_far,
                    cpu_is_ring0_pm,
                )?;
                value |= (byte & 0xff) << (8 * i);
            }
            return Ok(value);
        }

        // The ISA bus charge, BELOW the width split so a word access pays for the
        // two byte cycles it actually decomposes into rather than three (one for
        // the wide access plus one per byte), and above every DEVICE arm, because
        // the cycle happens on the bus before any device answers -- a charge
        // tucked inside one device's arm is how the OPL and DSP charges came to be
        // the only two in the tree.
        //
        // The two arms it sits below are `pci.read_io` (0xCF8-0xCFF, a PCI cycle
        // and not an ISA one) and the bus-master IDE block, neither of which
        // overlaps the charged set as configured; a guest that relocated BAR4 into
        // the X-bus range would skip the charge on those sixteen ports, which is
        // strictly the conservative direction.
        self.charge_isa_io_wait(port);

        // THE 0x3DA FAST PATH. GP2 reads this one port 2.003 billion times in a run, and in
        // decode order it walks `port_disabled` plus four UART/LPT range checks first, every
        // time. Answer it here instead. `Uart16450::read_port` and `Lpt::read_port` alone are
        // 1.635% and 0.483% SELF on the gp2 profile, and this is that traffic.
        //
        // WHY HERE AND NOT HIGHER, which is the whole correctness argument:
        //
        // * ABOVE the `width != BusWidth::Byte` block would be wrong. A word `IN` from 0x3DA
        //   decomposes into 0x3DA and 0x3DB and records THREE bus accesses; answering the word
        //   here would return a zero-extended byte and record one, moving recorded bus clocks.
        // * ABOVE the bus-master check would be wrong. `piix_ide_bm_base` is guest-programmable
        //   through BAR4 and masked only with `!0x0f`, so a guest can point the 16-byte
        //   bus-master block at 0x03D0, where `BusMasterIde::owns_io` claims 0x3DA and wins
        //   today. Hoisting past it would silently reassign that decode to the VGA.
        // * ABOVE `pci.read_io` buys nothing: it claims only 0xCF8-0xCFF.
        //
        // What it DOES skip is `port_disabled` and the four serial/LPT range checks, and those
        // are safe to skip because `port_enabled`/`port_disabled` are exact complements at this
        // port (both reduce to `video_subsystem_enabled()` for 0x3B0..=0x3DF except 0x3C3), and
        // COM1/COM2/LPT1/LPT2 bases are `const` with no setter, claiming 0x3F8/0x2F8 +7 and
        // 0x378/0x278 +2. None can reach 0x3DA.
        //
        // 0x3DA ONLY, deliberately: 0x3BA sits beside the 0x3BC LPT range and 0x3C2 is the
        // write-side dot-clock port, so hoisting the trio would change decode precedence.
        // A decline falls through to the in-order arm, which is unchanged.
        if port == 0x3DA
            && self.vega.port_enabled(port)
            && let Some(value) = self.read_vga_status_port(port, skip_io_touched)
        {
            return Ok(value);
        }

        if self.vega.port_disabled(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(0xff);
        }

        if let Some(value) = self.serial.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.serial2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        // The VGA status ports (3DA/3BA/3C2) are the ONLY arm in this function that
        // does not unconditionally set io_touched. In the lazy-read case for the
        // Approximate timing class they must NOT end the batch (io_touched stays
        // false) so a poll loop chains as `run_straight_line` continuations. Static
        // per-port dispatch: these three port numbers always land here, whether or
        // not lazy_port_reads is set, so the branch is a single bool test, never a
        // per-access classification.
        //
        // DECISION (batch-retroactive-rate subtlety): a batch shaped [lazy 3DA polls
        // ... OUT 0x3C2 lowering the dot clock] applies the new dot-clock rate to the
        // WHOLE batch at batch end (the pre-existing retroactive-rate behavior of
        // advance_devices/scale_bus), so the batch-end beam can land behind the last
        // lazy-predicted value this loop observed. Accepted as-is, no compensation:
        // a dot-clock switch is not beam-continuous on real hardware either, and the
        // write itself sets io_touched and ends the batch, so no further lazy read
        // can observe the stale prediction within the same batch.
        if matches!(port, 0x3DA | 0x3BA | 0x3C2) && self.vega.port_enabled(port) {
            // 0x3DA already answered above unless it declined; 3BA and 3C2 arrive here in order.
            if let Some(value) = self.read_vga_status_port(port, skip_io_touched) {
                return Ok(value);
            }
        } else if self.vega.port_enabled(port)
            && let Some(value) = self.vega.read_port(port)
        {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        // Port 0x61 bits 4/5 use the same lazy
        // per-port dispatch discipline as 3DA/3BA/3C2 above -- 0x61 always lands
        // here whether or not lazy_port_reads is set. Bits 0/1 (speaker gate/data)
        // are plain register state that cannot change mid-batch: the only writer
        // is `write_io`, which unconditionally sets io_touched and so ends the
        // batch before a later lazy read in the same batch could observe a stale
        // value. Bits 4/5 come from PIT channels 1/2 OUT, which `out_after`'s
        // GATE-stays-level assumption also depends on: GATE2 is wired from this
        // same port's bit 0, and its only writer is that same batch-ending
        // write_io, so GATE cannot move between this read and the batch end
        // either.
        if port == 0x61 {
            // The OUT peek is taken in BOTH timing classes, on exactly the same
            // grounds as the unconditional 0x40-0x42 counter peek below: it
            // changes only the VALUE, never whether the batch ends. The lazy
            // switch decides the BATCH question alone (see the io_touched set
            // after this block).
            //
            // Why the Accurate class needs the value too, and not just the
            // Approximate one. Bit 5 is channel-2 OUT, and the classic
            // no-sound PIT timing technique leaves GATE2 high with the data
            // enable LOW (0x61 bit 0 set, bit 1 clear), then polls bit 5. The
            // fine-batch-grain gate does NOT cover that case:
            // `speaker.data_enabled()` is false, and `note_pit_observer` arms
            // only on a 0x40-0x43 access, so a guest that programs channel 2
            // once and afterwards polls only 0x61 falls out of the 5 ms
            // observer window and back to the 1 ms coarse cap. Reading the LIVE
            // (batch-start) level there could report OUT up to a whole coarse
            // batch stale -- a mode-3 square-wave FALL is half a period from
            // the nearest cached rise, so the deadline cache does not bound it
            // either. With the peek the level is the one a real
            // `advance_devices` of the same clock total would produce, which is
            // the same contract the counter peek closed for 0x40-0x42.
            //
            // Both channels share the SAME elapsed-PIT-clocks conversion (same
            // rate, same batch-entry carry): computed once here rather than
            // twice inside two separate predicted_pit_out calls, since a
            // redundant conversion on this hot path.
            let elapsed_pit_clocks = self.elapsed_pit_clocks();
            let ch1 = self.pit.out_after(1, elapsed_pit_clocks);
            let ch2 = self.pit.out_after(2, elapsed_pit_clocks);
            if let (Some(ch1_out), Some(ch2_out)) = (ch1, ch2) {
                if !(self.lazy_port_reads || self.lazy_ports_386) && !skip_io_touched {
                    *self.io_touched = true;
                }
                let value = (self.speaker.control_bits() & 0x03)
                    | (u8::from(ch1_out) << 4)
                    | (u8::from(ch2_out) << 5);
                return Ok(u32::from(value));
            }
            // BCD fallback: at least one of channel 1/2 is BCD-programmed, so
            // `out_after` conservatively declined. Take the exact non-lazy path
            // (io_touched set, live read) in EITHER class rather than a second
            // implementation of the same bit composition.
            if !skip_io_touched {
                *self.io_touched = true;
            }
            // Bit 4 is the DRAM-refresh heartbeat: PIT channel 1 OUT (the AT
            // refresh timer, mode 2), not the speaker's standalone toggle. The PIT
            // seeds channel 1 at power-on so this pulses without guest programming.
            let value = (self.speaker.control_bits() & 0x03)
                | (u8::from(self.pit.channel_out(1)) << 4)
                | (u8::from(self.pit.channel_out(2)) << 5);
            return Ok(u32::from(value));
        }
        // A counter read is time-derived: devices only advance at batch END, so the
        // CE must be peeked at THIS instant or the guest reads the value the counter
        // had at batch start (`Counter::count_after`). The port test guards the
        // conversion so no other port pays for it; the peek itself runs in BOTH
        // timing classes, exactly like the 0x61/3DA peeks above -- it changes only
        // the VALUE read, never whether the batch ends on its own.
        //
        // L5 (corpus-lever campaign, 2026-08-31): hoisted from below the "all
        // remaining reads are exact" catch-all to ABOVE it, and given the exact
        // io_touched shape the 0x61/3DA arms already use (`lazy_port_reads ||
        // lazy_ports_386`), not the ring-0-only `skip_io_touched` gate. Before this
        // move, every 0x40-0x42 read fell through to the catch-all and so set
        // io_touched for any guest (V86 or real-mode) access in EITHER timing
        // class -- the counter's own peek logic below never got a chance to decide
        // the flag, only the reported VALUE. The catch-all's comment ("The ring-0
        // monitor's PIC OCW3 probe still honors the same skip_io_touched gate")
        // was correct for ports that stop there; this port never needed to.
        //
        // Safe on the SAME grounds the `0xE4` call-out's per-port safety table
        // already certifies for this exact port class (`jit/direct.rs`,
        // `classify`'s `0xe4` arm): `read_port_at` is a pure peek against
        // `elapsed_pit_clocks`. It does not advance the counter's own
        // reload/decrement schedule and it does not touch the PIC, so a guest
        // that polls 0x40-0x42 and expects an IRQ cannot observe a stale pendency
        // answer -- nothing about `interrupt_pending()` moves because this byte
        // was read. That is a stronger, port-specific proof than "ring-0 monitor
        // bookkeeping is not guest-visible activity" (the `skip_io_touched`
        // rationale), so it composes with a guest access the ring-0 exemption was
        // never meant to cover, and does not touch `skip_io_touched` or
        // `exempt_io_touched` at all -- exactly the 3DA/0x61 shape, not the
        // ring-0-monitor shape.
        if matches!(port, 0x40..=0x42) {
            let elapsed_pit_clocks = self.elapsed_pit_clocks();
            if let Some(value) = self.pit.read_port_at(port, elapsed_pit_clocks) {
                self.note_pit_observer();
                if !(self.lazy_port_reads || self.lazy_ports_386) && !skip_io_touched {
                    *self.io_touched = true;
                }
                return Ok(u32::from(value));
            }
        }
        // OPL status reads are intentionally exact. AdLib detection is a timer
        // probe, and letting the poll continue inside an approximate CPU batch
        // can starve the emulated timer progression enough to fail on fast CPU
        // modes. Keep every AdLib/SB OPL read batch-ending.
        if let Some(resolved) = opl_port(port) {
            // Always end the batch on an OPL status read, even under the ring-0 PM
            // monitor. The skip_io_touched exemption exists for the monitor's OWN
            // chipset pokes (remapped_pic_line's PIC OCW3 probe), but an OPL poll reflected
            // from a V86 guest is real guest device I/O: it must end the batch so the
            // OPL timer advances BETWEEN polls. Without this the whole AdLib
            // detection loop runs inside one batch, the timer only advances at batch
            // end (after every poll already read a stale 0x00), and detection fails.
            *self.io_touched = true;
            // Charge the poll its real ISA bus time in the fast modes so it
            // cannot outrun the 80 us OPL timer on a fast CPU (folded into the batch
            // device advance in run_until_tick). The 386 modes do not need this
            // charge because their slower clocks already span the window.
            if self.lazy_port_reads {
                *self.isa_io_clocks += isa_io_clocks(self.active_mode);
            }
            // The chip drives only the status byte on reads; data ports read open-bus.
            //
            // In the Approximate class the chip has not been stepped since the
            // batch started, so read the PREDICTED status instead of the live
            // one -- see `predicted_opl_status`. The Accurate 386 class advances
            // devices per instruction and keeps the live byte, byte-identically.
            let (value, pending_micros) =
                if self.lazy_port_reads && matches!(resolved, 0x0388 | 0x038a) {
                    self.predicted_opl_status()
                } else {
                    (self.opl.read_port(resolved).unwrap_or(0xff), 0)
                };
            // Diagnostic only; records the guest's own port, not the resolved one.
            self.opl_probe
                .record_read(port, value, self.core_clocks_so_far, pending_micros);
            return Ok(u32::from(value));
        }
        // DSP status reads are intentionally exact. SB reset/probe code polls
        // 0x22E for the reset ACK byte, so keeping that loop inside one
        // approximate CPU batch can starve the DSP settle timer.
        // All remaining reads are exact and end the batch. The ring-0 monitor's
        // PIC OCW3 probe still honors the same skip_io_touched gate.
        if !skip_io_touched {
            *self.io_touched = true;
        }
        if matches!(port, 0x224 | 0x225)
            && let Some(value) = self.sb16.read_port(port)
        {
            return Ok(u32::from(value));
        }
        if port == WAVETABLE_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.pic.set_irq_level(9, self.midi_mpu.irq_level());
            let value = self.wavetable_mpu.read_data_at(guest_tick);
            self.opl_probe.record_mpu_data_read(true, value, guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == WAVETABLE_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            let value = self.wavetable_mpu.status_at(guest_tick);
            self.opl_probe.record_mpu_status_read(true);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == MIDI_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.pic.set_irq_level(9, self.wavetable_mpu.irq_level());
            let value = self.midi_mpu.read_data_at(guest_tick);
            self.opl_probe
                .record_mpu_data_read(false, value, guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == MIDI_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            let value = self.midi_mpu.status_at(guest_tick);
            self.opl_probe.record_mpu_status_read(false);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        // AD1848 / Windows Sound System: 4 config-region ports at wss_base plus
        // the 4 codec ports at wss_base+4. read_port takes the in-region offset
        // and returns a u8, so the range MUST be checked before the call. The
        // region (default 0x530-0x537) never overlaps the SB16 (0x220-0x22F),
        // CT1745 mixer (0x224/5), or OPL (0x388/9) ports.
        if let Some(offset) = self.wss_offset(port) {
            return Ok(u32::from(self.wss.read_port(offset)));
        }
        if ide::IdeChannel::owns_port(port) {
            // Alternate status is non-destructive, and the IDE completion edge
            // already bounds the batch through `event_batch_cap`. That is the
            // exemption; the poll-skip arm below is the one thing that overrides
            // it, and only for the single read that arms.
            let lazy_alt_status =
                self.lazy_port_reads && port == ide::SECONDARY_CTRL && !io_touched_before_read;
            // THE ARM SITE for the device-armed ATA/ATAPI clock skip. This is
            // the seam BOTH backends share: the interpreter's `IN AL,DX` and the
            // native V86 port call-out (`jit/direct/callout.rs`, which passes
            // `ring0 = cpu.is_ring0_protected()`, false in V86) both land here,
            // which is what makes the lever backend-independent by construction
            // rather than by porting.
            //
            // `skip_io_touched` AND `io_touched_before_read` ARE NOT
            // INTERCHANGEABLE, and conflating them was a blocking review finding.
            // `io_touched_before_read` is snapshotted well above, BEFORE the
            // general set, and records whether an EARLIER access in this batch
            // already ended the batch. Under the ring-0-monitor exemption
            // nothing set the flag either, so it stays false -- meaning
            // `!io_touched_before_read` is satisfied in exactly the case it was
            // once claimed to exclude. The exclusion has to come from
            // `skip_io_touched` itself. Both terms are kept: the first declines
            // the monitor's own reads, the second declines a read that follows
            // an access which already ended the batch.
            let monitor_exempt = self.ata_poll_skip_enabled && lazy_alt_status && skip_io_touched;
            if monitor_exempt {
                self.ata_poll_skip.counters.monitor_exempt =
                    self.ata_poll_skip.counters.monitor_exempt.saturating_add(1);
            }
            let arm = self.ata_poll_skip_enabled
                && !self.ata_poll_skip_slice_too_short
                && lazy_alt_status
                && !skip_io_touched
                && self.ide.note_alt_status_read();
            let value = self.ide.read_port(port).unwrap_or(0xff);
            // SUPPRESS THE CLEAR, never force the set. `io_touched` is already
            // true by the time this arm runs UNLESS `skip_io_touched` suppressed
            // it, and that suppression is the documented "V86 trap tax, Part 1"
            // contract above: the TOKAEMM monitor's own chipset pokes must not
            // end the batch and signal through `exempt_io_touched` instead.
            // Writing `true` here would break that contract silently for any
            // alt-status read reaching the bus with `cpu_is_ring0_pm` set --
            // changing the monitor's batch geometry on the one fixture that runs
            // the monitor. Suppressing the clear is exactly "end the batch on
            // the arming read, and only where the exemption would otherwise have
            // fired", and it cannot write `io_touched` at all.
            if lazy_alt_status && !arm {
                *self.io_touched = false;
            }
            if arm {
                *self.ata_poll_skip_armed = true;
                self.ata_poll_skip.counters.arms =
                    self.ata_poll_skip.counters.arms.saturating_add(1);
            }
            return Ok(u32::from(value));
        }
        if ata::AtaDisk::owns_port(port) {
            // The primary channel: a mounted disk drives the task file; an empty
            // channel reads open-bus (0xFF), so a probe sees no device.
            let value = self
                .ata
                .as_mut()
                .and_then(|d| d.read_port(port))
                .unwrap_or(0xff);
            return Ok(u32::from(value));
        }
        if fdc::Fdc::owns_port(port) {
            return Ok(u32::from(self.fdc.read_port(port).unwrap_or(0xff)));
        }
        if !matches!(port, 0x224 | 0x225) {
            // An ISA port read costs about a microsecond of bus time on the
            // metal. Charge it in the fast modes for the same reason the OPL
            // polls charge it above, then service the reset-settle countdown
            // at the predicted point in the batch (the DSP analogue of
            // `predicted_opl_status`): the countdown otherwise moves only on
            // a device advance, and a tight probe loop (OuterRid polls 0x22E
            // one hundred times) runs entirely between advances. Gate on the
            // DSP's own ports: the charge must not leak to unrelated ports
            // that merely fall through this arm unclaimed.
            if self.lazy_port_reads && matches!(port, 0x226 | 0x22A | 0x22C | 0x22E | 0x22F) {
                *self.isa_io_clocks += isa_io_clocks(self.active_mode);
                let pending = self.pending_device_micros();
                self.sb16.service_reset_at(pending);
            }
            if let Some(value) = self.sb16.read_port(port) {
                self.opl_probe.record_sb(port, false, value);
                return Ok(u32::from(value));
            }
        }
        if let Some(value) = self.pic.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.dma.read_port(dma_page_register_port(port)) {
            return Ok(u32::from(value));
        }
        if port == 0x00e0 {
            return Ok(u32::from(LOTURA_ID_VALUE));
        }
        if port == 0x00e1 {
            return Ok(u32::from(self.active_mode.register_code()));
        }
        if port == 0x00e2 {
            // Lotura POST-pacing flag: 1 = fast (skip cosmetic delays), 0 = full.
            return Ok(u32::from(u8::from(self.fast_post)));
        }
        if port == 0x00e3 {
            // Toka-DOS service status: 0 ok, 1 absent, other = error.
            return Ok(u32::from(self.toka_service_status));
        }
        if port == 0x00e8 {
            // IzarraCD doorbell status: 0 = idle/done, 1 = request pending.
            return Ok(u32::from(*self.cd_doorbell_status));
        }
        if port == 0x0092 {
            // System control port A: bit 1 mirrors the A20 gate (the 8042 output
            // port is the single source of truth). Other bits read 0.
            return Ok(u32::from(u8::from(self.keyboard.a20_enabled()) << 1));
        }
        if (0x0200..=0x0207).contains(&port) {
            // The gameport is the strongest lazy candidate in the machine and the
            // only one whose VALUE does not move when the batch stops ending
            // here: `GamePort::read` advances only button replay state to the
            // supplied guest tick, then samples the four RC discharge deadlines
            // at that same in-batch instant. Nothing in `advance_devices` touches
            // those deadlines. Their writers are `charge` (the 0x201 WRITE) and
            // `set_state` (host-side injection, drained between `machine.run`
            // slices, so no batch boundary can reorder a button edge).
            //
            // The arm keys on DEVICE STATE, not on the persona switch, because
            // the persona is not what makes the read time-independent -- an idle
            // monostable is. `charge(now)` writes deadlines at least
            // `now + RC_BASE_NS`, so any read later in the same batch is still
            // before them, `is_idle` is false, and a charged axis takes today's
            // batch-ending path by construction. A mid-pulse read with a stick
            // attached is genuinely time-dependent and is served exactly as
            // before. What is left -- no stick, or both one-shots long expired --
            // is a constant function of state that no device advance can move.
            //
            // `!io_touched_before_read` stays: it is what stops this read from
            // clearing a flag that some EARLIER access in the same batch set.
            if self.gameport.is_idle(self.guest_tick_now()) && !io_touched_before_read {
                *self.io_touched = false;
            }
            return Ok(u32::from(self.gameport.read(self.guest_tick_now())));
        }
        if let Some(value) = self.unittester.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.rtc.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.keyboard.read_port(port) {
            // Reading the data register drops the 8042's keyboard or auxiliary
            // output line in the same I/O cycle. Keep the PIC's electrical input
            // in step here so LTIM cannot reassert a byte the guest consumed.
            if port == 0x60 {
                self.pic.set_irq_level(1, self.keyboard.irq1_level());
                self.pic.set_irq_level(12, self.keyboard.irq12_level());
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.device_ports.read_port(dma_page_register_port(port)) {
            return Ok(u32::from(value));
        }
        // Nothing decoded it. Float the data lines high, as an ISA bus with no
        // card driving them does; see `OpenBusPorts` for why this is not a fault.
        self.open_bus.note(port, false)?;
        Ok(u32::MAX >> (32 - width.bytes() * 8))
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.core_clocks_so_far = core_clocks_so_far;
        // See read_io's matching comment (V86 trap tax, Part 1): the ring-0
        // monitor's own device pokes (e.g. remapped_pic_line's PIC OCW3
        // select write) are chipset bookkeeping, not guest-visible activity, so
        // they are exempted from ending the batch in the Approximate class only.
        //
        // A20 carve-out: the batch loop's A20 seam ("any A20 write ... ends this
        // step" -- the before/after compare at batch entry) depends on EVERY
        // write that can move the A20 gate ending the batch, ring-0 or not.
        // Ports 0x92 (system control A), 0x60/0x64 (the 8042 path) can; keep
        // them batch-ending unconditionally. TOKAEMM's a20_apply is PTE-based
        // today (the real gate never drops), so this is belt-and-braces for a
        // future monitor that pokes the real gate, at zero hot-path cost (the
        // monitor's hot pokes are the PIC/EOI ports, not these three).
        //
        // PCI configuration writes are also batch-ending. A BAR or command
        // update rebuilds the direct-RAM map below, so the CPU must discard its
        // cached direct pages before it executes another guest instruction.
        let skip_io_touched = cpu_is_ring0_pm
            && self.lazy_port_reads
            && !matches!(port, 0x60 | 0x64 | 0x92 | 0x00e7)
            && !(PCI_CONFIG_ADDRESS_PORT..=PCI_CONFIG_DATA_END).contains(&port);
        if !skip_io_touched {
            *self.io_touched = true;
        } else {
            // See read_io: an exempted write still programs a device.
            *self.exempt_io_touched = true;
        }
        self.trace.record(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        // Keep in step with the PCI BIOS write arm in bios.rs handle_pci_bios,
        // which mirrors this post-write block for the HLE path.
        let pci_decode = self.vega.memory_decode_key();
        if self.pci.write_io(port, width, value, self.vega) {
            if self.vega.memory_decode_key() != pci_decode {
                self.ram_lookup.rebuild(self.memory.len(), self.vega);
                self.mark_direct_map_changed();
                debug_assert!(*self.io_touched);
            }
            if let Some(disk) = self.ata.as_mut() {
                self.bmide
                    .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
            }
            return Ok(());
        }

        if let Some(base) = self.pci.ide_bus_master_io_base()
            && bmide::BusMasterIde::owns_io(port, width, base)
        {
            if self.pci.ide_io_enabled() {
                self.bmide
                    .write_io(port, width, value, self.ata.as_mut(), base);
                if let Some(disk) = self.ata.as_mut() {
                    self.bmide
                        .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
                }
            }
            return Ok(());
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port write decomposes into byte cycles, mirroring
            // `read_io`: the low byte goes to the port and each higher byte to the
            // next port (`io_word_sub_port` keeps the IDE/ATA data registers on the
            // same port). The VGA index/data idiom (a single 16-bit `OUT 0x3C4`/
            // `0x3CE`/`0x3D4`) depends on this; it used to halt the VM with
            // WidthMismatch.
            for i in 0..width.bytes() {
                <Self as CpuBus>::write_io(
                    self,
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    value >> (8 * i),
                    core_clocks_so_far,
                    cpu_is_ring0_pm,
                )?;
            }
            return Ok(());
        }

        // See `read_io`'s matching site: below the width split (one charge per
        // byte cycle), above every device arm, and above `port_disabled` -- an
        // ISA cycle to a listed port happens whether or not the decode answers.
        self.charge_isa_io_wait(port);

        if self.vega.port_disabled(port) {
            return Ok(());
        }

        if let Some(opl_port) = opl_port(port) {
            let byte = value as u8;
            // Classify BEFORE the write: on a data port the destination register
            // is whatever the matching address port latched earlier, and the
            // write itself does not change that latch. Diagnostic only.
            let bank = u8::from(matches!(opl_port, 0x038a | 0x038b));
            let register = match opl_port {
                0x0389 | 0x038b => Some(self.opl.selected_register(usize::from(bank))),
                // An address-latch write addresses no register itself.
                _ => None,
            };
            self.opl_probe
                .record_write(port, bank, register, byte, self.core_clocks_so_far);
            self.opl.write_port(opl_port, byte);
            return Ok(());
        }
        if matches!(port, 0x224 | 0x225) && self.sb16.write_port(port, value as u8) {
            return Ok(());
        }
        if port == WAVETABLE_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.wavetable_mpu.write_data(value as u8, guest_tick);
            self.opl_probe
                .record_mpu_data_write(true, value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == WAVETABLE_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            self.wavetable_mpu.write_command_at(value as u8, guest_tick);
            self.opl_probe
                .record_mpu_command(true, value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == MIDI_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.midi_mpu.write_data(value as u8, guest_tick);
            self.opl_probe
                .record_mpu_data_write(false, value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == MIDI_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            self.midi_mpu.write_command_at(value as u8, guest_tick);
            self.opl_probe
                .record_mpu_command(false, value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        // AD1848 / Windows Sound System write path. write_port takes the in-region
        // offset and returns (), so the range is checked first (mirrors read_io).
        if let Some(offset) = self.wss_offset(port) {
            self.wss.write_port(offset, value as u8);
            self.sync_wss_board_irq();
            return Ok(());
        }
        if ide::IdeChannel::owns_port(port) {
            self.ide.write_port(port, value as u8);
            return Ok(());
        }
        if ata::AtaDisk::owns_port(port) {
            // Writes to an empty primary channel are dropped; a probe of a bare
            // channel must not fault. A mounted disk takes the task-file write.
            if let Some(disk) = self.ata.as_mut() {
                disk.write_port(port, value as u8);
                self.bmide
                    .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
            }
            return Ok(());
        }
        if fdc::Fdc::owns_port(port) {
            let now_ticks = self.guest_tick_now();
            self.fdc.write_port_at(port, value as u8, now_ticks);
            return Ok(());
        }
        if !matches!(port, 0x224 | 0x225) && self.sb16.write_port(port, value as u8) {
            // A reset armed mid-batch counts its settle from the write, not
            // from the batch start; fold the already-pending span in. See
            // `SbDsp::arm_reset_at`. Only the reset port: nothing on the
            // silicon moves the settle except the reset line, and a command
            // write (0x22C) during a live settle must not restart it.
            if self.lazy_port_reads && port == 0x226 {
                let pending = self.pending_device_micros();
                self.sb16.arm_reset_at(pending);
            }
            // Same for the pause countdown a 0x80 command arms: the DSP folds
            // the span only when this write completed that command.
            if self.lazy_port_reads && port == 0x22C {
                let pending = self.pending_device_micros();
                self.sb16.arm_pause_at(pending);
            }
            self.opl_probe.record_sb(port, true, value as u8);
            return Ok(());
        }
        if self
            .dma
            .write_port(dma_page_register_port(port), value as u8)
        {
            // The 8237A runs a memory-to-memory block transfer when the guest
            // arms a software DREQ on channel 0 (a write to the request register,
            // port 0x09) with mem-to-mem enabled in the command register. The
            // write above recorded the request; fire the block copy here.
            if port == 0x09 && self.dma.mem_to_mem_request_armed() {
                self.dma.mem_to_mem(self.memory);
                // This legacy burst API reports only a byte count, not its possibly
                // decrementing or wrapping destination spans. Retain the coarse fallback.
                *self.device_wrote_memory = true;
            }
            return Ok(());
        }
        if port == 0x61 {
            self.speaker.write_control(value as u8);
            self.pit.set_gate(2, value & 1 != 0);
            return Ok(());
        }
        if port == 0x0092 {
            // Fast A20 gate: bit 1 drives A20, routed through the 8042 so every A20
            // method agrees. Bit 0 (fast CPU reset) is not modeled.
            self.set_a20_gate(value & 0x02 != 0);
            return Ok(());
        }
        if (0x0200..=0x0207).contains(&port) {
            let now = self.guest_tick_now();
            self.gameport.charge(now);
            return Ok(());
        }
        if port == 0x00e1 {
            if let Some(mode) = GswMode::from_register_code(value as u8) {
                *self.pending_mode = Some(mode);
            }
            return Ok(());
        }
        if port == 0x00e3 {
            // Toka-DOS service command: 1 = Repair. Format and LoadBootRecord were
            // removed with the retired HLE DOS kernel.
            // The run loop performs it after this cycle (it needs &mut self).
            *self.pending_toka_service = Some(value as u8);
            return Ok(());
        }
        if port == 0x00e8 {
            // IzarraCD doorbell: execute the device request whose pointer the ROM
            // strategy stub stored in the low-RAM mailbox. The run loop performs
            // it after this cycle (it needs &mut self); the status reads busy
            // from this write until the request completes.
            *self.pending_cd_doorbell = Some(value as u8);
            *self.cd_doorbell_status = 1;
            return Ok(());
        }
        if self.unittester.write_port(port, value as u8) {
            return Ok(());
        }
        if port == 0x00e7 {
            // Lotura port 0xE7: bank a code-page font page into the window at
            // CODEPAGE_FONT_WINDOW. sel = cp*3 + size_index where size_index
            // 0=8x16 (4096 bytes), 1=8x14 (3584 bytes), 2=8x8 (2048 bytes).
            // Valid selectors are 0..14 (five code pages, three sizes each).
            // An out-of-range selector is silently ignored.
            let sel = value as usize;
            let cp = sel / 3;
            let size_index = sel % 3;
            if cp < 5 {
                let (size_off, len) = [(0usize, 4096usize), (4096, 3584), (7680, 2048)][size_index];
                let off = cp * 9728 + size_off;
                let page = &izarravm_firmware::CODEPAGE_FONTS[off..off + len];
                let mut written = 0u32;
                for (i, &byte) in page.iter().enumerate() {
                    if self
                        .memory
                        .write_u8(CODEPAGE_FONT_WINDOW as usize + i, byte)
                        .is_err()
                    {
                        break;
                    }
                    written += 1;
                }
                self.record_pending_device_memory_write(CODEPAGE_FONT_WINDOW, written);
            }
            return Ok(());
        }
        if self.rtc.write_port(port, value as u8) {
            return Ok(());
        }
        if self.serial.write_port(port, value as u8)
            || self.serial2.write_port(port, value as u8)
            || self.lpt.write_port(port, value as u8)
            || self.lpt2.write_port(port, value as u8)
        {
            return Ok(());
        }
        // IDENTITY, not the bare token: a 4F05 that moves margo_bank leaves the token
        // at its banked value while changing which bytes a live pointer means.
        let direct_write_before = self.vega.direct_write_identity();
        // Sampled BEFORE the write, because writing an index port is what moves the selector. Gated
        // at the call site: disarmed, this is one bool test on a device port write.
        let census_selector = if self.vga_wipe_census.enabled && self.vega.port_enabled(port) {
            self.vega.port_index_selector(port)
        } else {
            0
        };
        if self.vega.port_enabled(port) && self.vega.write_port(port, value as u8) {
            // ANY accepted VGA register write may re-point what the aperture READS as, and the
            // read-side registers (GC read map select, read mode, odd/even) are in NO identity:
            // in every planar mode the write identity below is pinned at (0, None), so gating
            // this on an identity change would leave the read side uncovered, which was a
            // measured stale-decode replay. The CPU applies it at the batch boundary behind a
            // has-aperture-code test, so for a guest that never executes from VRAM this store
            // is the entire cost.
            *self.aperture_content_changed = true;
            let direct_write_after = self.vega.direct_write_identity();
            if direct_write_after != direct_write_before {
                if self.vga_wipe_census.enabled {
                    self.vga_wipe_census.record_token_change(
                        port,
                        census_selector,
                        value as u8,
                        // Composite, matching `lib.rs`'s recorder: two scales in
                        // one census would make its rows incomparable.
                        vega_census_token(direct_write_before),
                        vega_census_token(direct_write_after),
                    );
                }
                self.mark_direct_data_map_changed();
                *self.io_touched = true;
                debug_assert!(*self.io_touched);
            }
            return Ok(());
        }
        // 0x43's latch commands freeze the CE, so they need the same in-batch peek
        // the counter read above takes; a count write needs no device state at all,
        // but shares the arm so the port test stays one range compare.
        if matches!(port, 0x40..=0x43) {
            let elapsed_pit_clocks = self.elapsed_pit_clocks();
            // A channel-0 control word (SC=0, RW!=0 -- RW=0 is the latch
            // command, which touches nothing) forces OUT to the mode's initial
            // level IMMEDIATELY, no CLK involved (8254 datasheet; 86Box and
            // DOSBox both drive the IRQ0 line from it). When the write lands
            // in the LOW half of a running mode-3 square wave, that
            // low-to-high move IS an IRQ0 rising edge, and the batch-end
            // advance can never see it: it advances the post-write state.
            // Tyrian 2000 reprograms channel 0 every video frame and its
            // whole 70 Hz clock starves without this edge.
            let byte = value as u8;
            // The peek declines BCD counters; treating "unknown" as HIGH
            // synthesizes no edge there, which is the pre-fix behavior. The
            // live `channel_out` level is the batch-START state and BCD is
            // also the one case the rise-based batch cap declines, so a
            // stale read could fabricate an edge; no PC software clocks
            // channel 0 in BCD (see `count_after`'s precedent).
            let control0_rising = port == 0x43
                && byte >> 6 == 0
                && (byte >> 4) & 0x3 != 0
                && (byte >> 1) & 0x7 != 0
                && !self.pit.out_after(0, elapsed_pit_clocks).unwrap_or(true);
            if self.pit.write_port_at(port, byte, elapsed_pit_clocks) {
                // The tick is only for the trace: gp2-class guests hammer the
                // latch command thousands of times a second, and the probe's
                // contract is that an unarmed trace costs a capacity check,
                // not a RatePhase preview per write.
                let trace_tick = if self.opl_probe.pit_trace_armed() {
                    self.guest_tick_now()
                } else {
                    0
                };
                self.opl_probe.record_pit_write(port, byte, trace_tick);
                if control0_rising {
                    self.opl_probe.count_irq0_edge(trace_tick);
                    self.pic.request(0);
                }
                self.note_pit_observer();
                return Ok(());
            }
        }
        if self.pic.write_port(port, value as u8) {
            return Ok(());
        }
        let a20_before = self.keyboard.a20_enabled();
        if self.keyboard.write_port(port, value as u8) {
            if self.keyboard.a20_enabled() != a20_before {
                self.advance_direct_mapping_epoch();
            }
            return Ok(());
        }
        if self
            .device_ports
            .write_port(dma_page_register_port(port), value as u8)
        {
            return Ok(());
        }
        // AT coprocessor latch strobes. OUT 0xF0 clears the motherboard's FERR
        // busy latch (the flip-flop behind the IRQ13 error path), OUT 0xF1
        // resets the x87; the BIOS INT 75h handler and guest FPU error/detect
        // paths write them as a matter of course (Catacomb 3-D strobes 0xF0 at
        // startup). Both are write-only strobes: the board claims the write and
        // drives nothing on a read, so reads deliberately fall through to open
        // bus below. No FERR->IRQ13 latch is modeled yet, so there is no state
        // to clear -- claiming the write IS the hardware behaviour.
        if matches!(port, 0x00f0 | 0x00f1) {
            return Ok(());
        }
        // Nothing decoded it, so the write goes nowhere -- which is what happens
        // on real hardware and is why Prince of Persia's OUT to 0x7421 must not
        // stop the machine. See `OpenBusPorts`.
        self.open_bus.note(port, true)
    }

    fn interrupt_pending(&self) -> bool {
        self.pic.interrupt_pending()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pic.acknowledge()
    }

    #[inline]
    fn requires_step_break(&self) -> bool {
        *self.io_touched || self.pending_soft_int.is_some()
    }

    /// `core_clocks_so_far = now` -- EXACTLY `read_io`'s own first statement
    /// (`self.core_clocks_so_far = core_clocks_so_far;`, `read_io`, a few lines below),
    /// just reachable before a lazy prediction rather than only inside the read itself.
    /// Idempotent with that statement: publishing `now` here and then calling
    /// `read_io(.., now, ..)` writes the same field the same value twice.
    ///
    /// MUST NOT touch `prior_runs_core_clocks`. An earlier revision zeroed it here,
    /// which corrupted `in_batch_clocks()` (`prior_runs_core_clocks + core_clocks_so_far`
    /// plus `scaled_bus_clocks`, `timing.rs`) for the rest of the run: every lazy prediction
    /// after the first call-out attempt in a batch -- `predicted_beam`, the seam's own
    /// `poll_project_dot_advance`, the lazy PIT/OPL peeks -- read a beam short by the
    /// whole prior-runs core total. `prior_runs_core_clocks` is published once per RUN by
    /// the batch loop (`machine/run.rs:1610`/`:1634`) and this method has no business
    /// resetting a batch-scoped field from a call that fires once per call-out ATTEMPT,
    /// decline or commit alike. Corroborated by measurement (GP2 poll-skip revision
    /// diagnostic pair, 2026-08-26): C1 conservation held to 1.1 ppb while the frame hash
    /// moved -- the charges were right and a prediction INPUT was wrong, which is exactly
    /// this signature.
    #[cfg(feature = "jit")]
    fn publish_core_clocks(&mut self, now: u64) {
        self.core_clocks_so_far = now;
    }

    /// The call-out-site poll-skip seam (GP2 design §4.2/§4.7h). Runs
    /// `try_poll_skip`'s io path (`run::try_poll_skip`, `machine/run.rs`) against the
    /// POD request instead of a real `PollLoop`, with three deliberate departures from
    /// that function, each forced by running mid-block rather than at a batch's run
    /// boundary:
    ///
    /// 1. **No `at_head` gate and no family dispatch.** The caller already proved both
    ///    (screens (d)-(g) in the design): `build_poll_loop_from` was called AT the
    ///    call-out's own slot, so containment stands in for `at_head`, and the request
    ///    only exists for an `Io`-family shape because only that family ever emits an
    ///    `IN` slot.
    /// 2. **No `poll_skip_backedge_housekeeping` call.** That call clears
    ///    `rep_resume_active` / `rep_execution.resume` and belongs to the
    ///    interpreter's own back-edge; a mid-block call-out has no REP resume state to
    ///    clear and must not touch `izarravm-cpu`-side fields it cannot even name
    ///    (BLOCKER C). `poll_commit_bus` -- the bus half of the SAME mechanism -- IS
    ///    called, a few lines below where the interpreter calls it.
    /// 3. **The core-clock instant is `now_at(n)` (`core_clocks_at_block_entry +
    ///    scale_core_clocks(prefix_raw + raw*n, ..)`), never
    ///    `commit_poll_skip_core`.** The interpreter's commit consumes `timing_rem` and
    ///    bumps `perf.poll_skip_spans`; a Direct block's own single end-of-block batch
    ///    call will scale this span's raw clocks exactly once, so committing here too
    ///    would double both (identity I7, obligation 4 item 3). The caller receives the
    ///    RAW clock total back and returns it through the block's existing raw clock
    ///    lane; this method commits only the BUS half, which has no such second charge.
    #[cfg(feature = "jit")]
    fn callout_poll_skip(
        &mut self,
        req: &CalloutPollSkipRequest,
    ) -> Result<CalloutPollSkipOutcome, CalloutPollDecline> {
        if !self.vega.poll_skip_status1_port_active() {
            return Err(CalloutPollDecline::PortInactive);
        }
        // M1's twin: `req.fetch_count` crossed the crate boundary as a plain `u8` with no
        // compile-time bound, so a caller bug that set it above 6 must refuse here rather than
        // index-panic on `req.fetches` (a fixed `[(u32, u32, u8); 6]`).
        let fetches = &req.fetches[..usize::from(req.fetch_count).min(6)];
        // `POLL_SKIP_IO_PORT`: the `poll_skip_status1_port_active` check just above is
        // `port_enabled(0x3da)` plus the colour-personality tests, so this seam's polled
        // port is that constant and the certificate can ask the ISA-charge question about
        // it without the request POD carrying a port.
        let Some(certificate) = self.poll_bus_certificate_from(fetches, POLL_SKIP_IO_PORT) else {
            return Err(CalloutPollDecline::BusCertificate);
        };
        let beam = self.predicted_beam();
        let status = self.vega.status1_bits(beam);
        let bit_set = status & req.status_mask != 0;
        if bit_set != req.spins_when_bit_set {
            return Err(CalloutPollDecline::NotSpinning);
        }
        let bit = req.status_mask.trailing_zeros() as u8;
        let Some(edge_dots) = self
            .vega
            .dots_until_status1_bit_change_from(beam, bit, !bit_set)
        else {
            return Err(CalloutPollDecline::NoEdge);
        };

        // `now_at_run(n)`, exactly the composition obligation 2c derives: ONE scaling of
        // (prefix + raw*n), never a sum of two separately-scaled terms. `now_at_run(0)` is
        // byte-for-byte the un-skipped call-out's own `now` (M-26's pin). RUN-scoped: it starts
        // from `core_clocks_at_block_entry`, which resets to (near) zero at the top of every run,
        // not at the top of the batch.
        let now_at_run = |iterations: u64| -> Option<u64> {
            let raw_total = req
                .raw_core_clocks
                .checked_mul(iterations)?
                .checked_add(req.prefix_raw)?;
            let scaled = scale_core_clocks(raw_total, req.core_num, req.core_den, req.timing_rem)?;
            req.core_clocks_at_block_entry.checked_add(scaled)
        };
        // BLOCKER 2's fix: two DIFFERENT totals, deliberately never conflated into one.
        //
        // `batch_instant(n)` is what `poll_project_dot_advance` needs -- it compares against
        // `MachineBus::in_batch_clocks()`, which is `prior_runs_core_clocks + core_clocks_so_far +
        // scaled_bus_clocks`, ALL THREE terms batch-absolute. `now_at_batch` adds the batch's
        // prior-runs core term (now that `publish_core_clocks` no longer zeroes it -- BLOCKER 1)
        // and `poll_project_scaled_bus_clocks` is already batch-absolute on its own, so
        // `batch_instant(0) == in_batch_clocks()` exactly: the same identity `now_at(0) == now`
        // established for the run-scoped term, now holding for the batch-scoped one too.
        let now_at_batch = |iterations: u64| -> Option<u64> {
            self.prior_runs_core_clocks
                .checked_add(now_at_run(iterations)?)
        };
        let batch_instant = |iterations: u64| -> Option<u64> {
            let core = now_at_batch(iterations)?;
            let bus = self.poll_project_scaled_bus_clocks(certificate, iterations)?;
            core.checked_add(bus)
        };
        // `run_spent(n)` is what the RUN-remaining `req.cap` actually bounds: RUN-scoped core plus
        // RUN-scoped bus GROWTH (the batch-absolute bus reading minus the batch-absolute bus total
        // this run STARTED at, `req.bus_scaled_at_run_entry`) -- algebraically identical to
        // `run_direct_block`'s own `used = total + (bus.in_batch_scaled_bus_clocks() -
        // bus_at_entry)`, extended by `n` skipped iterations. Comparing `req.cap` against a
        // batch-absolute bus reading (the pre-fix defect) subtracted the batch's prior bus clocks
        // from the budget a second time, because `req.cap` is already net of them.
        let bus_growth = |iterations: u64| -> Option<u64> {
            Some(
                self.poll_project_scaled_bus_clocks(certificate, iterations)?
                    .saturating_sub(req.bus_scaled_at_run_entry),
            )
        };
        let run_spent = |iterations: u64| -> Option<u64> {
            now_at_run(iterations)?.checked_add(bus_growth(iterations)?)
        };
        let reserved_total =
            |iterations: u64| -> Option<u64> { run_spent(iterations.checked_add(1)?) };

        let Some(spent) = run_spent(0) else {
            return Err(CalloutPollDecline::Cap);
        };
        // The 32-bit return-lane bound (design obligation 4). Factored into `poll_skip_upper_
        // bound` (Rank-4 killer, GP2 poll-skip revision review): no fixture ever produces a span
        // within four orders of magnitude of `LANE_SAFETY_CEILING`, so the clamp needed a
        // standalone unit test that does not depend on a real edge ever getting that far away.
        let upper = poll_skip_upper_bound(req.cap, spent, req.max_skipped_raw, req.raw_core_clocks);
        let min_iterations = req.min_iterations.max(1);
        if upper < min_iterations {
            return Err(CalloutPollDecline::Cap);
        }

        let admissible = |iterations: u64| -> bool {
            let Some(reserved) = reserved_total(iterations) else {
                return false;
            };
            if reserved > req.cap {
                return false;
            }
            let Some(candidate) = batch_instant(iterations) else {
                return false;
            };
            self.poll_project_dot_advance(candidate)
                .is_some_and(|dots| dots < edge_dots)
        };

        let mut low = min_iterations;
        let mut high = upper;
        let mut best = 0u64;
        while low <= high {
            let mid = low + (high - low) / 2;
            if admissible(mid) {
                best = mid;
                low = mid.saturating_add(1);
            } else {
                high = mid.saturating_sub(1);
            }
        }
        if best < min_iterations {
            return Err(CalloutPollDecline::Cap);
        }

        // `now_after` stays RUN-scoped: the caller (`port_read_al_dx`) feeds it straight into
        // `read_io`'s `core_clocks_so_far` argument, which is run-scoped by the whole existing
        // contract (`read_io`'s own first statement, and `run_budgeted_inner`'s `total` that seeds
        // it), never batch-scoped.
        let Some(now_after) = now_at_run(best) else {
            return Err(CalloutPollDecline::Cap);
        };
        let Some(skipped_raw_core_clocks) = req.raw_core_clocks.checked_mul(best) else {
            return Err(CalloutPollDecline::Cap);
        };
        // Commit the BUS half only (obligation 4 item 3's forbidden-call list): the raw
        // core clocks are handed back to the caller, which returns them through the
        // block's ordinary raw clock lane instead.
        self.poll_commit_bus(certificate, best);
        let committed_raw_bus_clocks = certificate.raw_clocks_per_iteration.saturating_mul(best);

        Ok(CalloutPollSkipOutcome {
            iterations: best,
            skipped_raw_core_clocks,
            committed_raw_bus_clocks,
            now_after,
        })
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.record(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        );
        // THE LANDING ADDRESS IS THE ONLY POSTER. A host-serviced BIOS INT is
        // recognized where the dispatch LANDS (`note_stub_fetch` on a
        // per-vector ROM stub), never at the `INT n` opcode: posting here as
        // well double-serviced two standard dispatch shapes (a guest hook
        // chaining to the saved default, and a copied vector landing on
        // another vector's stub). Real-hardware semantics follow from
        // landing-only posting: a hook that fully handles without chaining
        // gets NO HLE service (the hook replaced the ROM), a hook that chains
        // gets exactly one service at the landing, and a copied vector
        // services as the LANDED vector, once.
        //
        // This arm still posts the two shapes whose landing the fetch seam
        // cannot see:
        // (a) raw-program INT 20h/21h: their IVT entries target the low-RAM
        //     IRET at 0x600, not the per-vector table (0x27 IS table-seeded
        //     and rides the fetch seam like everything else).
        if self.program_runtime && matches!(vector, 0x20 | 0x21) {
            *self.pending_soft_int = Some(vector);
            return Ok(());
        }
        // (b) the legacy shared chain target FF00:0000, which period booters
        //     hardcode (IVT[0x13] -> FF00:0000, or a hook chaining there).
        //     That single address serves every vector, so the fetch seam
        //     cannot attribute a landing by address alone: stash the vector
        //     here and let the FF00:0000 fetch post it (consumed there; a
        //     per-vector stub landing also disarms it). Known corner, accepted
        //     for this legacy-only path: a nested intercepted INT inside a
        //     hook body overwrites the stash before the hook chains to
        //     FF00:0000, dropping the outer service; and a stash left armed by
        //     a non-chaining hook posts once if the guest later jumps to
        //     FF00:0000 outside any INT context.
        if self.soft_int_intercepted(vector)? {
            *self.last_int_vector = Some(vector);
        }
        Ok(())
    }
}

impl MachineBus<'_> {
    /// The one interception predicate for host-serviced software interrupts,
    /// shared by the two dispatch seams: `note_stub_fetch` (execution reaching
    /// the vector's ROM stub by any route - an `INT n` opcode's IVT dispatch,
    /// a DPMI host's simulate-real-mode-interrupt far dispatch, or a guest
    /// chaining to a saved default vector) and `interrupt_acknowledge` (the
    /// legacy-chain stash and the raw-program low-RAM vectors).
    ///
    /// The DOS multiplex vector (INT 2Fh) HLE is landing-scoped: it services
    /// a fetch that LANDS on the ROM stub, which happens only when every
    /// hook above chained down to the boot vector — the Toka-DOS kernel
    /// forwards AH=11h/15h there when the IzarraCD claim armed, and a guest
    /// TSR that chains behaves the same. A hook that fully handles 2Fh
    /// (JEMMEX, DOS/32A's stub) never lands, so it still owns the vector;
    /// in booter-inert mode 2Fh stands down entirely. (Before the IzarraCD
    /// consolidation this also required the IVT to still point at the stub,
    /// which made a chained landing unreachable.)
    /// The pure DOS vectors 0x20-0x2E are not intercepted at all outside the
    /// raw-program runtime now that the Rust DOS kernel is retired, and INT
    /// 67h is never intercepted (the TOKAEMM guest driver owns the EMS API).
    fn soft_int_intercepted(&mut self, vector: u8) -> Result<bool, BusError> {
        let dos_multiplex = vector == 0x2F;
        // 60h/68h/6Fh left this set when POST stopped seeding 60h-6Fh (defect
        // E2): a null vector never lands on the ROM stub, so those arms were
        // dead code. A guest that wants those APIs answered installs its own
        // handler, as on real hardware.
        let absent_resident_api = matches!(vector, 0x5C | 0x7A | 0x86 | 0xE4)
            && self.vector_points_at_rom_iret(vector)?;
        // A `new_raw_program` machine keeps INT 20h/21h/27h intercepted so the run
        // loop's guarded raw-program arm (`handle_raw_program_int`) services them.
        // Outside that runtime nothing intercepts the pure-DOS vectors any more.
        let raw_program_vector = self.program_runtime && matches!(vector, 0x20 | 0x21 | 0x27);
        let intercepted = matches!(
            vector,
            0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x17 | 0x18 | 0x19 | 0x1A | 0x40 | 0x42
        ) || raw_program_vector
            || absent_resident_api
            || dos_multiplex;
        // Every hard-disk boot runs booter-inert (try_boot_hard_disk), which
        // stands 2Fh down so a self-booting disk owns the vector. The
        // IzarraCD claim is the guest kernel's explicit opt-in to the BIOS CD
        // services, so an armed claim lifts that stand-down; a foreign booter
        // never rings the claim and keeps the vector to itself.
        Ok(intercepted && !(self.booter_inert && vector == 0x2F && !self.cd_redirector_armed))
    }

    /// The fetch-seam half of software-interrupt interception: execution has
    /// reached a per-vector ROM stub entry (see BIOS_INT_STUB_TABLE_ROM_OFFSET)
    /// or the legacy shared chain target FF00:0000. Posts the vector for the
    /// run loop's deferred HLE dispatch. Both landing shapes lead with a NOP,
    /// which guarantees a post-instruction break fires before the IRET, so the
    /// service still sees the INT frame on the stack. This is the ONLY poster
    /// for every dispatch route - INT opcode, a DPMI host's simulated
    /// real-mode interrupt, a guest chaining to a saved default - so each
    /// dispatch services exactly once. Repeated fetch charges of the same
    /// visit are absorbed by the pending_soft_int check (the pending vector is
    /// only cleared at the next batch entry, after the service ran and
    /// execution moved to the IRET byte, whose odd offset never posts).
    ///
    /// `address` is a LINEAR address supplied through `note_code_fetch_linear`:
    /// the stub table's identity is architectural, and a paging guest that
    /// shadows the BIOS F-page (JemmEx) still dispatches through linear
    /// FF00:02xx while backing it with another physical page. Residual
    /// divergence, recorded per review: a pmode guest running unrelated code
    /// AT linear 0xFF0xx/0xFF2xx (mapped wherever) posts a bogus service;
    /// deliberate-hostile only, no real DOS stack does this.
    pub(super) fn note_stub_fetch(&mut self, address: u32) {
        if address == BIOS_LEGACY_IRET_LINEAR {
            // The legacy shared nop;iret at FF00:0000: one address for every
            // vector, so attribution comes from the stash the `INT n` opcode
            // arm left behind. Consumed here; a landing with no armed stash
            // (a simulated jump with no preceding INT) stays a no-op.
            let stashed = self.last_int_vector.take();
            if self.pending_soft_int.is_none()
                && let Some(vector) = stashed
                && self.soft_int_intercepted(vector).unwrap_or(false)
            {
                *self.pending_soft_int = Some(vector);
            }
            return;
        }
        let offset = address.wrapping_sub(BIOS_INT_STUB_TABLE_LINEAR);
        if offset >= BIOS_INT_STUB_TABLE_LEN || offset & 1 != 0 {
            return; // outside the table, or the IRET byte (mid-stub resume)
        }
        let vector = (offset / 2) as u8;
        let intercepted = self.soft_int_intercepted(vector).unwrap_or(false);
        // An INTERCEPTED landing supersedes any armed legacy stash: the
        // dispatch the stash described has been attributed here by address
        // instead. A NON-intercepted landing must leave the stash alone - its
        // ack never armed it, and the machine's own timer ISR chains INT 1Ch
        // (stub 0x1C) every tick, so an unconditional disarm would race a
        // hardware IRQ against a live hook-chain attribution and drop the
        // chained service (round-2 review finding 1).
        if intercepted {
            *self.last_int_vector = None;
        }
        if let Some(pending) = *self.pending_soft_int {
            // The dedup above is vector-blind; the only legitimate repeat is a
            // re-charge of the SAME pending visit (round-1 review finding 3).
            debug_assert!(
                pending == vector,
                "stub fetch posted vector {vector:#04x} while {pending:#04x} is still pending"
            );
            return;
        }
        if intercepted {
            *self.pending_soft_int = Some(vector);
        }
    }

    fn vector_points_at_rom_iret(&mut self, vector: u8) -> Result<bool, BusError> {
        let address = usize::from(vector) * 4;
        let off = self.memory.read_u16(address)?;
        let seg = self.memory.read_u16(address + 2)?;
        // A vector is "still the BIOS default" when it points at its per-vector
        // ROM stub. The legacy shared IRET at FF00:0000 is also accepted:
        // period booters hardcode that address (IVT[0x13] -> FF00:0000 to chain
        // disk calls), and pre-table guests may restore a saved default.
        Ok(seg == BIOS_ROM_IRET_SEG && (off == bios_int_stub_off(vector) || off == 0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DevicePorts {
    ports: std::collections::BTreeMap<u16, u8>,
}

impl Default for DevicePorts {
    fn default() -> Self {
        let mut ports = std::collections::BTreeMap::new();
        for port in known_passive_ports() {
            ports.insert(port, 0xff);
        }
        Self { ports }
    }
}

impl DevicePorts {
    fn read_port(&self, port: u16) -> Option<u8> {
        self.ports.get(&port).copied()
    }

    fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(slot) = self.ports.get_mut(&port) else {
            return false;
        };
        *slot = value;
        true
    }
}

/// Unclaimed-port accounting and policy.
///
/// A real ISA machine floats an unclaimed read to all-ones and swallows an
/// unclaimed write; nothing faults. This machine used to raise a fatal
/// `CpuError` on both, which had two costs. It diverged from the hardware, and
/// it was a WORSE diagnostic than it looked: the run died at the FIRST port a
/// guest probed, so every later probe stayed invisible. A detection sweep across
/// eight bases reported one. SciTech's UVCONFIG died reading 0xC000 and Prince
/// of Persia died WRITING 0x7421, and neither told you what else it would have
/// touched.
///
/// So the default is now open bus, and the diagnostic is the port set this
/// accumulates across the whole run. `IZARRAVM_PORT_FATAL=<hex>[,<hex>]` puts
/// named ports back on the fatal path when you want the fault-site machinery to
/// name the exact CS:IP behind one specific probe.
#[derive(Debug, Default)]
pub struct OpenBusPorts {
    reads: u64,
    writes: u64,
    ports: std::collections::BTreeSet<u16>,
    fatal: std::collections::BTreeSet<u16>,
    reported: u32,
}

/// Stop naming new ports on stderr past this many, so a guest that sweeps a
/// whole range reports its shape without flooding the log. The port set keeps
/// accumulating either way; only the running commentary stops.
const OPEN_BUS_REPORT_LIMIT: u32 = 32;

impl OpenBusPorts {
    /// Parse `IZARRAVM_PORT_FATAL`: comma-separated hex ports that keep the old
    /// fatal behaviour.
    pub fn from_env() -> Self {
        let mut ports = Self::default();
        let Ok(spec) = std::env::var("IZARRAVM_PORT_FATAL") else {
            return ports;
        };
        for token in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match u16::from_str_radix(token.trim_start_matches("0x"), 16) {
                Ok(port) => {
                    ports.fatal.insert(port);
                }
                Err(_) => eprintln!("port-fatal: ignoring {token:?} (want hex)"),
            }
        }
        if !ports.fatal.is_empty() {
            eprintln!("port-fatal: {} port(s) kept fatal", ports.fatal.len());
        }
        ports
    }

    fn note(&mut self, port: u16, write: bool) -> Result<(), BusError> {
        if self.fatal.contains(&port) {
            return Err(BusError::UnsupportedPort { port });
        }
        if write {
            self.writes += 1;
        } else {
            self.reads += 1;
        }
        if self.ports.insert(port) && self.reported < OPEN_BUS_REPORT_LIMIT {
            self.reported += 1;
            let direction = if write { "write to" } else { "read from" };
            eprintln!("open-bus: {direction} unclaimed port {port:#06x}");
            if self.reported == OPEN_BUS_REPORT_LIMIT {
                eprintln!("open-bus: further ports counted but not named");
            }
        }
        Ok(())
    }

    /// Put `ports` back on the fatal path, the programmatic twin of
    /// `IZARRAVM_PORT_FATAL`. Tests of the fatal-fault diagnostics use this
    /// rather than the environment, which is process-global and would race the
    /// rest of the suite.
    pub fn set_fatal(&mut self, ports: &[u16]) {
        self.fatal.extend(ports.iter().copied());
    }

    /// Whether `port` floated this run. This is the observable that replaced the
    /// old `Err(UnsupportedPort)`: it still separates "a device decoded it and
    /// answered 0xFF" from "nothing decoded it", which a bare read of 0xFF
    /// cannot.
    pub fn floated(&self, port: u16) -> bool {
        self.ports.contains(&port)
    }

    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn writes(&self) -> u64 {
        self.writes
    }

    /// Every distinct port that floated this run, in ascending order.
    pub fn ports(&self) -> impl ExactSizeIterator<Item = u16> + '_ {
        self.ports.iter().copied()
    }
}

fn known_passive_ports() -> impl Iterator<Item = u16> {
    let ranges = [
        0x0000..=0x000f, // DMA controller 1
        0x0062..=0x0063, // system control port B (speaker now owns 0x61)
        0x0080..=0x008f, // DMA page registers
        0x00c0..=0x00df, // DMA controller 2
        0x0220..=0x022f, // Sound Blaster base
        0x0240..=0x024f, // C/MS Game Blaster alternate bases, the two the 0x280 entry
        0x0260..=0x026f, // below missed. Prince of Persia's sound detect sweeps base+3
        // across the standard set and only stopped faulting at the
        // LAST of them: the game still halted with
        // CpuError("unsupported I/O port 0x0243") a second into its
        // boot. 0x243 is the observed fault; 0x263 is the remaining
        // standard base, added by symmetry so the next probe in the
        // same sweep cannot halt the machine the same way. This
        // chipset fixes the Sound Blaster at 0x220, so 0x240 and
        // 0x260 hold no device and open bus is what the hardware
        // does.
        0x0280..=0x028f, // C/MS Game Blaster alternate-base probe range (Prince of
        // Persia's sound detect reads 0x283 and must see open bus,
        // not a fault -- the port-0x201 joystick-stub precedent)
        0x0388..=0x038b, // OPL2/OPL3 (intercepted by the chip, kept as a fallback)
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
        0x5658..=0x565b, // VMware backdoor probe (DX=0x5658, EAX='VMXh'): real,
                         // non-VMware hardware has nothing at this port, so a guest's `IN
                         // EAX, DX` detection probe must read open bus (all-ones), never the
                         // VMware magic response and never an UnsupportedPort fault. A dword
                         // IN decomposes into four byte reads at 0x5658-0x565b (the same
                         // io_word_sub_port widening as every other wide port access), so all
                         // four bytes are covered here. JEMMEX runs this probe during its own
                         // hypervisor-presence check and used to halt the machine with
                         // CpuError("unsupported I/O port 0x5658") before this stub existed.
    ];
    ranges.into_iter().flatten()
}

/// PIIX4 DMAAC aliases the upper page-register window onto the IBM AT window.
/// Port 92h remains the separate fast-A20 and reset control register.
fn dma_page_register_port(port: u16) -> u16 {
    match port {
        0x0090..=0x009f if port != 0x0092 => port - 0x10,
        _ => port,
    }
}

impl MachineBus<'_> {
    fn sync_mpu_irq(&mut self) {
        self.pic.set_irq_level(
            9,
            self.wavetable_mpu.irq_level() || self.midi_mpu.irq_level(),
        );
    }

    /// Apply the WSS board's IRQ-verify strobe to the interrupt controller.
    ///
    /// Bit 6 of the config register is a board latch wired straight to the
    /// selected ISA interrupt line: raising it drives that line, lowering it
    /// releases it, and re-selecting while it is held moves the assertion to the
    /// new line. An MSS install routine uses it to prove the line it picked
    /// actually reaches the CPU. The codec core has no interrupt controller, so
    /// it records the delta and this drains it on the guest's own `OUT` -- the
    /// shape `sync_mpu_irq` above uses for the MPU-401 line.
    ///
    /// `set_irq_level` is the right primitive rather than `request`: the board
    /// drives a *level*, and on the AT's edge-triggered 8259 the rising edge is
    /// what latches into the IRR, exactly as it would on the wire. The codec's
    /// own terminal-count edge stays on `request` and is unaffected.
    fn sync_wss_board_irq(&mut self) {
        let Some(strobe) = self.wss.take_board_irq_strobe() else {
            return;
        };
        if let Some(line) = strobe.released {
            self.pic.set_irq_level(line, false);
        }
        if let Some(line) = strobe.asserted {
            self.pic.set_irq_level(line, true);
        }
    }

    /// In-region offset (0..=7) of `port` within the AD1848 / WSS port window
    /// `[wss_base, wss_base + 8)`, or `None` when the codec is disabled or the
    /// port lies outside the window. The codec's read_port/write_port take this
    /// offset; the caller dispatches to them only on `Some`.
    fn wss_offset(&self, port: u16) -> Option<u16> {
        if !self.wss_enabled {
            return None;
        }
        port.checked_sub(self.wss_base).filter(|&off| off < 8)
    }

    /// Apply the A20 gate to a physical address before it reaches memory. The gate
    /// is the single 8042 output-port bit (shared with fast-A20 port 0x92); when
    /// it is closed, address line 20 is forced low. This is the motherboard-level
    /// effect, so it sits at the one CPU bus seam and covers fetches and data
    /// alike. Host-side pokes (write_physical_u8 and friends) deliberately bypass
    /// it: they address exact physical cells, not the guest's gated bus.
    /// pub(super) so the memory-poll executor's spin read gates the certified
    /// physical exactly like the certificate and the interpreter do.
    pub(super) fn apply_a20(&self, address: u32) -> u32 {
        if self.keyboard.a20_enabled() {
            address
        } else {
            address & A20_MASK
        }
    }

    #[inline]
    fn direct_vga_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
        write: bool,
    ) -> Option<(u32, usize)> {
        let gated = self.apply_a20(address);
        let byte_count = u32::try_from(bytes).ok()?;
        let end = gated.checked_add(byte_count)?;
        let video_end = izarravm_video::VGA_MODE13H_BASE + VGA_PLANAR_WINDOW_SIZE;
        if gated != address
            || bytes == 0
            || should_split(gated, access_width)
            || ((gated as usize & RAM_LOOKUP_PAGE_MASK) + bytes > RAM_LOOKUP_PAGE_SIZE)
            || gated < izarravm_video::VGA_MODE13H_BASE
            || end > video_end
        {
            return None;
        }
        let available = if write {
            self.vega.direct_write_token() != 0
        } else {
            self.vega.mode13_direct_page_available()
        };
        available.then_some((gated, gated as usize & RAM_LOOKUP_PAGE_MASK))
    }

    /// Whether `bytes` at `address` are one contiguous run inside Margo's linear
    /// framebuffer, and the gated address to serve them from.
    ///
    /// Deliberately WITHOUT the page-crossing clause the RAM and legacy-VGA
    /// probes above carry. Those two serve out of a host page table where a
    /// run that leaves the page changes backing store mid-copy; Margo's VRAM is
    /// one contiguous `Vec` and `margo_lfb_direct_bytes` bounds-checks the
    /// WHOLE range, so a page crossing changes nothing about the slice. Keeping
    /// the clause would have declined most real scanline copies: a 640-byte
    /// mode-101h row crosses a 4 KiB boundary once every seven rows, and a
    /// full-width REP MOVS of a 1024-byte row crosses on half of them.
    ///
    /// The A20 clause is NOT dead here. `A20_MASK` clears bit 20 at every
    /// address, not only under 1 MiB, and the 4 MiB LFB aperture spans bit 20 --
    /// so with A20 off the second and fourth megabytes alias down. Declining
    /// leaves them to the slow path, which applies the same gating one byte at
    /// a time.
    #[inline]
    fn direct_margo_lfb_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
    ) -> Option<u32> {
        let end = address.checked_add(u32::try_from(bytes).ok()?)?;
        if address < crate::MARGO_LFB_BASE || end > crate::MARGO_MMIO_BASE {
            return None;
        }
        let gated = self.apply_a20(address);
        if gated != address
            || bytes == 0
            || should_split(gated, access_width)
            || self.vega.margo_lfb_direct_bytes(gated, bytes).is_none()
        {
            return None;
        }
        Some(gated)
    }

    fn record_direct_vga_accesses(
        &mut self,
        address: u32,
        bytes: usize,
        width: BusWidth,
        kind: BusAccessKind,
    ) {
        self.vega.record_direct_access(address, bytes, kind);
        let wait_states = if self.active_mode.uses_approximate_timing() {
            video_wait_states_approx(self.active_mode.persona())
        } else {
            self.wait_states.video
        };
        let count = bytes / width.bytes() as usize;
        self.trace
            .record_memory_run(kind, address, count as u32, width, wait_states);
    }

    fn record_direct_ram_accesses(
        &mut self,
        address: u32,
        bytes: usize,
        width: BusWidth,
        kind: BusAccessKind,
    ) {
        if self.active_mode.uses_approximate_timing() {
            let count = bytes / width.bytes() as usize;
            self.trace
                .record_memory_run(kind, address, count as u32, width, self.cache.cost.l1);
            return;
        }
        for offset in (0..bytes).step_by(width.bytes() as usize) {
            let at = address + offset as u32;
            let wait_states = self.data_access_wait_states(at, width);
            self.trace.record(kind, at, width, wait_states);
        }
    }

    #[inline]
    fn direct_ram_range(&self, address: u32, width: BusWidth) -> Option<(usize, usize)> {
        self.direct_ram_bytes(address, width.bytes() as usize)
    }

    #[inline]
    pub(super) fn direct_ram_bytes(&self, address: u32, bytes: usize) -> Option<(usize, usize)> {
        let start = address as usize;
        let end = start.checked_add(bytes)?;
        if end <= 0x000A_0000 && end <= self.memory.len() {
            return Some((start, end));
        }
        self.ram_lookup.direct_bytes(address, bytes)
    }

    #[inline]
    fn direct_page_ram_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
    ) -> Option<(u32, usize, usize)> {
        let gated = self.apply_a20(address);
        if gated != address || bytes == 0 {
            return None;
        }
        if should_split(gated, access_width)
            || ((gated as usize & RAM_LOOKUP_PAGE_MASK) + bytes > RAM_LOOKUP_PAGE_SIZE)
        {
            return None;
        }
        self.direct_ram_bytes(gated, bytes)
            .map(|(start, end)| (gated, start, end))
    }

    /// `direct_page_ram_bytes` without the `should_split` term, and WITHOUT an `access_width`
    /// parameter because it no longer asks an alignment question at all.
    ///
    /// A sibling rather than a relaxation of `direct_page_ram_bytes`: that function's other
    /// callers are the bulk paths (`read_memory_bytes_direct`, `write_memory_bytes_direct`) whose
    /// `access_width` contract is different -- they fold a RUN at the access width, and a
    /// misaligned run would be mis-charged. Only the single-access direct read/write pair may
    /// come here, and each pays for it by calling `charge_direct_ram_split` instead of a wide
    /// charge.
    ///
    /// Page-locality is still required, and is what makes the byte-equality argument in
    /// `read_memory_direct` a statement about a whole page rather than about one address.
    #[inline]
    fn direct_page_ram_bytes_unaligned(
        &self,
        address: u32,
        bytes: usize,
    ) -> Option<(u32, usize, usize)> {
        let gated = self.apply_a20(address);
        if gated != address || bytes == 0 {
            return None;
        }
        if (gated as usize & RAM_LOOKUP_PAGE_MASK) + bytes > RAM_LOOKUP_PAGE_SIZE {
            return None;
        }
        self.direct_ram_bytes(gated, bytes)
            .map(|(start, end)| (gated, start, end))
    }

    fn read_phys_u8(&mut self, address: u32) -> Result<u8, BusError> {
        let mut byte = [0];
        self.read_phys(address, &mut byte)?;
        Ok(byte[0])
    }

    fn read_phys(&mut self, address: u32, out: &mut [u8]) -> Result<(), BusError> {
        let width = out.len();
        if width == 0 {
            return Ok(());
        }

        if let Some((start, end)) = self.direct_ram_bytes(address, width) {
            out.copy_from_slice(&self.memory.as_slice()[start..end]);
            return Ok(());
        }

        if let Some(offset) = rom_offset(address, width) {
            out.copy_from_slice(&self.rom[offset..offset + width]);
            return Ok(());
        }

        // STATUS.BUSY is answered as of THIS instant, not as of batch start (see
        // `Margo::read_mmio_u8_at`). The offset is only computed for the Margo
        // MMIO window; every other aperture below passes 0.
        let margo_elapsed_ns = if vega::margo_mmio_at(address) {
            self.elapsed_margo_ns()
        } else {
            0
        };
        if self.vega.read_memory(address, out, margo_elapsed_ns) {
            return Ok(());
        }

        if is_open_bus_uma(address, width) {
            // Unoccupied upper memory: open bus reads as 0xFF, matching a real
            // machine's floating data bus over an adapter-free UMA hole.
            out.fill(0xff);
            return Ok(());
        }

        let end = address as usize + width;
        if end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..end]);
            return Ok(());
        }

        // No device or memory answers this physical address. The data lines
        // float high instead of raising a CPU-visible memory fault.
        out.fill(0xff);
        Ok(())
    }

    fn write_memory_recorded<R: RamWriteRecorder>(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
        recorder: &mut R,
    ) -> Result<(), BusError> {
        let address = self.apply_a20(address);
        if self.vega.write_wide_memory(address, width, value) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return Ok(());
        }

        if should_split(address, width) {
            for offset in 0..width.bytes() {
                self.write_memory_recorded(
                    address + offset,
                    BusWidth::Byte,
                    (value >> (offset * 8)) & 0xff,
                    kind,
                    recorder,
                )?;
            }
            return Ok(());
        }

        if let Some((start, _)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8),
                BusWidth::Word => self.memory.write_u16(start, value as u16),
                BusWidth::Dword => self.memory.write_u32(start, value),
            }?;
            recorder.record_ram_write(address, width.bytes());
            return Ok(());
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        match width {
            BusWidth::Byte => self.write_memory_byte_recorded(address, value as u8, recorder),
            BusWidth::Word => {
                for (offset, byte) in (value as u16).to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte_recorded(address + offset as u32, byte, recorder)?;
                }
                Ok(())
            }
            BusWidth::Dword => {
                for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte_recorded(address + offset as u32, byte, recorder)?;
                }
                Ok(())
            }
        }
    }

    #[inline]
    fn byte_route(&self, address: u32) -> ByteRoute {
        if let Some((backing, _)) = self.direct_ram_bytes(address, 1) {
            ByteRoute::DirectRam(backing)
        } else if rom_offset(address, 1).is_some() {
            ByteRoute::Rom
        } else if is_open_bus_uma(address, 1) {
            ByteRoute::OpenBus
        } else {
            // VGA acceptance depends on live device state. Keep that final decision in the
            // mutation path, then distinguish a rejected device route from fallback RAM there.
            ByteRoute::DeviceOrFallbackRam
        }
    }

    fn write_memory_byte_recorded<R: RamWriteRecorder>(
        &mut self,
        address: u32,
        value: u8,
        recorder: &mut R,
    ) -> Result<(), BusError> {
        match self.byte_route(address) {
            ByteRoute::DirectRam(backing) => {
                self.memory.write_u8(backing, value)?;
                recorder.record_ram_write(address, 1);
            }
            ByteRoute::Rom | ByteRoute::OpenBus => {}
            ByteRoute::DeviceOrFallbackRam => {
                match self.vega.write_memory_u8(address, value) {
                    // The write that MOVED the Margo blit engine's busy time
                    // stamps its own in-batch instant as the origin that time is
                    // measured from. Margo drains once, at batch end, with the
                    // WHOLE batch's nanoseconds -- so without this credit an
                    // operation armed partway in would be billed for the part of
                    // the batch that ran before it started, and STATUS.BUSY would
                    // report idle before the modeled time had passed
                    // (`docs/vega/vega-technical-reference.md` section 9).
                    //
                    // This deliberately does NOT set `io_touched`. It used to:
                    // STATUS.BUSY is MMIO, so the guest's `margo_wait` spin
                    // cannot break its own batch, and ending the batch here was
                    // the only way the spin could see BUSY drop at the right
                    // instant. `Margo::status_busy_after` answers that read
                    // analytically now, the way `Counter::count_after` does for
                    // the PIT, so the break is no longer what buys section 9's
                    // exactness. It is dropped on those grounds alone: removing
                    // it was MEASURED wall-neutral on current main (+0.10% on
                    // prince-486, +0.64% on nascar-586, both inside noise), and
                    // the ~5% it once cost is not reproducible -- see the stale-
                    // cost note on `Machine::vega_edge_ticks`. Dropping it also
                    // stops this term arming that deadline at all, which is the
                    // dependency documented there.
                    //
                    // EDGE, not level: `VideoWrite::ArmedBlit` is reported only
                    // by a write that moved busy time (see `VideoWrite`). A blit
                    // can outlast a batch, and writes overlapped with a running
                    // one must NOT re-stamp the origin -- that would keep
                    // resetting the operation's start and stretch it forever.
                    VideoWrite::ArmedBlit => {
                        let elapsed_ns = self.elapsed_margo_ns();
                        self.vega.credit_blit_arm(elapsed_ns);
                        return Ok(());
                    }
                    VideoWrite::Accepted => return Ok(()),
                    VideoWrite::Unclaimed => {
                        if (address as usize) < self.memory.len() {
                            self.memory.write_u8(address as usize, value)?;
                            recorder.record_ram_write(address, 1);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Wait-states to charge for a DATA access at the post-A20 physical `address`,
    /// routed through the cosmetic cache so its tag state stays warm. The cache
    /// tiers ONLY cacheable RAM: a ROM or video/MMIO window keeps its existing
    /// `memory_wait_states` cost UNCHANGED (it is never cached, so it must not warm
    /// the model nor be re-timed by it). Cacheable RAM (conventional `< 0xA0000`
    /// and any extended RAM that is not a device window) is tiered, and the resolved
    /// tier's per-mode cost is charged.
    fn data_access_wait_states(&mut self, address: u32, width: BusWidth) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, width) {
            // Device/ROM: untiered, unchanged timing (both classes).
            return self.memory_wait_states(address);
        }
        if self.flat_data_cost {
            // Approximate class (486/586): charge the flat L1-resident cost and skip
            // the per-access tag-array tiering. The
            // benchmarks are L1-resident so cyc/iter stays near the accurate model;
            // the win is skipping ~3M tag lookups per run. Guest-invisible: only time.
            return self.cache.cost.l1;
        }
        self.cache.data_wait_states(address, width)
    }

    /// Wait-states for a single code-fetch byte at the post-A20 physical `address`.
    /// Code in cacheable RAM is charged the per-mode L1 constant (code is assumed
    /// I-cache resident); code fetched from ROM/device keeps `memory_wait_states`,
    /// so firmware/POST and any execution out of a device window are unchanged.
    fn code_fetch_wait_states(&self, address: u32) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, BusWidth::Byte) {
            self.memory_wait_states(address)
        } else {
            self.cache.code_fetch_wait_states()
        }
    }

    /// True iff `address` (post-A20, width `width`) lands in a ROM or video/MMIO
    /// window the cache must not tier. Mirrors the device-classification arm of
    /// `memory_wait_states_device` (the `wait_states.rom`/`wait_states.video`
    /// branches); the fall-through (cacheable RAM) returns false. Only called for
    /// `address >= 0xA0000`, so conventional RAM never reaches here.
    pub(super) fn is_device_window(&self, address: u32, width: BusWidth) -> bool {
        let bytes = width.bytes() as usize;
        #[cfg(test)]
        self.vega.note_device_window_question();
        if self.is_device_free_extended_ram(address, bytes) {
            // The screen is a hand-derived claim about the two claimants below,
            // so prove it on every debug run rather than trusting the
            // derivation -- the same discipline `Vega::owns_memory` applies to
            // its own `may_own_memory` prefilter. If this ever fires, a window
            // moved above 1 MB without `device_free_extended_floor` learning
            // about it, and the cost of believing the screen is a wrong charged
            // wait-state, not a crash.
            debug_assert!(
                !(rom_offset(address, bytes).is_some() || self.vega.owns_memory(address, bytes)),
                "extended-RAM screen accepted {address:#x} width {bytes}, which a window claims"
            );
            return false;
        }
        #[cfg(test)]
        self.vega.note_device_window_gauntlet_entry();
        rom_offset(address, bytes).is_some() || self.vega.owns_memory(address, bytes)
    }

    /// The device-window gauntlet with the extended-RAM screen BYPASSED: what
    /// `is_device_window` would answer if the screen did not exist.
    ///
    /// Test-only, and the exact sibling of `Vega::owns_memory_uncached`, which
    /// exists for the same reason one region down. A test that asked
    /// `is_device_window` whether the screen is safe would be circular: the
    /// screen returns `false` by returning early, so the answer would agree
    /// with itself. This is the independent second opinion the sweep compares
    /// against.
    #[cfg(test)]
    pub(super) fn device_window_uncached(&self, address: u32, width: BusWidth) -> bool {
        let bytes = width.bytes() as usize;
        rom_offset(address, bytes).is_some() || self.vega.owns_memory(address, bytes)
    }

    /// True when `address` (post-A20, `bytes` wide) is extended RAM that NO
    /// window can claim, so the gauntlet below would answer `false` after
    /// running every predicate in it.
    ///
    /// This is the hot screen for a 32-bit game. Its code and its data both live
    /// above 1 MB, so every instruction-fetch run and every data access it makes
    /// arrives at the device classification, which the `< 0x000A_0000` screen
    /// above was written for conventional memory and cannot help with. Measured
    /// on 2026-08-29 riprofile reports: `charge_classified_instruction_fetch_run`
    /// plus `Vega::owns_memory` carried 4.65% of duke3d-586 wall, 2.88% of
    /// gp2-586 and 2.49% of nascar-586.
    ///
    /// EXACTNESS, which is the whole contract -- this must not change a single
    /// charged wait-state. Two claimants exist above 1 MB. `rom_offset` claims
    /// `HIGH_ROM_BASE ..= u32::MAX` (its other region, `LOW_BIOS_BASE`, ends at
    /// 1 MB), and `Vega::may_own_memory` claims Margo's LFB+MMIO block and
    /// Distira's BAR. `device_free_extended_floor` is the lowest of those two
    /// Vega regions, and `HIGH_ROM_BASE` (0xFFFF_0000) is above `MARGO_LFB_BASE`
    /// (0xE000_0000), so it can never be the lowest. The window this returns
    /// true for therefore touches no claimant.
    ///
    /// The `bytes` term is belt-and-braces: both claimants decide from the BASE
    /// address alone, so a straddling access at the very top of the window is
    /// already classified by its base. Including the width can only make the
    /// screen decline more often, never wrongly accept.
    ///
    /// A20 is the way this screen could be wrong without ever looking wrong:
    /// with the gate masked, `0x0010_0000..0x0010_FFEF` aliases DOWN into
    /// conventional memory, so an address that reads as extended RAM can land in
    /// a claimed window. Every caller already folds the address first -- the
    /// whole wait-state family is documented "at the post-A20 physical
    /// `address`", and every call site was checked against that -- and the
    /// assertion below is what stops a future caller from quietly breaking it.
    /// The assertion is exact rather than approximate: a post-A20 address has
    /// bit 20 already resolved, so folding it a second time is the identity
    /// whatever the gate is doing.
    ///
    /// The window is NOT protected-mode-only. TokaDOS loads DOS=HIGH, so
    /// real-mode fetches out of the HMA (`0x0010_0000..0x0010_FFEF` with A20
    /// open) are extended RAM by this test and take the fast path on every boot.
    /// That is deliberate, not incidental.
    #[inline]
    pub(super) fn is_device_free_extended_ram(&self, address: u32, bytes: usize) -> bool {
        debug_assert_eq!(
            self.apply_a20(address),
            address,
            "the extended-RAM screen needs a POST-A20 address; {address:#x} is not folded"
        );
        self.extended_ram_screen
            && address >= 0x0010_0000
            && address.saturating_add(bytes as u32) <= self.vega.device_free_extended_floor()
    }

    #[inline]
    pub(super) fn memory_wait_states(&self, address: u32) -> u8 {
        // Conventional RAM (below the 0xA0000 video aperture) is never overlapped
        // by a ROM, VGA, Margo, or Distira window, so it always runs at RAM speed.
        // The hot fetch/data path hits this on every access, so keep it a tiny
        // inlinable check and defer the device-window gauntlet to a cold helper.
        // This matches the fall-through the gauntlet would reach anyway (it already
        // classifies by the base address only).
        if address < 0x000A_0000 {
            return self.wait_states.ram;
        }
        self.memory_wait_states_device(address)
    }

    #[cold]
    fn memory_wait_states_device(&self, address: u32) -> u8 {
        #[cfg(test)]
        self.vega.note_device_window_question();
        // Extended RAM reaches here because the caller's screen is `< 0xA0000`,
        // and it would fall all the way through to the `wait_states.ram` arm at
        // the bottom. Answer it here instead. See `is_device_free_extended_ram`.
        if self.is_device_free_extended_ram(address, 1) {
            debug_assert!(
                !(rom_offset(address, 1).is_some() || self.vega.owns_memory(address, 1)),
                "extended-RAM screen accepted {address:#x}, which a window claims"
            );
            return self.wait_states.ram;
        }
        #[cfg(test)]
        self.vega.note_device_window_gauntlet_entry();
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if self.vega.owns_memory(address, 1) {
            // The Approximate class charges the era bus latency of a real video
            // card (see `video_wait_states_approx`); the Accurate class keeps the
            // frozen profile value bit-for-bit.
            if self.active_mode.uses_approximate_timing() {
                video_wait_states_approx(self.active_mode.persona())
            } else {
                self.wait_states.video
            }
        } else {
            self.wait_states.ram
        }
    }
}

/// One-line forward to the single spelling of the alignment predicate
/// (`BusWidth::misaligned_at`). Kept as a named free function only because four call sites read
/// better as "does this access split" than as "is it misaligned"; it adds no logic of its own.
#[inline]
fn should_split(address: u32, width: BusWidth) -> bool {
    width.misaligned_at(address)
}

fn rom_offset(address: u32, width: usize) -> Option<usize> {
    let offset = if (HIGH_ROM_BASE..=u32::MAX).contains(&address) {
        address.wrapping_sub(HIGH_ROM_BASE)
    } else if (LOW_BIOS_BASE..LOW_BIOS_BASE + BIOS_ROM_SIZE as u32).contains(&address) {
        address - LOW_BIOS_BASE
    } else {
        return None;
    } as usize;

    (offset + width <= BIOS_ROM_SIZE).then_some(offset)
}

/// True if `address` (for an access of `width` bytes, entirely) falls in the
/// unoccupied part of the upper-memory area: the UMB-able holes between the
/// video option ROM span and the system BIOS, 0xC8000-0xEFFFF. On a real
/// machine nothing answers there unless an adapter or a memory manager's
/// page-frame claims it; this machine's own occupants (VGA BIOS data tables,
/// the code-page font bank, TOKAEMM's linear-to-extended-RAM UMB remap) all
/// live below 0xC8000 or are reached through paging at a physical address
/// above 1 MiB, so this range check never needs to special-case them.
///
/// Guests that probe the UMA for a free window (JEMMEX and other EMS/UMB
/// managers scanning for a page frame) rely on this reading as open bus
/// (conventionally 0xFF, writes ignored), exactly like the existing
/// 0x201/0x280-0x28F/0x5658 open-bus port conventions but for memory instead
/// of I/O space.
fn is_open_bus_uma(address: u32, width: usize) -> bool {
    let uma_occupied_end = UPPER_MEMORY_BASE + VGA_BIOS_SPAN_SIZE;
    let Some(end) = address.checked_add(width as u32) else {
        return false;
    };
    address >= uma_occupied_end && end <= SYSTEM_ROM_BASE
}
