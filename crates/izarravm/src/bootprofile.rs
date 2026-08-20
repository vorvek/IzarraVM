// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Boot-phase profiler: boots the user's real C: through the same Katea facade
//! the GUI uses, and attributes wall time to each phase of the boot separately.
//!
//! The realtime gate answers "how fast does Doom run"; it overwrites AUTOEXEC to
//! jump straight into the game, so POST, driver load, shell start and idle never
//! happen. This answers the complementary question -- where does the wall time
//! go before and around the game -- by cutting one run into five phases:
//!
//!   post      machine start        -> first INT 19h        (host-placed)
//!   boot      first INT 19h        -> end of AUTOEXEC.BAT  (guest MARK 1)
//!   idle      end of AUTOEXEC.BAT  -> idle window elapsed  (host-placed)
//!   exec      idle window elapsed  -> LOADTEST.COM entry   (guest MARK 3)
//!   diskload  LOADTEST.COM entry   -> file read to EOF     (guest MARK 4)
//!
//! `idle` is real COMMAND.COM prompt idle with no guest code of ours running in
//! it, and `exec` is COMMAND.COM parsing a command line and loading an image off
//! Katea. Both are workloads users feel and neither has ever been measured here.
//!
//! # `boot` is SUPPOSED to be slow. Do not optimize it away.
//!
//! Essentially all of `boot` is the FreeDOS kernel's F5/F8 window: the pause
//! that lets a user press F5 to skip CONFIG.SYS or F8 to single-step it. It
//! looks exactly like a busy-wait in every profile because it IS a wait, on
//! purpose, polling the keyboard so it can notice the key. Measured at 586:
//! `SWITCHES=/F` (which skips the window) takes the phase from 272,000,352
//! instructions and 8.324 s of wall to 1,567,521 and 0.050 s, and guest time
//! with it, from 1.970 s to 0.035 s. So the guest is doing no work there, and
//! anything else this phase measures is noise beside it.
//!
//! Deleting the wait is a product decision, not a performance one: `SWITCHES=/F`
//! and `/N` both take the escape hatch away from the user in practice.
//!
//! The real defect is the WINDOW'S UNIT, not the wait. The kernel sets
//! `SkipConfigSeconds db 2` (`kernel.asm:67`), i.e. two GUEST seconds -- but a
//! human waits in WALL seconds, and the two diverge by exactly this emulator's
//! interpretation overhead: 1.77 s of wall at 486 against 8.32 s at 586. The
//! pause therefore grows the FASTER the emulated machine is, which is backwards,
//! and is why 586 feels broken here while 486 feels fine. The byte cannot fix
//! it -- whole seconds only, and any value that reads well on one persona reads
//! badly on the other. What would is making the wait cheap, so guest time tracks
//! real time through it: FreeDOS's `GetBiosKey` spins on INT 16h where the same
//! `sti; hlt` its own `dosidle.asm` uses for `IDLEHALT` would serve. That is a
//! kernel source patch (this project is licensed and set up for them), gated on
//! an Open Watcom kernel rebuild plus a `tokados-hdd.img` regen.
//!
//! This is a profiler, NOT an A/B ladder. It runs once per persona, has no
//! pairing, ordering or lock discipline, and its slicing perturbs the run loop.
//! Never build an acceptance decision on it -- that is the realtime gate's job.

use izarravm_core::{GswMode, HardwareProfile, MASTER_CLOCK_HZ};
use izarravm_machine::{
    KateaStorageCounters, Machine, MachineProfile, PhaseMark, StopReason, phase_mark,
};
use serde_json::json;
use std::error::Error;
use std::path::Path;

/// Guest milliseconds per run slice. The host re-enters the run loop this often
/// so it can notice a phase mark and act on it. It is also much closer to the
/// GUI's shape (which paces in small slices) than one long run call would be.
const SLICE_MILLIS: u64 = 5;

/// Guest-time ceiling for reaching the end of AUTOEXEC.BAT. Generous: a cold
/// boot with TOKAEMM and TOKACD is seconds of guest time, not minutes.
const BOOT_CAP_SECONDS: u64 = 180;

/// Guest-time ceiling for the exec + disk-load phases once keys are injected.
const LOAD_CAP_SECONDS: u64 = 60;

/// In-memory files overlaid onto the mounted C:, as Katea's mount API takes
/// them: `(8.3 name, bytes)`.
type SystemOverrides = Vec<(String, Vec<u8>)>;

/// Phase ids handed to the RIP sampler, which tags each sample with the phase
/// that was live when it fired.
const RIP_PHASE_POST: u32 = 1;
const RIP_PHASE_BOOT: u32 = 2;
const RIP_PHASE_IDLE: u32 = 3;
const RIP_PHASE_EXEC: u32 = 4;
const RIP_PHASE_DISKLOAD: u32 = 5;

/// One phase's slice of the run, as differences between its two boundary marks.
#[derive(Debug, Clone)]
struct PhaseRow {
    name: &'static str,
    reached: bool,
    wall_seconds: f64,
    guest_seconds: f64,
    instructions: u64,
    direct_native_insns: u64,
    katea: KateaStorageCounters,
    machine_phases: Vec<(String, u64, u64)>,
    /// This phase's slice of the sampled CPU census. `None` unless
    /// `IZARRAVM_CPU_PROFILE` armed it.
    census: Option<PhaseCensus>,
}

