// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

// The hand-built perf_counters_json object expands one json! recursion level
// per key; the counter set is large enough to need headroom above the default.
#![recursion_limit = "512"]

mod bench;
mod bench_reference;
mod cmos;
mod crt;
mod gui;
mod prefs;
#[cfg(windows)]
mod riprofile;

use clap::Parser;
use izarravm_audio::AudioSubsystem;
use izarravm_core::{
    AppConfig, ConfigOverrides, GswMode, HardwareProfile, MASTER_CLOCK_HZ, MidiBackend, MidiConfig,
    MidiPortId, SbDma8, SbDma16, SbIrq, VideoCard,
};
use izarravm_cpu::CpuProfileSnapshot;
use izarravm_firmware::{
    SuiteRecordStatus, SuiteResults, boot_test_image, neurketa_image, parse_result_block, test_rom,
};
use izarravm_input::InputState;
use izarravm_machine::{
    ActiveDisplay, ExecutionBackend, Machine, MachineHostProfileSnapshot, MachineProfile,
    PerfCounters, StopReason, set_process_execution_backend,
};
use serde_json::json;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

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
    /// With --hdd-folder, return an error unless the guest reaches Lotura TestExit code 0.
    #[arg(long, requires = "hdd_folder")]
    expect_test_exit: bool,
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
    #[arg(long, env = "IZARRAVM_DOSROOT")]
    dosroot: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MidiConfigPresence {
    backend: bool,
    external_port: bool,
    soundfont: bool,
    mt32_control_rom: bool,
    mt32_pcm_rom: bool,
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
        Err(
            "this IzarraVM build requires an AVX2-capable x86-64 CPU; use --interpreter to run the portable CPU core",
        )
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "izarravm=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let execution_backend = requested_execution_backend(
        cli.interpreter,
        izarravm_cpu::NATIVE_BACKEND_COMPILED,
        izarravm_cpu::native_backend_available(),
    )?;
    set_process_execution_backend(execution_backend);
    if cli.profile_json.is_some() && cli.headless_profile_exe.is_none() && cli.hdd_folder.is_none()
    {
        return Err("--profile-json requires --headless-profile-exe or --hdd-folder".into());
    }
    let midi_presence = midi_config_presence(&cli)?;
    let mut config = load_config(&cli)?;
    // When the user gave no C: location (no --c_drive, no --dosroot, and the
    // config left at its "." default), use the per-user ~/.izarravm/c_drive (or,
    // with --portable, a c_drive beside the executable). The folder is just user
    // data now — Katea boots real FreeDOS from its own synthesized partition, so
    // nothing is installed onto it.
    if cli.c_drive.is_none() && cli.dosroot.is_none() && config.dos.c_drive == Path::new(".") {
        config.dos.c_drive = resolve_c_root(cli.portable);
    }
    let saved_prefs = prefs::GuiPrefs::load(&prefs::prefs_path(&config.dos.c_drive));
    merge_saved_midi(&mut config.audio.midi, &saved_prefs.midi, midi_presence);
    if !cli.portable {
        discover_munt_roms(&mut config.audio.midi, &state_dir_path());
    }
    let hardware = HardwareProfile::from_config(&config)?;
    let audio = AudioSubsystem::from_config(&config.audio);
    let input = InputState {
        keyboard_enabled: config.input.keyboard,
        mouse_enabled: config.input.mouse,
        joystick_enabled: config.input.joystick,
    };
    info!(
        cpu = %config.machine.cpu,
        hz = hardware.cpu.clock_rate().as_hz_f64(),
        memory_mib = config.machine.memory_mib,
        video = %config.machine.video,
        c_drive = %config.dos.c_drive.display(),
        audio_devices = audio.devices.len(),
        keyboard = input.keyboard_enabled,
        mouse = input.mouse_enabled,
        joystick = input.joystick_enabled,
        "configuration validated"
    );

    if cli.headless_config_check {
        return Ok(());
    }

    // Each headless mode that builds a Machine runs in its own function. A Machine
    // is a large value (CPU, VGA, Margo, audio chips inline); keeping all three
    // branches inline gave main a ~1.2 MB stack frame that overflowed on the
    // prologue, before clap could even print --help/--version. One Machine per
    // frame keeps every path well under the thread stack limit.
    if cli.headless_boot_suite {
        return run_boot_suite(&hardware);
    }

    if cli.headless_bench {
        return bench::run_bench(&hardware);
    }

    if let Some(path) = &cli.headless_bench_exe {
        return bench::run_bench_exe(path, &hardware);
    }

    if let Some(path) = &cli.headless_profile_exe {
        return bench::run_profile_exe(
            path,
            cli.profile_json.as_deref(),
            cli.profile_sample_stride,
            &hardware,
        );
    }

    if cli.headless_bandwidth {
        return bench::run_bandwidth(&hardware);
    }

    if cli.headless_test_rom {
        return run_test_rom(cli.bios.as_deref(), cli.cycles, &hardware);
    }

    if cli.headless_keyboard {
        return run_keyboard_demo(&hardware, cli.stdin_text.as_deref());
    }

    if cli.headless_izarra_bios {
        return run_izarra_bios();
    }

    if let Some(path) = &cli.headless_boot_floppy {
        return run_boot_floppy(path, cli.cycles, &hardware);
    }

    if let Some(path) = &cli.headless_boot_hdd {
        return run_boot_hdd(path, cli.cycles, &hardware);
    }

    if let Some(dir) = &cli.hdd_folder {
        let glide_ovl = load_state_glide_ovl(&state_dir_path());
        return run_boot_hdd_folder(
            dir,
            glide_ovl,
            cli.cycles,
            &hardware,
            cli.dump_result,
            cli.result_ppm.as_deref(),
            cli.profile_json.as_deref(),
            cli.expect_test_exit,
        );
    }

    if let Some(prog) = &cli.katea_run {
        let code = katea_run(prog, MachineProfile::from_hardware_profile(&hardware))?;
        std::process::exit(code);
    }

    let rom = match cli.bios.as_deref() {
        Some(path) => std::fs::read(path)?,
        None => izarravm_firmware::izarra_bios().to_vec(),
    };
    // The PC speaker is always-present motherboard hardware, so the host audio
    // output is opened regardless of which sound cards are enabled. AudioPlayer
    // falls back to silent if the host has no usable device.
    // Read host local time and resolve host-side cmos.bin now, on the main thread,
    // before the emulation thread spawns. now_local() is sound only single-threaded.
    let rtc_setup = cmos::RtcSetup::from_c_root(&config.dos.c_drive);
    let glide_ovl = load_state_glide_ovl(&state_dir_path());
    gui::run(
        MachineProfile::from_hardware_profile(&hardware),
        rom,
        config.dos.c_drive.clone(),
        config.dos.cd_image.clone(),
        config.audio.midi.clone(),
        glide_ovl,
        cli.margo_test_pattern,
        rtc_setup,
        config.input.joystick,
    )?;
    Ok(())
}

