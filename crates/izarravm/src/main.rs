// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// The hand-built perf_counters_json object expands one json! recursion level
// per key; the counter set is large enough to need headroom above the default.
#![recursion_limit = "512"]

mod bench;
mod bench_reference;
mod bootprofile;
mod cmos;
mod controller_names;
mod controller_profiles;
mod crt;
mod display_transform;
mod gui;
mod host_input;
mod ipe_trace;
mod mode_census_json;
mod preflight;
mod prefs;
#[cfg(windows)]
mod riprofile;
mod screendump;
mod startup;

use clap::Parser;
use izarravm_core::{
    AppConfig, GswMode, HardwareProfile, MASTER_CLOCK_HZ, MidiBackend, SbDma8, SbDma16, SbIrq,
    VideoCard,
};
use izarravm_cpu::CpuProfileSnapshot;
use izarravm_firmware::{
    SuiteRecordStatus, SuiteResults, boot_test_image, neurketa_image, parse_result_block, test_rom,
};
use izarravm_machine::{
    ActiveDisplay, ExecutionBackend, Machine, MachineHostProfileSnapshot, MachineProfile,
    PerfCounters, StopReason, set_process_execution_backend,
};
use mode_census_json::mode_census_json;
use serde_json::json;
use startup::ResolvedStartup;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use tracing::info;

/// Default cycle budget for --headless-test-rom. Large enough that test386.bin
/// reaches its POST-0x03 fault out of the box; halting ROMs return at their HLT
/// well before this, and --cycles tunes it down for quick runs.
const DEFAULT_TEST_ROM_CYCLES: u64 = 200_000_000;

/// Default cycle budget for --headless-boot-floppy. Well past POST plus the boot
/// sector's early work; --cycles tunes it up for a longer investigation.
const DEFAULT_BOOT_FLOPPY_CYCLES: u64 = 50_000_000;

/// Default cycle budget for --headless-boot-hdd. A real DOS boot from the HDD
/// image (MBR -> VBR -> kernel -> CONFIG.SYS -> shell) needs much more headroom
/// than the bare floppy boot-sector run; --cycles tunes it for investigation.
const DEFAULT_BOOT_HDD_CYCLES: u64 = 500_000_000;
const CPU_OPCODE_PROFILE_PRINT_LIMIT: usize = 24;
/// How many sampled addresses `print_cpu_profile` shows. The snapshot itself
/// carries all of them, so the boot profiler can difference two snapshots into
/// one phase; this is presentation only.
const HOT_ADDR_PRINT_LIMIT: usize = 64;

#[derive(Debug, Parser)]
#[command(version, about = "IzarraVM emulator scaffold")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    cpu: Option<GswMode>,
    /// Run the portable CPU interpreter and disable native block admission.
    #[arg(long)]
    interpreter: bool,
    #[arg(long)]
    memory_mib: Option<u16>,
    #[arg(long)]
    video: Option<VideoCard>,
    #[arg(long)]
    c_drive: Option<PathBuf>,
    /// Keep the C: drive, cmos.bin, and izarravm.conf beside the executable
    /// instead of in the per-user <home>/.izarravm. For self-contained installs.
    #[arg(long)]
    portable: bool,
    /// Custom SF2 or SF3 bank for the FluidSynth wavetable at P300.
    #[arg(long)]
    soundfont: Option<PathBuf>,
    /// P330 receiver: off, external, or Munt.
    #[arg(long)]
    midi_backend: Option<MidiBackend>,
    /// P330 host MIDI destination. Duplicate names are selected with
    /// --midi-port-ordinal, starting at zero.
    #[arg(long)]
    midi_port: Option<String>,
    #[arg(long, requires = "midi_port")]
    midi_port_ordinal: Option<u16>,
    #[arg(long)]
    mt32_control_rom: Option<PathBuf>,
    #[arg(long)]
    mt32_pcm_rom: Option<PathBuf>,
    #[arg(long)]
    sb_irq: Option<SbIrq>,
    #[arg(long)]
    sb_dma: Option<SbDma8>,
    #[arg(long)]
    sb_high_dma: Option<SbDma16>,
    #[arg(long, group = "run_mode")]
    headless_config_check: bool,
    #[arg(long, group = "run_mode")]
    headless_test_rom: bool,
    #[arg(long, group = "run_mode")]
    headless_boot_suite: bool,
    /// Run the built-in calibration probes and the local Dhrystone and
    /// Whetstone executables at .bench/dhrystone.exe and .bench/whetstone.exe.
    #[arg(long, group = "run_mode")]
    headless_bench: bool,
    /// Run one supplied DOS EXE through the raw-program bench harness in GSW-586.
    #[arg(long, group = "run_mode")]
    headless_bench_exe: Option<PathBuf>,
    /// Run one supplied DOS EXE twice in GSW-586: baseline, then profiling buckets.
    #[arg(long, group = "run_mode")]
    headless_profile_exe: Option<PathBuf>,
    /// Write profiling output as pretty JSON. Supported by --headless-profile-exe and
    /// --hdd-folder. HDD machine-phase timing requires IZARRAVM_MACHINE_PROFILE or
    /// IZARRAVM_CPU_PROFILE. The parent directory must exist.
    #[arg(long)]
    profile_json: Option<PathBuf>,
    /// Sample every Nth instruction in --headless-profile-exe.
    #[arg(long, default_value_t = 1024)]
    profile_sample_stride: u64,
    #[arg(long, group = "run_mode")]
    headless_bandwidth: bool,
    #[arg(long, group = "run_mode")]
    headless_keyboard: bool,
    #[arg(long, group = "run_mode")]
    headless_izarra_bios: bool,
    #[arg(long, group = "run_mode")]
    headless_boot_floppy: Option<PathBuf>,
    #[arg(long, group = "run_mode")]
    headless_boot_hdd: Option<PathBuf>,
    /// Boot the Katea host-folder facade: mount the given directory as C: through
    /// the real FreeDOS system files, run the BIOS, and print the boot diagnostics.
    /// The folder's top-level files are surfaced read-only beside the OS.
    #[arg(long, group = "run_mode")]
    hdd_folder: Option<PathBuf>,
    /// With --hdd-folder, print a machine-readable result block after stop:
    /// stop reason, CS:IP, full register state, and the 80x25 text page. For
    /// headless benchmark/timedemo runs whose result lands in text mode.
    #[arg(long)]
    dump_result: bool,
    /// With --hdd-folder, write the final framebuffer to this PPM (P6) path.
    /// For headless benchmark/timedemo runs whose result lands in graphics mode.
    #[arg(long)]
    result_ppm: Option<PathBuf>,
    /// With --hdd-folder, write the last COMPLETED raster to this PPM (P6) path.
    ///
    /// Not the same picture as --result-ppm, and the difference is the point.
    /// `--result-ppm` re-renders the whole frame at stop-time register state,
    /// so it reports what video memory holds. This writes the frame the scanout
    /// actually published, which is what a user sees. A defect that fills video
    /// memory correctly but never publishes it is INVISIBLE to --result-ppm and
    /// plain in this one: measured on Psycho Pinball, the re-render read 80.8%
    /// non-black while the screen was black.
    ///
    /// Writes nothing and says so when no frame has completed yet -- between a
    /// mode set and the first raster of the new mode there is genuinely no
    /// published frame, and a substitute would read as a measurement.
    #[arg(long)]
    presented_ppm: Option<PathBuf>,
    /// With --hdd-folder, write the video mode census to this JSON path at the
    /// end of the run: every geometry the guest programmed and how many times.
    ///
    /// The key is the CRTC's own fields, not a presented pixel count, because
    /// 200 visible lines double scanned and 199 lines single scanned both
    /// present 400 raster lines and a pixel height cannot separate them.
    ///
    /// The COUNT is the part no other instrument reports. A guest that replays
    /// its CRTC register table every frame reads in the thousands where a guest
    /// that sets a mode once reads two or three, and that is the property a
    /// raster-erase defect hid behind for as long as it did.
    ///
    /// The counter itself always runs and costs nothing on the hot path, because
    /// a mode set is rare. Only the write is gated on this flag. Unlike
    /// --screen-dump-dir it does not slice the run, so a census run is still a
    /// benchmark path.
    #[arg(long, requires = "hdd_folder")]
    mode_census: Option<PathBuf>,
    /// With --hdd-folder, return an error unless the guest reaches Lotura TestExit code 0.
    #[arg(long, requires = "hdd_folder")]
    expect_test_exit: bool,
    /// With --hdd-folder, sample the screen every --screen-dump-interval-ms
    /// GUEST milliseconds into this directory, plus a `screens.jsonl` index.
    /// For headless runs nobody watches: the index says whether the picture is
    /// still changing, which is how a corpus sweep tells a running game from
    /// one parked on a menu. Default off. Slices the run, so it is a
    /// diagnostic, not a benchmark path.
    #[arg(long, requires = "hdd_folder")]
    screen_dump_dir: Option<PathBuf>,
    /// Guest milliseconds between screen samples.
    #[arg(long, requires = "screen_dump_dir", default_value_t = 5_000)]
    screen_dump_interval_ms: u64,
    /// With --hdd-folder, mount a CD image (ISO or CUE/BIN, the formats the GUI
    /// mount accepts) before boot. For fixtures whose game reads data, FMV or
    /// CD audio from the disc.
    #[arg(long, requires = "hdd_folder")]
    cd_image: Option<PathBuf>,
    /// With --hdd-folder, type keys at fixed guest-cycle offsets: `cycles:text`
    /// steps separated by `;`, offsets strictly increasing, `\r` for Enter. For
    /// games whose benchmark window sits behind a title screen or a menu. The
    /// schedule is deterministic, so an equal-work comparison still holds.
    #[arg(long, requires = "hdd_folder")]
    inject_keys: Option<String>,
    /// With --hdd-folder, drive the mouse at fixed guest-cycle offsets:
    /// `cycles:action` steps separated by `;`, offsets strictly increasing.
    /// Actions are `home`, `move:<dx>,<dy>`, `down`, `up` and `click`. Deltas are
    /// mickeys, not pixels (the INT 33h driver owns that ratio). For games whose
    /// menus are mouse-only, which no keystroke schedule can reach. Combine with
    /// --inject-keys freely; the two schedules merge by offset.
    #[arg(long, requires = "hdd_folder")]
    inject_mouse: Option<String>,
    /// Boot the C: drive exactly as the GUI does and attribute wall time per
    /// boot phase: POST, boot, prompt idle, command exec, and disk load. Uses
    /// the same folder the GUI would (see --c-drive). A profiler, not a ladder.
    #[arg(long, group = "run_mode")]
    headless_boot_profile: bool,
    /// With --headless-boot-profile, the guest path the disk-load phase reads
    /// (for example C:\UNISOUND.COM). Defaults to the largest root-level file.
    #[arg(long, requires = "headless_boot_profile")]
    load_file: Option<String>,
    /// With --headless-boot-profile, guest seconds to sit at the DOS prompt.
    #[arg(long, default_value_t = 10)]
    idle_seconds: u64,
    /// Boot real FreeDOS from a temp Katea disk and run a single DOS program,
    /// exiting with its DOS exit code (the Katea replacement for --headless-run).
    #[arg(long, group = "run_mode")]
    katea_run: Option<PathBuf>,
    #[arg(long)]
    stdin_text: Option<String>,
    #[arg(long)]
    bios: Option<PathBuf>,
    #[arg(long)]
    cycles: Option<u64>,
    #[arg(long)]
    margo_test_pattern: bool,
    /// Fallback C: root. Also read from `IZARRAVM_DOSROOT`, which is why the value parser
    /// accepts an EMPTY string: clap's stock `PathBuf` parser rejects one, and it rejects it
    /// the same way whether the empty value came from the command line or from the
    /// environment. A shell (or a campaign script) that leaves `IZARRAVM_DOSROOT=` set to an
    /// empty value therefore made EVERY invocation of the emulator fail with
    /// "a value is required for '--dosroot <DOSROOT>' but none was supplied", naming an
    /// argument the user never typed. An empty path is not a root, so it reads as absent
    /// instead; `resolve_with` drops it.
    #[arg(long, env = "IZARRAVM_DOSROOT", value_parser = empty_tolerant_path)]
    dosroot: Option<PathBuf>,
}

/// A `PathBuf` value parser that accepts the empty string. See the `dosroot` comment: the point
/// is that an empty ENVIRONMENT value must not be a hard parse error. Callers treat an empty
/// path as absent.
fn empty_tolerant_path(value: &str) -> Result<PathBuf, std::convert::Infallible> {
    Ok(PathBuf::from(value))
}

/// Arm the diagnostic guest-store watchpoint from `IZARRAVM_WATCH_WRITE=<hex>[,<hex len>]`
/// (PHYSICAL addresses, default length 0x100). Reports go to stderr, capped, naming the
/// store route and the guest CS:IP that issued it. Answers "who wrote here?" for a memory
/// corruption whose writer is not the obvious owner of the range -- the reason it exists is
/// a guest that scribbled PCM over the DOS INT 21h dispatch stub. Off unless the variable
/// is set, and the store path then pays one Relaxed load.
fn arm_write_watch_from_env() {
    let Some(spec) = std::env::var("IZARRAVM_WATCH_WRITE")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let mut parts = spec.split(',');
    let parse = |t: &str| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok();
    let Some(addr) = parts.next().and_then(parse) else {
        eprintln!("watch-write: could not parse IZARRAVM_WATCH_WRITE={spec}");
        return;
    };
    let len = parts.next().and_then(parse).unwrap_or(0x100);
    if let Some(limit) = std::env::var("IZARRAVM_WATCH_WRITE_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        izarravm_cpu::set_write_watch_limit(limit);
    }
    izarravm_cpu::set_write_watch(addr, len);
    eprintln!("watch-write: armed over {addr:#08x}..{:#08x}", addr + len);
}

fn requested_execution_backend(
    interpreter: bool,
    native_backend_compiled: bool,
    native_backend_available: bool,
) -> Result<ExecutionBackend, &'static str> {
    if interpreter || !native_backend_compiled {
        return Ok(ExecutionBackend::Interpreter);
    }
    if native_backend_available {
        Ok(ExecutionBackend::Automatic)
    } else {
        // Owner ruling, 2026-09-04: AVX2 is a hard requirement for this build
        // (`.cargo/config.toml` targets x86-64-v3), so `preflight::require_avx2`
        // already exited the process before `main` reached this point on a host
        // that would land here. This branch stays as defense in depth for a
        // native-compiled, non-x86_64/Windows/Linux target where the compiled-in
        // check differs from the runtime one -- it is not a reachable path on the
        // hosts this build ships for, and there is no `--interpreter` escape left
        // to point at.
        Err("this IzarraVM build requires an AVX2-capable x86-64 CPU")
    }
}

/// Locals-free shim. `require_avx2` must be the first thing this process
/// does, and it must run before `main`'s own prologue can execute a single
/// instruction that assumes the feature set it is about to check for --
/// see `preflight`'s module comment. That is a source-order property
/// ONLY if `main` has no locals of its own: a `main` with any local that
/// forces callee-save xmm spills gets a prologue of VEX `vmovapd` moves
/// before the first statement runs on a build compiled `-C
/// target-cpu=x86-64-v3`, which faults on exactly the non-AVX host this
/// check exists to catch. Keeping the real body in a separate
/// `#[inline(never)]` function is what keeps `main` itself empty of
/// locals; verified by disassembly (`dev_docs/2026-09-04-avx2-preflight-review.md`
/// F1), not assumed.
fn main() -> Result<(), Box<dyn Error>> {
    preflight::require_avx2();
    real_main()
}

#[inline(never)]
fn real_main() -> Result<(), Box<dyn Error>> {
    // R2.20(c)'s compare-loop microbenchmark: off unless
    // `IZARRAVM_REFLECTED_CALL_MEMO_BENCH` is set, in which case it prints to
    // stderr and does not otherwise affect this run. Gated under the feature
    // (Fable review 2026-09-03), same as the rest of the module.
    #[cfg(feature = "reflected-call-memo")]
    izarravm_cpu::reflected_call_memo::maybe_run_compare_bench();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "izarravm=info".into()),
        )
        .init();

    let cli = Cli::parse();
    arm_write_watch_from_env();
    let execution_backend = requested_execution_backend(
        cli.interpreter,
        izarravm_cpu::NATIVE_BACKEND_COMPILED,
        izarravm_cpu::native_backend_available(),
    )?;
    set_process_execution_backend(execution_backend);
    if cli.profile_json.is_some()
        && cli.headless_profile_exe.is_none()
        && cli.hdd_folder.is_none()
        && !cli.headless_boot_profile
    {
        return Err(
            "--profile-json requires --headless-profile-exe, --hdd-folder, or \
             --headless-boot-profile"
                .into(),
        );
    }
    let startup = ResolvedStartup::from_cli(&cli)?;
    let hardware = startup.hardware();

    if cli.headless_config_check {
        return Ok(());
    }

    // Each headless mode that builds a Machine runs in its own function. A Machine
    // is a large value (CPU, VGA, Margo, audio chips inline); keeping all three
    // branches inline gave main a ~1.2 MB stack frame that overflowed on the
    // prologue, before clap could even print --help/--version. One Machine per
    // frame keeps every path well under the thread stack limit.
    if cli.headless_boot_suite {
        return run_boot_suite(hardware);
    }

    if cli.headless_bench {
        return bench::run_bench(hardware);
    }

    if let Some(path) = &cli.headless_bench_exe {
        return bench::run_bench_exe(path, hardware);
    }

    if let Some(path) = &cli.headless_profile_exe {
        return bench::run_profile_exe(
            path,
            cli.profile_json.as_deref(),
            cli.profile_sample_stride,
            hardware,
        );
    }

    if cli.headless_bandwidth {
        return bench::run_bandwidth(hardware);
    }

    if cli.headless_test_rom {
        return run_test_rom(cli.bios.as_deref(), cli.cycles, hardware);
    }

    if cli.headless_keyboard {
        return run_keyboard_demo(hardware, cli.stdin_text.as_deref());
    }

    if cli.headless_izarra_bios {
        return run_izarra_bios();
    }

    if let Some(path) = &cli.headless_boot_floppy {
        return run_boot_floppy(path, cli.cycles, hardware);
    }

    if let Some(path) = &cli.headless_boot_hdd {
        return run_boot_hdd(path, cli.cycles, hardware);
    }

    if let Some(dir) = &cli.hdd_folder {
        let glide_ovl = startup.load_global_glide_ovl();
        return run_boot_hdd_folder(
            dir,
            glide_ovl,
            cli.cycles,
            hardware,
            cli.dump_result,
            cli.result_ppm.as_deref(),
            cli.presented_ppm.as_deref(),
            cli.mode_census.as_deref(),
            cli.profile_json.as_deref(),
            cli.expect_test_exit,
            cli.inject_keys.as_deref(),
            cli.inject_mouse.as_deref(),
            cli.cd_image.as_deref(),
            cli.screen_dump_dir
                .as_deref()
                .map(|dir| (dir, cli.screen_dump_interval_ms)),
        );
    }

    if cli.headless_boot_profile {
        if let Some(path) = cli.profile_json.as_deref() {
            validate_profile_json_parent(path)?;
        }
        return bootprofile::run(
            startup.c_drive(),
            hardware,
            cli.load_file.as_deref(),
            cli.idle_seconds,
            cli.profile_json.as_deref(),
        );
    }

    if let Some(prog) = &cli.katea_run {
        let code = katea_run(prog, MachineProfile::from_hardware_profile(hardware))?;
        std::process::exit(code);
    }

    let rom = match cli.bios.as_deref() {
        Some(path) => std::fs::read(path)?,
        None => izarravm_firmware::izarra_bios().to_vec(),
    };
    gui::run(startup.into_gui(rom, cli.margo_test_pattern))?;
    Ok(())
}

/// Run the clean-room boot suite and print its result block.
fn run_boot_suite(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new_boot_image(
        MachineProfile::from_hardware_profile(hardware),
        boot_test_image(),
    )?;
    maybe_enable_unit_sim(&mut machine);
    // The suite is wall-time-bound (PIT ticks and device-settle delays), so the
    // cycle budget scales with the clock to cover the same span at any GSW mode.
    // Half a second covers the timer probe and the 453-byte report at 38400 baud.
    let budget = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 2);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    // Report the result block, which holds the runtime outcome (the timer test
    // patches its record here). The serial dump is an earlier static snapshot.
    let results = parse_result_block(machine.memory().as_slice())?;
    for record in &results.records {
        let status = match record.status {
            SuiteRecordStatus::Begin => "BEGIN",
            SuiteRecordStatus::Pass => "PASS",
            SuiteRecordStatus::Fail => "FAIL",
            SuiteRecordStatus::Measure => "MEASURE",
        };
        match &record.value {
            Some(value) => println!("{status} {} {value}", record.name),
            None => println!("{status} {}", record.name),
        }
    }
    println!("records: {}", results.records.len());
    println!("stop: {stop_reason:?}");
    print_com1(&machine.serial_text());
    maybe_report_unit_sim(&mut machine);
    if let Some(message) = boot_suite_failure_summary(&results) {
        return Err(message.into());
    }
    Ok(())
}

fn boot_suite_failure_summary(results: &SuiteResults) -> Option<String> {
    let failures: Vec<&str> = results
        .records
        .iter()
        .filter(|record| record.status == SuiteRecordStatus::Fail)
        .map(|record| record.name.as_str())
        .collect();
    if failures.is_empty() {
        None
    } else {
        Some(format!("boot suite reported FAIL: {}", failures.join(", ")))
    }
}

/// Boot a BIOS/test ROM headless and print the screen text plus POST code.
fn run_test_rom(
    bios: Option<&Path>,
    cycles: Option<u64>,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    let rom = select_rom(bios)?;
    let mut machine = Machine::new(MachineProfile::from_hardware_profile(hardware), &rom)?;
    maybe_enable_unit_sim(&mut machine);
    let budget = cycles.unwrap_or(DEFAULT_TEST_ROM_CYCLES);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    let screen = machine.screen_text();
    let screen_text = screen.as_text();
    info!(
        ?stop_reason,
        clocks = machine.elapsed_clocks(),
        bus_cycles = machine.bus_trace().cycles().len(),
        first_line = %screen.line_string(0),
        "test ROM completed"
    );
    println!("{screen_text}");
    print_com1(&machine.serial_text());
    println!("post: {:#04x}", machine.io_port(0x80).unwrap_or(0));
    println!("stop: {stop_reason:?}");
    maybe_report_unit_sim(&mut machine);
    Ok(())
}

/// Boot the keyboard ROM, type --stdin-text into it, and print the screen.
fn run_keyboard_demo(
    hardware: &HardwareProfile,
    stdin_text: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    use izarravm_firmware::kbd_bios;
    let mut machine = Machine::new(MachineProfile::from_hardware_profile(hardware), kbd_bios())?;
    machine.run_until_halt_or_cycles(200_000)?;
    for ch in stdin_text.unwrap_or("").chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
            machine.run_until_halt_or_cycles(200_000)?;
        }
    }
    println!("{}", machine.screen_text().as_text());
    Ok(())
}

/// Boot the Izarra3000 BIOS headless, run POST to halt, print the VDTS records.
/// Its own function because a Machine is a large inline value (combining the
/// headless paths overflows main's stack frame).
fn run_izarra_bios() -> Result<(), Box<dyn Error>> {
    let hardware = HardwareProfile::from_config(&AppConfig::default())?;
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(&hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    // Exercise the complete Izarra storage profile while leaving every medium
    // unbootable, so POST can probe the ATA device and INT 19h still reaches its
    // deterministic terminal halt.
    machine.mount_hdd(vec![0; 512]);
    // The graphical POST blit and RAM sweep need more than the old 200 ms budget.
    let budget = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 1);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    let results = parse_result_block(machine.memory().as_slice())?;
    for record in &results.records {
        let status = match record.status {
            SuiteRecordStatus::Begin => "BEGIN",
            SuiteRecordStatus::Pass => "PASS",
            SuiteRecordStatus::Fail => "FAIL",
            SuiteRecordStatus::Measure => "MEASURE",
        };
        match &record.value {
            Some(value) => println!("{status} {} {value}", record.name),
            None => println!("{status} {}", record.name),
        }
    }
    println!("records: {}", results.records.len());
    println!("declared: {}", results.declared_record_count);
    println!("stop: {stop_reason:?}");
    if stop_reason != StopReason::Halted {
        return Err(format!("Izarra BIOS did not reach its terminal halt: {stop_reason:?}").into());
    }
    if let Some(message) = izarra_bios_failure_summary(&results) {
        return Err(message.into());
    }
    Ok(())
}

const IZARRA_BIOS_REQUIRED_RECORDS: &[(&str, SuiteRecordStatus)] = &[
    ("suite.izarra", SuiteRecordStatus::Begin),
    ("self.framework", SuiteRecordStatus::Pass),
    ("self.extaccess", SuiteRecordStatus::Pass),
    ("component.cpu_gsw", SuiteRecordStatus::Pass),
    ("component.video_margo", SuiteRecordStatus::Pass),
    ("video.margo_caps", SuiteRecordStatus::Measure),
    ("memory.ramtest", SuiteRecordStatus::Pass),
    ("memory.detected_kib", SuiteRecordStatus::Measure),
    ("component.cpu_lotura", SuiteRecordStatus::Pass),
    ("cpu.gsw_mode", SuiteRecordStatus::Measure),
    ("component.kbd_8042", SuiteRecordStatus::Pass),
    ("component.timer_pit", SuiteRecordStatus::Pass),
    ("component.serial_com1", SuiteRecordStatus::Pass),
    ("component.audio_sbdsp", SuiteRecordStatus::Pass),
    ("sound.dsp_version", SuiteRecordStatus::Measure),
    ("component.audio_opl", SuiteRecordStatus::Pass),
    ("component.floppy_fdc", SuiteRecordStatus::Pass),
    ("component.disk_hdd", SuiteRecordStatus::Pass),
    ("component.optical_atapi", SuiteRecordStatus::Pass),
];