/// One phase's slice of the sampled CPU census: the difference between the two
/// boundary snapshots.
///
/// Exact in all three tables. The group and opcode buckets difference exactly
/// because a bucket present at the later mark either existed at the earlier one
/// or started from zero, and the address table differences exactly because
/// `hot_addrs` is the complete map rather than a truncated head.
///
/// This is the gap the harness shipped with. Its own header called the
/// whole-run census "honest enough because idle dominates the instruction
/// count", and flagged that a census read as "the idle loop" is an inference.
/// With boot at 272 M instructions against idle's 1.44 G that inference was
/// already thin, and with the kernel's idle halt on it is worthless. This
/// replaces it with a measurement.
#[derive(Debug, Clone, Default)]
struct PhaseCensus {
    stride: u64,
    /// `(group, instructions, guest clocks, sample ns)`
    groups: Vec<(&'static str, u64, u64, u64)>,
    /// `(opcode, group, instructions, sample ns)`
    opcodes: Vec<(u16, &'static str, u64, u64)>,
    /// `(linear, samples)`, descending.
    addrs: Vec<(u32, u64)>,
}

impl PhaseCensus {
    fn instructions(&self) -> u64 {
        self.groups.iter().map(|row| row.1).sum()
    }
}

/// Difference two census snapshots into one phase. `None` when either boundary
/// carries none, which is every run that did not arm `IZARRAVM_CPU_PROFILE`.
fn census_delta(
    before: Option<&izarravm_cpu::CpuProfileSnapshot>,
    after: Option<&izarravm_cpu::CpuProfileSnapshot>,
) -> Option<PhaseCensus> {
    let (before, after) = (before?, after?);
    let groups = after
        .groups
        .iter()
        .map(|now| {
            let then = before.groups.iter().find(|old| old.name == now.name);
            (
                now.name,
                now.instructions
                    .saturating_sub(then.map_or(0, |old| old.instructions)),
                now.guest_core_clocks
                    .saturating_sub(then.map_or(0, |old| old.guest_core_clocks)),
                now.sample_wall_ns
                    .saturating_sub(then.map_or(0, |old| old.sample_wall_ns)),
            )
        })
        .collect();
    let opcodes = after
        .opcodes
        .iter()
        .map(|now| {
            let then = before.opcodes.iter().find(|old| old.opcode == now.opcode);
            (
                now.opcode,
                now.group,
                now.instructions
                    .saturating_sub(then.map_or(0, |old| old.instructions)),
                now.sample_wall_ns
                    .saturating_sub(then.map_or(0, |old| old.sample_wall_ns)),
            )
        })
        .filter(|row| row.2 > 0)
        .collect();
    let earlier: std::collections::HashMap<u32, u64> = before.hot_addrs.iter().copied().collect();
    let mut addrs: Vec<(u32, u64)> = after
        .hot_addrs
        .iter()
        .map(|&(lin, samples)| {
            (
                lin,
                samples.saturating_sub(earlier.get(&lin).copied().unwrap_or(0)),
            )
        })
        .filter(|&(_, samples)| samples > 0)
        .collect();
    addrs.sort_by_key(|&(lin, samples)| (std::cmp::Reverse(samples), lin));
    Some(PhaseCensus {
        stride: after.sample_stride,
        groups,
        opcodes,
        addrs,
    })
}

impl PhaseRow {
    /// Guest seconds per wall second: 1.0 is real time, 0.24 is the sluggish
    /// prompt the owner reported.
    fn real_time_factor(&self) -> f64 {
        if self.wall_seconds <= 0.0 {
            return 0.0;
        }
        self.guest_seconds / self.wall_seconds
    }

    /// Share of this phase's instructions that ran as native code.
    fn native_coverage(&self) -> f64 {
        if self.instructions == 0 {
            return 0.0;
        }
        self.direct_native_insns as f64 / self.instructions as f64
    }
}

/// How the run ended, and what it managed to measure before that.
struct BootProfileRun {
    mode: GswMode,
    rows: Vec<PhaseRow>,
    stop: StopReason,
    load_target: Option<String>,
    /// Set when the injected keystrokes never produced a LOADTEST entry mark.
    /// The disk phases fail soft: everything before them still reports.
    keystroke_injection_failed: bool,
    screen: String,
}

/// Run the whole boot profile for one persona.
pub fn run(
    dir: &Path,
    hardware: &HardwareProfile,
    load_file: Option<&str>,
    idle_seconds: u64,
    profile_json: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let mode = hardware.cpu;
    let clock = mode.clock_rate();
    let slice_clocks = clock
        .clocks_for_fraction_floor(SLICE_MILLIS, 1000)
        .max(1_000);

    // Pick the disk-load target before booting, so a bad pick fails before the
    // expensive part rather than after it.
    let load_target = match load_file {
        Some(explicit) => Some(explicit.to_string()),
        None => auto_pick_load_target(dir)?,
    };

    let overrides = build_overrides(dir, load_target.as_deref())?;
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_hdd_folder_with_user_overrides(dir, overrides)?;
    machine.enable_phase_marks();

    // The same optional instruments --hdd-folder honours, so a phase table, a
    // guest census and a RIP profile can all be collected in one run.
    //
    // The census is whole-run, not per-phase: it is a sampled histogram with no
    // boundary support. That is honest enough here because idle dominates the
    // instruction count, but a census read as "the idle loop" is an inference,
    // not a measurement.
    let cpu_profile_stride = std::env::var("IZARRAVM_CPU_PROFILE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    // "" | "0" count as unset on both variables: a pwsh harness that assigns
    // an empty string intending "cleared" leaves the variable set, and a bare
    // is_some()/map() would arm the instrument (measured 2026-08-15).
    let machine_profile =
        crate::machine_profile_requested(std::env::var("IZARRAVM_MACHINE_PROFILE").ok().as_deref());
    if let Some(stride) = cpu_profile_stride {
        machine.enable_host_profiling(stride);
    } else if machine_profile {
        machine.enable_machine_profiling();
    }
    #[cfg(windows)]
    let rip_sampler = crate::rip_profile_path().map(|path| {
        let sampler = crate::riprofile::Sampler::start();
        (sampler, path)
    });

    let start_wall = std::time::Instant::now();
    machine.record_host_phase_mark(phase_mark::RUN_START);
    set_rip_phase(RIP_PHASE_POST);

    // Phases 1 and 2: POST, then boot, ending when AUTOEXEC.BAT runs MARK 1.
    // POST_END lands inside this window; the sampler phase follows it.
    let mut stop = StopReason::CycleLimit { requested: 0 };
    let boot_cap = clock.clocks_for_fraction_floor(BOOT_CAP_SECONDS, 1);
    let mut spent = 0u64;
    let mut post_seen = false;
    while spent < boot_cap && !has_mark(&machine, phase_mark::BOOT_END) {
        let reason = machine.run_cycles(slice_clocks)?;
        spent = spent.saturating_add(slice_clocks);
        if !post_seen && has_mark(&machine, phase_mark::POST_END) {
            post_seen = true;
            set_rip_phase(RIP_PHASE_BOOT);
        }
        if let Some(terminal) = terminal_stop(&reason) {
            stop = terminal;
            break;
        }
    }

    let reached_boot = has_mark(&machine, phase_mark::BOOT_END);
    let mut keystroke_injection_failed = false;

    if reached_boot {
        // Phase 3: real COMMAND.COM prompt idle. Host-timed, because the point
        // is to measure the shell's own loop with nothing of ours running in it.
        set_rip_phase(RIP_PHASE_IDLE);
        let idle_clocks = clock.clocks_for_fraction_floor(idle_seconds, 1);
        let mut idled = 0u64;
        while idled < idle_clocks {
            let reason = machine.run_cycles(slice_clocks)?;
            idled = idled.saturating_add(slice_clocks);
            if let Some(terminal) = terminal_stop(&reason) {
                stop = terminal;
                break;
            }
        }
        machine.record_host_phase_mark(phase_mark::IDLE_END);

        // Phases 4 and 5: type the command, then let it read. Loading the image
        // off Katea is itself the "load a program off the hard drive" case.
        if let Some(target) = load_target.as_deref()
            && !is_terminal(&stop)
        {
            set_rip_phase(RIP_PHASE_EXEC);
            inject_command(&mut machine, &format!("C:\\LOADTEST.COM {target}"))?;
            let load_cap = clock.clocks_for_fraction_floor(LOAD_CAP_SECONDS, 1);
            let mut loaded = 0u64;
            let mut exec_seen = false;
            while loaded < load_cap && !has_mark(&machine, phase_mark::LOAD_END) {
                let reason = machine.run_cycles(slice_clocks)?;
                loaded = loaded.saturating_add(slice_clocks);
                if !exec_seen && has_mark(&machine, phase_mark::EXEC_END) {
                    exec_seen = true;
                    set_rip_phase(RIP_PHASE_DISKLOAD);
                }
                if let Some(terminal) = terminal_stop(&reason) {
                    stop = terminal;
                    break;
                }
            }
            // The documented soft failure: injected keys are the one part of
            // this harness that depends on the guest cooperating.
            keystroke_injection_failed = !exec_seen;
        }
    }

    let wall = start_wall.elapsed();
    #[cfg(windows)]
    if let Some((Some(mut sampler), path)) = rip_sampler {
        sampler.name_phases(&[
            (RIP_PHASE_POST, "post"),
            (RIP_PHASE_BOOT, "boot"),
            (RIP_PHASE_IDLE, "idle"),
            (RIP_PHASE_EXEC, "exec"),
            (RIP_PHASE_DISKLOAD, "diskload"),
        ]);
        sampler.stop_and_report(Path::new(&path));
    }

    if cpu_profile_stride.is_some() {
        crate::bench::print_cpu_profile(&machine.cpu().profile_snapshot());
    }
    // IZARRAVM_DIRECT_BARRIER_CENSUS=1 ranks what stopped block formation. The
    // `--hdd-folder` path already emitted this into its JSON; a boot profile
    // needs it too, because on a 16-bit workload the ranking IS the question --
    // `classify`'s Word allowlist was built for 32-bit game code, so what it
    // refuses here is a different population entirely.
    print_barrier_census(machine.cpu().direct_barrier_census_snapshot());
    let run = BootProfileRun {
        mode,
        rows: build_rows(machine.phase_marks()),
        stop,
        load_target,
        keystroke_injection_failed,
        screen: machine.screen_text().as_text(),
    };
    print_report(&run, wall, reached_boot, machine_profile);
    if let Some(path) = profile_json {
        write_json(path, &run, wall, dir)?;
    }
    // Exercise the same final validation and handle-flush path as normal shutdown.
    machine.flush_hdd_folder();
    if !reached_boot {
        return Err(format!(
            "boot profile never reached the end of AUTOEXEC.BAT within {BOOT_CAP_SECONDS} \
             guest seconds (stop={:?}); the screen text above shows how far it got",
            run.stop
        )
        .into());
    }
    Ok(())
}

const CENSUS_GROUP_ROWS: usize = 6;
const CENSUS_OPCODE_ROWS: usize = 10;
const CENSUS_ADDR_ROWS: usize = 12;

/// Print the census one phase at a time. Every share is within its own phase,
/// so a row reads as "this much of THIS phase" and never has to be divided back
/// out of a whole-run total.
fn print_phase_census(rows: &[PhaseRow]) {
    if rows.iter().all(|row| row.census.is_none()) {
        return;
    }
    println!();
    println!("=== per-phase cpu census ===");
    println!(
        "shares are within the phase. the census forces interpretation, so a \
         phase's native% here is 0 by construction;"
    );
    println!("read it against the native% in the table above, not instead of it.");
    for row in rows {
        let Some(census) = &row.census else { continue };
        let instructions = census.instructions();
        if !row.reached || instructions == 0 {
            continue;
        }
        let sample_ns: u64 = census.groups.iter().map(|g| g.3).sum::<u64>().max(1);
        println!();
        println!(
            "--- {} : {instructions} instructions, sample stride {} ---",
            row.name, census.stride
        );

        let mut groups = census.groups.clone();
        groups.sort_by_key(|g| std::cmp::Reverse(g.3));
        println!(
            "  {:<14} {:>13} {:>8} {:>12} {:>8}",
            "group", "instr", "instr%", "sample_ms", "sample%"
        );
        for g in groups.iter().filter(|g| g.1 > 0).take(CENSUS_GROUP_ROWS) {
            println!(
                "  {:<14} {:>13} {:>7.2}% {:>12.3} {:>7.2}%",
                g.0,
                g.1,
                100.0 * g.1 as f64 / instructions as f64,
                g.3 as f64 / 1_000_000.0,
                100.0 * g.3 as f64 / sample_ns as f64,
            );
        }

        let mut opcodes = census.opcodes.clone();
        opcodes.sort_by_key(|o| std::cmp::Reverse((o.3, o.2)));
        println!(
            "  {:<8} {:<14} {:>13} {:>8} {:>12}",
            "opcode", "group", "instr", "instr%", "sample_ms"
        );
        for o in opcodes.iter().take(CENSUS_OPCODE_ROWS) {
            println!(
                "  {:<8} {:<14} {:>13} {:>7.2}% {:>12.3}",
                format_census_opcode(o.0),
                o.1,
                o.2,
                100.0 * o.2 as f64 / instructions as f64,
                o.3 as f64 / 1_000_000.0,
            );
        }

        let addr_total: u64 = census.addrs.iter().map(|&(_, n)| n).sum::<u64>().max(1);
        println!(
            "  {:<10} {:>9} {:>8}  region",
            "linear", "samples", "phase%"
        );
        for &(lin, samples) in census.addrs.iter().take(CENSUS_ADDR_ROWS) {
            println!(
                "  {lin:08X}   {samples:>9} {:>7.2}%  {}",
                100.0 * samples as f64 / addr_total as f64,
                address_region(lin),
            );
        }
    }
}

/// Which side of the Direct JIT's admission gates an address sits on.
/// `key_for_phys` refuses 0xA0000..0x100000 outright, so labelling it here is
/// what makes an address row legible against the coverage column beside it --
/// a hot row in a refused region can never be helped by block admission.
fn address_region(lin: u32) -> &'static str {
    match lin {
        0x000f_0000..0x0010_0000 => "BIOS ROM (JIT-refused)",
        0x000c_0000..0x000f_0000 => "option ROM (JIT-refused)",
        0x000a_0000..0x000c_0000 => "VGA aperture (JIT-refused)",
        _ => "RAM",
    }
}

/// Rank the structural stops that refused block formation, when the opt-in
/// census collected any. Ranked by `runtime_hits` plus the two exit columns:
/// per [[barrier-census-mispredicts-both-ways]], `unbound_exits` alone is not a
/// ceiling and compile attempts (`hits`) must not drive prioritisation.
fn print_barrier_census(snapshot: Option<izarravm_cpu::DirectBarrierCensusSnapshot>) {
    let Some(snapshot) = snapshot else {
        return;
    };
    if snapshot.rows.is_empty() {
        println!();
        println!("=== direct barrier census: no structural stops recorded ===");
        return;
    }
    let mut rows = snapshot.rows;
    rows.sort_by_key(|row| {
        std::cmp::Reverse((
            row.runtime_hits,
            row.unbound_exits + row.dynamic_unbound_exits,
        ))
    });
    let total_runtime: u64 = rows.iter().map(|row| row.runtime_hits).sum::<u64>().max(1);
    println!();
    println!("=== direct barrier census (top {BARRIER_CENSUS_ROWS} by runtime_hits) ===");
    // `size` and `form` are carried because on a 16-bit workload they are the
    // discriminating columns: a Word row refused here is a candidate for the
    // allowlist, a Dword row is the pre-existing 32-bit population.
    println!(
        "{:<8} {:<20} {:<6} {:<8} {:>12} {:>7} {:>11} {:>11}",
        "opcode", "stop", "size", "form", "runtime", "run%", "unbound", "dyn_unbound"
    );
    for row in rows.iter().take(BARRIER_CENSUS_ROWS) {
        println!(
            "{:<8} {:<20} {:<6} {:<8} {:>12} {:>6.2}% {:>11} {:>11}",
            format_census_opcode(row.opcode),
            row.stop_reason,
            row.operand_size,
            row.operand_form,
            row.runtime_hits,
            100.0 * row.runtime_hits as f64 / total_runtime as f64,
            row.unbound_exits,
            row.dynamic_unbound_exits,
        );
    }
}

/// Same rendering `bench::print_cpu_profile` uses, so a census row and a census
/// opcode row can be read against each other without a mental conversion.
fn format_census_opcode(opcode: u16) -> String {
    if opcode & 0xff00 == 0x0f00 {
        format!("0F {:02X}", opcode as u8)
    } else {
        format!("{opcode:02X}")
    }
}

const BARRIER_CENSUS_ROWS: usize = 30;

/// Tag subsequent RIP samples with `phase`. A no-op off Windows, where the
/// sampler does not exist.
fn set_rip_phase(phase: u32) {
    #[cfg(windows)]
    crate::riprofile::set_phase(phase);
    #[cfg(not(windows))]
    let _ = phase;
}

fn has_mark(machine: &Machine, id: u8) -> bool {
    machine.phase_marks().iter().any(|mark| mark.id == id)
}

fn is_terminal(stop: &StopReason) -> bool {
    !matches!(stop, StopReason::CycleLimit { .. })
}

/// A slice's stop reason, if it ended the run rather than just the slice.
fn terminal_stop(reason: &StopReason) -> Option<StopReason> {
    match reason {
        StopReason::CycleLimit { .. } => None,
        other => Some(other.clone()),
    }
}

/// Type a command at the DOS prompt and press Enter.
fn inject_command(machine: &mut Machine, command: &str) -> Result<(), Box<dyn Error>> {
    // One short slice per keystroke: COMMAND.COM has to poll INT 16h and echo
    // the character before the next scancode pair arrives, or the type-ahead
    // buffer swallows keys and the command line comes out mangled.
    let per_key = machine
        .profile()
        .cpu
        .clock_rate()
        .clocks_for_fraction_floor(2, 1000)
        .max(1_000);
    for ch in command.chars().chain(std::iter::once('\r')) {
        for code in crate::ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
            machine.run_cycles(per_key)?;
        }
    }
    Ok(())
}

/// Turn the recorded boundaries into one row per phase. A phase whose closing
/// mark never fired reports `reached: false` rather than a wrong number.
fn build_rows(marks: &[PhaseMark]) -> Vec<PhaseRow> {
    const PHASES: [(&str, u8, u8); 5] = [
        ("post", phase_mark::RUN_START, phase_mark::POST_END),
        ("boot", phase_mark::POST_END, phase_mark::BOOT_END),
        ("idle", phase_mark::BOOT_END, phase_mark::IDLE_END),
        ("exec", phase_mark::IDLE_END, phase_mark::EXEC_END),
        ("diskload", phase_mark::EXEC_END, phase_mark::LOAD_END),
    ];
    let find = |id: u8| marks.iter().find(|mark| mark.id == id);
    PHASES
        .iter()
        .map(|&(name, from, to)| {
            let (Some(start), Some(end)) = (find(from), find(to)) else {
                return PhaseRow {
                    name,
                    reached: false,
                    wall_seconds: 0.0,
                    guest_seconds: 0.0,
                    instructions: 0,
                    direct_native_insns: 0,
                    katea: KateaStorageCounters::default(),
                    machine_phases: Vec::new(),
                    census: None,
                };
            };
            let katea = match (start.katea, end.katea) {
                (Some(before), Some(after)) => KateaStorageCounters {
                    sector_reads: after.sector_reads.saturating_sub(before.sector_reads),
                    host_file_reads: after.host_file_reads.saturating_sub(before.host_file_reads),
                    host_file_opens: after.host_file_opens.saturating_sub(before.host_file_opens),
                    host_bytes: after.host_bytes.saturating_sub(before.host_bytes),
                    host_read_operations: after
                        .host_read_operations
                        .saturating_sub(before.host_read_operations),
                    host_read_bytes: after.host_read_bytes.saturating_sub(before.host_read_bytes),
                    host_wall_ns: after.host_wall_ns.saturating_sub(before.host_wall_ns),
                    // A running max is a level, not an accumulator: subtracting
                    // two of them means nothing. Carried through as the
                    // session-to-date maximum, the same way the overlay
                    // occupancy levels below are.
                    host_read_max_ns: after.host_read_max_ns,
                    host_readahead_hits: after
                        .host_readahead_hits
                        .saturating_sub(before.host_readahead_hits),
                    host_readahead_fills: after
                        .host_readahead_fills
                        .saturating_sub(before.host_readahead_fills),
                    run_scan_steps: after.run_scan_steps.saturating_sub(before.run_scan_steps),
                    fat_sector_reads: after
                        .fat_sector_reads
                        .saturating_sub(before.fat_sector_reads),
                    dir_or_free_sector_reads: after
                        .dir_or_free_sector_reads
                        .saturating_sub(before.dir_or_free_sector_reads),
                    sector_writes: after.sector_writes.saturating_sub(before.sector_writes),
                    int13_read_commands: after
                        .int13_read_commands
                        .saturating_sub(before.int13_read_commands),
                    int13_read_sectors: after
                        .int13_read_sectors
                        .saturating_sub(before.int13_read_sectors),
                    int13_read_wait_ticks: after
                        .int13_read_wait_ticks
                        .saturating_sub(before.int13_read_wait_ticks),
                    int13_write_commands: after
                        .int13_write_commands
                        .saturating_sub(before.int13_write_commands),
                    int13_write_sectors: after
                        .int13_write_sectors
                        .saturating_sub(before.int13_write_sectors),
                    int13_write_wait_ticks: after
                        .int13_write_wait_ticks
                        .saturating_sub(before.int13_write_wait_ticks),
                    pio_read_commands: after
                        .pio_read_commands
                        .saturating_sub(before.pio_read_commands),
                    pio_read_sectors: after
                        .pio_read_sectors
                        .saturating_sub(before.pio_read_sectors),
                    pio_read_wait_ticks: after
                        .pio_read_wait_ticks
                        .saturating_sub(before.pio_read_wait_ticks),
                    pio_write_commands: after
                        .pio_write_commands
                        .saturating_sub(before.pio_write_commands),
                    pio_write_sectors: after
                        .pio_write_sectors
                        .saturating_sub(before.pio_write_sectors),
                    pio_write_wait_ticks: after
                        .pio_write_wait_ticks
                        .saturating_sub(before.pio_write_wait_ticks),
                    dma_read_commands: after
                        .dma_read_commands
                        .saturating_sub(before.dma_read_commands),
                    dma_read_sectors: after
                        .dma_read_sectors
                        .saturating_sub(before.dma_read_sectors),
                    dma_read_wait_ticks: after
                        .dma_read_wait_ticks
                        .saturating_sub(before.dma_read_wait_ticks),
                    dma_write_commands: after
                        .dma_write_commands
                        .saturating_sub(before.dma_write_commands),
                    dma_write_sectors: after
                        .dma_write_sectors
                        .saturating_sub(before.dma_write_sectors),
                    dma_write_wait_ticks: after
                        .dma_write_wait_ticks
                        .saturating_sub(before.dma_write_wait_ticks),
                    overlay_resident_sectors: after.overlay_resident_sectors,
                    overlay_pending_sectors: after.overlay_pending_sectors,
                    pending_unmapped_sectors: after.pending_unmapped_sectors,
                    // A gauge, like the occupancy levels above: how many entries
                    // the anti-clobber guard is holding right now, not how many
                    // it held during this phase.
                    blocked_projection_keys: after.blocked_projection_keys,
                    spill_operations: after
                        .spill_operations
                        .saturating_sub(before.spill_operations),
                    spill_bytes: after.spill_bytes.saturating_sub(before.spill_bytes),
                    spill_wall_ns: after.spill_wall_ns.saturating_sub(before.spill_wall_ns),
                    projection_operations: after
                        .projection_operations
                        .saturating_sub(before.projection_operations),
                    projection_bytes: after
                        .projection_bytes
                        .saturating_sub(before.projection_bytes),
                    projection_wall_ns: after
                        .projection_wall_ns
                        .saturating_sub(before.projection_wall_ns),
                    // A level, like `host_read_max_ns` above.
                    projection_max_ns: after.projection_max_ns,
                    metadata_projection_passes: after
                        .metadata_projection_passes
                        .saturating_sub(before.metadata_projection_passes),
                    host_write_failures: after
                        .host_write_failures
                        .saturating_sub(before.host_write_failures),
                },
                _ => KateaStorageCounters::default(),
            };
            let machine_phases = end
                .machine_phases
                .phases
                .iter()
                .map(|after| {
                    let before = start
                        .machine_phases
                        .phases
                        .iter()
                        .find(|p| p.name == after.name);
                    let (wall_ns, count) = match before {
                        Some(before) => (
                            after.wall_ns.saturating_sub(before.wall_ns),
                            after.count.saturating_sub(before.count),
                        ),
                        None => (after.wall_ns, after.count),
                    };
                    (after.name.to_string(), wall_ns, count)
                })
                .collect();
            PhaseRow {
                name,
                reached: true,
                wall_seconds: end.wall.duration_since(start.wall).as_secs_f64(),
                guest_seconds: end.master_ticks.saturating_sub(start.master_ticks) as f64
                    / MASTER_CLOCK_HZ as f64,
                instructions: end
                    .perf
                    .instructions
                    .saturating_sub(start.perf.instructions),
                direct_native_insns: end
                    .perf
                    .jit_direct_insns
                    .saturating_sub(start.perf.jit_direct_insns),
                katea,
                machine_phases,
                census: census_delta(start.cpu_profile.as_ref(), end.cpu_profile.as_ref()),
            }
        })
        .collect()
}

/// Read the folder's real AUTOEXEC.BAT and append the boot-done mark, then
/// overlay it along with the two guest helpers.
///
/// The host folder is never written. In user-folder mode the payload's own
/// AUTOEXEC.BAT is dropped (it is the user's), and an override that is not in
/// Katea's `DOS_FOLDER_BINARIES` list lands at the root with its 8.3 name
/// reserved, so this copy shadows the host file rather than colliding with it.
fn build_overrides(
    dir: &Path,
    load_target: Option<&str>,
) -> Result<SystemOverrides, Box<dyn Error>> {
    let host_autoexec = dir.join("AUTOEXEC.BAT");
    let mut text = match std::fs::read(&host_autoexec) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // No AUTOEXEC.BAT is legitimate: the mount seeds one, and the boot still
        // has a shell to reach. Start from empty and add only our own line.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(Box::new(err)),
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str("\r\n");
    }
    // `@` so the mark does not echo: this line runs inside the boot being
    // measured, and console output would put video and teletype work in it.
    text.push_str(&format!("@C:\\MARK.COM {}\r\n", phase_mark::BOOT_END));