/// The C: root for a normal launch: `<home>/.izarravm/c_drive`, or `c_drive`
/// beside the executable under `--portable`. Created if missing. Inlined from the
/// retired HLE crate — it was only a path helper, not DOS emulation.
fn resolve_c_root(portable: bool) -> PathBuf {
    let dir = c_root_path(portable);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The C: root path (no filesystem side effects), split out so it is testable
/// without creating directories.
fn c_root_path(portable: bool) -> PathBuf {
    if portable {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("c_drive")
    } else {
        state_dir_path().join("c_drive")
    }
}

fn load_state_glide_ovl(state_dir: &Path) -> Option<Vec<u8>> {
    let canonical = state_dir.join("GLIDE2X.OVL");
    let path = canonical.is_file().then_some(canonical).or_else(|| {
        std::fs::read_dir(state_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case("GLIDE2X.OVL"))
            })
            .min()
    })?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            info!(path = %path.display(), "using global GLIDE2X.OVL fallback");
            Some(bytes)
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "could not read global GLIDE2X.OVL fallback");
            None
        }
    }
}

fn state_dir_path() -> PathBuf {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".izarravm")
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

/// Boot the Izarra 3000 BIOS headless, run POST to halt, print the VDTS records.
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
    profile_json: Option<&Path>,
    expect_test_exit: bool,
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
    let budget = cycles.unwrap_or(DEFAULT_BOOT_HDD_CYCLES);
    #[cfg(windows)]
    let rip_sampler =
        std::env::var_os("IZARRAVM_RIP_PROFILE").map(|path| (riprofile::Sampler::start(), path));
    let start_wall = std::time::Instant::now();
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    let wall = start_wall.elapsed();
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
        let target = dump_override.or_else(|| snapshot.hot_addrs.first().map(|&(t, _)| (t, 0x180)));
        if let Some((top, dump_len)) = target {
            let read_u32 = |machine: &mut Machine, addr: u32| -> u32 {
                u32::from_le_bytes([
                    machine.read_physical_u8(addr),
                    machine.read_physical_u8(addr.wrapping_add(1)),
                    machine.read_physical_u8(addr.wrapping_add(2)),
                    machine.read_physical_u8(addr.wrapping_add(3)),
                ])
            };
            let read_linear = |machine: &mut Machine, lin: u32| -> Option<u8> {
                if machine.cpu().control.cr0 & 0x8000_0000 == 0 {
                    return Some(machine.read_physical_u8(lin));
                }
                let cr3 = machine.cpu().control.cr3 & !0xfff;
                let pde = read_u32(machine, cr3 + (lin >> 22) * 4);
                if pde & 1 == 0 {
                    return None;
                }
                let physical = if pde & 0x80 != 0 {
                    (pde & 0xffc0_0000) | (lin & 0x003f_ffff)
                } else {
                    let pte = read_u32(machine, (pde & !0xfff) + ((lin >> 12) & 0x3ff) * 4);
                    if pte & 1 == 0 {
                        return None;
                    }
                    (pte & !0xfff) | (lin & 0xfff)
                };
                Some(machine.read_physical_u8(physical))
            };
            let start = top.saturating_sub(0x40) & !0xf;
            println!();
            println!("=== bytes around hottest address {top:08X} (paging-walked linear) ===");
            for row in 0..dump_len.div_ceil(16) {
                let base = start + row * 16;
                let bytes: Vec<String> = (0..16)
                    .map(|i| match read_linear(&mut machine, base + i) {
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
    // Diff-trace prototype (IZARRAVM_DIFF_TRACE): flush the buffered trace writer now
    // that the run loop returned, or its last partial buffer's worth of lines -- most
    // often exactly the tail we care about -- is silently lost at process exit. This
    // is the path extender/game repros run through, so the flush matters most here.
    izarravm_cpu::flush_diff_trace();

    let cs = machine.cpu().registers.cs().selector;
    let ip = machine.cpu().registers.eip as u16;
    println!("folder: {}", dir.display());
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
    if dump_result {
        print_dump_result(&mut machine, &stop_reason);
    }
    if let Some(path) = result_ppm {
        write_framebuffer_ppm(&mut machine, path)?;
        println!("screenshot: {}", path.display());
    }
    // Reconcile guest writes back to the host folder. Katea's write engine
    // buffers guest file changes until a flush; without this, anything the
    // guest wrote (a `dir > log.txt` capture, a rebound executable) is
    // silently discarded at exit, which defeats the mounted-folder contract
    // and the guest-side debug channel it enables.
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
    let machine_profile = machine.host_profile_snapshot();
    let machine_phases = machine_profile.phases;
    let classified_wall_ns = machine_phases
        .iter()
        .map(|phase| phase.wall_ns)
        .sum::<u64>();
    let total_wall_ns = wall.as_nanos().min(u128::from(u64::MAX)) as u64;
    let report = json!({
        "schema": "izarravm-hdd-profile-v1",
        "workload": workload.display().to_string(),
        "mode": mode.canonical_name(),
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
        "direct_stalls": direct_stall_json(&machine.cpu().direct_stall_snapshot()),
        "perf": bench::perf_counters_json(
            perf,
            machine.cpu().poll_skip_memory(),
            machine.cpu().fast_map_probe_counters(),
        ),
    });
    std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

fn direct_barrier_census_json(
    snapshot: Option<izarravm_cpu::DirectBarrierCensusSnapshot>,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return serde_json::Value::Null;
    };
    json!({
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
    })
}

fn direct_stall_json(snapshot: &izarravm_cpu::DirectStallSnapshot) -> serde_json::Value {
    json!({
        "dormant": snapshot
            .dormant
            .iter()
            .map(|(label, count)| json!({ "reason": label, "count": count }))
            .collect::<Vec<_>>(),
        "link_refusals": snapshot
            .link_refusals
            .iter()
            .map(|(label, count)| json!({ "reason": label, "count": count }))
            .collect::<Vec<_>>(),
        "side_exit_segment_limit": snapshot.side_exit_segment_limit,
        "side_exit_x87_eligibility": snapshot.side_exit_x87_eligibility,
    })
}

fn direct_barrier_census_row_json(row: &izarravm_cpu::DirectBarrierCensusRow) -> serde_json::Value {
    json!({
        "opcode": row.opcode,
        "unbound_exits": row.unbound_exits,
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
    })
}

fn machine_profile_requested(value: Option<&str>) -> bool {
    value.is_some_and(|value| !matches!(value, "" | "0"))
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
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        return Err(format!(
            "profile JSON parent directory does not exist: {}",
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
fn write_framebuffer_ppm(machine: &mut Machine, path: &Path) -> Result<(), Box<dyn Error>> {
    use std::io::Write;

    let (pixels, width, height) = machine.capture_frame_argb();
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{width} {height}\n255\n")?;
    for color in pixels {
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
fn ascii_to_set1(ch: char) -> Vec<u8> {
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

fn load_config(cli: &Cli) -> Result<AppConfig, Box<dyn Error>> {
    let mut config = if let Some(path) = &cli.config {
        AppConfig::from_toml_path(path)?
    } else {
        AppConfig::default()
    };

    let c_drive = cli.c_drive.clone().or_else(|| cli.dosroot.clone());
    let external_midi_port = cli.midi_port.as_ref().map(|name| MidiPortId {
        name: name.clone(),
        ordinal: cli.midi_port_ordinal.unwrap_or(0),
    });
    config.apply_overrides(ConfigOverrides {
        cpu: cli.cpu,
        memory_mib: cli.memory_mib,
        video: cli.video,
        c_drive,
        soundfont: cli.soundfont.clone(),
        midi_backend: cli.midi_backend,
        external_midi_port,
        mt32_control_rom: cli.mt32_control_rom.clone(),
        mt32_pcm_rom: cli.mt32_pcm_rom.clone(),
        sb_irq: cli.sb_irq,
        sb_dma: cli.sb_dma,
        sb_high_dma: cli.sb_high_dma,
    });

    Ok(config)
}

fn midi_config_presence(cli: &Cli) -> Result<MidiConfigPresence, Box<dyn Error>> {
    let mut presence = MidiConfigPresence::default();
    if let Some(path) = &cli.config {
        let text = std::fs::read_to_string(path)?;
        let value: toml::Value = toml::from_str(&text)?;
        if let Some(midi) = value
            .get("audio")
            .and_then(|audio| audio.get("midi"))
            .and_then(toml::Value::as_table)
        {
            presence.backend = midi.contains_key("backend");
            presence.external_port = midi.contains_key("external_port");
            presence.soundfont = midi.contains_key("soundfont");
            presence.mt32_control_rom = midi.contains_key("mt32_control_rom");
            presence.mt32_pcm_rom = midi.contains_key("mt32_pcm_rom");
        }
    }
    presence.backend |= cli.midi_backend.is_some();
    presence.external_port |= cli.midi_port.is_some();
    presence.soundfont |= cli.soundfont.is_some();
    presence.mt32_control_rom |= cli.mt32_control_rom.is_some();
    presence.mt32_pcm_rom |= cli.mt32_pcm_rom.is_some();
    Ok(presence)
}

fn merge_saved_midi(config: &mut MidiConfig, saved: &MidiConfig, presence: MidiConfigPresence) {
    if !presence.backend {
        config.backend = saved.backend;
    }
    if !presence.external_port {
        config.external_port.clone_from(&saved.external_port);
    }
    if !presence.soundfont {
        config.soundfont.clone_from(&saved.soundfont);
    }
    if !presence.mt32_control_rom {
        config.mt32_control_rom.clone_from(&saved.mt32_control_rom);
    }
    if !presence.mt32_pcm_rom {
        config.mt32_pcm_rom.clone_from(&saved.mt32_pcm_rom);
    }
}

fn discover_munt_roms(config: &mut MidiConfig, state_dir: &Path) {
    if config.mt32_control_rom.is_some() || config.mt32_pcm_rom.is_some() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return;
    };
    let files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    let named = |name: &str| {
        files.iter().find(|path| {
            path.file_name()
                .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
        })
    };
    for (control_name, pcm_name) in [
        ("MT32_CONTROL.ROM", "MT32_PCM.ROM"),
        ("CM32L_CONTROL.ROM", "CM32L_PCM.ROM"),
    ] {
        if let (Some(control), Some(pcm)) = (named(control_name), named(pcm_name)) {
            config.mt32_control_rom = Some(control.clone());
            config.mt32_pcm_rom = Some(pcm.clone());
            return;
        }
    }
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