fn izarra_bios_failure_summary(results: &SuiteResults) -> Option<String> {
    let mut issues = Vec::new();
    if results.version != 1 {
        issues.push(format!("unsupported result version {}", results.version));
    }
    if usize::from(results.declared_record_count) != results.records.len() {
        issues.push(format!(
            "declared {} records but parsed {}",
            results.declared_record_count,
            results.records.len()
        ));
    }
    if results.records.len() != IZARRA_BIOS_REQUIRED_RECORDS.len() {
        issues.push(format!(
            "expected {} required records but parsed {}",
            IZARRA_BIOS_REQUIRED_RECORDS.len(),
            results.records.len()
        ));
    }

    let mut names = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for record in &results.records {
        if !names.insert(record.name.as_str()) {
            duplicates.insert(record.name.as_str());
        }
    }
    if !duplicates.is_empty() {
        issues.push(format!(
            "duplicate records: {}",
            duplicates.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let required_names: BTreeSet<&str> = IZARRA_BIOS_REQUIRED_RECORDS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let unexpected: Vec<&str> = names.difference(&required_names).copied().collect();
    if !unexpected.is_empty() {
        issues.push(format!("unexpected records: {}", unexpected.join(", ")));
    }

    for &(name, expected_status) in IZARRA_BIOS_REQUIRED_RECORDS {
        let Some(record) = results.records.iter().find(|record| record.name == name) else {
            issues.push(format!("missing required record: {name}"));
            continue;
        };
        if record.status == SuiteRecordStatus::Fail {
            issues.push(format!("failed required record: {name}"));
        } else if record.status != expected_status {
            issues.push(format!(
                "required record {name} has {:?}, expected {expected_status:?}",
                record.status
            ));
        }
        if expected_status == SuiteRecordStatus::Measure
            && record.value.as_deref().is_none_or(str::is_empty)
        {
            issues.push(format!("required measurement has no value: {name}"));
        }
    }

    (!issues.is_empty()).then(|| format!("Izarra BIOS gate failed: {}", issues.join("; ")))
}

/// Mount a floppy IMG, run the Izarra BIOS so INT 19h bootstraps it, and print
/// CS:IP plus a short trace of low memory. A human reads the trace to confirm the
/// boot sector executed: CS:IP leaving the BIOS region (CS far below 0xF000) and
/// the boot sector bytes sitting at 0000:7C00 mean INT 19h loaded and jumped.
fn run_boot_floppy(
    path: &Path,
    cycles: Option<u64>,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    let image = std::fs::read(path)?;
    let image_len = image.len();
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_floppy(image).map_err(|message| {
        format!(
            "cannot mount {} ({image_len} bytes): {message}",
            path.display()
        )
    })?;
    // The bootstrap runs after POST, which is wall-time bound, so the default budget
    // sits well past POST plus the boot sector's own early work. A long headless
    // investigation passes --cycles to run further, so honor it when given.
    let budget = cycles.unwrap_or(DEFAULT_BOOT_FLOPPY_CYCLES);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    // Diff-trace prototype (IZARRAVM_DIFF_TRACE): flush the buffered trace writer now
    // that the run loop returned, or its last partial buffer's worth of lines -- most
    // often exactly the tail we care about -- is silently lost at process exit.
    izarravm_cpu::flush_diff_trace();

    let cs = machine.cpu().registers.cs().selector;
    let ip = machine.cpu().registers.eip as u16;
    println!("image: {} ({image_len} bytes)", path.display());
    println!("stop: {stop_reason:?}");
    println!("CS:IP = {cs:04X}:{ip:04X}");
    // The first bytes of the loaded boot sector and where the CPU landed. A boot
    // sector that ran leaves CS below the BIOS region (0xF000).
    let mut at_7c00 = [0u8; 16];
    for (offset, byte) in at_7c00.iter_mut().enumerate() {
        *byte = machine.read_physical_u8(0x7c00 + offset as u32);
    }
    let hex: Vec<String> = at_7c00.iter().map(|byte| format!("{byte:02X}")).collect();
    println!("0000:7C00 = {}", hex.join(" "));
    if cs < 0xf000 {
        println!("boot: boot sector is executing outside the BIOS region");
    } else {
        println!("boot: still in the BIOS (no boot, or read error)");
    }
    print_video_summary(&mut machine);
    Ok(())
}

/// Mount a hard-disk IMG, run the Izarra BIOS so INT 19h bootstraps it from LBA 0
/// (the MBR, which chains to the partition VBR), and print CS:IP plus the loaded
/// sector and the text screen. The diagnostic loop for the Katea FAT32 HDD boot:
/// a human reads the screen for the FreeDOS boot messages and the `C:\>` prompt,
/// and CS:IP / a CpuError for where a boot fault landed.
fn run_boot_hdd(
    path: &Path,
    cycles: Option<u64>,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    let image = std::fs::read(path)?;
    let image_len = image.len();
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_hdd(image);
    let budget = cycles.unwrap_or(DEFAULT_BOOT_HDD_CYCLES);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;

    let cs = machine.cpu().registers.cs().selector;
    let ip = machine.cpu().registers.eip as u16;
    println!("image: {} ({image_len} bytes)", path.display());
    println!("stop: {stop_reason:?}");
    println!("CS:IP = {cs:04X}:{ip:04X}");
    let mut at_7c00 = [0u8; 16];
    for (offset, byte) in at_7c00.iter_mut().enumerate() {
        *byte = machine.read_physical_u8(0x7c00 + offset as u32);
    }
    let hex: Vec<String> = at_7c00.iter().map(|byte| format!("{byte:02X}")).collect();
    println!("0000:7C00 = {}", hex.join(" "));
    if cs < 0xf000 {
        println!("boot: boot sector is executing outside the BIOS region");
    } else {
        println!("boot: still in the BIOS (no boot, or read error)");
    }
    print_video_summary(&mut machine);
    Ok(())
}

/// The fixed 8.3 name the target program is overlaid as on the Katea C: drive.
/// DOS dispatches .COM vs .EXE by the MZ signature, but a faithful extension is
/// kept (uppercased, truncated to 3 chars; defaults to COM when there is none).
fn katea_run_prog_name(prog: &std::path::Path) -> String {
    let ext = prog
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "COM".to_string());
    let ext: String = ext.chars().take(3).collect();
    format!("PROG.{ext}")
}

/// A temp directory that removes itself (and any contents) when dropped — on
/// success, an early `?` return, or a panic.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(path: std::path::PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Boot real FreeDOS from an empty temp dir (the target + RUNNER.COM + a runner
/// AUTOEXEC overlaid InMemory) via Katea, run the target through RUNNER.COM, and
/// return its DOS exit code. The screen text is printed for diagnostics.
fn katea_run(prog: &std::path::Path, profile: MachineProfile) -> Result<i32, Box<dyn Error>> {
    let bytes = std::fs::read(prog)?;
    let name = katea_run_prog_name(prog);
    let autoexec = format!("@echo off\r\nRUNNER {name}\r\n").into_bytes();
    let overrides = vec![
        ("AUTOEXEC.BAT".to_string(), autoexec),
        (
            "RUNNER.COM".to_string(),
            izarravm_firmware::runner_com().to_vec(),
        ),
        (name, bytes),
    ];

    // A self-cleaning temp dir: removed on every path (success, the `?` errors
    // below, or a panic), so a corpus run that hits errors can't accumulate dirs.
    let dir = TempDir::new(std::env::temp_dir().join(format!(
        "katea_run_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )))?;

    let mut machine = Machine::new(profile, izarravm_firmware::izarra_bios())?;
    machine.mount_hdd_folder_with(dir.path(), overrides)?;
    let stop = machine.run_until_halt_or_cycles(500_000_000)?;
    print!("{}", machine.screen_text().as_text());

    let code = match stop {
        StopReason::TestExit { code } | StopReason::DosExit { code } => i32::from(code),
        other => {
            eprintln!("katea-run: did not reach a program exit (stop={other:?})");
            1
        }
    };
    Ok(code)
}

/// One scheduled input event, fired once the run has burned `at_cycles` guest
/// cycles since start. `--inject-keys` and `--inject-mouse` both parse into this
/// and are merged into one offset-ordered schedule, so a game that needs a
/// keystroke and a click can be driven by both at once.
struct Injection {
    at_cycles: u64,
    event: InjectionEvent,
}

enum InjectionEvent {
    Keys(String),
    Mouse(MouseAction),
}

/// A `--inject-mouse` action, expressed in PS/2 packets rather than screen
/// coordinates. `Machine::set_mouse_absolute` cannot be used here: it maps onto
/// the GUI's 640x200 virtual space (`MOUSE_GUEST_MAX_Y` is 199), so it cannot
/// address a 640x480 menu at all. Relative packets are also simply what the
/// hardware sends.
enum MouseAction {
    /// Drive the pointer hard into the driver's minimum corner. The INT 33h
    /// driver clamps to its own `min_x`/`min_y`, so where this lands is exact no
    /// matter where the pointer started -- which is what lets a following `move`
    /// address a known pixel without the harness tracking any state.
    Home,
    /// Relative motion in MICKEYS -- raw PS/2 counts, y positive downward --
    /// not pixels. The harness cannot convert: how far a mickey moves the
    /// pointer is the INT 33h driver's mickey-to-pixel ratio, which the guest
    /// may change at any time through function 0Fh. At TOKAMOUS's defaults one
    /// mickey is one pixel across and two are one pixel down, so a schedule is
    /// derived once against a screenshot and then replays exactly.
    Move { dx: i32, dy: i32 },
    /// Press (1) or release (0) the left button, as a zero-motion packet.
    Button(u8),
    /// Press and release.
    Click,
}

/// Guest milliseconds between injected mouse packets. The 8042 paces auxiliary
/// bytes out at one per millisecond and a TOKAMOUS IntelliMouse packet is four
/// bytes, so the guest can never drain faster than 250 packets/s; injecting
/// quicker only grows the aux queue. 5 ms is the same 200 Hz ceiling the GUI's
/// own `MOUSE_FLUSH_HZ` holds to.
const MOUSE_PACKET_SPACING_MS: u64 = 5;

/// Packets `MouseAction::Home` sends. Each carries the PS/2 9-bit maximum, which
/// the INT 33h driver scales by its mickey-to-pixel ratio: at TOKAMOUS's
/// defaults that is 255 px per packet horizontally and 127 vertically, but a
/// game may set a coarser ratio through function 0Fh and homing has to overshoot
/// under the worst one it plausibly picks. At a ratio of 64 -- four times
/// coarser than the default vertical -- a packet is worth 31 pixels, so covering
/// a 480-line screen needs 16. Twenty leaves margin and costs 100 ms of guest
/// time, which is nothing against a schedule measured in guest seconds.
const MOUSE_HOME_PACKETS: usize = 20;

/// Largest motion one PS/2 packet can carry, so a longer move is split into
/// steps with a dwell between them: the device would deliver the same total
/// over later samples on its own, but a per-step dwell keeps the schedule's
/// timing explicit and deterministic.
const MOUSE_PACKET_MAX_DELTA: i32 = 255;

/// How long `click` holds the button down, in guest milliseconds.
///
/// A press and a release one packet apart is 5 ms, and that is NOT enough. A
/// menu that samples the button once a frame simply never sees it: Grand Prix 2
/// swallowed exactly that click on its startup menu, with the pointer verified
/// to be on the button. Nothing repeats during the hold -- a real PS/2 mouse
/// sends one packet per state change and the driver latches it -- so this is
/// purely how long the two packets are separated by. 100 ms is a human click
/// and clears any per-frame poll.
const MOUSE_CLICK_HOLD_MS: u64 = 100;

/// One injected PS/2 packet, plus how long to run the guest afterwards. The
/// dwell is per-packet because a click's press has to outlast a frame while
/// motion should stay at the aux-drain rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MousePacket {
    dx: i32,
    dy: i32,
    buttons: u8,
    dwell_ms: u64,
}

/// A stop that ends the whole run, as opposed to a slice merely using up its
/// own cycle allowance. Only `CycleLimit` is a slice boundary; everything else
/// (halt, TestExit, a CPU error) must propagate instead of being run past.
fn non_cycle_stop(reason: &StopReason) -> Option<StopReason> {
    match reason {
        StopReason::CycleLimit { .. } => None,
        other => Some(other.clone()),
    }
}

/// Which halves of a key's make/break pair a `{name}` token emits.
enum KeyEdge {
    /// Make then break: a tap, the default and what a bare `{name}` means.
    Tap,
    /// Make only, leaving the key down until a `Release` arrives.
    Press,
    /// Break only.
    Release,
}

/// Expand injection text into one scancode group per keypress. Plain characters
/// go through `ascii_to_set1`; `{name}` names a key that has no ASCII spelling.
///
/// The modifiers are the reason this exists. Prince of Persia advances its title
/// and cutscene screens on SHIFT, which is a bare make/break pair with no
/// character behind it, so an ASCII-only path cannot express it at all.
///
/// `{+name}` presses without releasing and `{-name}` releases, which is how a
/// schedule holds a key down across the steps between them.
fn text_to_scancode_groups(text: &str) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    let mut groups = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('{') {
            let (name, after) = tail
                .split_once('}')
                .ok_or_else(|| format!("--inject-keys has an unclosed {{ in {text:?}"))?;
            // A leading + or - splits the make and break apart so a key can be
            // HELD across the steps between them. `{right}` is a tap and cannot
            // express "keep running": the guest tracks key-down state from the
            // scancode stream, so holding means a make with no break until the
            // release is scheduled. Prince of Persia is the case in hand --
            // `{shift}` to start the game, then `{+right}` so the prince runs
            // instead of standing in the first room.
            let (name, edge) = match name.strip_prefix('+') {
                Some(held) => (held, KeyEdge::Press),
                None => match name.strip_prefix('-') {
                    Some(released) => (released, KeyEdge::Release),
                    None => (name, KeyEdge::Tap),
                },
            };
            let make: u8 = match name.to_ascii_lowercase().as_str() {
                "shift" => 0x2a,
                "ctrl" => 0x1d,
                "alt" => 0x38,
                "esc" => 0x01,
                "space" => 0x39,
                "enter" => 0x1c,
                "up" => 0x48,
                "down" => 0x50,
                "left" => 0x4b,
                "right" => 0x4d,
                "tab" => 0x0f,
                // Set 1 make codes. F1..F10 are the contiguous run 0x3B..0x44;
                // F11 and F12 are 0x57 and 0x58, NOT 0x45 and 0x46, which are
                // NumLock and ScrollLock. F11/F12 arrived with the 101-key
                // layout and did not extend the run.
                //
                // Added for the compatibility board: Pinball Fantasies selects
                // its table with F1..F4, so without these its row could not
                // reach gameplay, and gameplay is the only place its
                // 256-pixel-wide mode X exists.
                "f1" => 0x3b,
                "f2" => 0x3c,
                "f3" => 0x3d,
                "f4" => 0x3e,
                "f5" => 0x3f,
                "f6" => 0x40,
                "f7" => 0x41,
                "f8" => 0x42,
                "f9" => 0x43,
                "f10" => 0x44,
                "f11" => 0x57,
                "f12" => 0x58,
                other => return Err(format!("--inject-keys: unknown key {{{other}}}").into()),
            };
            groups.push(match edge {
                KeyEdge::Tap => vec![make, make | 0x80],
                KeyEdge::Press => vec![make],
                KeyEdge::Release => vec![make | 0x80],
            });
            rest = after;
        } else {
            let ch = rest.chars().next().expect("rest is non-empty");
            let codes = ascii_to_set1(ch);
            if codes.is_empty() {
                return Err(format!("--inject-keys: no scancode for {ch:?}").into());
            }
            groups.push(codes);
            rest = &rest[ch.len_utf8()..];
        }
    }
    Ok(groups)
}

/// Split a `cycles:payload` schedule into its steps, checking that the offsets
/// strictly increase. That check is what makes a schedule deterministic: the
/// same build replays the same event at the same guest cycle every run, so a
/// gate's equal-work comparison still holds.
fn parse_injection_steps<'a>(
    flag: &str,
    spec: &'a str,
) -> Result<Vec<(u64, &'a str)>, Box<dyn Error>> {
    let mut steps = Vec::new();
    let mut previous: Option<u64> = None;
    for raw in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let (cycles, payload) = raw
            .split_once(':')
            .ok_or_else(|| format!("{flag} step {raw:?} is not <cycles>:<payload>"))?;
        let at_cycles: u64 = cycles.trim().parse()?;
        if previous.is_some_and(|last| at_cycles <= last) {
            return Err(format!(
                "{flag} offsets must strictly increase; {at_cycles} does not follow {}",
                previous.unwrap_or_default()
            )
            .into());
        }
        previous = Some(at_cycles);
        steps.push((at_cycles, payload));
    }
    Ok(steps)
}

/// Parse `--inject-keys`: `cycles:text` steps separated by `;`, for example
/// `200000000:\r;400000000:\r`.
fn parse_key_injections(spec: &str) -> Result<Vec<Injection>, Box<dyn Error>> {
    parse_injection_steps("--inject-keys", spec)?
        .into_iter()
        .map(|(at_cycles, text)| {
            // `\r` is the one escape worth having: a bare carriage return cannot
            // survive a shell argument, and Enter is what dismisses a title screen.
            Ok(Injection {
                at_cycles,
                event: InjectionEvent::Keys(text.replace("\\r", "\r").replace("\\n", "\n")),
            })
        })
        .collect()
}

/// Parse `--inject-mouse`: `cycles:action` steps separated by `;`, for example
/// `6000000000:home;6100000000:move:320,386;6200000000:click`.
fn parse_mouse_injections(spec: &str) -> Result<Vec<Injection>, Box<dyn Error>> {
    parse_injection_steps("--inject-mouse", spec)?
        .into_iter()
        .map(|(at_cycles, action)| {
            let action = match action.trim() {
                "home" => MouseAction::Home,
                "down" => MouseAction::Button(1),
                "up" => MouseAction::Button(0),
                "click" => MouseAction::Click,
                other => {
                    let deltas = other.strip_prefix("move:").ok_or_else(|| {
                        format!(
                            "--inject-mouse: unknown action {other:?} \
                             (want home, move:<dx>,<dy>, down, up or click)"
                        )
                    })?;
                    let (dx, dy) = deltas.split_once(',').ok_or_else(|| {
                        format!("--inject-mouse: move needs <dx>,<dy>, got {deltas:?}")
                    })?;
                    MouseAction::Move {
                        dx: dx.trim().parse()?,
                        dy: dy.trim().parse()?,
                    }
                }
            };
            Ok(Injection {
                at_cycles,
                event: InjectionEvent::Mouse(action),
            })
        })
        .collect()
}

/// Merge the two schedules into one ordered by offset. Each flag's own offsets
/// already strictly increase; a key and a click may legitimately share an
/// offset, and a stable sort then fires the key first.
fn merged_injections(
    inject_keys: Option<&str>,
    inject_mouse: Option<&str>,
) -> Result<Vec<Injection>, Box<dyn Error>> {
    let mut merged = match inject_keys {
        Some(spec) => parse_key_injections(spec)?,
        None => Vec::new(),
    };
    if let Some(spec) = inject_mouse {
        merged.extend(parse_mouse_injections(spec)?);
    }
    merged.sort_by_key(|step| step.at_cycles);
    Ok(merged)
}

/// Expand one mouse action into the PS/2 packets that convey it. Each element is
/// a `(dx, dy, buttons)` triple the caller injects one packet at a time, with a
/// slice of guest time between them so the INT 74h ISR can consume each.
///
/// `buttons` is threaded through every packet, including motion, so a `down`,
/// `move`, `up` sequence reads as a drag rather than a click that lets go.
fn mouse_action_packets(action: &MouseAction, buttons: &mut u8) -> Vec<MousePacket> {
    let packet = |dx, dy, mask| MousePacket {
        dx,
        dy,
        buttons: mask,
        dwell_ms: MOUSE_PACKET_SPACING_MS,
    };
    match action {
        MouseAction::Home => {
            vec![
                packet(-MOUSE_PACKET_MAX_DELTA, -MOUSE_PACKET_MAX_DELTA, *buttons);
                MOUSE_HOME_PACKETS
            ]
        }
        MouseAction::Move { dx, dy } => {
            let steps = (dx.abs().max(dy.abs()) as u64).div_ceil(MOUSE_PACKET_MAX_DELTA as u64);
            let (mut left_x, mut left_y) = (*dx, *dy);
            (0..steps)
                .map(|_| {
                    let step_x = left_x.clamp(-MOUSE_PACKET_MAX_DELTA, MOUSE_PACKET_MAX_DELTA);
                    let step_y = left_y.clamp(-MOUSE_PACKET_MAX_DELTA, MOUSE_PACKET_MAX_DELTA);
                    left_x -= step_x;
                    left_y -= step_y;
                    packet(step_x, step_y, *buttons)
                })
                .collect()
        }
        MouseAction::Button(mask) => {
            *buttons = *mask;
            vec![packet(0, 0, *mask)]
        }
        MouseAction::Click => {
            *buttons = 0;
            vec![
                MousePacket {
                    dwell_ms: MOUSE_CLICK_HOLD_MS,
                    ..packet(0, 0, 1)
                },
                packet(0, 0, 0),
            ]
        }
    }
}

/// Guest milliseconds of emulation per `render_audio` call in the headless audio
/// capture. Short enough that the DSP's own render ring (8192 frames, 186 ms at
/// 44.1 kHz) never overflows between drains, which is what makes the captured
/// stream continuous rather than a series of survivors.
const AUDIO_CAPTURE_SLICE_MS: u64 = 10;
/// OPL3 native synthesis rate; `render_audio` counts its window in these.
const AUDIO_CAPTURE_OPL_HZ: f64 = 49_716.0;
/// Sample rate of the captured WAV, matching the machine's DAC rate.
const AUDIO_CAPTURE_DAC_HZ: u32 = 44_100;

/// Where the headless observer sends the mix it pulled.
///
/// `render_audio` -- OPL3 synthesis and its resample, the SB16 DSP drain, the
/// AD1848/WSS leg, the CD pull, the speaker drain and the CT1745 sum -- is
/// reached headlessly from exactly one place, and until now only when
/// `IZARRAVM_AUDIO_WAV` named a file. That made every wall this project has ever
/// recorded a wall with no audio mixing in it, and made the WAV path unusable as
/// a cost proxy: it also grows a ~29 MB buffer and serializes it.
///
/// The two cost modes exist as a PAIR. Both slice the run at the same cadence and
/// compute the same window; only one of them renders. Their wall difference is
/// audio's share; either one alone would measure slicing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AudioSinkMode {
    /// Today's behaviour, reached only via `IZARRAVM_AUDIO_WAV`. Unchanged.
    Wav(PathBuf),
    /// Armed leg: render, fold into a checksum, drop. No buffer, no file.
    Count,
    /// Disarmed control leg: slice and compute the window, render nothing.
    Skip,
}

impl AudioSinkMode {
    fn label(&self) -> &'static str {
        match self {
            AudioSinkMode::Wav(_) => "wav",
            AudioSinkMode::Count => "count",
            // `off` is the value the env var takes and the value the ladder
            // greps for; keep the two spellings the same.
            AudioSinkMode::Skip => "off",
        }
    }
}

/// Parse `IZARRAVM_AUDIO_COST`. Unknown values are an ERROR, not a default:
/// a typo that silently disarmed the observer would produce a pair of identical
/// legs and a share of zero, which is a wrong answer rather than a missing one.
fn parse_audio_cost_mode(value: &str) -> Result<AudioSinkMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "count" => Ok(AudioSinkMode::Count),
        "off" | "skip" => Ok(AudioSinkMode::Skip),
        other => Err(format!(
            "IZARRAVM_AUDIO_COST: unknown mode {other:?} (expected `count` or `off`)"
        )),
    }
}

/// Resolve the observer from the environment, once, before the run starts.
///
/// An EMPTY value counts as unset in both variables. That is not tidiness: in
/// pwsh, `[Environment]::SetEnvironmentVariable(name, $null, "Process")` leaves
/// the variable empty-but-set and children inherit it, so a rig that thought it
/// had cleared the variable would otherwise arm an observer on every leg of every
/// board. Setting BOTH variables is a hard error rather than a silent winner.
fn resolve_audio_sink() -> Result<Option<AudioSinkMode>, String> {
    resolve_audio_sink_from(
        std::env::var_os("IZARRAVM_AUDIO_WAV"),
        std::env::var("IZARRAVM_AUDIO_COST").ok(),
    )
}

/// The whole of [`resolve_audio_sink`]'s decision, split from the environment
/// read so it can be pinned by a test without mutating process environment.
fn resolve_audio_sink_from(
    wav: Option<std::ffi::OsString>,
    cost: Option<String>,
) -> Result<Option<AudioSinkMode>, String> {
    let wav = wav.filter(|path| !path.is_empty());
    let cost = cost.filter(|mode| !mode.trim().is_empty());
    match (wav, cost) {
        (Some(_), Some(_)) => Err(
            "IZARRAVM_AUDIO_WAV and IZARRAVM_AUDIO_COST are both set; they are different \
             observers of the same call and one would silently win"
                .to_string(),
        ),
        (Some(path), None) => Ok(Some(AudioSinkMode::Wav(PathBuf::from(path)))),
        (None, Some(mode)) => parse_audio_cost_mode(&mode).map(Some),
        (None, None) => Ok(None),
    }
}

/// Cadence of the observer, in guest milliseconds, from
/// `IZARRAVM_AUDIO_COST_SLICE_MS`. The GUI renders about once per millisecond
/// when it keeps up, so the cadence has to be a knob: if the measured share moves
/// between 10 ms and 1 ms, the cost is per-call and the lever is batching.
fn audio_capture_slice_ms() -> Result<u64, String> {
    parse_audio_cost_slice_ms(std::env::var("IZARRAVM_AUDIO_COST_SLICE_MS").ok())
}

fn parse_audio_cost_slice_ms(value: Option<String>) -> Result<u64, String> {
    let Some(raw) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(AUDIO_CAPTURE_SLICE_MS);
    };
    match raw.trim().parse::<u64>() {
        Ok(ms) if ms > 0 => Ok(ms),
        _ => Err(format!(
            "IZARRAVM_AUDIO_COST_SLICE_MS: expected a positive integer, got {raw:?}"
        )),
    }
}

/// FNV-style fold over both channels of every frame, through `black_box` so LLVM
/// cannot decide the mix is dead and sink it.
///
/// Two integer ops per frame over ~7M frames is single-digit milliseconds against
/// a ~139 s run, i.e. below 0.01%, and it is paid by the armed leg only -- so it
/// biases the measured share UPWARD. `S` is a conservative bound on the mix's own
/// cost, which is the direction to err in; do not correct for it.
///
/// Free-standing rather than a method so a test can fold a WAV capture's frames
/// with the identical arithmetic and prove the counting sink saw the same mix.
fn fold_audio_frames(seed: u64, frames: &[(i16, i16)]) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut fold = seed;
    for (left, right) in frames {
        fold = fold
            .wrapping_mul(FNV_PRIME)
            .wrapping_add(*left as u16 as u64);
        fold = fold
            .wrapping_mul(FNV_PRIME)
            .wrapping_add(*right as u16 as u64);
    }
    std::hint::black_box(fold)
}