    let mut overrides = vec![
        ("AUTOEXEC.BAT".to_string(), text.into_bytes()),
        (
            "MARK.COM".to_string(),
            izarravm_firmware::mark_com().to_vec(),
        ),
    ];
    if load_target.is_some() {
        overrides.push((
            "LOADTEST.COM".to_string(),
            izarravm_firmware::loadtest_com().to_vec(),
        ));
    }
    Ok(overrides)
}

/// The largest root-level host file whose name is already a valid 8.3, so the
/// name the guest sees is the name on disk. Katea mangles a colliding or
/// over-long name, and a mangled target would silently read the wrong file.
fn auto_pick_load_target(dir: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let mut best: Option<(u64, String)> = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_uppercase();
        if !is_plain_83(&name) {
            continue;
        }
        // Our own overlays shadow the host copies, so they are served from RAM
        // and would measure nothing.
        if matches!(name.as_str(), "AUTOEXEC.BAT" | "MARK.COM" | "LOADTEST.COM") {
            continue;
        }
        let len = entry.metadata()?.len();
        if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
            best = Some((len, name));
        }
    }
    Ok(best.map(|(_, name)| format!("C:\\{name}")))
}

/// Whether `name` is already a canonical 8.3 name needing no mangling.
fn is_plain_83(name: &str) -> bool {
    let ok = |part: &str, limit: usize| {
        !part.is_empty()
            && part.len() <= limit
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_^$~!#%&-{}()@'`".contains(c))
    };
    match name.split_once('.') {
        Some((stem, ext)) => ok(stem, 8) && ok(ext, 3) && !ext.contains('.'),
        None => ok(name, 8),
    }
}

fn print_report(run: &BootProfileRun, wall: std::time::Duration, reached_boot: bool, phases: bool) {
    println!();
    println!("=== boot phase profile: {} ===", run.mode.canonical_name());
    if let Some(target) = &run.load_target {
        println!("disk-load target: {target}");
    } else {
        println!("disk-load target: none found (pass --load-file to name one)");
    }
    println!("stop: {:?}", run.stop);
    println!("total wall: {:.3}s", wall.as_secs_f64());
    println!();
    println!(
        "{:<10} {:>9} {:>9} {:>8} {:>10} {:>9} {:>9} {:>10} {:>9}",
        "phase", "wall_s", "guest_s", "rt", "insns", "native%", "sectors", "hostreads", "host_ms"
    );
    for row in &run.rows {
        if !row.reached {
            println!("{:<10} {:>9}", row.name, "not reached");
            continue;
        }
        println!(
            "{:<10} {:>9.3} {:>9.3} {:>8.3} {:>10} {:>8.1}% {:>9} {:>9} {:>10.1}",
            row.name,
            row.wall_seconds,
            row.guest_seconds,
            row.real_time_factor(),
            row.instructions,
            row.native_coverage() * 100.0,
            row.katea.sector_reads,
            row.katea.host_file_reads,
            row.katea.host_wall_ns as f64 / 1_000_000.0,
        );
    }
    println!();
    println!("rt = guest seconds per wall second; 1.000 is real time.");

    // The run scan is only meaningful next to the sector count it multiplies, and
    // the host opens only next to the sectors they serve: both are ratios, and
    // both were one-per-sector before the read path was fixed.
    //
    // READ THE DENOMINATOR CAREFULLY. `sector_reads` counts sectors the facade
    // RESOLVES, and since the reconcile pass gained its cluster-chain memo a
    // chain walk served from that memo resolves none. `run_scan_steps` is bumped
    // only in `data_sector`, which a chain walk never reaches, so this ratio has
    // always been "run-table steps per DATA sector" over a denominator that also
    // counted FAT sectors -- and the memo shrank the denominator by 78-96% on the
    // measured rows without touching the numerator. It is a directional
    // diagnostic, not a quantity to compare across builds.
    for row in &run.rows {
        if row.reached && row.katea.sector_reads > 0 {
            println!(
                "{:<10} {:.1} run-table steps per sector served, {} bytes over {} host reads \
                 in {} opens",
                row.name,
                row.katea.run_scan_steps as f64 / row.katea.sector_reads as f64,
                row.katea.host_bytes,
                row.katea.host_file_reads,
                row.katea.host_file_opens,
            );
        }
    }

    if phases {
        println!();
        println!("machine phases per boot phase (wall ms / count):");
        for row in &run.rows {
            if !row.reached {
                continue;
            }
            let cells: Vec<String> = row
                .machine_phases
                .iter()
                .filter(|(_, wall_ns, _)| *wall_ns > 0)
                .map(|(name, wall_ns, count)| {
                    format!("{name} {:.1}/{count}", *wall_ns as f64 / 1_000_000.0)
                })
                .collect();
            println!("  {:<10} {}", row.name, cells.join("  "));
        }
    }

    print_phase_census(&run.rows);

    if run.keystroke_injection_failed {
        println!();
        println!(
            "NOTE: the injected keystrokes never reached LOADTEST.COM, so the exec and \
             diskload phases did not run. Everything above them is unaffected."
        );
    }
    if !reached_boot {
        println!();
        println!("--- screen at give-up ---");
        println!("{}", run.screen);
    }
}