/// Nothing stages the machine for a headless capture any more, and that is the
/// point: `render_audio` returns the machine's line-out, decided entirely by
/// CT1745 registers the guest owns. `HostAudioGains` lived here because the GUI
/// used to push two host-only multipliers into the machine before every render
/// and a capture that skipped them recorded something the user never heard --
/// an instrument 21.6 dB out from the thing it was pointed at. Both multipliers
/// are gone from the machine, so the capture and the GUI agree by construction
/// rather than by remembering to copy a staging step.
/// Headless capture of the host audio mix: an OBSERVER of a run, not a run mode.
///
/// It owns two things only -- how finely the run has to be sliced so the DSP's
/// render ring never overflows between drains, and the growing PCM buffer. Every
/// path that advances the guest goes through [`run_sliced`], so the capture
/// composes with whatever else that path is doing (key and mouse injection, in
/// particular). An earlier cut made the capture its own branch of the run
/// if/else, which silently discarded `--inject-keys`/`--inject-mouse`: the
/// capture ran, the WAV was written, and the recorded audio was of a title that
/// had never been given the input it was supposed to react to.
///
/// The sink is a MODE, not a path (see [`AudioSinkMode`]): the same observer
/// either writes a WAV, folds the mix into a checksum and drops it, or renders
/// nothing at all while slicing the run identically. That last mode is what makes
/// the pair `count` vs `off` a measurement of AUDIO rather than a measurement of
/// slicing.
struct AudioCapture {
    /// What happens to the mix once it has been rendered -- and, for `Skip`,
    /// whether it is rendered at all. Resolved ONCE at construction so the run
    /// loop never reads an environment variable.
    mode: AudioSinkMode,
    /// Only ever non-empty under [`AudioSinkMode::Wav`]. The cost modes must not
    /// retain a single frame: a duke3d-486 run is ~7.2M frames ≈ 29 MB of `Vec`
    /// growth, pure instrument cost with no analogue in the GUI.
    pcm: Vec<(i16, i16)>,
    /// Guest cycles per `render_audio` call.
    slice: u64,
    /// The `slice` in guest milliseconds, kept for the end-of-run report: the
    /// per-window fixed cost scales with the cadence, so a cost number that does
    /// not carry its cadence cannot be compared with another one.
    slice_ms: u64,
    /// Windows actually rendered (or, under `Skip`, that WOULD have been
    /// rendered), and the OPL-native samples they asked for. Both are computed
    /// identically in every mode, which is what lets the ladder assert them equal
    /// across the armed and disarmed legs.
    windows: u64,
    native_samples: u64,
    /// Frames the mix returned, and a fold over every one of them. The fold has
    /// two jobs: it makes the mix un-elidable, and equality across observations
    /// proves the mix is deterministic. Zero under `Skip`.
    out_frames: u64,
    checksum: u64,
    /// Nanoseconds inside `render_audio` itself, one `Instant` pair per window
    /// (not per sample). A CROSS-CHECK on the wall ladder, never its headline:
    /// it is inclusive of the instrument and blind to the cache effects the audio
    /// path imposes on the CPU path.
    render_ns: u128,
    /// OPL-native samples per guest cycle. The window is derived from the cycles
    /// a slice actually ran rather than from a constant, because a composed run
    /// does not advance in fixed steps: an injection path runs 2 ms per scancode
    /// and its own dwell per mouse packet. Rendering a fixed slice's worth of
    /// audio for a short advance would stretch the captured stream exactly over
    /// the input the capture exists to hear the reaction to.
    opl_samples_per_cycle: f64,
    /// `IZARRAVM_AUDIO_WAV_WALL=1` paces the window from HOST wall time, which
    /// is what the GUI actually does. The two differ by exactly the real-time
    /// factor: an emulator running the guest at 0.3x still has to hand the sound
    /// card 44100 frames every real second, and the guest-clocked DSP stream can
    /// only supply 0.3 of them. Capturing both says whether a title is silent
    /// because the mix is wrong or because the guest is too slow to feed it.
    wall_paced: bool,
    debt: f64,
    last_wall: std::time::Instant,
}

impl AudioCapture {
    fn new(mode: AudioSinkMode, hardware: &HardwareProfile, slice_ms: u64) -> Self {
        let clock = hardware.cpu.clock_rate();
        Self {
            mode,
            pcm: Vec::new(),
            slice: clock.clocks_for_fraction_floor(slice_ms, 1000).max(1_000),
            slice_ms,
            windows: 0,
            native_samples: 0,
            out_frames: 0,
            checksum: 0,
            render_ns: 0,
            // The GUI derives its OPL sample count from WALL time; pacing it from
            // GUEST time instead models a host that keeps up exactly, which is
            // the condition to test first -- a mix that is silent even there is
            // broken independently of how fast the emulator runs.
            opl_samples_per_cycle: AUDIO_CAPTURE_OPL_HZ
                / clock.clocks_for_fraction_floor(1, 1).max(1) as f64,
            // "" | "0" count as unset for the same pwsh reason as
            // `resolve_audio_sink`: an empty assignment must not flip pacing.
            wall_paced: machine_profile_requested(
                std::env::var("IZARRAVM_AUDIO_WAV_WALL").ok().as_deref(),
            ),
            debt: 0.0,
            last_wall: std::time::Instant::now(),
        }
    }

    /// Drain the host mix for the `cycles` the guest just ran. Called after every
    /// guest advance, whatever placed it.
    fn after_slice(&mut self, machine: &mut Machine, cycles: u64) {
        if self.wall_paced {
            let now = std::time::Instant::now();
            self.debt += now.duration_since(self.last_wall).as_secs_f64() * AUDIO_CAPTURE_OPL_HZ;
            self.last_wall = now;
        } else {
            self.debt += cycles as f64 * self.opl_samples_per_cycle;
        }
        let want = self.debt.floor() as usize;
        self.debt -= want as f64;
        let native_samples = want.min(AUDIO_CAPTURE_OPL_HZ as usize / 2);
        if native_samples == 0 {
            return;
        }
        // Counted BEFORE the mode split, and from the same arithmetic in every
        // mode: the disarmed leg's whole job is to prove it performed the same
        // slicing and computed the same window as the armed one.
        self.windows += 1;
        self.native_samples += native_samples as u64;
        match self.mode {
            // The disarmed control leg: the run is sliced identically, the window
            // is computed identically, and the mix is not rendered. The pair
            // therefore isolates audio instead of isolating slicing.
            AudioSinkMode::Skip => {}
            AudioSinkMode::Wav(_) => {
                let frames = machine.render_audio(native_samples);
                self.pcm.extend(frames);
            }
            AudioSinkMode::Count => {
                // The `Instant` pair is TWO clock reads per window -- ~16k windows
                // on a duke3d-486 run, a few hundred microseconds against ~139 s
                // -- and only the ARMED leg pays them. Like the fold below, that
                // biases the measured share UPWARD: `S` comes out a conservative
                // bound on the mix's own cost rather than an under-statement.
                let started = std::time::Instant::now();
                let frames = machine.render_audio(native_samples);
                self.render_ns += started.elapsed().as_nanos();
                self.out_frames += frames.len() as u64;
                self.checksum = fold_audio_frames(self.checksum, &frames);
                drop(frames);
            }
        }
    }

    /// End of run. Mode-aware, and it must stay that way: this runs BEFORE the
    /// wall reading, and the cost modes have no path to write to -- an earlier
    /// shape would have serialized an empty WAV to a path it did not have.
    fn finish(&self) -> Result<(), Box<dyn Error>> {
        match &self.mode {
            AudioSinkMode::Wav(path) => {
                write_wav(path, &self.pcm, AUDIO_CAPTURE_DAC_HZ)?;
                println!("audio capture: wrote {}", path.display());
            }
            // Deliberately stderr and deliberately NOT a `--profile-json` field:
            // no pinned artifact and no scoreboard parser moves for an
            // instrument.
            AudioSinkMode::Count | AudioSinkMode::Skip => eprintln!("{}", self.cost_report()),
        }
        Ok(())
    }

    /// The one-line end-of-run report for the cost modes. Split out so a test can
    /// read it without capturing stderr.
    fn cost_report(&self) -> String {
        format!(
            "audio cost: mode={} pacing={} slice_ms={} windows={} native_samples={} \
             out_frames={} checksum=0x{:016x} render_ns={}",
            self.mode.label(),
            if self.wall_paced { "wall" } else { "guest" },
            self.slice_ms,
            self.windows,
            self.native_samples,
            self.out_frames,
            self.checksum,
            self.render_ns,
        )
    }
}

/// Advance the guest by `cycles`, subdividing into capture slices and draining
/// the mix after each when a capture is armed. With no capture this is the one
/// `run_until_halt_or_cycles` call it has always been, so a run without
/// `IZARRAVM_AUDIO_WAV` keeps its exact device-servicing boundaries.
///
/// Returns the stop reason and the cycles actually spent -- a terminal stop ends
/// the run partway through the request, and the caller's own pacing has to know.
fn run_sliced(
    machine: &mut Machine,
    cycles: u64,
    capture: &mut Option<AudioCapture>,
    dumper: &mut Option<screendump::ScreenDumper>,
) -> Result<(StopReason, u64), Box<dyn Error>> {
    // The unobserved path stays ONE `run_until_halt_or_cycles` call, byte for
    // byte what it was. Slicing moves where the machine services device events,
    // so the shipped fixtures must never take a sliced branch.
    if capture.is_none() && dumper.is_none() {
        return Ok((machine.run_until_halt_or_cycles(cycles)?, cycles));
    }
    let slice = capture
        .as_ref()
        .map(|capture| capture.slice)
        .into_iter()
        .chain(dumper.as_ref().map(|dumper| dumper.slice))
        .min()
        .unwrap_or(cycles)
        .max(1);
    let mut spent = 0u64;
    let mut stop = StopReason::CycleLimit { requested: cycles };
    while spent < cycles {
        let step = slice.min(cycles - spent);
        let reason = machine.run_until_halt_or_cycles(step)?;
        spent += step;
        if let Some(capture) = capture.as_mut() {
            capture.after_slice(machine, step);
        }
        if let Some(dumper) = dumper.as_mut() {
            dumper.after_slice(machine);
        }
        if non_cycle_stop(&reason).is_some() {
            stop = reason;
            break;
        }
    }
    Ok((stop, spent))
}