fn write_json(
    path: &Path,
    run: &BootProfileRun,
    wall: std::time::Duration,
    workload: &Path,
) -> Result<(), Box<dyn Error>> {
    let report = json!({
        "schema": "izarravm-boot-phase-profile-v2",
        "workload": workload.display().to_string(),
        "mode": run.mode.canonical_name(),
        "total_wall_seconds": wall.as_secs_f64(),
        "stop": format!("{:?}", run.stop),
        "load_target": run.load_target,
        "keystroke_injection_failed": run.keystroke_injection_failed,
        "phases": run.rows.iter().map(|row| json!({
            "name": row.name,
            "reached": row.reached,
            "wall_seconds": row.wall_seconds,
            "guest_seconds": row.guest_seconds,
            "real_time_factor": row.real_time_factor(),
            "instructions": row.instructions,
            "direct_native_insns": row.direct_native_insns,
            "direct_native_coverage": row.native_coverage(),
            "katea": {
                "sector_reads": row.katea.sector_reads,
                "host_file_reads": row.katea.host_file_reads,
                "host_file_opens": row.katea.host_file_opens,
                "host_bytes": row.katea.host_bytes,
                "host_read_operations": row.katea.host_read_operations,
                "host_read_bytes": row.katea.host_read_bytes,
                "host_wall_ns": row.katea.host_wall_ns,
                "run_scan_steps": row.katea.run_scan_steps,
                "sector_writes": row.katea.sector_writes,
                "int13_read_commands": row.katea.int13_read_commands,
                "int13_read_sectors": row.katea.int13_read_sectors,
                "int13_read_wait_ticks": row.katea.int13_read_wait_ticks,
                "int13_write_commands": row.katea.int13_write_commands,
                "int13_write_sectors": row.katea.int13_write_sectors,
                "int13_write_wait_ticks": row.katea.int13_write_wait_ticks,
                "pio_read_commands": row.katea.pio_read_commands,
                "pio_read_sectors": row.katea.pio_read_sectors,
                "pio_read_wait_ticks": row.katea.pio_read_wait_ticks,
                "pio_write_commands": row.katea.pio_write_commands,
                "pio_write_sectors": row.katea.pio_write_sectors,
                "pio_write_wait_ticks": row.katea.pio_write_wait_ticks,
                "dma_read_commands": row.katea.dma_read_commands,
                "dma_read_sectors": row.katea.dma_read_sectors,
                "dma_read_wait_ticks": row.katea.dma_read_wait_ticks,
                "dma_write_commands": row.katea.dma_write_commands,
                "dma_write_sectors": row.katea.dma_write_sectors,
                "dma_write_wait_ticks": row.katea.dma_write_wait_ticks,
                "overlay_resident_sectors": row.katea.overlay_resident_sectors,
                "overlay_pending_sectors": row.katea.overlay_pending_sectors,
                "pending_unmapped_sectors": row.katea.pending_unmapped_sectors,
                "spill_operations": row.katea.spill_operations,
                "spill_bytes": row.katea.spill_bytes,
                "spill_wall_ns": row.katea.spill_wall_ns,
                "host_read_max_ns": row.katea.host_read_max_ns,
                "host_readahead_hits": row.katea.host_readahead_hits,
                "host_readahead_fills": row.katea.host_readahead_fills,
                "projection_max_ns": row.katea.projection_max_ns,
                "projection_operations": row.katea.projection_operations,
                "projection_bytes": row.katea.projection_bytes,
                "projection_wall_ns": row.katea.projection_wall_ns,
                "metadata_projection_passes": row.katea.metadata_projection_passes,
                "host_write_failures": row.katea.host_write_failures,
                "blocked_projection_keys": row.katea.blocked_projection_keys,
            },
            "machine_phases": row.machine_phases.iter().map(|(name, wall_ns, count)| json!({
                "name": name,
                "wall_ns": wall_ns,
                "count": count,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

#[cfg(test)]
#[path = "bootprofile_test.rs"]
mod tests;