/// Write 16-bit stereo PCM as a canonical 44-byte-header RIFF/WAVE file.
fn write_wav(path: &Path, pcm: &[(i16, i16)], rate: u32) -> Result<(), Box<dyn Error>> {
    let data_len = (pcm.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // channels
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for (l, r) in pcm {
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Mount a host folder as C: through the Katea facade (real FreeDOS system files
/// plus the folder's top-level files, read-only), run the BIOS so INT 19h boots
/// it, and print the same diagnostics as `run_boot_hdd`. The lazy-facade analogue
/// of the flat-image boot loop.
#[allow(clippy::too_many_arguments)]
fn run_boot_hdd_folder(
    dir: &Path,
    glide_ovl: Option<Vec<u8>>,
    cycles: Option<u64>,
    hardware: &HardwareProfile,
    dump_result: bool,
    result_ppm: Option<&Path>,
    presented_ppm: Option<&Path>,
    mode_census: Option<&Path>,
    profile_json: Option<&Path>,
    expect_test_exit: bool,
    inject_keys: Option<&str>,
    inject_mouse: Option<&str>,
    cd_image: Option<&Path>,
    screen_dump: Option<(&Path, u64)>,
) -> Result<(), Box<dyn Error>> {
    if let Some(path) = profile_json {
        validate_profile_json_parent(path)?;
    }
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    let overlays = glide_ovl
        .into_iter()
        .map(|bytes| ("GLIDE2X.OVL".to_string(), bytes))
        .collect();
    machine.mount_hdd_folder_with_user_overrides(dir, overlays)?;
    // Same loader the GUI mount uses, so the accepted formats and the error
    // messages stay one list.
    if let Some(path) = cd_image {
        machine.mount_cd(gui::load_cd_image_from_path(path)?);
    }
    maybe_enable_unit_sim(&mut machine);
    maybe_enable_smc_trace(&mut machine);
    // Calibration census tool: IZARRAVM_CPU_PROFILE=<stride> turns on the same
    // sampled per-opcode CPU profile the bench harness uses, dumped after the
    // run. Reads the guest-clock attribution of e.g. the x87 opcode rows
    // (D8-DF) for a timedemo without touching guest-visible state.
    // IZARRAVM_MACHINE_PROFILE=1 measures only the batch-level machine phases,
    // leaving the direct compiler enabled for representative throughput runs.
    let cpu_profile_stride = std::env::var("IZARRAVM_CPU_PROFILE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let machine_profile_value = std::env::var("IZARRAVM_MACHINE_PROFILE").ok();
    let machine_profile = machine_profile_requested(machine_profile_value.as_deref());
    if let Some(stride) = cpu_profile_stride {
        machine.enable_host_profiling(stride);
    } else if machine_profile {
        machine.enable_machine_profiling();
    }
    // BIOS fixed-disk census: IZARRAVM_INT13_PROFILE=1 counts INT 13h calls, the
    // sectors each one moved, and the wall the service burned. Off by default,
    // and the machine gates every increment at its call site.
    if std::env::var("IZARRAVM_INT13_PROFILE").as_deref() == Ok("1") {
        machine.enable_int13_profile();
    }
    let budget = cycles.unwrap_or(DEFAULT_BOOT_HDD_CYCLES);
    #[cfg(windows)]
    let rip_sampler = rip_profile_path().map(|path| (riprofile::Sampler::start(), path));
    let injections = merged_injections(inject_keys, inject_mouse)?;
    // Periodic phase sampling, off unless IZARRAVM_PHASE_INTERVAL_MS names a guest-millisecond
    // interval. Armed BEFORE the run and closed after it: the two host-placed edges below are
    // what give the first and last periodic intervals a left and right boundary, and placing
    // them outside the run means no `run_until_halt_or_cycles` boundary moves.
    let phase_interval_ms = std::env::var("IZARRAVM_PHASE_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0);
    if let Some(ms) = phase_interval_ms {
        let per_ms = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 1000);
        machine.arm_periodic_phase_marks(per_ms.saturating_mul(ms), budget);
        machine.record_host_phase_mark(izarravm_machine::phase_mark::BENCH_START);
    }
    // Windowed IPE trace, off unless IZARRAVM_IPE_WINDOW_TRACE names a writable path. The parent
    // is checked HERE rather than at write time so a bad path fails in a second instead of after
    // a multi-minute fixture run. Read-only observer: it neither slices the run nor writes
    // anything from inside it. See `ipe_trace`.
    let ipe_trace_path = ipe_trace::requested_path();
    if let Some(path) = &ipe_trace_path {
        validate_output_parent(path, "IPE window trace")?;
        machine.arm_ipe_window_trace(ipe_trace::WINDOW_ENTRIES);
    }
    // Headless capture of the HOST audio mix (`IZARRAVM_AUDIO_WAV=<path>`).
    // Everything else headless observes the guest SIDE of the sound card: the
    // per-second `[SB]` trace proves the DSP is programmed and that DMA is
    // handing it real PCM, but `render_audio` -- the OPL + DSP + WSS + CD +
    // speaker sum with the CT1745 gains applied -- only ever runs in the GUI,
    // so "the guest is producing audio" and "audio reaches the speakers" could
    // not be distinguished without a person and a pair of ears. This pumps the
    // same call the GUI's `pump_audio` does, paced by GUEST time (i.e. the
    // ideal real-time host), and writes the result as a 44.1 kHz stereo WAV.
    //
    // Diagnostic only, and off unless the variable is set: it slices the run,
    // which moves where device events are serviced, so a captured run is NOT
    // comparable against a pinned fixture invariant.
    //
    // The capture is an OBSERVER, not a run mode: it composes with key and mouse
    // injection rather than replacing it, so `--inject-mouse ... IZARRAVM_AUDIO_WAV=x.wav`
    // records the audio of a title that actually received its input.
    //
    // `IZARRAVM_AUDIO_COST=count|off` arms the same observer as a COST
    // instrument instead: `count` renders and folds the mix away, `off` slices
    // the run identically and renders nothing. Their wall difference is the audio
    // share; neither leg writes a file.
    let audio_slice_ms = audio_capture_slice_ms()?;
    let mut capture =
        resolve_audio_sink()?.map(|mode| AudioCapture::new(mode, hardware, audio_slice_ms));
    // Periodic headless screen sampling, off unless --screen-dump-dir is given.
    let mut dumper = match screen_dump {
        Some((dir, interval_ms)) => Some(screendump::ScreenDumper::new(
            dir,
            hardware
                .cpu
                .clock_rate()
                .clocks_for_fraction_floor(interval_ms, 1000),
        )?),
        None => None,
    };
    let start_wall = std::time::Instant::now();
    // The no-injection, no-capture path stays ONE `run_until_halt_or_cycles`
    // call, byte for byte what it was: slicing the run moves where the machine
    // services device events, so the shipped Doom and Quake fixtures must not
    // take a sliced branch.
    let stop_reason = if injections.is_empty() {
        run_sliced(&mut machine, budget, &mut capture, &mut dumper)?.0
    } else {
        let guest_ms = |ms: u64| {
            hardware
                .cpu
                .clock_rate()
                .clocks_for_fraction_floor(ms, 1000)
                .max(1_000)
        };
        // One short slice per scancode, as `inject_command` does: the guest has
        // to poll INT 16h and consume each key before the next arrives, or the
        // type-ahead buffer swallows it. Mouse packets carry their own dwell,
        // which is longer -- paced by the 8042's aux byte rate, and longer still
        // for a click, which has to outlast a frame.
        let per_key = guest_ms(2);
        let mut spent = 0u64;
        let mut reason = None;
        let mut buttons = 0u8;
        // Run `slice` more cycles, reporting a terminal stop. Returns false once
        // the run is over, so the caller stops feeding it input. Goes through
        // `run_sliced`, so an armed audio capture observes this path too.
        let advance = |machine: &mut Machine,
                       spent: &mut u64,
                       reason: &mut Option<StopReason>,
                       capture: &mut Option<AudioCapture>,
                       dumper: &mut Option<screendump::ScreenDumper>,
                       slice: u64|
         -> Result<bool, Box<dyn Error>> {
            let slice = slice.min(budget.saturating_sub(*spent));
            if slice == 0 {
                return Ok(false);
            }
            let (stop, ran) = run_sliced(machine, slice, capture, dumper)?;
            *spent += ran;
            if let Some(terminal) = non_cycle_stop(&stop) {
                *reason = Some(terminal);
                return Ok(false);
            }
            Ok(true)
        };
        for step in &injections {
            let gap = step.at_cycles.saturating_sub(spent);
            if gap > 0
                && !advance(
                    &mut machine,
                    &mut spent,
                    &mut reason,
                    &mut capture,
                    &mut dumper,
                    gap,
                )?
            {
                break;
            }
            match &step.event {
                InjectionEvent::Keys(text) => {
                    for group in text_to_scancode_groups(text)? {
                        for code in group {
                            machine.inject_key_scancodes(&[code]);
                            if !advance(
                                &mut machine,
                                &mut spent,
                                &mut reason,
                                &mut capture,
                                &mut dumper,
                                per_key,
                            )? {
                                break;
                            }
                        }
                    }
                }
                InjectionEvent::Mouse(action) => {
                    for packet in mouse_action_packets(action, &mut buttons) {
                        machine.inject_mouse_relative(packet.dx, packet.dy, packet.buttons);
                        let dwell = guest_ms(packet.dwell_ms);
                        if !advance(
                            &mut machine,
                            &mut spent,
                            &mut reason,
                            &mut capture,
                            &mut dumper,
                            dwell,
                        )? {
                            break;
                        }
                    }
                }
            }
            if reason.is_some() {
                break;
            }
        }
        match reason {
            Some(terminal) => terminal,
            None => {
                run_sliced(
                    &mut machine,
                    budget.saturating_sub(spent),
                    &mut capture,
                    &mut dumper,
                )?
                .0
            }
        }
    };
    if let Some(capture) = &capture {
        capture.finish()?;
    }
    if let Some(dumper) = dumper.take() {
        dumper.finish();
    }
    // The wall reading comes FIRST. `record_host_phase_mark` routes to the full mark, which
    // takes a `cpu_profile` snapshot when profiling is armed -- the untruncated hot-address sort
    // that is at its largest exactly here, at end of run -- and a run with both the sampler and
    // `IZARRAVM_CPU_PROFILE` armed would otherwise absorb that snapshot into its headline wall.
    let wall = start_wall.elapsed();
    // Close the last periodic interval. Placed AFTER the run returns, so it cannot move a run
    // boundary; without it the tail (up to one interval, plus everything after the final batch,
    // plus the loop's several early returns) is never bounded on the right.
    if phase_interval_ms.is_some() {
        machine.record_host_phase_mark(izarravm_machine::phase_mark::BENCH_END);
    }
    // Written AFTER the wall reading, for the same reason the phase marks are closed here: the
    // render walks every window and touches the filesystem, and neither belongs inside the
    // measured interval.
    if let Some(path) = &ipe_trace_path {
        ipe_trace::write_trace(path, &machine)?;
        println!(
            "ipe window trace: {} windows -> {}",
            machine.ipe_windows().len() + usize::from(machine.ipe_window_tail().is_some()),
            path.display()
        );
    }
    #[cfg(windows)]
    if let Some((Some(sampler), path)) = rip_sampler {
        sampler.stop_and_report(std::path::Path::new(&path));
    }
    let timedemo = extract_timedemo_realtics(&machine.screen_text().as_text());
    if let Some(path) = profile_json {
        write_hdd_profile_json(
            path,
            dir,
            hardware.cpu,
            budget,
            wall,
            &stop_reason,
            timedemo,
            &machine,
        )?;
    }
    if cpu_profile_stride.is_some() {
        let snapshot = machine.cpu().profile_snapshot();
        bench::print_cpu_profile(&snapshot);
        // Dump the raw bytes around the hottest sampled address so the region compiler's
        // target loop can be disassembled straight from the census. The histogram records
        // LINEAR addresses; walk the live page tables (a plain physical-read PDE/PTE walk,
        // 4 MB pages included) so a paged guest (JemmEx maps Doom NON-identity - the
        // identity-assumed first cut dumped unrelated data bytes) yields real code.
        // IZARRAVM_DUMP_LINEAR=<hex>[,<len-hex>] overrides the dump window (default: around
        // the run's own hottest address). Needed because the hottest-address LIST wants a
        // demo-COMPLETE budget, but the byte dump wants a mid-demo stop (the walk uses the
        // stop-time CR3; a post-demo stop lands in a V86/monitor context that does not map
        // the game's pages) - two different runs, so the second must be told where to look.
        let dump_override = std::env::var("IZARRAVM_DUMP_LINEAR").ok().and_then(|v| {
            let mut parts = v.split(',');
            let addr = u32::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
            let len = parts
                .next()
                .and_then(|l| u32::from_str_radix(l.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0x180);
            Some((addr, len))
        });
        // Full-histogram sidecar (IZARRAVM_CPU_PROFILE_ADDRS=<path>): every sampled
        // `(linear, samples)` row as CSV, not the top-64 head the print shows. Two
        // deterministic runs with budgets at a window's two boundaries difference
        // into that window's histogram, which is how the DOS-kernel-vs-game split
        // is cut without a per-mark serialization of the whole address map.
        if let Ok(path) = std::env::var("IZARRAVM_CPU_PROFILE_ADDRS") {
            let mut out = String::with_capacity(snapshot.hot_addrs.len() * 20 + 64);
            out.push_str(&format!(
                "# sample_stride={} rows={}\nlinear_hex,samples\n",
                snapshot.sample_stride,
                snapshot.hot_addrs.len()
            ));
            for &(lin, samples) in &snapshot.hot_addrs {
                out.push_str(&format!("{lin:08X},{samples}\n"));
            }
            std::fs::write(&path, out)?;
            println!(
                "cpu-profile addrs sidecar: {} rows -> {path}",
                snapshot.hot_addrs.len()
            );
        }
        let target = dump_override.or_else(|| snapshot.hot_addrs.first().map(|&(t, _)| (t, 0x180)));
        if let Some((top, dump_len)) = target {
            let start = top.saturating_sub(0x40) & !0xf;
            println!();
            println!("=== bytes around hottest address {top:08X} (paging-walked linear) ===");
            for row in 0..dump_len.div_ceil(16) {
                let base = start + row * 16;
                let bytes: Vec<String> = (0..16)
                    .map(|i| match machine.read_linear_u8(base + i) {
                        Some(byte) => format!("{byte:02X}"),
                        None => "--".to_string(),
                    })
                    .collect();
                println!("{base:08X}  {}", bytes.join(" "));
            }
        }
    }
    let machine_profile_snapshot = machine.host_profile_snapshot();
    if machine_profile_snapshot.machine_phase_timing_enabled {
        bench::print_machine_profile(&machine_profile_snapshot, wall);
        // Hit rate of the batch cap's device-edge deadline cache. The CpuBatch
        // count above is the batch total; this says how many of those entries had
        // to run the ~15-query device pull-scan instead of one compare.
        let (batches, scans) = machine.device_edge_cache_counts();
        let served = 100.0 * (batches.saturating_sub(scans)) as f64 / batches.max(1) as f64;
        println!(
            "device-edge cache: {scans} scans over {batches} batch entries ({served:.2}% served from cache)"
        );
    }
    // Run-shape diagnostics (insns/run + break reasons). Unconditional: the counters are
    // always maintained, so unlike the sampled profile above this print costs nothing.
    bench::print_perf_counter_row(
        "hdd-folder",
        hardware.cpu,
        machine.cpu().perf_counters(),
        machine.cpu().fast_map_probe_counters(),
    );
    maybe_report_unit_sim(&mut machine);
    maybe_report_smc_trace(&mut machine);
    maybe_report_slow_read_histo(&machine);
    // Diff-trace prototype (IZARRAVM_DIFF_TRACE): flush the buffered trace writer now
    // that the run loop returned, or its last partial buffer's worth of lines -- most
    // often exactly the tail we care about -- is silently lost at process exit. This
    // is the path extender/game repros run through, so the flush matters most here.
    izarravm_cpu::flush_diff_trace();

    let cs = machine.cpu().registers.cs().selector;
    let ip = machine.cpu().registers.eip as u16;
    println!("folder: {}", dir.display());
    println!("stop: {stop_reason:?}");
    // Ports nothing decoded. Silent when a run touches only modelled hardware,
    // so a line here means the guest went looking for something that is not
    // there -- which used to be a fatal stop and is now just a note.
    let open_bus = machine.open_bus_ports();
    if open_bus.reads() > 0 || open_bus.writes() > 0 {
        let ports: Vec<String> = open_bus.ports().map(|p| format!("{p:#06x}")).collect();
        println!(
            "open-bus: {} read(s), {} write(s) across {} port(s): {}",
            open_bus.reads(),
            open_bus.writes(),
            ports.len(),
            ports.join(" ")
        );
    }
    println!("CS:IP = {cs:04X}:{ip:04X}");
    let mut at_7c00 = [0u8; 16];
    for (offset, byte) in at_7c00.iter_mut().enumerate() {
        *byte = machine.read_physical_u8(0x7c00 + offset as u32);
    }
    let hex: Vec<String> = at_7c00.iter().map(|byte| format!("{byte:02X}")).collect();
    println!("0000:7C00 = {}", hex.join(" "));
    if cs < 0xf000 {
        println!("boot: boot sector is executing outside the BIOS region");
    } else {
        println!("boot: still in the BIOS (no boot, or read error)");
    }
    print_video_summary(&mut machine);
    if dump_result {
        print_dump_result(&mut machine, &stop_reason);
        write_margo_store_ppms(&mut machine, result_ppm)?;
    }
    if let Some(path) = result_ppm {
        write_framebuffer_ppm(&mut machine, path)?;
        println!("screenshot: {}", path.display());
    }
    if let Some(path) = presented_ppm {
        if write_presented_ppm(&mut machine, path)? {
            println!("presented: {}", path.display());
        } else {
            println!("presented: none (no completed raster)");
        }
    }
    if let Some(path) = mode_census {
        // Slice 0b of `dev_docs/2026-09-05-distira-async-overlap-review.md`
        // section 1: this used to read `distira_scanout_state` first
        // because THAT call happened to drain Distira's triangle queue and
        // every other accessor below was `&self` and silently read whatever
        // was left over -- an ordering that worked by luck, not by
        // construction. `distira_census`/`distira_register_writes`/
        // `distira_register_reads`/`distira_aperture_traffic` are now
        // `&mut self` and each joins the raster queue itself
        // (`Distira::join_raster`), so this call order no longer matters:
        // every one of them sees the same fully-drained state regardless of
        // which runs first.
        let scanout_state = machine.distira_scanout_state();
        // Each accessor is bound to a local before the call: `&mut self`
        // means each one's borrow of `machine` has to end before the next
        // starts, and a shared `&ModeCensus`/`&DistiraCensus` held live
        // across the whole `mode_census_json(...)` argument list (as a
        // direct method-call argument would be) would conflict with that.
        let vga_census = machine.mode_census().clone();
        let distira_census = machine.distira_census();
        let register_writes = machine.distira_register_writes();
        let register_reads = machine.distira_register_reads();
        let aperture_traffic = machine.distira_aperture_traffic();
        let offset_bits_or = machine.distira_offset_bits_or();
        let swap_to_next_drain = machine.distira_swap_to_next_drain_stats();
        let mut census = mode_census_json(
            &vga_census,
            &distira_census,
            Some(scanout_state),
            &register_writes,
            &register_reads,
            aperture_traffic,
            offset_bits_or,
            swap_to_next_drain,
        );
        let (vbe_linear, vbe_banked) = machine.vbe_mode_set_window_counts();
        census["margo"] = json!({
            "active": machine.margo_active(),
            "display": machine.margo_display().map(|display| json!({
                "mode": format!("0x{:04x}", display.mode),
                "width": display.width,
                "height": display.height,
                "bpp": display.bpp,
                "pitch": display.pitch,
                "start": display.start,
            })),
            "vbe_mode_sets_linear": vbe_linear,
            "vbe_mode_sets_banked": vbe_banked,
            "vga_pass": machine.distira_vga_pass(),
            "video_reset": machine.distira_video_reset(),
            "init_enable": machine.distira_init_enable(),
            "fbi_init0": machine.distira_fbi_init0(),
            "video_reset_falling_edges": machine.distira_video_reset_falling_edges(),
            "fbi_init0_byte0_enables": machine.distira_fbi_init0_byte0_enables(),
        });
        std::fs::write(
            path,
            serde_json::to_string_pretty(&census)?
                + "
",
        )?;
        println!("mode census: {}", path.display());
    }
    // Final reconcile. Katea projects completed write commands to the host as
    // they happen, but a write whose FAT, directory or path was still
    // incomplete at the last command boundary is held in the guest-write store;
    // without this, anything left there (a `dir > log.txt` capture, a rebound
    // executable) is silently discarded at exit, which defeats the
    // mounted-folder contract and the guest-side debug channel it enables. This
    // also flushes the open host handles.
    machine.flush_hdd_folder();

    // Wall time + realtics extraction (for 2+3). Makes A/B runs self-contained.
    println!("wall: {:.3}s", wall.as_secs_f64());
    if let Some((gametics, realtics)) = timedemo {
        println!("timed {} gametics in {} realtics", gametics, realtics);
    }
    if expect_test_exit && !matches!(stop_reason, StopReason::TestExit { code: 0 }) {
        return Err(format!("expected TestExit code 0, got {stop_reason:?}").into());
    }

    Ok(())
}

/// The Katea counter surface, as one JSON object.
///
/// Factored out of `write_hdd_profile_json` so the key list is reachable from a
/// test: `main_test.rs` pins it against `KATEA_COUNTER_KEYS`, which is the only
/// thing that makes "a counter was added but never exported" a test failure
/// rather than a silence.
fn katea_counters_json(k: &izarravm_machine::KateaStorageCounters) -> serde_json::Value {
    json!({
        "sector_reads": k.sector_reads,
        "host_file_reads": k.host_file_reads,
        "host_bytes": k.host_bytes,
        "host_read_operations": k.host_read_operations,
        "host_read_bytes": k.host_read_bytes,
        "host_wall_ns": k.host_wall_ns,
        "host_read_max_ns": k.host_read_max_ns,
        "host_readahead_hits": k.host_readahead_hits,
        "host_readahead_fills": k.host_readahead_fills,
        "host_file_opens": k.host_file_opens,
        "run_scan_steps": k.run_scan_steps,
        "fat_sector_reads": k.fat_sector_reads,
        "dir_or_free_sector_reads": k.dir_or_free_sector_reads,
        "sector_writes": k.sector_writes,
        "int13_read_commands": k.int13_read_commands,
        "int13_read_sectors": k.int13_read_sectors,
        "int13_read_wait_ticks": k.int13_read_wait_ticks,
        "int13_write_commands": k.int13_write_commands,
        "int13_write_sectors": k.int13_write_sectors,
        "int13_write_wait_ticks": k.int13_write_wait_ticks,
        "pio_read_commands": k.pio_read_commands,
        "pio_read_sectors": k.pio_read_sectors,
        "pio_read_wait_ticks": k.pio_read_wait_ticks,
        "pio_write_commands": k.pio_write_commands,
        "pio_write_sectors": k.pio_write_sectors,
        "pio_write_wait_ticks": k.pio_write_wait_ticks,
        "dma_read_commands": k.dma_read_commands,
        "dma_read_sectors": k.dma_read_sectors,
        "dma_read_wait_ticks": k.dma_read_wait_ticks,
        "dma_write_commands": k.dma_write_commands,
        "dma_write_sectors": k.dma_write_sectors,
        "dma_write_wait_ticks": k.dma_write_wait_ticks,
        "overlay_resident_sectors": k.overlay_resident_sectors,
        "overlay_pending_sectors": k.overlay_pending_sectors,
        "pending_unmapped_sectors": k.pending_unmapped_sectors,
        "spill_operations": k.spill_operations,
        "spill_bytes": k.spill_bytes,
        "spill_wall_ns": k.spill_wall_ns,
        "projection_operations": k.projection_operations,
        "projection_bytes": k.projection_bytes,
        "projection_wall_ns": k.projection_wall_ns,
        "projection_max_ns": k.projection_max_ns,
        "metadata_projection_passes": k.metadata_projection_passes,
        "host_write_failures": k.host_write_failures,
        // A gauge: entries the anti-clobber guard is holding right now. A
        // non-zero reading at the end of a run means a file never reached
        // the host folder, which is a criterion that has to be readable.
        "blocked_projection_keys": k.blocked_projection_keys,
        // The mount instrument. Levels, written once before the guest ran, so
        // unlike everything above them they are not a function of this session's
        // guest activity at all -- they price the folder.
        "mount_prime_ns": k.mount_prime_ns,
        "mount_seed_ns": k.mount_seed_ns,
        "mount_total_ns": k.mount_total_ns,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_hdd_profile_json(
    path: &Path,
    workload: &Path,
    mode: GswMode,
    budget: u64,
    wall: std::time::Duration,
    stop_reason: &StopReason,
    timedemo: Option<(u32, u32)>,
    machine: &Machine,
) -> Result<(), Box<dyn Error>> {
    validate_profile_json_parent(path)?;

    let wall_seconds = wall.as_secs_f64();
    let master_ticks = machine.master_ticks();
    let guest_seconds = master_ticks as f64 / MASTER_CLOCK_HZ as f64;
    let perf = machine.cpu().perf_counters();
    let instructions = perf.instructions.max(1);
    let video = machine.video_host_metrics();
    let machine_profile = machine.host_profile_snapshot();
    let machine_phases = machine_profile.phases;
    let classified_wall_ns = machine_phases
        .iter()
        .map(|phase| phase.wall_ns)
        .sum::<u64>();
    let total_wall_ns = wall.as_nanos().min(u128::from(u64::MAX)) as u64;
    let margo_display = machine.margo_display();
    #[allow(unused_mut)]
    let mut report = json!({
        "schema": "izarravm-hdd-profile-v2",
        "workload": workload.display().to_string(),
        "mode": mode.canonical_name(),
        // Guest-clock model version (see `izarravm_cpu::TIMING_MODEL_EPOCH`). Bumped whenever
        // a change alters guest clocks charged per instruction for any persona; rt numbers
        // recorded under different epochs are not comparable as performance.
        "timing_model_epoch": izarravm_cpu::TIMING_MODEL_EPOCH,
        "cycle_budget": budget,
        "stop": stop_reason_json(stop_reason),
        "wall_seconds": wall_seconds,
        "guest_seconds": guest_seconds,
        "real_time_factor": guest_seconds / wall_seconds.max(f64::MIN_POSITIVE),
        "master_ticks": master_ticks,
        "elapsed_budget_clocks": machine.elapsed_clocks(),
        "executed_cpu_core_clocks": machine.cpu().elapsed_clocks,
        "raw_bus_clocks": machine.raw_bus_clocks(),
        "scaled_bus_clocks": machine.scaled_bus_clocks(),
        "instructions_per_host_second": perf.instructions as f64 / wall_seconds.max(f64::MIN_POSITIVE),
        "budget_clocks_per_host_second": machine.elapsed_clocks() as f64 / wall_seconds.max(f64::MIN_POSITIVE),
        "cpu_core_clocks_per_host_second": machine.cpu().elapsed_clocks as f64 / wall_seconds.max(f64::MIN_POSITIVE),
        "direct_native_coverage": perf.jit_direct_insns as f64 / instructions as f64,
        "direct_slow_exits_per_100_instructions": 100.0 * perf.jit_direct_side_exits as f64 / instructions as f64,
        "active_display": format!("{:?}", machine.active_display()),
        "legacy_video_mode": format!("{:?}", machine.active_video_mode()),
        "vga_text_rows_rendered": machine.vga_text_rows_rendered(),
        "margo_display": margo_display.map(|display| json!({
            "mode": format!("0x{:04x}", display.mode),
            "width": display.width,
            "height": display.height,
            "bpp": display.bpp,
            "pitch": display.pitch,
            "start": display.start,
        })),
        "video_host": {
            "margo_lfb_direct_read_bytes": video.margo_lfb_direct_read_bytes,
            "margo_lfb_direct_write_bytes": video.margo_lfb_direct_write_bytes,
            "margo_lfb_slow_read_bytes": video.margo_lfb_slow_read_bytes,
            "margo_lfb_slow_write_bytes": video.margo_lfb_slow_write_bytes,
            "margo_banked_direct_read_bytes": video.margo_banked_direct_read_bytes,
            "margo_banked_direct_write_bytes": video.margo_banked_direct_write_bytes,
            "margo_banked_slow_read_bytes": video.margo_banked_slow_read_bytes,
            "margo_banked_slow_write_bytes": video.margo_banked_slow_write_bytes,
            "margo_scanout_rows_converted": video.margo_scanout_rows_converted,
            "margo_scanout_pixels_converted": video.margo_scanout_pixels_converted,
        },
        "timedemo": timedemo.map(|(gametics, realtics)| json!({
            "gametics": gametics,
            "realtics": realtics,
        })),
        "machine_phase_timing_enabled": machine_profile.machine_phase_timing_enabled,
        "machine_phases": machine_phases.iter().map(|phase| json!({
            "name": phase.name,
            "wall_ns": phase.wall_ns,
            "count": phase.count,
        })).collect::<Vec<_>>(),
        "classified_wall_ns": classified_wall_ns,
        "unattributed_wall_ns": total_wall_ns.saturating_sub(classified_wall_ns),
        "direct_barrier_census": direct_barrier_census_json(
            machine.cpu().direct_barrier_census_snapshot()
        ),
        "phase_marks": phase_mark_series_json(machine.phase_marks()),
        "int13_profile": int13_profile_json(machine.int13_profile()),
        "katea_geometry": machine.katea_geometry_report().map(|g| json!({
            "sectors_per_cluster": g.sectors_per_cluster,
            "fat_sectors": g.fat_sectors,
            "partition_sectors": g.partition_sectors,
            "total_sectors": g.total_sectors,
            "count_of_clusters": g.count_of_clusters,
        })),
        "hdd_sector_cache": machine.hdd_sector_cache_counters().map(|(hits, misses)| json!({
            "hits": hits,
            "misses": misses,
        })),
        "io_stall_ticks": machine.io_stall_ticks(),
        "halted_ticks": machine.halted_ticks(),
        "pit_bulk_advance": pit_bulk_advance_json(machine),
        // The CD columns. `cd_pio_bytes` is the byte sum out of the ATAPI data
        // phases and is invariant to batch geometry, so it is the fidelity
        // falsifier for anything that changes how the host slices a CD read.
        // `cd_accesses` is NOT: it counts device advances that carried bytes and
        // collapses as batches lengthen. `atapi_packet_commands` is the
        // batch-invariant per-command currency (one per 2048-byte sector under
        // TOKACD).
        "cd_pio_bytes": machine.cd_pio_byte_count(),
        "cd_accesses": machine.cd_access_count(),
        "atapi_packet_commands": machine.atapi_packet_command_count(),
        "ata_poll_skip": ata_poll_skip_json(machine),
        "katea": machine.katea_storage_counters().as_ref().map(katea_counters_json),
        "direct_stalls": direct_stall_json(&machine.cpu().direct_stall_snapshot()),
        "vga_wipe_census": vga_wipe_census_json(machine.vga_wipe_census_snapshot()),
        "shadow_l1": shadow_l1_json(machine.shadow_l1_diagnostics()),
        "opl": opl_diagnostics_json(machine.opl_diagnostics(), machine.opl_trace()),
        "sb_dsp": sb_dsp_json(machine.sb_dsp_diagnostics()),
        "mpu": mpu_json(machine.mpu_diagnostics(), machine.midi_trace()),
        "timer": timer_json(machine),
        "perf": bench::perf_counters_json(
            perf,
            machine.cpu().poll_skip_memory(),
            machine.cpu().fast_map_probe_counters(),
            machine.cpu().fast_map_audit_counters(),
            machine.cpu().code_watch_edge_counters(),
            #[cfg(feature = "poll-head-probe")]
            Some(machine.cpu().poll_head_probe()),
            #[cfg(not(feature = "poll-head-probe"))]
            None,
        ),
    });
    #[cfg(feature = "direct-link-refusal-census")]
    {
        report["direct_link_refusal_census"] =
            direct_link_refusal_census_json(machine.cpu().direct_link_refusal_census_snapshot());
    }
    #[cfg(feature = "reflected-call-diagnostic")]
    {
        report["reflected_call_diagnostic"] =
            reflected_call_diagnostic_json(machine.cpu().reflected_call_diagnostic_snapshot());
    }
    // Slice 1 (record-and-measure): additive, unconditional on both arms of the KNOB within
    // this build (plan section 7.4) -- all-zero / `{"armed":false}` when
    // `IZARRAVM_REFLECTED_CALL_MEMO` is unset. Gated under `feature = "reflected-call-memo"`
    // (Fable review 2026-09-03), same shape as `reflected_call_diagnostic` above: a plain
    // build carries neither the key nor the module.
    #[cfg(feature = "reflected-call-memo")]
    {
        let raw = izarravm_cpu::reflected_call_memo_json(machine.cpu());
        report["reflected_call_memo"] =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw));
    }
    #[cfg(feature = "direct-callout-attribution")]
    {
        report["direct_callout_attribution"] =
            direct_callout_attribution_json(machine.cpu().direct_callout_attribution_snapshot());
    }
    #[cfg(feature = "smc-census")]
    {
        report["smc_census"] = smc_census_json(machine.cpu().direct_smc_census_snapshot(), perf);
    }
    // Stage 0 of the far-return slice (design §5.0a). A SIBLING top-level object for the same
    // reason the entry-attribution block below is one: `perf_counters_json`'s ordered key list
    // must stay identical between the plain and the instrumented build.
    // L9 stage 0, THROWAWAY. A SIBLING top-level object for the same reason the two blocks around
    // it are siblings: `perf_counters_json`'s ordered key list must read the same in a plain build
    // and an instrumented one.
    #[cfg(feature = "seg-head-diagnostic")]
    {
        let stalls = machine.cpu().direct_stall_snapshot();
        report["seg_head_diagnostic"] = serde_json::json!({
            "entries": stalls.seg_head_diagnostic_entries,
            "successor_pins_written": stalls.seg_head_successor_pins_written,
            "successor_absent": stalls.seg_head_successor_absent,
            "selector_repeat": stalls.seg_head_selector_repeat,
            "dirty_multi": stalls.seg_head_dirty_multi,
            "eligible": stalls.seg_head_eligible,
            "eligible_successor_pins_written": stalls.seg_head_eligible_successor_pins_written,
            "eligible_successor_absent": stalls.seg_head_eligible_successor_absent,
            "eligible_selector_repeat": stalls.seg_head_eligible_selector_repeat,
            "segment_write_block_head_entries": stalls.segment_write_block_head_entries,
        });
    }
    #[cfg(feature = "retf-arity-census")]
    if let Some((histogram, sites)) = machine.cpu().retf_arity_snapshot() {
        report["retf_arity_census"] = serde_json::json!({
            // Cell n = RETF EXECUTIONS taken at sites with exactly n distinct targets; the last
            // cell is the saturated sites. Execution-weighted, never site-weighted.
            "executions_by_distinct_targets": histogram.to_vec(),
            "sites": sites,
        });
    }
    // Census precedent, deliberately (design H7): a SIBLING top-level object, emitted OUTSIDE
    // `perf_counters_json`, so that function's signature and ordered key list stay identical
    // between the plain and the observer build and
    // `perf_counter_json_exposes_the_complete_counter_surface` passes in both.
    #[cfg(feature = "direct-entry-attribution")]
    {
        report["direct_entry_attribution"] = direct_entry_attribution_json(
            machine.cpu().direct_entry_attribution_snapshot(),
            machine
                .cpu()
                .direct_stall_snapshot()
                .decode_pack_late_view_miss,
        );
    }
    std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

/// Guest OPL activity for the run. `key_on_writes` against
/// `status_reads_timer_expired` is what this exists for: silent music with
/// key-ons present means notes were struck and lost downstream, while silent
/// music with polls but no expiries means the driver's tick never fired.
fn opl_diagnostics_json(
    opl: izarravm_machine::OplDiagnostics,
    trace: &[izarravm_machine::OplTraceEntry],
) -> serde_json::Value {
    json!({
        "register_writes": opl.register_writes,
        "status_reads": opl.status_reads,
        "status_reads_timer_expired": opl.status_reads_timer_expired,
        "timer_control_writes": opl.timer_control_writes,
        "key_on_writes": opl.key_on_writes,
        "key_off_writes": opl.key_off_writes,
        "trace": trace.iter().map(|e| json!({
            "w": e.write,
            "port": e.port,
            "bank": e.bank,
            "reg": e.register,
            "val": e.value,
            "clk": e.core_clocks,
            "us": e.pending_micros,
        })).collect::<Vec<_>>(),
    })
}

/// OFF-FEATURE: byte-identical to pre-S1a main. S1 (`dev_docs/2026-09-01-bus-clock-diag.md`)
/// shadow-tag probe counters: hit/miss per access class against the real 486 L1 geometry
/// (8 KiB, 4-way, 16-byte lines, 128 sets, pseudo-LRU), charging nothing. `enabled` is false
/// unless `IZARRAVM_SHADOW_L1_PROBE=1`; every count is 0 when disabled, and `hit_ratio`
/// reports 0.0 on an empty class rather than dividing by zero. Coverage is the interpreter /
/// FastMap-direct access stream only -- see the `shadow_cache` module doc in
/// `izarravm-machine` for what the JIT's bulk per-block charges leave uncovered.
#[cfg(not(feature = "shadow-cache-probe"))]
fn shadow_l1_json(diag: izarravm_machine::ShadowL1Diagnostics) -> serde_json::Value {
    fn class_json(counts: izarravm_machine::ShadowClassCounts) -> serde_json::Value {
        json!({
            "hits": counts.hits,
            "misses": counts.misses,
            "accesses": counts.accesses(),
            "hit_ratio": counts.hit_ratio(),
        })
    }
    json!({
        "enabled": diag.enabled,
        "code_fetch": class_json(diag.code_fetch),
        "data_read": class_json(diag.data_read),
        "data_write": class_json(diag.data_write),
    })
}

/// S1a-d (`dev_docs/2026-09-05-s2-cache-differentiation-design.md`) shadow-tag probe
/// counters: hit/miss per access class against the real per-persona L1 geometry (486: 8 KiB
/// unified, 4-way, 16-byte lines, 128 sets, pseudo-LRU, write-through; 586: split 16+16 KiB,
/// 2-way, 32-byte lines, 256 sets, 1-bit LRU, write-back with a 512 KiB / 32-byte / 4-way /
/// 4096-set L2), charging nothing. `enabled` is false unless `IZARRAVM_SHADOW_L1_PROBE=1`;
/// every count is 0 when disabled, and `hit_ratio` reports 0.0 on an empty class rather than
/// dividing by zero. Coverage: the interpreter / FastMap-direct access stream, PLUS (S1b) the
/// JIT's per-block code-fetch stream once armed, PLUS (S1d slice 2) the DATA accesses of every
/// Kth JIT block entry diverted to the interpreter (`sample_k`/`total_entries`/
/// `sampled_entries`/`sampled_fraction`) -- see the `shadow_cache` module doc in
/// `izarravm-machine` for what remains unprobed (the un-sampled 1-1/K of JIT data accesses).
#[cfg(feature = "shadow-cache-probe")]
fn shadow_l1_json(diag: izarravm_machine::ShadowL1Diagnostics) -> serde_json::Value {
    fn class_json(counts: izarravm_machine::ShadowClassCounts) -> serde_json::Value {
        json!({
            "hits": counts.hits,
            "misses": counts.misses,
            "accesses": counts.accesses(),
            "hit_ratio": counts.hit_ratio(),
            "miss_ratio": counts.miss_ratio(),
        })
    }
    fn level_json(level: izarravm_machine::ShadowLevelDiagnostics) -> serde_json::Value {
        json!({
            "code_fetch": class_json(level.code_fetch),
            "data_read": class_json(level.data_read),
            "data_write": class_json(level.data_write),
            "write_back_victims": level.write_back_victims,
        })
    }
    json!({
        "enabled": diag.enabled,
        "persona": diag.persona.map(|p| format!("{p:?}")),
        "code_fetch": class_json(diag.code_fetch),
        "data_read": class_json(diag.data_read),
        "data_write": class_json(diag.data_write),
        "write_back_victims": diag.write_back_victims,
        "l2": level_json(diag.l2),
        "sample_k": diag.sample_k,
        "total_entries": diag.total_entries,
        "sampled_entries": diag.sampled_entries,
        "sampled_fraction": diag.sampled_fraction(),
    })
}

/// The guest timer-clock block: IRQ0 edges delivered to the PIC, SB16 IRQ
/// requests, guest PIT writes, and the `IZARRAVM_PIT_TRACE` series (real
/// ports 0x40-0x43 plus pseudo-ports 0xF0 = IRQ0 edge, 0xF1 = SB IRQ).
fn timer_json(machine: &izarravm_machine::Machine) -> serde_json::Value {
    let (irq0_edges, sb_irq_requests, pit_writes) = machine.timer_diagnostics();
    json!({
        "irq0_edges": irq0_edges,
        "sb_irq_requests": sb_irq_requests,
        "pit_writes": pit_writes,
        "pit_trace": machine.pit_write_trace().iter().map(|e| json!({
            "port": e.port,
            "val": e.value,
            "tick": e.master_ticks,
        })).collect::<Vec<_>>(),
    })
}

/// Guest MPU-401 activity for both parts, plus the `IZARRAVM_MIDI_TRACE`
/// access trace. Which part carries traffic says which music device the game
/// drives; the trace's tick spacing says the tempo it drives it at.
/// `output_messages`/`output_bytes` count at the GUI drain and read 0 in a
/// headless profile -- see `MpuDiagnostics::output_messages`.
fn mpu_json(
    (wavetable, midi): (
        izarravm_machine::MpuDiagnostics,
        izarravm_machine::MpuDiagnostics,
    ),
    trace: &[izarravm_machine::MidiTraceEntry],
) -> serde_json::Value {
    fn one(d: izarravm_machine::MpuDiagnostics) -> serde_json::Value {
        json!({
            "command_writes": d.command_writes,
            "data_writes": d.data_writes,
            "data_reads": d.data_reads,
            "status_reads": d.status_reads,
            "uart_enters": d.uart_enters,
            "resets": d.resets,
            "start_playbacks": d.start_playbacks,
            "stop_playbacks": d.stop_playbacks,
            "output_messages": d.output_messages,
            "output_bytes": d.output_bytes,
        })
    }
    json!({
        "wavetable": one(wavetable),
        "midi": one(midi),
        "trace": trace.iter().map(|e| json!({
            "k": match e.kind {
                izarravm_machine::MidiTraceKind::CommandWrite => "cmd",
                izarravm_machine::MidiTraceKind::DataWrite => "data",
                izarravm_machine::MidiTraceKind::DataRead => "read",
                izarravm_machine::MidiTraceKind::Output => "out",
            },
            "wt": e.wavetable,
            "val": e.value,
            "tick": e.master_ticks,
        })).collect::<Vec<_>>(),
    })
}

/// Guest Sound Blaster DSP activity. `reset_acknowledges` against `resets` is
/// what says whether the guest ever found the card at all.
fn sb_dsp_json(sb: izarravm_machine::SbDspDiagnostics) -> serde_json::Value {
    json!({
        "resets": sb.resets,
        "reset_acknowledges": sb.reset_acknowledges,
        "command_bytes": sb.command_bytes,
        "data_reads": sb.data_reads,
        "status_reads": sb.status_reads,
    })
}

/// The VGA direct-write-token wipe attribution, or `null` when `IZARRAVM_VGA_WIPE_CENSUS` was not
/// set. Transitions and gap buckets are emitted as sparse lists so a run that never moved the token
/// does not carry a 64-cell matrix of zeros.
fn vga_wipe_census_json(
    snapshot: Option<izarravm_machine::VgaWipeCensusSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    json!({
        "events": snapshot.events,
        "key_overflow": snapshot.key_overflow,
        "applies": snapshot.applies,
        "applies_same_token": snapshot.applies_same_token,
        "rows": snapshot.rows.iter().map(|row| json!({
            "port": format!("0x{:03X}", row.port),
            "selector": format!("0x{:02X}", row.selector),
            "value": format!("0x{:02X}", row.value),
            "count": row.count,
        })).collect::<Vec<_>>(),
        "transitions": snapshot.transitions.iter().enumerate().flat_map(|(before, row)| {
            row.iter().copied().enumerate().filter(|&(_, count)| count != 0).map(move |(after, count)| {
                json!({ "before": before, "after": after, "count": count })
            }).collect::<Vec<_>>()
        }).collect::<Vec<_>>(),
        "gap_buckets": snapshot.gap_buckets.iter().copied().enumerate()
            .filter(|&(_, count)| count != 0)
            .map(|(bucket, count)| json!({
                "min_instructions": 1u64 << bucket,
                "count": count,
            })).collect::<Vec<_>>(),
    })
}

/// The entry-attribution observer's sibling object.
///
/// Every phase carries BOTH `ticks_raw` (no subtraction of any kind) and `ticks_corrected`
/// (`ticks_raw - overhead x marks`, allowed to go NEGATIVE and reported as such), because the
/// aggregate subtraction is a modelling step and the reader has to be able to undo it. A phase
/// whose corrected value lands within one `resolution_floor_ns` per mark of zero is reported
/// `below_resolution: true` and never as "0" — design section 4 prices two phases at or under the
/// floor, and the instrument must say so rather than report them free.
///
/// The section 6 fit is NOT computed here: the bins are exported and
/// `.bench/scripts/entry-attribution-report.py` does the weighted least squares, the CIs and the
/// condition number. One implementation of the numerics, in the place that also prints the
/// pre-registered verdict.
#[cfg(feature = "direct-entry-attribution")]
fn direct_entry_attribution_json(
    snapshot: Option<izarravm_cpu::DirectEntryAttributionSnapshot>,
    decode_pack_late_view_miss: u64,
) -> serde_json::Value {
    use izarravm_cpu::{
        COMPILE_SITES, FALLBACK_TAG_NAMES, LANE_NAMES, OUTLIER_TICKS, P0_MARK_LINE, PHASE_NAMES,
        POPULATION_NAMES, PRE_P0_REFUSAL_SITES, REFUSAL_SITES, native_bin_parts,
    };
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    let tsc_hz = snapshot.tsc_hz as f64;
    // `tsc_hz` is derived from an `Instant` pair across the run and is zero only if the run was
    // too short to measure or the TSC did not advance. `serde_json` cannot encode a NaN -- it
    // serialises as `null` and every downstream ratio becomes unreadable -- so an unusable clock
    // yields 0.0 and the `tsc_hz: 0` field is what tells the reader the ns columns are void.
    let ns_of = |ticks: f64| {
        if tsc_hz > 0.0 {
            ticks / tsc_hz * 1e9
        } else {
            0.0
        }
    };
    let floor_ns = ns_of(snapshot.overhead_ticks as f64);
    let lanes = LANE_NAMES
        .iter()
        .enumerate()
        .map(|(lane, name)| {
            let phases = PHASE_NAMES
                .iter()
                .enumerate()
                .map(|(phase, phase_name)| {
                    let raw = snapshot.ticks_raw[lane][phase];
                    let marks = snapshot.marks[lane][phase];
                    let corrected =
                        raw as i128 - (snapshot.overhead_ticks as i128) * (marks as i128);
                    let ns = ns_of(corrected as f64);
                    let per_mark_ns = if marks == 0 { 0.0 } else { ns / marks as f64 };
                    json!({
                        "phase": phase_name,
                        "ticks_raw": raw,
                        "marks": marks,
                        "ticks_corrected": corrected as f64,
                        "ns": ns,
                        "ns_per_mark": per_mark_ns,
                        "below_resolution": marks > 0 && per_mark_ns.abs() <= floor_ns,
                    })
                })
                .collect::<Vec<_>>();
            let totals = POPULATION_NAMES
                .iter()
                .enumerate()
                .map(|(index, population)| {
                    json!({
                        "population": population,
                        "ticks": snapshot.totals[lane][index],
                        "ns": ns_of(snapshot.totals[lane][index] as f64),
                        "ends": snapshot.totals_count[lane][index],
                    })
                })
                .collect::<Vec<_>>();
            let refusal_site = REFUSAL_SITES
                .iter()
                .enumerate()
                .filter(|&(index, _)| snapshot.refusal_site[lane][index] != 0)
                .map(|(index, (label, line))| {
                    json!({
                        "index": index,
                        "site": label,
                        "run_rs_line": line,
                        "count": snapshot.refusal_site[lane][index],
                        "above_p0_mark": PRE_P0_REFUSAL_SITES.contains(&index),
                    })
                })
                .collect::<Vec<_>>();
            let compile_site = COMPILE_SITES
                .iter()
                .enumerate()
                .map(|(index, (label, line))| {
                    json!({
                        "index": index,
                        "site": label,
                        "run_rs_line": line,
                        "count": snapshot.compile_site[lane][index],
                    })
                })
                .collect::<Vec<_>>();
            let fallback_tags = FALLBACK_TAG_NAMES
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    json!({ "tag": label, "count": snapshot.fallback_tags[lane][index] })
                })
                .collect::<Vec<_>>();
            let native_bins = snapshot.native_bins[lane]
                .iter()
                .enumerate()
                .filter(|&(_, row)| row[0] != 0)
                .map(|(bin, row)| {
                    // The instrument's own inverse, never a second copy of the packing.
                    let (instructions, hop_bin, self_loop) = native_bin_parts(bin);
                    json!({
                        "bin": bin,
                        "instructions": instructions,
                        "linked_transfers_class": hop_bin,
                        "self_loop": self_loop,
                        "count": row[0],
                        "ticks": row[1],
                        "instructions_sum": row[2],
                        "linked_transfers_sum": row[3],
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "lane": name,
                "lane_index": lane,
                "phases": phases,
                "totals": totals,
                "refusal_site": refusal_site,
                "compile_site": compile_site,
                "fallback_tags": fallback_tags,
                "native_bins": native_bins,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "arm": snapshot.arm,
        "sample_n": snapshot.sample_n,
        "overhead_ticks": snapshot.overhead_ticks,
        "calibration_bracket_ticks": snapshot.calibration_bracket_ticks,
        "calibration_mark_ticks": snapshot.calibration_mark_ticks,
        "resolution_floor_ns": floor_ns,
        "tsc_hz": snapshot.tsc_hz,
        "outlier_clamp_ticks": OUTLIER_TICKS,
        "outlier_ticks": snapshot.outlier_ticks,
        "outlier_marks": snapshot.outlier_marks,
        "lane_pin_mismatches": snapshot.lane_pin_mismatches,
        // A3's `marks(P0) == decode_probes` identity needs every traversal that counted a probe
        // and never reached the P0 mark. Four break sites in the continuation loop's `screened`
        // match sit ABOVE the `ea_begin!` call and are the three `brk_cont_*` keys (the
        // fetch-limit break shares `brk_cont_not_continuable` with the plain not-continuable one);
        // a FIFTH site, the late decode-cache view miss in the interpreted fallback below
        // `dispatch_continuation`, reuses `brk_cont_decode_miss` and sits BELOW `ea_begin!`.
        // `note_decode_pack_late_view_miss` actually has TWO feed sites: one inside the
        // non-continuable break arm ABOVE `ea_begin!` (which does not touch `brk_cont_decode_miss`
        // at all) and this one BELOW it. The subtraction below is sound only because the ABOVE
        // feed is argued to be identically zero elsewhere; if it ever moves, this field stops
        // reflecting the whole late-view-miss population. Must be subtracted back out of the
        // identity, or it over-counts by exactly this many. Emitted here so the report does not
        // have to reach into another object for it.
        "decode_pack_late_view_miss": decode_pack_late_view_miss,
        // The `run.rs` line the P0 mark sits on -- the line `above_p0_mark` partitions the
        // refusal sites by, published so the reader is not left inferring it.
        "p0_mark_line": P0_MARK_LINE,
        "phase_names": PHASE_NAMES,
        "lane_names": LANE_NAMES,
        "population_names": POPULATION_NAMES,
        "lanes": lanes,
    })
}

fn direct_barrier_census_json(
    snapshot: Option<izarravm_cpu::DirectBarrierCensusSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    let report = json!({
        "rows": snapshot.rows.iter().map(direct_barrier_census_row_json).collect::<Vec<_>>(),
        "unbound_targets": snapshot
            .unbound_targets
            .iter()
            .map(|(label, count)| json!({ "kind": label, "count": count }))
            .collect::<Vec<_>>(),
        "dynamic_miss_targets": snapshot
            .dynamic_miss_targets
            .iter()
            .map(|(label, count)| json!({ "kind": label, "count": count }))
            .collect::<Vec<_>>(),
    });
    // Inserted AFTER the `json!` under cfg, following the poll-head-probe precedent: emitting the
    // keys as `null` when the feature is off would silently change every profile JSON the campaign
    // diffs against, which is worse than a broken pin because nothing announces it.
    #[cfg(feature = "barrier-census-closure")]
    let report = {
        let mut report = report;
        report["closure"] = json!({
            "classified_static": snapshot.classified_static,
            "static_unbound_exits": snapshot.static_unbound_exits,
            // Must be zero. Nonzero means the census saw fewer exits than the counter did, which
            // is a mid-run arm or a classifier gap, never a fact about the guest.
            "unattributed_static": snapshot
                .static_unbound_exits
                .saturating_sub(snapshot.classified_static),
            "classified_dynamic": snapshot.classified_dynamic,
            "dynamic_miss_exits": snapshot.dynamic_miss_exits,
            "unattributed_dynamic": snapshot
                .dynamic_miss_exits
                .saturating_sub(snapshot.classified_dynamic),
            "rejected_unattributed": snapshot.rejected_unattributed,
            "dynamic_rejected_unattributed": snapshot.dynamic_rejected_unattributed,
            "rejected_barrier_overwrites": snapshot.rejected_barrier_overwrites,
        });
        report
    };
    // B.3's dormant-heat histogram, a SIBLING block to `closure` rather than more keys inside it:
    // the closure block is pinned key-for-key by
    // `direct_barrier_census_json_exposes_the_closure_block`, and that pin is the thing that makes
    // a silent schema drift fail loudly. Growing it would spend the pin instead of using it.
    #[cfg(feature = "barrier-census-closure")]
    let report = {
        let mut report = report;
        let class_of = |targets: &[(&'static str, u64)], want: &str| {
            targets
                .iter()
                .find(|(label, _)| *label == want)
                .map_or(0, |(_, count)| *count)
        };
        let class = |targets: &[(&'static str, u64)]| class_of(targets, "dormant_heat");
        let head_static: u64 = snapshot
            .dormant_heat_sites
            .iter()
            .map(|site| site.static_exits)
            .sum();
        let head_dynamic: u64 = snapshot
            .dormant_heat_sites
            .iter()
            .map(|site| site.dynamic_exits)
            .sum();
        report["dormant_heat"] = json!({
            // The C3 identity, readable inside ONE object: head + truncated tail must equal the
            // class total on each lane. A nonzero difference is an instrument defect, never a
            // fact about the guest.
            "class_static": class(&snapshot.unbound_targets),
            "class_dynamic": class(&snapshot.dynamic_miss_targets),
            "head_static": head_static,
            "head_dynamic": head_dynamic,
            "truncated_static": snapshot.dormant_heat_truncated_static,
            "truncated_dynamic": snapshot.dormant_heat_truncated_dynamic,
            "unattributed_static": class(&snapshot.unbound_targets)
                .saturating_sub(head_static + snapshot.dormant_heat_truncated_static),
            "unattributed_dynamic": class(&snapshot.dynamic_miss_targets)
                .saturating_sub(head_dynamic + snapshot.dormant_heat_truncated_dynamic),
            "distinct_sites": snapshot.dormant_heat_distinct_sites,
            "walked_entries_run_wide": snapshot.walked_entries_run_wide,
            // Hex, per the `vga_wipe_census_json` port/selector/value precedent. A guest linear
            // read as decimal is unusable: the reader's next move is to cross it against a map
            // file, a disassembly or the SMC shape table, all of which are hex, and 141.7M exits'
            // worth of addresses is not a place to make them convert by hand.
            "sites": snapshot.dormant_heat_sites.iter().map(|site| json!({
                "linear": format!("0x{:08X}", site.linear),
                "static_exits": site.static_exits,
                "dynamic_exits": site.dynamic_exits,
                "compile_walked": site.compile_walked,
                "imm_lane_matched": site.imm_lane_matched,
                "disp_lane_matched": site.disp_lane_matched,
            })).collect::<Vec<_>>(),
        });
        // The `Rejected` twin. Same block shape, same closure identity, and it locates the largest
        // pool on the board that still had no addresses.
        let rejected_head_static: u64 = snapshot
            .rejected_sites
            .iter()
            .map(|site| site.static_exits)
            .sum();
        let rejected_head_dynamic: u64 = snapshot
            .rejected_sites
            .iter()
            .map(|site| site.dynamic_exits)
            .sum();
        report["rejected_sites"] = json!({
            "class_static": class_of(&snapshot.unbound_targets, "rejected"),
            "class_dynamic": class_of(&snapshot.dynamic_miss_targets, "rejected"),
            "head_static": rejected_head_static,
            "head_dynamic": rejected_head_dynamic,
            "truncated_static": snapshot.rejected_truncated_static,
            "truncated_dynamic": snapshot.rejected_truncated_dynamic,
            "unattributed_static": class_of(&snapshot.unbound_targets, "rejected")
                .saturating_sub(rejected_head_static + snapshot.rejected_truncated_static),
            "unattributed_dynamic": class_of(&snapshot.dynamic_miss_targets, "rejected")
                .saturating_sub(rejected_head_dynamic + snapshot.rejected_truncated_dynamic),
            "distinct_sites": snapshot.rejected_distinct_sites,
            "sites": snapshot.rejected_sites.iter().map(|site| json!({
                "linear": format!("0x{:08X}", site.linear),
                "static_exits": site.static_exits,
                "dynamic_exits": site.dynamic_exits,
                "compile_walked": site.compile_walked,
                "imm_lane_matched": site.imm_lane_matched,
                "disp_lane_matched": site.disp_lane_matched,
            })).collect::<Vec<_>>(),
        });
        // L7 DIAGNOSTIC (2026-08-31). The `Absent` twin.
        let absent_head_static: u64 = snapshot
            .absent_sites
            .iter()
            .map(|site| site.static_exits)
            .sum();
        let absent_head_dynamic: u64 = snapshot
            .absent_sites
            .iter()
            .map(|site| site.dynamic_exits)
            .sum();
        report["absent_sites"] = json!({
            "class_static": class_of(&snapshot.unbound_targets, "absent"),
            "class_dynamic": class_of(&snapshot.dynamic_miss_targets, "absent"),
            "head_static": absent_head_static,
            "head_dynamic": absent_head_dynamic,
            "truncated_static": snapshot.absent_truncated_static,
            "truncated_dynamic": snapshot.absent_truncated_dynamic,
            "unattributed_static": class_of(&snapshot.unbound_targets, "absent")
                .saturating_sub(absent_head_static + snapshot.absent_truncated_static),
            "unattributed_dynamic": class_of(&snapshot.dynamic_miss_targets, "absent")
                .saturating_sub(absent_head_dynamic + snapshot.absent_truncated_dynamic),
            "distinct_sites": snapshot.absent_distinct_sites,
            "sites": snapshot.absent_sites.iter().map(|site| json!({
                "linear": format!("0x{:08X}", site.linear),
                "static_exits": site.static_exits,
                "dynamic_exits": site.dynamic_exits,
                "compile_walked": site.compile_walked,
            })).collect::<Vec<_>>(),
        });
        report
    };
    #[cfg(feature = "direct-admission-census")]
    let report = {
        let mut report = report;
        report["admission_declines"] = serde_json::Value::from_iter(
            snapshot
                .admission_declines
                .iter()
                .map(|(label, count)| json!({ "kind": label, "count": count })),
        );
        report
    };
    // The `RetryCause::AdmissionMatrix` per-reason split (design note
    // `dev_docs/descent2-l1-stack-width-design-2026-08-30.md` §2(H)): the two `stack_width_kind`
    // producers of that cause, told apart. `NEW BLOCK`, not folded into `admission_declines`
    // above -- that array means something else (dispatcher-entry declines) and the corpus already
    // carries legs on disk that read it.
    #[cfg(feature = "direct-admission-census")]
    let report = {
        let mut report = report;
        report["stack_width_declines"] = json!({
            "reasons": snapshot
                .stack_width_declines
                .iter()
                .map(|(label, count)| json!({ "kind": label, "count": count }))
                .collect::<Vec<_>>(),
            "distinct_sites": snapshot.stack_width_decline_distinct_sites,
            // Hex, matching `dormant_heat_sites` / `rejected_sites`: the reader's next move is a
            // disassembly or map-file cross-reference, both hex.
            "sites": snapshot.stack_width_decline_sites.iter().map(|site| json!({
                "linear": format!("0x{:08X}", site.linear),
                "reason": site.reason,
                "count": site.count,
            })).collect::<Vec<_>>(),
        });
        report
    };
    report
}

#[cfg(feature = "direct-link-refusal-census")]
fn direct_link_refusal_census_json(
    snapshot: Option<izarravm_cpu::DirectLinkRefusalCensusSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    json!({
        "seen": snapshot.seen,
        "missing_id": snapshot.missing_id,
        "invalid_id": snapshot.invalid_id,
        "guard_mask_popcount_histogram": snapshot.guard_mask_popcount_histogram,
        "rows": snapshot.rows.iter().map(|row| json!({
            "id": row.id,
            "source": {
                "linear": row.source_linear,
                "physical": row.source_physical,
                "mode_key": row.source_mode_key,
                "generation": row.source_generation,
            },
            "slot": row.slot,
            "target": {
                "linear": row.target_linear,
                "mode_key": row.target_mode_key,
                "last_attempted_generation": row.last_target_generation,
            },
            "state": row.state,
            "unbound_exits": row.unbound_exits,
            "buckets": row.buckets.iter().map(|(kind, count)| json!({
                "kind": kind,
                "count": count,
            })).collect::<Vec<_>>(),
            "last_guard_mask_popcount": row.last_guard_mask_popcount,
        })).collect::<Vec<_>>(),
    })
}

/// Slice 0b of the reflected-call HLE design's trip-shape instrument
/// (`dev_docs/2026-09-03-reflected-call-hle-design.md`,
/// `dev_docs/2026-09-03-reflected-call-hle-review.md`,
/// `dev_docs/2026-09-04-reflected-call-slice0b-plan.md` §9's key list).
/// Additive over slice 0's schema: no `izarravm-hdd-profile-v2` bump, no
/// `KATEA_COUNTER_KEYS` change (plan §8's file-list note).
#[cfg(feature = "reflected-call-diagnostic")]
fn reflected_call_diagnostic_json(
    snapshot: Option<izarravm_cpu::ReflectedCallDiagnosticSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    fn stat_json(s: &izarravm_cpu::StatSummary) -> serde_json::Value {
        json!({
            "min": s.min,
            "median": s.median,
            "max": s.max,
            "mean": s.mean,
            "sample_count": s.sample_count,
        })
    }
    json!({
        "mode": snapshot.mode,
        "trips_total": snapshot.trips_total,
        "trips_unmatched": snapshot.trips_unmatched,
        "probe_ns_per_read": snapshot.probe_ns_per_read,
        "compare_ns_per_read": snapshot.compare_ns_per_read,
        "window_start_insns": snapshot.window_start_insns,
        "window_end_insns": snapshot.window_end_insns,
        "keys": snapshot.keys.iter().map(|k| json!({
            "vector": format!("0x{:02x}", k.vector),
            "ah": format!("0x{:02x}", k.ah),
            "trips": k.trips,
            "unmatched": k.unmatched,
            "closed_by": k.closed_by.iter().map(|(name, n)| json!({
                "rule": name,
                "count": n,
            })).collect::<Vec<_>>(),
            "instructions": stat_json(&k.instructions),
            "core_clocks": stat_json(&k.core_clocks),
            "dispatcher_entries": stat_json(&k.dispatcher_entries),
            "cr3_writes_per_trip": stat_json(&k.cr3_writes),
            "distinct_entry_images": k.distinct_entry_images,
            "distinct_cs_eip": k.distinct_cs_eip,
            "varying_fields": k.varying_fields,
            "entry_image_top16": k.entry_image_top16_trips,
            "entry_image_cum_share_1": k.entry_image_cum_share_1,
            "entry_image_cum_share_4": k.entry_image_cum_share_4,
            "entry_image_cum_share_8": k.entry_image_cum_share_8,
            "entry_image_cum_share_16": k.entry_image_cum_share_16,
            "nested_int_trips": k.nested_int_trips,
            "far_transfer_trips": k.far_transfer_trips,
            "hw_edge_trips": k.hw_edge_trips,
            "gp_trap_trips": k.gp_trap_trips,
            "pf_trap_trips": k.pf_trap_trips,
            "rdtsc_trips": k.rdtsc_trips,
            "port_io_trips": k.port_io_trips,
            "x87_trips": k.x87_trips,
            "task_switch_trips": k.task_switch_trips,
            "task_gate_trips": k.task_gate_trips,
            "task_gate_unknown_trips": k.task_gate_unknown_trips,
            "bda_key_pending_trips": k.bda_key_pending_trips,
            "bda_no_key_trips": k.bda_no_key_trips,
            "bda_unknown_trips": k.bda_unknown_trips,
            "near_match_diffs": k.near_match_diffs.iter().map(|(name, far_return, far_transfer)| json!({
                "field": name,
                "far_return": far_return,
                "far_transfer": far_transfer,
            })).collect::<Vec<_>>(),
            "near_match_cs_eip": k.near_match_cs_eip.iter().map(|(name, far_return, far_transfer)| json!({
                "field": name,
                "far_return": far_return,
                "far_transfer": far_transfer,
            })).collect::<Vec<_>>(),
            "cs_base_differed_on_match": k.cs_base_differed_on_match,
            "modes_seen": k.modes_seen_any.iter().map(|(name, n)| json!({
                "mode": name,
                "trips": n,
            })).collect::<Vec<_>>(),
            "mode_defect_trips": k.mode_defect_trips,
            "batch_straddle_trips": k.batch_straddle_trips,
            "soft_int_posts": k.soft_int_posts,
            "clock_charge_events": stat_json(&k.clock_charge_events),
            "trips_in_window": k.trips_in_window,
            "read_set_size": stat_json(&k.read_set_size),
            "read_set_size_windowed": stat_json(&k.read_set_size_windowed),
            "read_set_size_physical": stat_json(&k.read_set_size_physical),
            "read_set_size_physical_windowed": stat_json(&k.read_set_size_physical_windowed),
            "write_set_size": stat_json(&k.write_set_size),
            "write_set_size_windowed": stat_json(&k.write_set_size_windowed),
            "translation_set_size": stat_json(&k.translation_set_size),
            "translation_set_size_windowed": stat_json(&k.translation_set_size_windowed),
            "translation_set_over_cap": k.translation_set_over_cap,
            "stack_segments_over_cap_trips": k.stack_segments_over_cap_trips,
            "entry_images_over_cap": k.entry_images_over_cap,
            "write_addresses_over_cap": k.write_addresses_over_cap,
            "class_n_addresses_over_cap": k.class_n_addresses_over_cap,
            "trips_reads_over_cap": k.trips_reads_over_cap,
            "trips_writes_over_cap": k.trips_writes_over_cap,
            "reads_total": k.reads_total,
            "reads_under_other_cr3": k.reads_under_other_cr3,
            "read_class_counts": k.read_class_counts.iter().map(|(name, n)| json!({
                "class": name,
                "count": n,
            })).collect::<Vec<_>>(),
            "write_class_counts": k.write_class_counts.iter().map(|(name, n)| json!({
                "class": name,
                "count": n,
            })).collect::<Vec<_>>(),
            "write_class_r": k.write_class_r,
            "write_class_d": k.write_class_d,
            "write_class_n": k.write_class_n,
            "write_class_n_trips": k.write_class_n_trips,
            "write_unknown_pre": k.write_unknown_pre,
            "write_dead_8kb": k.write_dead_8kb,
            "write_not_plain_ram": k.write_not_plain_ram,
            "top_write_addresses": k.top_write_addresses.iter().map(|(addr, count)| json!({
                "address": format!("0x{addr:08x}"),
                "count": count,
            })).collect::<Vec<_>>(),
            "class_n_addresses": k.class_n_addresses.iter().map(|(addr, class, count)| json!({
                "address": format!("0x{addr:08x}"),
                "class": class,
                "count": count,
            })).collect::<Vec<_>>(),
            "warm_clock_samples": k.warm_clock_samples,
            "warm_clock_distinct": k.warm_clock_distinct,
            "warm_clock_longest_run": k.warm_clock_longest_run,
        })).collect::<Vec<_>>(),
    })
}

/// Stage A of the SMC census. Design `dev_docs/smc-census-design.md` §3/§4/§8 layer 1, cut by the
/// adversarial review of 2026-08-15.
///
/// The closure asserts below are the instrument's Gate D: an instrument that does not close is
/// not evidence, so a mismatch panics rather than warning. They are `assert!`, not
/// `debug_assert!`, and they run on the release binary that produces the census.
#[cfg(feature = "smc-census")]
fn smc_census_json(
    snapshot: Option<izarravm_cpu::DirectSmcCensusSnapshot>,
    perf: &izarravm_cpu::PerfCounters,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    let whole = &snapshot.whole_run;
    let units = whole.units;

    // Closures against the always-on production counters. These are the four the design names.
    assert_eq!(
        units.scan_calls, perf.smc_scan_calls,
        "SMC census scan calls did not close against perf.smc_scan_calls"
    );
    assert_eq!(
        units.keys_scanned, perf.smc_scan_keys,
        "SMC census scanned keys did not close against perf.smc_scan_keys"
    );
    assert_eq!(
        units.lane_accept_keys, perf.smc_lane_accepts,
        "SMC census lane accepts did not close against perf.smc_lane_accepts"
    );
    assert_eq!(
        units.choke_block_only + units.choke_both,
        perf.smc_scan_calls,
        "SMC census 2x2 block-scan column did not close against perf.smc_scan_calls"
    );

    smc_census_assert_phase_closed(whole);
    smc_census_assert_phase_closed(&snapshot.windowed);

    json!({
        "schema": "izarravm-smc-census-v1",
        "stage": "A",
        "window": snapshot.window.map(|(start, end)| json!([start, end])),
        "clock_instructions": snapshot.clock,
        // Review finding M11: R0's SMC_wall is NOT derivable from this instrument. Stage A carries
        // no sampled timer, so the only source is an external RIP-sample profile of a census-OFF
        // build. R6's shares are shares of measured WORK UNITS inside the invalidation choke; the
        // two are multiplied, never equated.
        "r0_smc_wall_source": "external RIP-sample profile (samply) of a census-off release build",
        "phases": [
            smc_census_phase_json(whole),
            smc_census_phase_json(&snapshot.windowed),
        ],
    })
}

#[cfg(feature = "smc-census")]
fn smc_census_assert_phase_closed(phase: &izarravm_cpu::DirectSmcCensusPhase) {
    let units = phase.units;
    assert_eq!(
        units.choke_calls,
        units.choke_block_only + units.choke_narrow_only + units.choke_both + units.choke_neither,
        "SMC census choke 2x2 did not close"
    );
    assert_eq!(
        units.scan_calls,
        units.scan_calls_no_kill + units.scan_calls_lane_only + units.scan_calls_kill,
        "SMC census scan-call split did not close"
    );
    assert_eq!(
        units.keys_scanned,
        units.keys_no_kill + units.keys_lane_only + units.keys_kill,
        "SMC census scan-key split did not close"
    );
    assert_eq!(
        units.keys_scanned, units.window_len_sum,
        "SMC census window lengths did not sum to the scanned keys"
    );
    // Every key in the window took exactly one of five exits: the inline coverage pre-filter
    // skipped it before any probe, the `entries` lookup missed, it did not overlap, it was claimed
    // by a lane, or it died.
    assert_eq!(
        units.keys_scanned,
        units.probes_elided
            + units.entries_get_misses
            + units.keys_surviving
            + units.lane_accept_keys
            + units.keys_killed,
        "SMC census per-key exits did not close"
    );
    assert_eq!(
        units.keys_scanned,
        units.probes_elided + units.entries_get_calls,
        "SMC census pre-filter split did not close"
    );
    assert_eq!(
        units.entries_get_calls,
        units.keys_killed
            + units.keys_surviving
            + units.lane_accept_keys
            + units.entries_get_misses,
        "SMC census probe exits did not close"
    );
    assert_eq!(
        units.probe_divergences, 0,
        "SMC census saw the inline pre-filter skip a row the authoritative test would have killed"
    );
    assert_eq!(
        units.point_rows_scanned, 0,
        "SMC census scanned a Seen/Dormant point row; window-shrink forbids those on PageKeys"
    );
    assert_eq!(
        units.survivors_moved,
        units.keys_surviving + units.lane_accept_keys + units.probes_elided,
        "SMC census survivor moves did not close"
    );
    assert_eq!(
        units.page_visits,
        units.page_removes + units.page_absent,
        "SMC census page visits did not close"
    );
    assert_eq!(
        units.page_removes,
        units.page_reinserts + units.page_dropped_empty,
        "SMC census page round trip did not close"
    );
    assert_eq!(
        units.window_searches, units.page_removes,
        "SMC census window searches did not match present pages"
    );
    // `remove_waiting_sources` runs exactly twice per effective unlink (direct.rs, both call
    // sites in `unlink_block`), and `unlink_block` has exactly one caller, `retire_block`.
    assert_eq!(
        units.waiting_retain_calls,
        2 * units.unlink_calls_effective,
        "SMC census waiting-retain passes did not close against effective unlinks"
    );
    assert_eq!(
        units.unlink_calls, units.retire_calls_effective,
        "SMC census unlink calls did not close against effective retires"
    );
    assert_eq!(
        units.unlink_calls_effective, units.unlink_calls,
        "SMC census saw an ineffective unlink, which retire_block cannot produce"
    );

    // Exact page totals, not the Space-Saving row sum: displacement makes the row sum an upper
    // bound, so only these close.
    let totals = phase.page_totals;
    assert_eq!(totals.page_visits, units.page_removes);
    assert_eq!(totals.keys_scanned, units.keys_scanned);
    assert_eq!(totals.keys_killed, units.keys_killed);
    assert_eq!(totals.keys_surviving, units.keys_surviving);
    assert_eq!(totals.lane_accepts, units.lane_accept_keys);
    assert_eq!(totals.page_keys_len_sum, units.page_keys_len_sum);
    // `no_kill_visits` has no independent counter to close against, which is exactly how it sat
    // dead — declared, summed and exported as a false zero — through the first review. Bound it
    // on both sides instead. Killing page VISITS are `page_removes - no_kill_visits`; every
    // killing CALL contains at least one of them, and each one kills at least one key. A dead
    // counter reads `page_removes - 0`, which blows the upper bound by orders of magnitude.
    let killing_visits = units
        .page_removes
        .checked_sub(totals.no_kill_visits)
        .expect("SMC census counted more no-kill page visits than page visits");
    assert!(
        killing_visits >= units.scan_calls_kill,
        "SMC census found fewer killing page visits than killing calls"
    );
    assert!(
        killing_visits <= units.keys_killed,
        "SMC census found more killing page visits than killed keys"
    );

    let mut previous = u64::MAX;
    let mut lower_sum = 0u64;
    for row in &phase.pages {
        assert!(
            row.counts.keys_killed <= previous,
            "SMC census page rows are not ranked by keys_killed"
        );
        previous = row.counts.keys_killed;
        assert!(row.error.keys_killed <= row.counts.keys_killed);
        assert!(row.error.keys_scanned <= row.counts.keys_scanned);
        assert!(row.error.page_visits <= row.counts.page_visits);
        lower_sum += row.counts.keys_killed - row.error.keys_killed;
    }
    assert!(
        lower_sum <= totals.keys_killed,
        "SMC census page lower bounds exceeded the exact kill total"
    );
    assert!(phase.pages.len() as u64 <= u64::from(phase.page_rows_capacity));
    assert_eq!(
        phase.page_slot_claims,
        phase.pages.len() as u64 + phase.page_displacements,
        "SMC census slot claims did not close against rows plus displacements"
    );
}

#[cfg(feature = "smc-census")]
fn smc_census_page_counts_json(
    counts: izarravm_cpu::DirectSmcCensusPageCounts,
) -> serde_json::Value {
    json!({
        "page_visits": counts.page_visits,
        "keys_scanned": counts.keys_scanned,
        "keys_killed": counts.keys_killed,
        "keys_surviving": counts.keys_surviving,
        "lane_accepts": counts.lane_accepts,
        "no_kill_visits": counts.no_kill_visits,
        "page_keys_len_sum": counts.page_keys_len_sum,
    })
}

#[cfg(feature = "smc-census")]
fn smc_census_phase_json(phase: &izarravm_cpu::DirectSmcCensusPhase) -> serde_json::Value {
    let u = phase.units;
    let totals = phase.page_totals;
    // R1's input: the top-four share, computed from LOWER bounds (design §9.5).
    let top4_lower: u64 = phase
        .pages
        .iter()
        .take(4)
        .map(|row| row.counts.keys_killed - row.error.keys_killed)
        .sum();
    let kills = totals.keys_killed;
    let ratio = |numerator: u64, denominator: u64| {
        if denominator == 0 {
            serde_json::Value::Null
        } else {
            json!(numerator as f64 / denominator as f64)
        }
    };
    json!({
        "label": phase.label,
        "units": {
            "choke_calls": u.choke_calls,
            "choke_block_only": u.choke_block_only,
            "choke_narrow_only": u.choke_narrow_only,
            "choke_both": u.choke_both,
            "choke_neither": u.choke_neither,
            "choke_wholesale": u.choke_wholesale,
            "scan_calls": u.scan_calls,
            "scan_calls_no_kill": u.scan_calls_no_kill,
            "scan_calls_lane_only": u.scan_calls_lane_only,
            "scan_calls_kill": u.scan_calls_kill,
            "scan_calls_absent_page": u.scan_calls_absent_page,
            "keys_no_kill": u.keys_no_kill,
            "keys_lane_only": u.keys_lane_only,
            "keys_kill": u.keys_kill,
            "keys_surviving_in_kill_calls": u.keys_surviving_in_kill_calls,
            "page_visits": u.page_visits,
            "page_removes": u.page_removes,
            "page_absent": u.page_absent,
            "page_reinserts": u.page_reinserts,
            "page_dropped_empty": u.page_dropped_empty,
            "window_searches": u.window_searches,
            "page_keys_len_sum": u.page_keys_len_sum,
            "window_len_sum": u.window_len_sum,
            "keys_scanned": u.keys_scanned,
            "entries_get_misses": u.entries_get_misses,
            "probes_elided": u.probes_elided,
            "entries_get_calls": u.entries_get_calls,
            "probe_divergences": u.probe_divergences,
            "point_rows_scanned": u.point_rows_scanned,
            "keys_killed": u.keys_killed,
            "keys_surviving": u.keys_surviving,
            "lane_accept_keys": u.lane_accept_keys,
            "survivors_moved": u.survivors_moved,
            "drain_calls": u.drain_calls,
            "drain_elements": u.drain_elements,
            "waiting_retain_calls": u.waiting_retain_calls,
            "waiting_map_len_sum": u.waiting_map_len_sum,
            "waiting_sources_visited": u.waiting_sources_visited,
            "waiting_entries_dropped": u.waiting_entries_dropped,
            "retire_calls": u.retire_calls,
            "retire_calls_effective": u.retire_calls_effective,
            "unlink_calls": u.unlink_calls,
            "unlink_calls_effective": u.unlink_calls_effective,
            "inbound_links_walked": u.inbound_links_walked,
            "inbound_links_reparked": u.inbound_links_reparked,
            "decode_dependency_slots": u.decode_dependency_slots,
            "release_range_bytes": u.release_range_bytes,
        },
        "page_totals": smc_census_page_counts_json(totals),
        "page_rows_capacity": phase.page_rows_capacity,
        "page_slot_claims": phase.page_slot_claims,
        "page_displacements": phase.page_displacements,
        "pages": phase.pages.iter().map(|row| json!({
            "page": row.page,
            "physical_base": u64::from(row.page) << 12,
            "counts": smc_census_page_counts_json(row.counts),
            "error": smc_census_page_counts_json(row.error),
        })).collect::<Vec<_>>(),
        "derived": {
            // R1. Lower-bound top-four share. Review finding M2: the worst-case per-row deflation
            // is kills / 64, so an S4 landing inside [0.60, 0.66] is INSIDE the error band and
            // must be read as R1's middle arm, not the licensing arm.
            "r1_s4_lower": ratio(top4_lower, kills),
            "r1_space_saving_error_bound": ratio(kills, u64::from(phase.page_rows_capacity)),
            // R2. Review finding M7: W is surviving keys over scanned keys, not the no-kill call
            // key sum, because a killing call also scans survivors a presence filter would elide.
            "r2_w_surviving": ratio(u.keys_surviving, u.keys_scanned),
            // C1's mechanism witness. `probes_elided / keys_scanned` is the share of the window
            // the inline coverage pre-filter decided without touching `entries`; the ON-arm
            // reconciliation is `probes_elided + keys_surviving(ON) == keys_surviving(OFF)`.
            "c1_probes_elided_share": ratio(u.probes_elided, u.keys_scanned),
            "r2_w_no_kill_calls": ratio(u.keys_no_kill, u.keys_scanned),
            // R6 inputs are unit counts, not shares; the report converts them.
            "mean_page_occupancy": ratio(u.page_keys_len_sum, u.page_removes),
            "mean_window_length": ratio(u.window_len_sum, u.page_removes),
            "mean_waiting_map_len": ratio(u.waiting_map_len_sum, u.waiting_retain_calls),
        },
    })
}

#[cfg(feature = "direct-callout-attribution")]
fn direct_callout_attribution_json(
    snapshot: Option<izarravm_cpu::DirectCallOutAttributionSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    // EVERY structural check this writer used to restate -- row count, row order and labels,
    // per-row closure, totals, port ordering, and the ports-table identity over the port-class
    // helpers -- now lives once, on the producer's own type
    // (`DirectCallOutAttributionSnapshot::assert_closed` in izarravm-cpu). The duplicated
    // versions are exactly what went stale: a four-name label list survived two helpers landing,
    // and the ports identity was written as a literal `{0, 4}` index sum in both crates and
    // updated in neither, so the first armed descent2 run aborted at teardown rather than in CI.
    // Nothing here re-derives those identities; this function's remaining job is the JSON shape.
    snapshot.assert_closed();
    // The writer's own contract, which the producer cannot state: the emitted `helper` strings
    // are the exported label list, in its order. If a helper lands and this crate is not
    // rebuilt against the new list, the schema pin below and this assertion disagree.
    for (row, expected) in snapshot
        .helpers
        .iter()
        .zip(izarravm_cpu::DIRECT_CALLOUT_HELPER_LABELS)
    {
        assert_eq!(row.helper, expected);
    }

    json!({
        "schema": "izarravm-direct-callout-attribution-v1",
        "helpers": snapshot.helpers.iter().map(|row| json!({
            "helper": row.helper,
            "attempts": row.counts.attempts,
            "continued": row.counts.continued,
            "step_break": row.counts.step_break,
            "abnormal": row.counts.abnormal,
        })).collect::<Vec<_>>(),
        "ports": snapshot.ports.iter().map(|row| json!({
            "port": row.port,
            "attempts": row.counts.attempts,
            "continued": row.counts.continued,
            "step_break": row.counts.step_break,
            "abnormal": row.counts.abnormal,
        })).collect::<Vec<_>>(),
        "totals": {
            "attempts": snapshot.totals.attempts,
            "continued": snapshot.totals.continued,
            "step_break": snapshot.totals.step_break,
            "abnormal": snapshot.totals.abnormal,
        },
    })
}

/// The whole-run BIOS fixed-disk census. All zero unless `IZARRAVM_INT13_PROFILE=1`.
///
/// `read_count_hist` buckets are `1, 2, 3-4, 5-8, 9-16, 17-32, 33-64, 65-127, 128+`
/// sectors. The first bucket carried the load-time question this census was built
/// to answer, and it has been answered: with the old 100 us `COMMAND_LATENCY_TICKS`
/// a 512-byte call paid three times more latency than transfer, so the effective
/// rate was a property of the call SIZE rather than of the modelled 16.7 MB/s,
/// and 98.7% of a Duke Nukem 3D load was single-sector. That latency is now ZERO,
/// so the histogram no longer predicts a rate — read it as the call-size shape of
/// the workload, and read `stall_ticks` against `read_sectors - cache_hits` for
/// the charge.
/// The device-armed ATA/ATAPI clock-skip mechanism counters.
///
/// READ THESE BEFORE READING THE WALL. The wall number alone cannot separate
/// "the arm never fired" from "the arm fired and the target was wrong", and the
/// two have completely different fixes. `spans` against the sector count and
/// `ticks` against `master_ticks` are what falsify the second.
///
/// `enabled` is emitted alongside because unset does not mean the same thing on
/// both sides of the default flip; a leg that recorded zeros without recording
/// its arm is not evidence.
/// PIT bulk-advance mechanism counters (`IZARRAVM_PIT_BULK_ADVANCE`).
///
/// `advance_clocks + loop_clocks` is `guest_seconds * 1,193,182` by
/// construction, whatever the guest did, so the split between the two is the
/// whole reading: it is the fraction of the per-input-CLK loop the lever
/// actually removed. `enabled=false` with `advances=0` and
/// `declines_knob_off == loop_advances` is the inert OFF arm, stated rather
/// than assumed.
fn pit_bulk_advance_json(machine: &izarravm_machine::Machine) -> serde_json::Value {
    let c = machine.pit_bulk_advance_counters();
    json!({
        "enabled": machine.pit_bulk_advance_enabled(),
        "advances": c.advances,
        "advance_clocks": c.advance_clocks,
        "loop_advances": c.loop_advances,
        "loop_clocks": c.loop_clocks,
        "declines_knob_off": c.declines_knob_off,
        "declines_bcd": c.declines_bcd,
        "declines_illegal_reload": c.declines_illegal_reload,
        "declines_span_too_wide": c.declines_span_too_wide,
        "transitions": c.transitions,
    })
}

fn ata_poll_skip_json(machine: &izarravm_machine::Machine) -> serde_json::Value {
    let c = machine.ata_poll_skip_counters();
    json!({
        "enabled": machine.ata_poll_skip_enabled(),
        "arms": c.arms,
        "spans": c.spans,
        "ticks": c.ticks,
        "blocks": c.blocks,
        "declines_not_pending": c.declines_not_pending,
        "declines_below_floor": c.declines_below_floor,
        "declines_deadline_clamped": c.declines_deadline_clamped,
        "declines_halted": c.declines_halted,
        "monitor_exempt": c.monitor_exempt,
        // Terminal alt-status run lengths, bucketed 1, 2, 3-4, 5-8, 9-16,
        // 17-32, 33-64, 65+. All zero unless IZARRAVM_ATA_POLL_SKIP_DIAG armed
        // it; it exists so ATA_POLL_RUN can be re-derived from a run rather than
        // a rebuild.
        "run_histogram": machine.ata_poll_run_histogram().to_vec(),
    })
}

fn int13_profile_json(p: izarravm_machine::Int13Profile) -> serde_json::Value {
    json!({
        "read_calls": p.read_calls,
        "read_sectors": p.read_sectors,
        "write_calls": p.write_calls,
        "write_sectors": p.write_sectors,
        "verify_calls": p.verify_calls,
        "verify_sectors": p.verify_sectors,
        "control_calls": p.control_calls,
        "read_count_hist": p.read_count_hist.to_vec(),
        "cache_hits": p.cache_hits,
        "stall_ticks": p.stall_ticks,
        "host_wall_ns": p.host_wall_ns,
        "read1_fat": p.read1_fat,
        "read1_data": p.read1_data,
        "read1_other": p.read1_other,
        "read1_buf_conv": p.read1_buf_conv,
        "read1_buf_uma": p.read1_buf_uma,
        "read1_buf_hma": p.read1_buf_hma,
        "read_buf_conv": p.read_buf_conv,
        "read_buf_uma": p.read_buf_uma,
        "read_buf_hma": p.read_buf_hma,
    })
}

/// The periodic phase-mark series, as offsets from the first mark.
///
/// `Instant` is not serialisable, so wall is emitted as a nanosecond offset from `marks[0]`,
/// the same shape the boot profiler's `build_rows` uses via `duration_since`.
///
/// Emits absolute counters, not deltas. Differencing consecutive entries is the consumer's job
/// and keeps this honest: a delta series hides whether a counter went backwards, and several of
/// these (`katea`, the perf counters) are cumulative by contract.
///
/// READ THE STALL COLUMNS BEFORE COMPARING INTERVALS. `stall_for_master_ticks` grants guest time
/// for zero emulation work while the host burns real wall inside Katea, so a loading interval
/// looks fast in raw wall-over-guest for an accounting reason rather than an emulation-rate one.
/// Net out `katea_host_wall_ns` and `io_stall_ticks` first. The rt in this series is NOT the rt
/// the realtime gate ratchets on.
fn phase_mark_series_json(marks: &[izarravm_machine::PhaseMark]) -> serde_json::Value {
    let Some(first) = marks.first() else {
        return serde_json::Value::Array(Vec::new());
    };
    let rows: Vec<_> = marks
        .iter()
        .map(|mark| {
            json!({
                "id": mark.id,
                "wall_offset_ns": mark.wall.duration_since(first.wall).as_nanos() as u64,
                "master_ticks": mark.master_ticks,
                "elapsed_clocks": mark.elapsed_clocks,
                "instructions": mark.perf.instructions,
                "jit_direct_insns": mark.perf.jit_direct_insns,
                "jit_direct_entries": mark.perf.jit_direct_entries,
                "io_stall_ticks": mark.io_stall_ticks,
                "halted_ticks": mark.halted_ticks,
                // THE CD COLUMNS. Read `cd_pio_bytes` for throughput: it is a
                // byte sum and is split-invariant, so a lever that changes batch
                // geometry cannot move it without changing what the drive
                // delivered. `cd_accesses` is batch-shaped by construction (one
                // per device advance that carried bytes) and must never be
                // graded on; it is here so the ratio against `cd_pio_bytes` can
                // be read as batch shape. `atapi_packet_commands` is the
                // batch-invariant command count -- one per 2048-byte sector
                // under TOKACD, i.e. the sector rate, which is what a per-sector
                // assertion needs and what the 2026-08-20 disk-read audit had to
                // infer from `brk_step` for want of this column.
                "cd_pio_bytes": mark.cd_pio_bytes,
                "cd_accesses": mark.cd_accesses,
                "atapi_packet_commands": mark.atapi_packet_commands,
                "katea_host_wall_ns": mark.katea.as_ref().map(|k| k.host_wall_ns),
                // `_max_level_ns`, not `_max_ns`: this series is cumulative and
                // consumers difference it column by column, and the difference of
                // two running maxima is not the maximum over the interval. The
                // suffix says "read me as a level" so a uniform differencer
                // cannot quietly produce a meaningless number.
                "katea_host_read_max_level_ns": mark.katea.as_ref().map(|k| k.host_read_max_ns),
                "katea_host_readahead_hits": mark.katea.as_ref().map(|k| k.host_readahead_hits),
                "katea_host_readahead_fills": mark.katea.as_ref().map(|k| k.host_readahead_fills),
                "katea_sector_reads": mark.katea.as_ref().map(|k| k.sector_reads),
                "katea_host_bytes": mark.katea.as_ref().map(|k| k.host_bytes),
                "katea_host_file_reads": mark.katea.as_ref().map(|k| k.host_file_reads),
                "katea_host_read_operations": mark.katea.as_ref().map(|k| k.host_read_operations),
                "katea_host_read_bytes": mark.katea.as_ref().map(|k| k.host_read_bytes),
                "katea_host_file_opens": mark.katea.as_ref().map(|k| k.host_file_opens),
                "katea_run_scan_steps": mark.katea.as_ref().map(|k| k.run_scan_steps),
                // The FAT / directory region split. Zero unless
                // IZARRAVM_KATEA_REGION_CENSUS=1 armed the volume at mount.
                "katea_fat_sector_reads": mark.katea.as_ref().map(|k| k.fat_sector_reads),
                "katea_dir_or_free_sector_reads": mark.katea.as_ref().map(|k| k.dir_or_free_sector_reads),
                "katea_sector_writes": mark.katea.as_ref().map(|k| k.sector_writes),
                "katea_int13_read_commands": mark.katea.as_ref().map(|k| k.int13_read_commands),
                "katea_int13_read_sectors": mark.katea.as_ref().map(|k| k.int13_read_sectors),
                "katea_int13_read_wait_ticks": mark.katea.as_ref().map(|k| k.int13_read_wait_ticks),
                "katea_int13_write_commands": mark.katea.as_ref().map(|k| k.int13_write_commands),
                "katea_int13_write_sectors": mark.katea.as_ref().map(|k| k.int13_write_sectors),
                "katea_int13_write_wait_ticks": mark.katea.as_ref().map(|k| k.int13_write_wait_ticks),
                "katea_pio_read_commands": mark.katea.as_ref().map(|k| k.pio_read_commands),
                "katea_pio_read_sectors": mark.katea.as_ref().map(|k| k.pio_read_sectors),
                "katea_pio_read_wait_ticks": mark.katea.as_ref().map(|k| k.pio_read_wait_ticks),
                "katea_pio_write_commands": mark.katea.as_ref().map(|k| k.pio_write_commands),
                "katea_pio_write_sectors": mark.katea.as_ref().map(|k| k.pio_write_sectors),
                "katea_pio_write_wait_ticks": mark.katea.as_ref().map(|k| k.pio_write_wait_ticks),
                "katea_dma_read_commands": mark.katea.as_ref().map(|k| k.dma_read_commands),
                "katea_dma_read_sectors": mark.katea.as_ref().map(|k| k.dma_read_sectors),
                "katea_dma_read_wait_ticks": mark.katea.as_ref().map(|k| k.dma_read_wait_ticks),
                "katea_dma_write_commands": mark.katea.as_ref().map(|k| k.dma_write_commands),
                "katea_dma_write_sectors": mark.katea.as_ref().map(|k| k.dma_write_sectors),
                "katea_dma_write_wait_ticks": mark.katea.as_ref().map(|k| k.dma_write_wait_ticks),
                "katea_overlay_resident_sectors": mark.katea.as_ref().map(|k| k.overlay_resident_sectors),
                "katea_overlay_pending_sectors": mark.katea.as_ref().map(|k| k.overlay_pending_sectors),
                "katea_pending_unmapped_sectors": mark.katea.as_ref().map(|k| k.pending_unmapped_sectors),
                "katea_spill_operations": mark.katea.as_ref().map(|k| k.spill_operations),
                "katea_spill_bytes": mark.katea.as_ref().map(|k| k.spill_bytes),
                "katea_spill_wall_ns": mark.katea.as_ref().map(|k| k.spill_wall_ns),
                "katea_projection_operations": mark.katea.as_ref().map(|k| k.projection_operations),
                "katea_projection_bytes": mark.katea.as_ref().map(|k| k.projection_bytes),
                "katea_projection_wall_ns": mark.katea.as_ref().map(|k| k.projection_wall_ns),
                "katea_projection_max_level_ns": mark.katea.as_ref().map(|k| k.projection_max_ns),
                "katea_metadata_projection_passes": mark.katea.as_ref().map(|k| k.metadata_projection_passes),
                "katea_host_write_failures": mark.katea.as_ref().map(|k| k.host_write_failures),
                "katea_blocked_projection_keys": mark.katea.as_ref().map(|k| k.blocked_projection_keys),
                // The fixed-disk census. All zero unless IZARRAVM_INT13_PROFILE=1.
                "int13_read_calls": mark.int13.read_calls,
                "int13_read_sectors": mark.int13.read_sectors,
                "int13_write_calls": mark.int13.write_calls,
                "int13_write_sectors": mark.int13.write_sectors,
                "int13_verify_calls": mark.int13.verify_calls,
                "int13_control_calls": mark.int13.control_calls,
                "int13_cache_hits": mark.int13.cache_hits,
                "int13_stall_ticks": mark.int13.stall_ticks,
                "int13_host_wall_ns": mark.int13.host_wall_ns,
                "int13_read1_fat": mark.int13.read1_fat,
                "int13_read1_data": mark.int13.read1_data,
                "int13_read1_other": mark.int13.read1_other,
                "int13_read1_buf_conv": mark.int13.read1_buf_conv,
                "int13_read1_buf_uma": mark.int13.read1_buf_uma,
                "int13_read1_buf_hma": mark.int13.read1_buf_hma,
                "int13_read_buf_conv": mark.int13.read_buf_conv,
                "int13_read_buf_uma": mark.int13.read_buf_uma,
                "int13_read_buf_hma": mark.int13.read_buf_hma,
                // The JIT / SMC / decode series QUESTION 1 correlates against the
                // dip window. Absolute, like everything else here.
                // Bytes written into device (VGA aperture) memory. In Duke's
                // 320x200 screen-buffered mode one presented frame is a 64,000-byte
                // blit to 0xA0000, so this is a real per-interval FRAME COUNTER --
                // the thing a min-FPS hitch has to be found in. `smc_lane_accepts`
                // is NOT a substitute: it counts lane admissions, which stop once a
                // lane is established, and it varies 80x across a steady demo.
                "device_write_bytes": mark.perf.device_write_bytes,
                "device_write_ranges": mark.perf.device_write_ranges,
                "decode_probes": mark.perf.decode_probes,
                "decode_misses": mark.perf.decode_misses,
                "decode_inval_smc": mark.perf.decode_inval_smc,
                "decode_inval_cs_load": mark.perf.decode_inval_cs_load,
                "decode_inval_other": mark.perf.decode_inval_other,
                "decode_inval_cr3": mark.perf.decode_inval_cr3,
                "decode_inval_cr0": mark.perf.decode_inval_cr0,
                "decode_inval_task_switch": mark.perf.decode_inval_task_switch,
                "cr3_code_flush_taken": mark.perf.cr3_code_flush_taken,
                "cr3_code_flush_skipped": mark.perf.cr3_code_flush_skipped,
                "cr3_link_context_selects": mark.perf.cr3_link_context_selects,
                "cr3_link_graph_retires": mark.perf.cr3_link_graph_retires,
                "translation_page_writes": mark.perf.translation_page_writes,
                "translation_a_stores": mark.perf.translation_a_stores,
                "translation_d_stores": mark.perf.translation_d_stores,
                "translation_pages_marked": mark.perf.translation_pages_marked,
                "tlb_walks": mark.perf.tlb_walks,
                "code_invalidations": mark.perf.code_invalidations,
                "smc_narrow_kills": mark.perf.smc_narrow_kills,
                "smc_lane_accepts": mark.perf.smc_lane_accepts,
                "smc_heat_demotions": mark.perf.smc_heat_demotions,
                "jit_direct_compile_attempts": mark.perf.jit_direct_compile_attempts,
                "jit_direct_blocks_installed": mark.perf.jit_direct_blocks_installed,
                "jit_direct_compile_ns": mark.perf.jit_direct_compile_ns,
                // The reject column this block never had. Whole-run rejects were readable from
                // the profile's `perf` block and the phase block carried none at all, so a
                // phase-scoped bar on the data-segment treadmill could not be graded. Split by
                // ARM at the source, because the strict half is the only one stage 2 can reach.
                "jit_direct_reject_data_segment": mark.perf.jit_direct_reject_data_segment,
                "jit_direct_reject_data_segment_strict": mark.perf.jit_direct_reject_data_segment_strict,
                "jit_direct_reject_data_segment_masked": mark.perf.jit_direct_reject_data_segment_masked,
                "jit_direct_reject_data_segment_real": mark.perf.jit_direct_reject_data_segment_real,
                "jit_direct_reject_data_segment_v86": mark.perf.jit_direct_reject_data_segment_v86,
                "jit_direct_reject_data_segment_pm16": mark.perf.jit_direct_reject_data_segment_pm16,
                "jit_direct_reject_data_segment_pm32": mark.perf.jit_direct_reject_data_segment_pm32,
                "jit_direct_cache_resets": mark.perf.jit_direct_cache_resets,
                "jit_direct_arena_compactions": mark.perf.jit_direct_arena_compactions,
                // The wall the compaction rebuild actually cost, measured rather than
                // regressed out of interval wall. `jit_direct_compile_ns` does NOT include it.
                "jit_direct_arena_compaction_ns": mark.perf.jit_direct_arena_compaction_ns,
                // Batch-break histogram. These say what LIMITS batch length, which is the
                // difference between "the guest changed shape" and "we stopped being able to
                // stay in a straight line". `brk_cont_decode_miss` rising with a falling
                // instructions/entry implicates decode footprint; `brk_cont_not_continuable`
                // rising implicates the SMC/dispatch path instead.
                //
                // These are TWO LEVELS, not one flat histogram. The partition of
                // `straight_line_runs` is EXACTLY {brk_decode_or_branch, brk_step,
                // brk_interrupt, brk_cap, brk_halt, brk_rep_resume, brk_fatal}; the three
                // `brk_cont_*` counters are the BREAKDOWN of `brk_decode_or_branch` alone, which
                // is bumped on the same line pair as each of them, so brk_decode_or_branch ==
                // cont_decode_miss + cont_not_continuable + cont_page_cross. Summing all ten
                // double-counts every continuation break. `brk_fatal` is counted in the
                // `run_budgeted` wrapper, on the `?`-propagation return path the other six never
                // take.
                "straight_line_runs": mark.perf.straight_line_runs,
                "brk_decode_or_branch": mark.perf.brk_decode_or_branch,
                "brk_cont_decode_miss": mark.perf.brk_cont_decode_miss,
                "brk_cont_not_continuable": mark.perf.brk_cont_not_continuable,
                "brk_cont_page_cross": mark.perf.brk_cont_page_cross,
                "brk_step": mark.perf.brk_step,
                "brk_interrupt": mark.perf.brk_interrupt,
                "brk_cap": mark.perf.brk_cap,
                "brk_halt": mark.perf.brk_halt,
                "brk_rep_resume": mark.perf.brk_rep_resume,
                "brk_fatal": mark.perf.brk_fatal,
                // `smc_scan_keys / smc_scan_calls` is the MEAN OVERLAP-SCAN LENGTH of
                // `invalidate_physical_range`. Without it a rise in SMC cost cannot be split
                // into "more events" (calls) and "each event dearer" (keys per call), and the
                // two select completely different fixes.
                "smc_scan_calls": mark.perf.smc_scan_calls,
                "smc_scan_keys": mark.perf.smc_scan_keys,
                "smc_heat_chunks_hot": mark.perf.smc_heat_chunks_hot,
                // Native-side exit and cold-chain accounting. A coverage dip that shows up here
                // as unresolved static-unbound growth is block formation, not admission.
                "jit_direct_side_exits": mark.perf.jit_direct_side_exits,
                "jit_direct_unresolved_exits": mark.perf.jit_direct_unresolved_exits,
                "jit_direct_unresolved_static_unbound": mark.perf.jit_direct_unresolved_static_unbound,
                "jit_direct_unresolved_static_hidden": mark.perf.jit_direct_unresolved_static_hidden,
                "jit_direct_unresolved_dynamic_miss_or_unbound": mark
                    .perf
                    .jit_direct_unresolved_dynamic_miss_or_unbound,
                "jit_direct_unresolved_dynamic_hidden": mark.perf.jit_direct_unresolved_dynamic_hidden,
                // The direct page cache. Its miss ratio is the working-set proxy that does not go
                // through the decode table. NOT data-only: the instruction-prefetch refill bumps
                // the same two counters (decode.rs:1158/:1168) on a fetch-page miss, so this ratio
                // mixes code and data footprint and cannot be read as a pure data-side number.
                "direct_page_hits": mark.perf.direct_page_hits,
                "direct_page_misses": mark.perf.direct_page_misses,
                "wipes_direct_map": mark.fast_map_audit.wipes_direct_map,
                "wipes_direct_data_map": mark.fast_map_audit.wipes_direct_data_map,
                "wipes_tlb_flush": mark.fast_map_audit.wipes_tlb_flush,
                "wipes_admission": mark.fast_map_audit.wipes_admission,
                "wipe_pages_cleared": mark.fast_map_audit.wipe_pages_cleared,
            })
        })
        .collect();
    serde_json::Value::Array(rows)
}

fn direct_stall_json(snapshot: &izarravm_cpu::DirectStallSnapshot) -> serde_json::Value {
    let report = json!({
        "dormant": snapshot
            .dormant
            .iter()
            .map(|(label, count)| json!({ "reason": label, "count": count }))
            .collect::<Vec<_>>(),
        // The `compile_retry` row of `dormant` split by which compile-walk gate gave up. `count`
        // is attempts and sums to that row exactly; `keys` counts the distinct block keys each
        // cause parked, which is the column that joins against the dormant population a barrier
        // census reports.
        "jit_direct_retry_causes": snapshot
            .retry_causes
            .iter()
            .map(|counts| {
                json!({ "cause": counts.cause, "count": counts.count, "keys": counts.keys })
            })
            .collect::<Vec<_>>(),
        // The other currency on the same 466 keys: static-unbound EXITS that landed on a dormant
        // key, split by the cause it was parked with. `jit_direct_retry_causes` says how many
        // keys each gate parked; this says how much traffic those keys absorb, which is what a
        // lift policy is priced against. Barrier-census gated, so a plain run reports zeroes.
        "jit_direct_retry_cause_hits": snapshot
            .retry_cause_hits
            .iter()
            .map(|(label, hits)| json!({ "cause": label, "hits": hits }))
            .collect::<Vec<_>>(),
        // The retry lift's own pair, read as a ratio: `reparks / lifts` is how often a lifted key
        // came straight back with the same cause, which is the arm's miss rate.
        "jit_direct_retry_lifts": snapshot.retry_lifts,
        "jit_direct_retry_lift_reparks": snapshot.retry_lift_reparks,
        // The S4d M5 measurement: interpreted MOV SS / POP SS split by whether the load moved
        // the SS record. The SS call-out rows resume only on the unchanged shape, so this ratio
        // is what decides whether they are worth building.
        "ss_load_same_record": snapshot.ss_load_same_record,
        "ss_load_changed_record": snapshot.ss_load_changed_record,
        "link_refusals": snapshot
            .link_refusals
            .iter()
            .map(|(label, count)| json!({ "reason": label, "count": count }))
            .collect::<Vec<_>>(),
        "links_cleared": snapshot
            .links_cleared
            .iter()
            .map(|(label, count)| json!({ "cause": label, "count": count }))
            .collect::<Vec<_>>(),
        "chain_requirement_narrowed": snapshot
            .chain_requirement_narrowed
            .iter()
            .map(|(label, count)| json!({ "cause": label, "count": count }))
            .collect::<Vec<_>>(),
        "entry_chain_reject_own_pass": snapshot.entry_chain_reject_own_pass,
        "entry_chain_admitted": snapshot.entry_chain_admitted,
        "entry_chain_masked_reject": snapshot.entry_chain_masked_reject,
        "side_exit_segment_limit": snapshot.side_exit_segment_limit,
        "side_exit_x87_eligibility": snapshot.side_exit_x87_eligibility,
        "side_exit_divide_guard": snapshot.side_exit_divide_guard,
        "side_exit_callout_step_break": snapshot.side_exit_callout_step_break,
        "side_exit_callout_abnormal": snapshot.side_exit_callout_abnormal,
        "jit_direct_callout_executed": snapshot.callout_executed,
        "jit_direct_callout_port_v86_served": snapshot.callout_port_v86_served,
        "jit_direct_callout_port_imm8_served": snapshot.callout_port_imm8_served,
        // The `0xE6` row's RECONCILIATION NUMERATOR, and the only always-on one it has: the
        // per-port `direct_callout_attribution` table is behind a feature and is keyed by port
        // rather than by helper, so it cannot answer "did the OUT arm serve anything" on a plain
        // census build -- which is the acceptance question for a default-OFF row whose ON arm is
        // supposed to make a barrier census row disappear.
        "jit_direct_callout_port_out_imm8_served": snapshot.callout_port_out_imm8_served,
        // `0xEE`'s reconciliation numerator, the DX-port twin of the row above. Unlike `0xE6` this
        // row ships unconditionally, so a nonzero read here says the corpus's miglia/123-talk
        // rows are being served rather than that a knob is off.
        "jit_direct_callout_port_out_dx_served": snapshot.callout_port_out_dx_served,
        "jit_direct_callout_interpret_one_executed": snapshot.callout_interpret_one_executed,
        "jit_direct_callout_interpret_one_resync": snapshot.callout_interpret_one_resync,
        // The price of the PREFIX half of the segment mask: resyncs the suffix-only rule would
        // have carried. A subset of the resync count above, not a separate lane.
        "jit_direct_callout_interpret_one_resume_refused_prefix_use":
            snapshot.callout_interpret_one_resume_refused_prefix_use,
        // Which arm of the knob this run compiled under. Reported so a ladder leg cannot be read
        // as the arm it named while having run the other one.
        "jit_direct_callout_segment_resume": snapshot.callout_segment_resume_enabled,
        "jit_direct_callout_interpret_one_resync_fault": snapshot.callout_interpret_one_resync_fault,
        "jit_direct_callout_interpret_one_abnormal": snapshot.callout_interpret_one_abnormal,
        "jit_direct_callout_interpret_one_demoted": snapshot.callout_interpret_one_demoted,
        // Per-ROW, beside the scalars rather than instead of them. The scalars sum these four
        // columns and stay the family's headline; this array is what the plan's "a row demoted on
        // the loader at more than 50% is refuted" rule has to read, because a whole-CPU ratio
        // cannot say WHICH of the nine admitted rows resynced.
        "jit_direct_callout_interpret_one_rows": snapshot
            .callout_interpret_one_rows
            .iter()
            .map(|counts| {
                json!({
                    "row": counts.row,
                    "executed": counts.executed,
                    "resync": counts.resync,
                    "resync_fault": counts.resync_fault,
                    "demoted": counts.demoted,
                    "resume_refused_prefix_use": counts.resume_refused_prefix_use,
                })
            })
            .collect::<Vec<_>>(),
        "jit_direct_callout_deferred_code_writes": snapshot.callout_deferred_code_writes,
        "jit_direct_callout_slot_cap_hits": snapshot.callout_slot_cap_hits,
        "jit_direct_compile_page_overflows": snapshot.compile_page_overflows,
        "jit_direct_compile_page_search_steps": snapshot.compile_page_search_steps,
        "jit_direct_demoted_callout_sites": snapshot.demoted_callout_sites,
        "jit_direct_demoted_callout_sites_refused": snapshot.demoted_callout_sites_refused,
        "jit_direct_reject_callout_privileged": snapshot.reject_callout_privileged,
        // Unprefixed, unlike their `jit_direct_` neighbours, and deliberately: these are the
        // names the round-2 acceptance gate pre-registered for its bars, and a reader diffing a
        // JSON leg against the design doc should find them spelled the same way.
        "callout_governor_trials": snapshot.callout_governor_trials,
        "callout_governor_lazy": snapshot.callout_governor_lazy,
        "callout_governor_io_touching": snapshot.callout_governor_io_touching,
        "segment_write_block_head_entries": snapshot.segment_write_block_head_entries,
        "segment_write_block_head_insns": snapshot.segment_write_block_head_insns,
        "smc_lane_trials": snapshot.lane_trials,
        "smc_lane_trial_installs": snapshot.lane_trial_installs,
        // Lever B's grant split and Task 0's two F2 counters, in PLAIN builds and deliberately:
        // the exact stop floor `regrants <= (budget - 1) * first_grants` and the non-vacuity
        // criterion "heat_demote_trial_spent falls" are read on the WALL arms, and both counters
        // are heat-coupled, so a census binary would give the wrong sign.
        "smc_lane_trial_first_grants": snapshot.lane_trial_first_grants,
        "smc_lane_trial_regrants": snapshot.lane_trial_regrants,
        "smc_lane_trial_budget_refusals": snapshot.lane_trial_budget_refusals,
        "heat_demote_trial_spent": snapshot.heat_demote_trial_spent,
        "heat_demote_trial_spent_earned": snapshot.heat_demote_trial_spent_earned,
        "smc_disp_lane_registrations": snapshot.disp_lane_registrations,
        "smc_imm8_lane_registrations": snapshot.imm8_lane_registrations,
        "smc_count_lane_registrations": snapshot.count_lane_registrations,
        // The two Option D arms: `IZARRAVM_DISP_STORE_LANES` is DEFAULT ON since the
        // 2026-08-23 ladder, `IZARRAVM_DISP_LOAD_WIDEN` is still default OFF and is blocked on
        // the lane-cap re-price. Unconditional here, like their four neighbours: a heat-coupled
        // counter
        // that is present on one leg and absent on the other confounds the arm with a counter
        // surface change, and these ship in PLAIN builds precisely so the ladder needs no census
        // binary to prove which arm a leg ran.
        "smc_disp_store_lane_registrations": snapshot.disp_store_lane_registrations,
        "smc_disp_load_widen_lane_registrations": snapshot.disp_load_widen_lane_registrations,
        // The `IZARRAVM_JCC_SHADOW` arm's four site classes, shipped in PLAIN builds for the same
        // reason the six lane counters above are: the ON and OFF legs of that slice share ONE
        // binary, so the ARM has to be readable out of the run's own JSON rather than out of which
        // binary ran. All four are zero on the OFF arm by construction, which makes a zero ON
        // reading a VACUOUS leg -- the knob was not exported -- and not an inert result.
        "jcc_sites_simple": snapshot.jcc_sites_simple,
        "jcc_sites_overflow": snapshot.jcc_sites_overflow,
        "jcc_sites_signed_xor": snapshot.jcc_sites_signed_xor,
        "jcc_sites_signed_xor_zf": snapshot.jcc_sites_signed_xor_zf,
        "hold_load_bias_probes": snapshot.hold_load_bias_probes,
        "align_test_al_sites": snapshot.align_test_al_sites,
        // The lane-budget split. Registrations say what lanes a run's installed blocks took;
        // these say what the shared `MAX_BLOCK_IMM_LANES` budget turned away IN THOSE SAME
        // BLOCKS, per family and on the cap arm only. Both are folded in at install, so the two
        // share a denominator and their ratio means something. A widening ladder that reads its
        // registrations flat while these are large has a budget result rather than a lane-class
        // one, which is the pair of readings the two numbers exist to separate.
        "smc_imm_lane_cap_refusals": snapshot.imm_lane_cap_refusals,
        "smc_imm8_lane_cap_refusals": snapshot.imm8_lane_cap_refusals,
        "smc_count_lane_cap_refusals": snapshot.count_lane_cap_refusals,
        "smc_disp_lane_cap_refusals": snapshot.disp_lane_cap_refusals,
        "smc_disp_store_lane_cap_refusals": snapshot.disp_store_lane_cap_refusals,
        "smc_disp_load_widen_lane_cap_refusals": snapshot.disp_load_widen_lane_cap_refusals,
        "decode_pack_late_view_miss": snapshot.decode_pack_late_view_miss,
        // The far-transfer slice's ledger and its three refusal cells, shared by native RETF
        // (`far_ret_native`) and, since L8, native CALL FAR ptr16:16 (`far_call_native`) --
        // one knob, `IZARRAVM_DIRECT_RETF_V86` (default `v86` since the 2026-08-24 ladder), gates
        // both. All zero on the `0` ESCAPE, which is the cheapest check that a ladder leg named
        // the arm it meant to, and all live on a default binary. Unconditional here for the
        // reason its neighbours are: a counter present on one leg and absent on the other
        // confounds the arm with a counter-surface change.
        "far_ret_native": snapshot.far_ret_native,
        "far_call_native": snapshot.far_call_native,
        "blocks_installed_baking_cs": snapshot.blocks_installed_baking_cs,
        "far_link_refused_cs": snapshot.far_link_refused_cs,
        "far_link_refused_limit": snapshot.far_link_refused_limit,
        "far_link_cut_on_widen": snapshot.far_link_cut_on_widen,
        "x87_top_retires_suppressed": snapshot.x87_top_retires_suppressed,
        "x87_top_sticky_crossings": snapshot.x87_top_sticky_crossings,
        // The data-segment reject governor. All zero on the OFF arm, which is the cheapest
        // check that a ladder leg named the arm it meant to. `distinct_layouts` is the go/no-go
        // input for the per-layout variant slice and is not a bar.
        "data_segment_retires_suppressed": snapshot.data_segment_retires_suppressed,
        "data_segment_sticky_crossings": snapshot.data_segment_sticky_crossings,
        "data_segment_link_declines": snapshot.data_segment_link_declines,
        "data_segment_live_promotions": snapshot.data_segment_live_promotions,
        "data_segment_distinct_layouts": snapshot.data_segment_distinct_layouts.to_vec(),
        // Sticky-decline memo, always on. `decline_memo_hits / admission_declines[dormant_probe]`
        // is the acceptance instrument; a census-gated counter would leave the wall build unable
        // to say whether the memo fired.
        "decline_memo_hits": snapshot.decline_memo_hits,
        "decline_memo_advances": snapshot.decline_memo_advances,
        "decline_memo_sweeps": snapshot.decline_memo_sweeps,
        // GP2 call-out-site poll skip (`IZARRAVM_DIRECT_POLL_SKIP`). `poll_skip_spans` /
        // `poll_skip_iterations` are indexed by `PollLoop::diagnostic_class()` (0 = 3-slot, 1 =
        // 5-slot, 2 = paired 6-slot).
        "jit_direct_poll_attempts": snapshot.poll_attempts,
        "jit_direct_poll_decline_port": snapshot.poll_declined_port,
        "jit_direct_poll_decline_port_source": snapshot.poll_declined_port_source,
        "jit_direct_poll_decline_mask_source": snapshot.poll_declined_mask_source,
        "jit_direct_poll_decline_knob": snapshot.poll_declined_knob,
        "jit_direct_poll_decline_eligibility": snapshot.poll_declined_eligibility,
        "jit_direct_poll_decline_sixteen_bit": snapshot.poll_declined_sixteen_bit,
        "jit_direct_poll_decline_shape": snapshot.poll_declined_shape,
        "jit_direct_poll_decline_cap": snapshot.poll_declined_cap,
        "jit_direct_poll_decline_seam": snapshot.poll_declined_seam,
        "jit_direct_poll_skip_spans": snapshot.poll_skip_spans.to_vec(),
        "jit_direct_poll_skip_iterations": snapshot.poll_skip_iterations.to_vec(),
        "jit_direct_poll_skip_raw_core_clocks": snapshot.poll_skip_raw_core_clocks,
        "jit_direct_poll_skip_raw_bus_clocks": snapshot.poll_skip_raw_bus_clocks,
        "jit_direct_poll_skip_max_span": snapshot.poll_skip_max_span,
        "jit_direct_poll_skip_last_head": snapshot.poll_skip_last_head,
        "jit_direct_link_context_rebinds": snapshot.link_context_rebinds,
        "jit_direct_link_waiting_entries": snapshot.link_waiting_entries,
        "jit_direct_link_linear_blocks_entries": snapshot.link_linear_blocks_entries,
    });
    // Task 0's census-gated half. The park pair is read as a mean WITH its censoring rate
    // `(smc_heat_demotions - park_lifts) / smc_heat_demotions`: never-lifted and reset-ended parks
    // contribute nothing and are the longest, so the mean is a LOWER bound on park length.
    #[cfg(feature = "direct-admission-census")]
    let report = {
        let mut report = report;
        report["heat_demote_trial_spent_earned_head3"] =
            snapshot.heat_demote_trial_spent_earned_head3.into();
        report["lane_install_demote_no_lanes"] = snapshot.lane_install_demote_no_lanes.into();
        report["lane_install_demote_trial_spent"] = snapshot.lane_install_demote_trial_spent.into();
        report["dormant_heat_park_epochs"] = snapshot.dormant_heat_park_epochs.into();
        report["dormant_heat_park_lifts"] = snapshot.park_lifts.into();
        report
    };
    report
}

fn direct_barrier_census_row_json(row: &izarravm_cpu::DirectBarrierCensusRow) -> serde_json::Value {
    json!({
        "opcode": row.opcode,
        "unbound_exits": row.unbound_exits,
        "dynamic_unbound_exits": row.dynamic_unbound_exits,
        "stop_reason": row.stop_reason,
        "modrm_reg": row.modrm_reg,
        "operand_form": row.operand_form,
        "operand_size": row.operand_size,
        "address_size": row.address_size,
        "prefix_mask": row.prefix_mask,
        "hits": row.hits,
        "runtime_hits": row.runtime_hits,
        "native_prefix_instructions": row.native_prefix_instructions,
        "native_suffix_instructions": row.native_suffix_instructions,
        "max_native_prefix": row.max_native_prefix,
        "max_native_suffix": row.max_native_suffix,
        "port": row.port,
    })
}

fn machine_profile_requested(value: Option<&str>) -> bool {
    value.is_some_and(|value| !matches!(value, "" | "0"))
}

/// The RIP-sampler output path from `IZARRAVM_RIP_PROFILE`, with `""` and `"0"`
/// counting as unset. `var_os` returns `Some("")` for a set-but-empty variable,
/// and pwsh writes exactly that when a harness assigns `= ""` intending OFF, so
/// a bare `var_os(..).map(..)` arms the sampler - which suspends the emulator
/// thread every 500 us - on every leg of a board (measured 2026-08-15).
fn rip_profile_path_from(value: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    value.filter(|path| !path.is_empty() && path.as_os_str() != std::ffi::OsStr::new("0"))
}

#[cfg(windows)]
fn rip_profile_path() -> Option<std::ffi::OsString> {
    rip_profile_path_from(std::env::var_os("IZARRAVM_RIP_PROFILE"))
}

/// True when IZARRAVM_UNIT_SIM requests the trace-driven unit-growth simulator (any value other
/// than "" or "0"). The simulator only observes retired interpreter instructions and never touches
/// guest-visible state, so a headless game/binary run stays byte-identical whether it is on or off.
fn unit_sim_requested() -> bool {
    std::env::var("IZARRAVM_UNIT_SIM")
        .ok()
        .as_deref()
        .is_some_and(|value| !matches!(value, "" | "0"))
}

/// Turn on the unit simulator before a headless run when IZARRAVM_UNIT_SIM asks for it. Enabling
/// early means the sim observes the whole run. A no-op when the env var is unset or the binary was
/// built without feature `jit`.
fn maybe_enable_unit_sim(machine: &mut Machine) {
    if unit_sim_requested() {
        machine.set_unit_sim_enabled(true);
    }
}

/// Turn on the CPU's SMC trace before a headless run when `IZARRAVM_SMC_TRACE` asks for it (any
/// value other than "" or "0"). The trace only observes the invalidation choke and never touches
/// guest-visible state, so a headless run stays byte-identical whether it is on or off.
fn maybe_enable_smc_trace(machine: &mut Machine) {
    if std::env::var("IZARRAVM_SMC_TRACE")
        .ok()
        .as_deref()
        .is_some_and(|value| !matches!(value, "" | "0"))
    {
        machine.set_smc_trace_enabled(true);
    }
}

/// Write the SMC trace summary at the end of a headless run. `IZARRAVM_SMC_TRACE_OUT` names the
/// file; without it the lines go to stdout alongside the perf row. A no-op when the trace was
/// never enabled.
fn maybe_report_smc_trace(machine: &mut Machine) {
    let Some(lines) = machine.take_smc_trace_report() else {
        return;
    };
    match std::env::var("IZARRAVM_SMC_TRACE_OUT")
        .ok()
        .filter(|p| !p.is_empty())
    {
        Some(path) => {
            let body = lines.join("\n") + "\n";
            if let Err(error) = std::fs::write(&path, body) {
                eprintln!("smc_trace: could not write {path}: {error}");
                for line in lines {
                    println!("{line}");
                }
            } else {
                println!("smc_trace written to {path} ({} lines)", lines.len());
            }
        }
        None => {
            for line in lines {
                println!("{line}");
            }
        }
    }
}

/// Take the unit simulator's ladder report at the end of a headless run and print its per-rung
/// evidence lines (two per rung, eight total for the four-rung measurement set `{L0, L4, L6, P}`),
/// followed by the per-port io histogram lines when `IZARRAVM_IO_HIST=1`. A no-op when the sim was
/// never enabled (`take_unit_sim_report` returns `None`) or the binary was built without feature
/// `jit`.
#[cfg(feature = "jit")]
fn maybe_report_unit_sim(machine: &mut Machine) {
    // Read the io histogram FIRST (it borrows the sim); `take_unit_sim_report` then consumes it.
    let io_hist_lines = machine.unit_sim_io_hist().map(|hist| io_hist_lines(&hist));
    if let Some(reports) = machine.take_unit_sim_report() {
        for line in unit_sim_report_lines(&reports) {
            println!("{line}");
        }
    }
    if let Some(lines) = io_hist_lines {
        for line in lines {
            println!("{line}");
        }
    }
}

/// Format the per-port io-read histogram (`IZARRAVM_IO_HIST=1`): one `io_hist port=0xNNN count=...`
/// line per port, sorted by count descending, capped at the top 16. `hist` is already sorted.
#[cfg(feature = "jit")]
fn io_hist_lines(hist: &[(u16, u64)]) -> Vec<String> {
    hist.iter()
        .take(16)
        .map(|&(port, count)| format!("io_hist port={port:#06x} count={count}"))
        .collect()
}

#[cfg(not(feature = "jit"))]
fn maybe_report_unit_sim(_machine: &mut Machine) {}

/// Print the non-direct data-read page histogram at the end of a headless run
/// (`IZARRAVM_SLOW_READ_HISTO=1`). A no-op when the instrument was never armed.
fn maybe_report_slow_read_histo(machine: &Machine) {
    let Some(pages) = machine.slow_read_histo() else {
        return;
    };
    let (misaligned, seen) = machine.slow_read_alignment().unwrap_or((0, 0));
    for line in slow_read_histo_lines(
        &pages,
        machine.cpu().perf_counters().data_slow_reads,
        misaligned,
        seen,
    ) {
        println!("{line}");
    }
}

/// The 4 KiB linear pages a real-mode DOS guest's non-direct reads can land in, in the order the
/// report prints them. These are the four candidate answers to N2's question, and each implies a
/// DIFFERENT slice: the aperture needs a video-side fast path, UMB/EMS RAM needs
/// `ram_lookup_page_is_direct` granularity, and anything above 1 MiB is neither.
const SLOW_READ_REGIONS: [(&str, u32, u32); 6] = [
    ("conventional_00000_9FFFF", 0x00, 0x9f),
    ("vga_aperture_A0000_AFFFF", 0xa0, 0xaf),
    ("text_B0000_BFFFF", 0xb0, 0xbf),
    ("umb_ems_C0000_EFFFF", 0xc0, 0xef),
    ("bios_F0000_FFFFF", 0xf0, 0xff),
    ("above_1MiB", 0x100, u32::MAX),
];

/// Format the histogram: one `slow_read_region` line per region, then the top 24
/// `slow_read_page` lines, then one `slow_read_total` line carrying the histogram's own sum
/// against `data_slow_reads`.
///
/// The total line is not decoration. One `data_slow_reads` contributor -- the REP CMPS destination
/// read in `strings.rs` -- is deliberately not bucketed because it holds a PHYSICAL address, so
/// the two numbers agreeing is what licenses reading the region split as the whole story.
fn slow_read_histo_lines(
    pages: &[(u32, u64)],
    data_slow_reads: u64,
    misaligned: u64,
    seen: u64,
) -> Vec<String> {
    let mut lines = Vec::new();
    let bucketed: u64 = pages.iter().map(|&(_, count)| count).sum();
    for &(name, first, last) in &SLOW_READ_REGIONS {
        let count: u64 = pages
            .iter()
            .filter(|&&(page, _)| page >= first && page <= last)
            .map(|&(_, count)| count)
            .sum();
        let percent = if bucketed == 0 {
            0.0
        } else {
            count as f64 * 100.0 / bucketed as f64
        };
        lines.push(format!(
            "slow_read_region {name} count={count} pct={percent:.2}"
        ));
    }
    for &(page, count) in pages.iter().take(24) {
        lines.push(format!(
            "slow_read_page page={:#07x} linear={:#010x} count={count}",
            page,
            page << 12
        ));
    }
    let misaligned_pct = if seen == 0 {
        0.0
    } else {
        misaligned as f64 * 100.0 / seen as f64
    };
    lines.push(format!(
        "slow_read_align misaligned={misaligned} of={seen} pct={misaligned_pct:.2}"
    ));
    lines.push(format!(
        "slow_read_total bucketed={bucketed} data_slow_reads={data_slow_reads} distinct_pages={}",
        pages.len()
    ));
    lines
}

/// Nearest-rank percentile of an ascending-sorted slice. `p` is a whole percent (50, 90). Empty
/// input yields 0.
#[cfg(feature = "jit")]
fn nearest_rank_percentile(sorted_ascending: &[usize], p: u32) -> u64 {
    if sorted_ascending.is_empty() {
        return 0;
    }
    let n = sorted_ascending.len();
    let rank = ((f64::from(p) / 100.0) * n as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(n - 1);
    sorted_ascending[index] as u64
}

/// Format the unit-simulator ladder's evidence lines: two lines per rung, so the four-rung
/// measurement set `{L0, L4, L6, P}` emits eight lines. The `cfg=` tag is the SECOND field of every
/// line (tag first, so the rungs eyeball-diff against the C-pre evidence). See [`unit_sim_rung_lines`]
/// for the per-rung field layout.
#[cfg(feature = "jit")]
#[allow(clippy::type_complexity)] // Signature fixed by the Track C task 3 reporting contract.
pub(crate) fn unit_sim_report_lines(
    reports: &[(&'static str, izarravm_cpu::SimReport, Vec<(usize, u32)>)],
) -> Vec<String> {
    reports
        .iter()
        .flat_map(|(cfg, report, histogram)| unit_sim_rung_lines(cfg, report, histogram))
        .collect()
}

/// Format one ladder rung's two evidence lines from its label, headline report, and per-unit
/// `(member_count, entry_physical_page)` histogram. The first line is the headline counters plus the
/// structural `insns_per_entry` metric (`retired_in_units / entries`, 0.000000 when `entries == 0`).
/// The second line summarizes the member-count distribution so the evaluation step can reason about
/// member caps without a per-unit retired count (which the API does not expose). `excl_units` counts
/// units whose entry sits in the BIOS/UMA physical window (page 0xF0, or pages 0xA0..=0xFF).
#[cfg(feature = "jit")]
fn unit_sim_rung_lines(
    cfg: &str,
    report: &izarravm_cpu::SimReport,
    histogram: &[(usize, u32)],
) -> Vec<String> {
    let insns_per_entry = if report.entries == 0 {
        0.0
    } else {
        report.retired_in_units as f64 / report.entries as f64
    };
    // The active-stream residency (rung P): the must-execute stream with elided poll iterations
    // removed. `ipe_active` prices a wait entry at one dispatch per deadline slice (the primary
    // quotient); `ipe_active_slice` additionally charges one dispatch per absorbed budget yield (the
    // pessimistic quotient). Both equal `insns_per_entry` on every non-P rung (all elision counters
    // are zero there).
    let active = report.retired_in_units.saturating_sub(report.elided_insns);
    let ipe_active = if report.entries == 0 {
        0.0
    } else {
        active as f64 / report.entries as f64
    };
    let slice_denom = report.entries + report.wait_batch_ends;
    let ipe_active_slice = if slice_denom == 0 {
        0.0
    } else {
        active as f64 / slice_denom as f64
    };
    let headline = format!(
        "unit_sim cfg={cfg} entries={} retired_in_units={} linked_transfers={} loop_links={} \
call_links={} ret_links={} itc_hits={} ght_hits={} ght_ret_hits={} unresolved_exits={} \
side_exits_io={} side_exits_async={} io_callouts={} sim_invalidations={} sim_restamps={} \
units_built={} units_rebuilt={} elided_insns={} elided_waits={} wait_batch_ends={} \
spin_noio_insns={} insns_per_entry={insns_per_entry:.6} ipe_active={ipe_active:.6} \
ipe_active_slice={ipe_active_slice:.6}",
        report.entries,
        report.retired_in_units,
        report.linked_transfers,
        report.loop_links,
        report.call_links,
        report.ret_links,
        report.itc_hits,
        report.ght_hits,
        report.ght_ret_hits,
        report.unresolved_exits,
        report.side_exits_io,
        report.side_exits_async,
        report.io_callouts,
        report.sim_invalidations,
        report.sim_restamps,
        report.units_built,
        report.units_rebuilt,
        report.elided_insns,
        report.elided_waits,
        report.wait_batch_ends,
        report.spin_noio_insns,
    );

    let mut members: Vec<usize> = histogram.iter().map(|&(count, _)| count).collect();
    members.sort_unstable();
    let members_max = members.last().copied().unwrap_or(0) as u64;
    let over = |threshold: usize| members.iter().filter(|&&m| m > threshold).count();
    let excl_units = histogram
        .iter()
        .filter(|&&(_, page)| page == 0xF0 || (0xA0..=0xFF).contains(&page))
        .count();
    let hist = format!(
        "unit_sim_hist cfg={cfg} units={} members_p50={} members_p90={} members_max={members_max} \
units_over_64={} units_over_128={} units_over_256={} excl_units={excl_units}",
        histogram.len(),
        nearest_rank_percentile(&members, 50),
        nearest_rank_percentile(&members, 90),
        over(64),
        over(128),
        over(256),
    );

    vec![headline, hist]
}

fn validate_profile_json_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    validate_output_parent(path, "profile JSON")
}

/// Fail early when a host output path names a directory that does not exist. `what` names the
/// output in the message, so a run that would only fail at the END (after the guest work is
/// already spent) fails at startup instead.
fn validate_output_parent(path: &Path, what: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        return Err(format!(
            "{what} parent directory does not exist: {}",
            parent.display()
        )
        .into());
    }
    Ok(())
}

fn stop_reason_json(stop: &StopReason) -> serde_json::Value {
    match stop {
        StopReason::Halted => json!({ "kind": "halted" }),
        StopReason::CycleLimit { requested } => {
            json!({ "kind": "cycle_limit", "requested": requested })
        }
        StopReason::CpuError(message) => json!({ "kind": "cpu_error", "message": message }),
        StopReason::DosExit { code } => json!({ "kind": "dos_exit", "code": code }),
        StopReason::TestExit { code } => json!({ "kind": "test_exit", "code": code }),
    }
}

/// Parse Doom-style timedemo output from the guest text screen.
/// Looks for lines like "timed 2134 gametics in 907 realtics".
fn extract_timedemo_realtics(text: &str) -> Option<(u32, u32)> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(after_timed) = line.strip_prefix("timed ")
            && let Some((g_str, rest)) = after_timed.split_once(" gametics in ")
            && let Some(r_str) = rest.strip_suffix(" realtics")
            && let (Ok(g), Ok(r)) = (g_str.parse::<u32>(), r_str.parse::<u32>())
        {
            return Some((g, r));
        }
    }
    None
}

/// Print a machine-readable result block for a headless benchmark/timedemo run:
/// stop reason, CS:IP, full register state, and the full 80x25 text page. A
/// caller greps between the BEGIN/END markers for a benchmark's own reported
/// numbers (e.g. Doom's "timed N gametics in M realtics" or an fps line).
fn print_dump_result(machine: &mut Machine, stop_reason: &StopReason) {
    let regs = &machine.cpu().registers;
    let cs = regs.cs().selector;
    let ip = regs.eip as u16;
    println!("--- BEGIN RESULT ---");
    println!("stop: {stop_reason:?}");
    println!("CS:IP = {cs:04X}:{ip:04X}");
    println!(
        "EAX={:08X} EBX={:08X} ECX={:08X} EDX={:08X}",
        regs.eax(),
        regs.ebx(),
        regs.ecx(),
        regs.edx()
    );
    println!(
        "ESP={:08X} EBP={:08X} ESI={:08X} EDI={:08X}",
        regs.esp(),
        regs.ebp(),
        regs.esi(),
        regs.edi()
    );
    println!("EIP={:08X} EFLAGS={:08X}", regs.eip, regs.eflags);
    println!(
        "CS={:04X} DS={:04X} ES={:04X} SS={:04X} FS={:04X} GS={:04X}",
        cs,
        regs.segment(izarravm_cpu::SegmentIndex::Ds).selector,
        regs.segment(izarravm_cpu::SegmentIndex::Es).selector,
        regs.segment(izarravm_cpu::SegmentIndex::Ss).selector,
        regs.segment(izarravm_cpu::SegmentIndex::Fs).selector,
        regs.segment(izarravm_cpu::SegmentIndex::Gs).selector,
    );
    println!("--- text page (80x25) ---");
    println!("{}", machine.screen_text().as_text());
    println!("--- END RESULT ---");
}

/// Write the current framebuffer to a binary PPM (P6) file: the full raw
/// pixel dump a graphics-mode benchmark result (e.g. 3DBench2's fps readout)
/// lands in. Resolves DAC indices through the active 6-bit or 8-bit palette.
/// Write the last COMPLETED raster as a P6 PPM. Returns false when no frame has
/// been published yet, in which case nothing is written: see the
/// `presented_ppm` flag comment for why an absent frame is reported rather than
/// substituted.
fn write_presented_ppm(machine: &mut Machine, path: &Path) -> Result<bool, Box<dyn Error>> {
    use std::io::Write;

    let Some((pixels, width, height)) = machine.presented_frame_argb() else {
        return Ok(false);
    };
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(
        out,
        "P6
{width} {height}
255
"
    )?;
    for color in pixels {
        out.write_all(&[(color >> 16) as u8, (color >> 8) as u8, color as u8])?;
    }
    Ok(true)
}

fn write_framebuffer_ppm(machine: &mut Machine, path: &Path) -> Result<(), Box<dyn Error>> {
    let (pixels, width, height) = machine.capture_frame_argb();
    write_argb_ppm(path, &pixels, width, height)
}

/// Decode Margo even when Distira owns the mux, plus pal8 rereads of the same
/// bytes. Sidecar files go next to `--result-ppm` when that flag is set.
fn write_margo_store_ppms(
    machine: &mut Machine,
    result_ppm: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let Some((pixels, width, height)) = machine.margo_store_frame_argb() else {
        println!("margo store: none (no VBE session)");
        return Ok(());
    };
    let nonzero = pixels.iter().filter(|&&pixel| pixel != 0).count();
    println!(
        "margo store: {width}x{height} non-zero={nonzero}/{} ({:.1}%)",
        pixels.len(),
        if pixels.is_empty() {
            0.0
        } else {
            100.0 * nonzero as f64 / pixels.len() as f64
        }
    );
    if let Some(base) = result_ppm {
        let native = sidecar_ppm(base, "margo");
        write_argb_ppm(&native, &pixels, width as usize, height as usize)?;
        println!("margo store ppm: {}", native.display());
    }

    for (label, w, h) in [("pal8-640x480", 640u32, 480u32), ("pal8-320x120", 320, 120)] {
        let Some(pal8) = machine.margo_store_pal8_argb(w, h) else {
            continue;
        };
        let pal_nonzero = pal8.iter().filter(|&&pixel| pixel != 0).count();
        println!(
            "margo {label}: non-zero={pal_nonzero}/{} ({:.1}%)",
            pal8.len(),
            if pal8.is_empty() {
                0.0
            } else {
                100.0 * pal_nonzero as f64 / pal8.len() as f64
            }
        );
        if let Some(base) = result_ppm {
            let path = sidecar_ppm(base, &format!("margo-{label}"));
            write_argb_ppm(&path, &pal8, w as usize, h as usize)?;
            println!("margo {label} ppm: {}", path.display());
        }
    }
    Ok(())
}

fn sidecar_ppm(result_ppm: &Path, suffix: &str) -> PathBuf {
    let stem = result_ppm
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("result");
    result_ppm.with_file_name(format!("{stem}-{suffix}.ppm"))
}

fn write_argb_ppm(
    path: &Path,
    pixels: &[u32],
    width: usize,
    height: usize,
) -> Result<(), Box<dyn Error>> {
    use std::io::Write;

    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{width} {height}\n255\n")?;
    for &color in pixels {
        out.write_all(&[(color >> 16) as u8, (color >> 8) as u8, color as u8])?;
    }
    Ok(())
}

/// After a headless run, report the active scanout and whether it holds meaningful
/// content. Legacy text mode also includes the 80x25 page.
fn print_video_summary(machine: &mut Machine) {
    use izarravm_machine::VideoMode;

    let active_display = machine.active_display();
    let display_name = match active_display {
        ActiveDisplay::VgaRaster => "VGA raster",
        ActiveDisplay::MargoLfb => "Margo linear framebuffer",
        ActiveDisplay::Distira => "Distira",
    };
    println!("active display: {display_name}");

    if active_display == ActiveDisplay::VgaRaster {
        let mode = machine.active_video_mode();
        let mode_name = match mode {
            VideoMode::Text => "text (03h)",
            VideoMode::Mode13h => "mode 13h (320x200x256)",
            VideoMode::Planar => "planar (EGA/VGA 16-color)",
            VideoMode::ModeX => "mode X (unchained 256-color)",
            VideoMode::Cga => "CGA graphics (320x200x4 / 640x200x2)",
            VideoMode::Hercules => "Hercules graphics (720x348 monochrome)",
        };
        println!("video mode: {mode_name}");

        if matches!(mode, VideoMode::Text) {
            let frame = machine.screen_text();
            let text = frame.as_text();
            let printable = text.chars().filter(|c| !c.is_whitespace()).count();
            println!("text non-blank glyphs: {printable}");
            println!("--- 80x25 text ---");
            println!("{text}");
            println!("--- end text ---");
        }
    }

    let (pixels, width, height) = machine.capture_frame_argb();
    let total = pixels.len();
    let nonzero = pixels.iter().filter(|&&pixel| pixel != 0).count();
    println!("framebuffer: {width}x{height} ({total} px)");
    println!(
        "non-zero pixels: {nonzero} ({:.1}%)",
        if total == 0 {
            0.0
        } else {
            100.0 * nonzero as f64 / total as f64
        }
    );
    if let Some(display) = machine.margo_display() {
        let (linear, banked) = machine.vbe_mode_set_window_counts();
        println!(
            "margo: mode={:#06x} {}x{}x{} pitch={} start={} linear_sets={} banked_sets={} \
             vga_pass={} video_reset={} video_reset_falling_edges={} fbi_init0_enables={}",
            display.mode,
            display.width,
            display.height,
            display.bpp,
            display.pitch,
            display.start,
            linear,
            banked,
            machine.distira_vga_pass(),
            machine.distira_video_reset(),
            machine.distira_video_reset_falling_edges(),
            machine.distira_fbi_init0_byte0_enables(),
        );
    }
    let mut histogram = std::collections::HashMap::new();
    for &color in &pixels {
        *histogram.entry(color).or_insert(0u32) += 1;
    }
    let mut entries: Vec<(u32, u32)> = histogram.into_iter().collect();
    entries.sort_by_key(|&(color, count)| (Reverse(count), color));
    let top: Vec<String> = entries
        .iter()
        .take(8)
        .map(|(color, count)| format!("#{color:06X}: {count}"))
        .collect();
    println!("distinct colors: {}", entries.len());
    println!("top colors: {}", top.join(", "));
}

/// Minimal ASCII to Set 1 make+break for the demo (lowercase letters, digits,
/// space). Extend if the demo needs more than typing words.
/// US-layout Set 1 make codes for the 26 letters, indexed a..=z.
const LETTER_MAKE: [u8; 26] = [
    0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
];

/// Map an ASCII character to its US-layout Set 1 make code and whether Shift is
/// held to produce it. Returns None for characters with no single-key mapping.
fn ascii_key(ch: char) -> Option<(u8, bool)> {
    let plain = |make: u8| Some((make, false));
    let shifted = |make: u8| Some((make, true));
    match ch {
        'a'..='z' => plain(LETTER_MAKE[ch as usize - 'a' as usize]),
        'A'..='Z' => shifted(LETTER_MAKE[ch as usize - 'A' as usize]),
        ' ' => plain(0x39),
        '\r' | '\n' => plain(0x1c),
        '\t' => plain(0x0f),
        '\x08' => plain(0x0e),
        '\x1b' => plain(0x01),
        '1' => plain(0x02),
        '2' => plain(0x03),
        '3' => plain(0x04),
        '4' => plain(0x05),
        '5' => plain(0x06),
        '6' => plain(0x07),
        '7' => plain(0x08),
        '8' => plain(0x09),
        '9' => plain(0x0a),
        '0' => plain(0x0b),
        '!' => shifted(0x02),
        '@' => shifted(0x03),
        '#' => shifted(0x04),
        '$' => shifted(0x05),
        '%' => shifted(0x06),
        '^' => shifted(0x07),
        '&' => shifted(0x08),
        '*' => shifted(0x09),
        '(' => shifted(0x0a),
        ')' => shifted(0x0b),
        '-' => plain(0x0c),
        '_' => shifted(0x0c),
        '=' => plain(0x0d),
        '+' => shifted(0x0d),
        '[' => plain(0x1a),
        '{' => shifted(0x1a),
        ']' => plain(0x1b),
        '}' => shifted(0x1b),
        ';' => plain(0x27),
        ':' => shifted(0x27),
        '\'' => plain(0x28),
        '"' => shifted(0x28),
        '`' => plain(0x29),
        '~' => shifted(0x29),
        '\\' => plain(0x2b),
        '|' => shifted(0x2b),
        ',' => plain(0x33),
        '<' => shifted(0x33),
        '.' => plain(0x34),
        '>' => shifted(0x34),
        '/' => plain(0x35),
        '?' => shifted(0x35),
        _ => None,
    }
}

/// Build the Set 1 scancode sequence for typing a character: the make and break
/// of the key, wrapped in left-Shift make/break when the glyph needs Shift.
pub(crate) fn ascii_to_set1(ch: char) -> Vec<u8> {
    let Some((make, shift)) = ascii_key(ch) else {
        return Vec::new();
    };
    let mut codes = Vec::with_capacity(4);
    if shift {
        codes.push(0x2a); // left Shift make
    }
    codes.push(make);
    codes.push(make | 0x80); // key break
    if shift {
        codes.push(0xaa); // left Shift break
    }
    codes
}

/// Print whatever the guest wrote to COM1 (the serial port), under a header so
/// it reads apart from the screen dump. Prints nothing when COM1 stayed silent,
/// so a ROM that only touches the screen keeps a clean output.
fn print_com1(serial: &str) {
    if serial.is_empty() {
        return;
    }
    println!("--- COM1 ---");
    print!("{serial}");
    if !serial.ends_with('\n') {
        println!();
    }
}

/// The BIOS ROM to boot: the file passed with --bios, or the built-in test ROM.
fn select_rom(bios: Option<&Path>) -> Result<Vec<u8>, Box<dyn Error>> {
    match bios {
        Some(path) => Ok(std::fs::read(path)?),
        None => Ok(test_rom().to_vec()),
    }
}

/// Map a Windows LANGID to one of the 17 guest layout indices. Regions that
/// share a language but use different keyboards are matched on the full LANGID
/// first; everything else falls back to the primary-language default, then US.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn layout_index_from_langid(langid: u16) -> u8 {
    match langid {
        0x0809 => return 1,          // en-GB -> UK
        0x080c => return 6,          // fr-BE -> Belgium
        0x0813 => return 6,          // nl-BE -> Belgium
        0x0c0c => return 7,          // fr-CA -> Canadian French
        0x100c => return 12,         // fr-CH -> Swiss French
        0x0807 => return 13,         // de-CH -> Swiss German
        0x040a | 0x0c0a => return 2, // es-ES (traditional/modern) -> Spain
        _ => {}
    }
    match langid & 0x03ff {
        0x09 => 0,  // English (other) -> US
        0x0a => 16, // Spanish (non-Spain, i.e. Latin America) -> LA
        0x0c => 3,  // French (other) -> France
        0x07 => 4,  // German (other) -> Germany
        0x10 => 5,  // Italian -> Italy
        0x06 => 8,  // Danish -> Denmark
        0x13 => 9,  // Dutch (other) -> Netherlands
        0x14 => 10, // Norwegian -> Norway
        0x16 => 11, // Portuguese -> Portugal
        0x0b => 14, // Finnish -> Finland
        0x1d => 15, // Swedish -> Sweden
        _ => 0,
    }
}

/// The default code-page index (sub-project A order: 437=0, 850=1, 860=2,
/// 863=3, 865=4) for each guest keyboard layout. Frozen to match the firmware
/// `kbd_layout_codepage` table emitted by the layout converter.
pub(crate) fn codepage_index_for_layout(layout: u8) -> u8 {
    const CP: [u8; 17] = [0, 0, 1, 1, 1, 1, 1, 3, 4, 1, 4, 2, 1, 1, 1, 1, 1];
    *CP.get(usize::from(layout)).unwrap_or(&0)
}

/// The host keyboard layout as a guest index, or None when it cannot be read
/// (non-Windows).
#[cfg(target_os = "windows")]
pub(crate) fn host_keyboard_layout_index() -> Option<u8> {
    #[link(name = "user32")]
    unsafe extern "system" {
        #[link_name = "GetKeyboardLayout"]
        fn get_keyboard_layout(thread_id: u32) -> usize;
    }
    let hkl = unsafe { get_keyboard_layout(0) };
    let langid = (hkl & 0xffff) as u16;
    Some(layout_index_from_langid(langid))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn host_keyboard_layout_index() -> Option<u8> {
    None
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;

#[cfg(test)]
#[path = "tokados_test.rs"]
mod tokados_smoke;
