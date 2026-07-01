mod bench_reference;
mod cmos;
mod crt;
mod gui;
mod prefs;

use clap::Parser;
use izarravm_audio::AudioSubsystem;
use izarravm_core::{
    AppConfig, ConfigOverrides, GswMode, HardwareProfile, MidiBackend, SbDma8, SbDma16, SbIrq,
    VideoCard,
};
use izarravm_cpu::CpuProfileSnapshot;
use izarravm_firmware::{
    SuiteRecordStatus, boot_test_image, neurketa_image, parse_result_block, test_rom,
};
use izarravm_input::InputState;
use izarravm_machine::{
    Machine, MachineHostProfileSnapshot, MachineProfile, PerfCounters, StopReason,
};
use serde_json::json;
use std::cmp::Reverse;
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

#[derive(Debug, Parser)]
#[command(version, about = "IzarraVM emulator scaffold")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    cpu: Option<GswMode>,
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
    #[arg(long)]
    soundfont: Option<PathBuf>,
    #[arg(long)]
    midi_backend: Option<MidiBackend>,
    #[arg(long)]
    sb_irq: Option<SbIrq>,
    #[arg(long)]
    sb_dma: Option<SbDma8>,
    #[arg(long)]
    sb_high_dma: Option<SbDma16>,
    #[arg(long)]
    headless_config_check: bool,
    #[arg(long)]
    headless_test_rom: bool,
    #[arg(long)]
    headless_boot_suite: bool,
    #[arg(long)]
    headless_bench: bool,
    /// Run one supplied DOS EXE through the raw-program bench harness in GSW-586.
    #[arg(long)]
    headless_bench_exe: Option<PathBuf>,
    /// Run one supplied DOS EXE twice in GSW-586: baseline, then profiling buckets.
    #[arg(long)]
    headless_profile_exe: Option<PathBuf>,
    /// Write --headless-profile-exe output as pretty JSON. Parent directory must exist.
    #[arg(long)]
    profile_json: Option<PathBuf>,
    /// Sample every Nth instruction in --headless-profile-exe.
    #[arg(long, default_value_t = 1024)]
    profile_sample_stride: u64,
    #[arg(long)]
    headless_bandwidth: bool,
    #[arg(long)]
    headless_keyboard: bool,
    #[arg(long)]
    headless_izarra_bios: bool,
    #[arg(long)]
    headless_boot_floppy: Option<PathBuf>,
    #[arg(long)]
    headless_boot_hdd: Option<PathBuf>,
    /// Boot the Katea host-folder facade: mount the given directory as C: through
    /// the real FreeDOS system files, run the BIOS, and print the boot diagnostics.
    /// The folder's top-level files are surfaced read-only beside the OS.
    #[arg(long)]
    hdd_folder: Option<PathBuf>,
    /// Boot real FreeDOS from a temp Katea disk and run a single DOS program,
    /// exiting with its DOS exit code (the Katea replacement for --headless-run).
    #[arg(long)]
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

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "izarravm=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let mut config = load_config(&cli)?;
    // When the user gave no C: location (no --c_drive, no --dosroot, and the
    // config left at its "." default), use the per-user ~/.izarravm/c_drive (or,
    // with --portable, a c_drive beside the executable). The folder is just user
    // data now — Katea boots real FreeDOS from its own synthesized partition, so
    // nothing is installed onto it.
    if cli.c_drive.is_none() && cli.dosroot.is_none() && config.dos.c_drive == Path::new(".") {
        config.dos.c_drive = resolve_c_root(cli.portable);
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
        hz = hardware.clock_hz,
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
        return run_bench(&hardware);
    }

    if let Some(path) = &cli.headless_bench_exe {
        return run_bench_exe(path, &hardware);
    }

    if let Some(path) = &cli.headless_profile_exe {
        return run_profile_exe(
            path,
            cli.profile_json.as_deref(),
            cli.profile_sample_stride,
            &hardware,
        );
    }

    if cli.headless_bandwidth {
        return run_bandwidth(&hardware);
    }

    if cli.headless_test_rom {
        return run_test_rom(cli.bios.as_deref(), cli.cycles, &hardware);
    }

    if cli.headless_keyboard {
        return run_keyboard_demo(&hardware, cli.stdin_text.as_deref());
    }

    if cli.headless_izarra_bios {
        return run_izarra_bios(&hardware);
    }

    if let Some(path) = &cli.headless_boot_floppy {
        return run_boot_floppy(path, cli.cycles, &hardware);
    }

    if let Some(path) = &cli.headless_boot_hdd {
        return run_boot_hdd(path, cli.cycles, &hardware);
    }

    if let Some(dir) = &cli.hdd_folder {
        return run_boot_hdd_folder(dir, cli.cycles, &hardware);
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
    let audio_enabled = true;
    // Read host local time and resolve host-side cmos.bin now, on the main thread,
    // before the emulation thread spawns. now_local() is sound only single-threaded.
    let rtc_setup = cmos::RtcSetup::from_c_root(&config.dos.c_drive);
    gui::run(
        MachineProfile::from_hardware_profile(&hardware),
        rom,
        config.dos.c_drive.clone(),
        config.dos.cd_image.clone(),
        audio_enabled,
        cli.margo_test_pattern,
        rtc_setup,
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
        #[allow(deprecated)]
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".izarravm").join("c_drive")
    }
}

/// Run the clean-room boot suite and print its result block.
fn run_boot_suite(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new_boot_image(
        MachineProfile::from_hardware_profile(hardware),
        boot_test_image(),
    )?;
    // The suite is wall-time-bound (PIT ticks and device-settle delays), so the
    // cycle budget scales with the clock to cover the same span at any GSW mode.
    // 200 ms (clock_hz / 5) matches the original 5,000,000 cycles at 25 MHz.
    let budget = hardware.clock_hz / 5;
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
    Ok(())
}

/// One bench run result: raw guest clocks, the reported iteration count and
/// self-check value, and the host wall time.
struct BenchRun {
    clocks: u64,
    iterations: u32,
    aux: u32,
    wall: std::time::Duration,
    /// Host-side perf counters for this run (decode-cache + straight-line diagnostics).
    perf: PerfCounters,
    machine_profile: MachineHostProfileSnapshot,
    cpu_profile: CpuProfileSnapshot,
}

/// How a benchmark payload is loaded: baked into the Neurketa boot image and
/// chosen by a selector byte, or a freestanding DOS .EXE.
#[derive(Debug)]
enum BenchSource<'a> {
    BootSelector(u8),
    DosExe(&'a [u8]),
}

fn run_bench_one(
    hardware: &HardwareProfile,
    mode: GswMode,
    source: &BenchSource<'_>,
    budget: u64,
) -> Result<BenchRun, Box<dyn Error>> {
    run_bench_one_profiled(hardware, mode, source, budget, None)
}

fn run_bench_one_profiled(
    hardware: &HardwareProfile,
    mode: GswMode,
    source: &BenchSource<'_>,
    budget: u64,
    sample_stride: Option<u64>,
) -> Result<BenchRun, Box<dyn Error>> {
    let profile = MachineProfile::from_hardware_profile(hardware);
    let mut machine = match source {
        BenchSource::BootSelector(selector) => {
            let mut m = Machine::new_boot_image(profile, neurketa_image())?;
            m.set_bench_selector(*selector);
            m
        }
        BenchSource::DosExe(exe) => Machine::new_raw_program(profile, exe)?,
    };
    machine.set_mode(mode);
    if let Some(sample_stride) = sample_stride {
        machine.enable_host_profiling(sample_stride);
    }
    let started = std::time::Instant::now();
    let stop = machine.run_until_halt_or_cycles(budget)?;
    let wall = started.elapsed();
    if !matches!(stop, StopReason::TestExit { .. }) {
        return Err(format!(
            "bench {} {source:?} did not exit cleanly: {stop:?}",
            mode.canonical_name()
        )
        .into());
    }
    let mut perf = machine.cpu().perf_counters().clone();
    perf.cache_tier_lookups = machine.cache_tier_lookups();
    Ok(BenchRun {
        clocks: machine.elapsed_clocks(),
        iterations: machine.bench_iterations(),
        aux: machine.bench_aux(),
        wall,
        perf,
        machine_profile: machine.host_profile_snapshot(),
        cpu_profile: machine.cpu().profile_snapshot(),
    })
}

/// A benchmark payload the harness can run: a display name, how the payload is
/// loaded, and the lowest GSW mode it applies to. FP payloads need an FPU, so
/// they start at 486.
struct Bench {
    name: &'static str,
    source: BenchSource<'static>,
    min_mode: GswMode,
    /// Floating-point operations per reported iteration. When set, the harness
    /// reports `MFLOPS = iters_per_sec * flops_per_iter / 1e6` and bands against it
    /// (Whetstone, the FP oracle). `None` benches band against raw `iters/sec`.
    flops_per_iter: Option<f64>,
}

const BENCHES: &[Bench] = &[
    Bench {
        name: "sieve",
        source: BenchSource::BootSelector(1),
        min_mode: GswMode::Gsw286,
        flops_per_iter: None,
    },
    Bench {
        name: "fp-mandel",
        source: BenchSource::BootSelector(3),
        min_mode: GswMode::Gsw486,
        flops_per_iter: None,
    },
    Bench {
        name: "dhrystone",
        source: BenchSource::DosExe(izarravm_firmware::DHRYSTONE_EXE),
        min_mode: GswMode::Gsw286,
        flops_per_iter: None,
    },
    // Whetstone: the FP oracle (486+). `flops_per_iter` is the per-sweep FLOP weight,
    // anchored so the era-calibrated 486 lands at ~6.5 MFLOPS (Roy Longbottom); the
    // 586 is then tuned to ~34.5 MFLOPS via fp_timing(I586). See whetstone.c.
    Bench {
        name: "whetstone",
        source: BenchSource::DosExe(izarravm_firmware::WHETSTONE_EXE),
        min_mode: GswMode::Gsw486,
        flops_per_iter: Some(WHETSTONE_FLOPS_PER_SWEEP),
    },
];

/// FLOP weight per Whetstone sweep (the value reported as one iteration). Anchored
/// to the era 486DX2-66 Whetstone figure (~6.5 MFLOPS): the measured 486 throughput
/// (250.0 sweeps/sec, era-calibrated 486 timing) times this over 1e6 == 6.5. A pure
/// units constant; the physical 586/486 ratio lives in fp_timing(I586).
const WHETSTONE_FLOPS_PER_SWEEP: f64 = 26000.0;

/// Rank the modes from slowest to fastest so a benchmark's min_mode gates which
/// modes it runs in.
fn mode_rank(mode: GswMode) -> u8 {
    match mode {
        GswMode::Gsw286 => 0,
        GswMode::Gsw386 => 1,
        GswMode::Gsw486 => 2,
        GswMode::Gsw586 => 3,
    }
}

/// Run every benchmark in each CPU mode it applies to, printing one labeled row
/// per benchmark per mode. The per-mode baseline (boot and report overhead) is
/// measured once per mode and subtracted from BootSelector payloads. DosExe
/// payloads have their own startup and report the full elapsed clocks.
fn run_bench(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    // The run stops at the guest's CMD_EXIT, so this is only a safety cap.
    const BENCH_BUDGET: u64 = 50_000_000_000;

    let modes = [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    // Per-mode baseline clocks for BootSelector benches, indexed by mode_rank.
    let mut baseline = [0u64; 4];
    for mode in modes {
        baseline[mode_rank(mode) as usize] =
            run_bench_one(hardware, mode, &BenchSource::BootSelector(0), BENCH_BUDGET)?.clocks;
    }

    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    // Collected for the host-side perf summary printed after the table.
    let mut perf_rows: Vec<(&'static str, GswMode, PerfCounters)> = Vec::new();
    for bench in BENCHES {
        for mode in modes {
            if mode_rank(mode) < mode_rank(bench.min_mode) {
                continue;
            }
            let run = run_bench_one(hardware, mode, &bench.source, BENCH_BUDGET)?;
            perf_rows.push((bench.name, mode, run.perf.clone()));
            let baseline_clocks = match bench.source {
                BenchSource::BootSelector(_) => baseline[mode_rank(mode) as usize],
                BenchSource::DosExe(_) => 0,
            };
            let work = run.clocks.saturating_sub(baseline_clocks);
            let iters = u64::from(run.iterations.max(1));
            let cyc_per_iter = work as f64 / iters as f64;
            let guest_secs = work as f64 / mode.clock_hz() as f64;
            let iters_per_sec = if guest_secs > 0.0 {
                iters as f64 / guest_secs
            } else {
                0.0
            };
            let wall_secs = run.wall.as_secs_f64();
            let rt = if wall_secs > 0.0 {
                guest_secs / wall_secs
            } else {
                0.0
            };
            // For an FP bench (flops_per_iter set) the metric of record is MFLOPS;
            // it is what we print and band against. Other benches band on iters/sec.
            let (band_value, mflops_suffix) = match bench.flops_per_iter {
                Some(w) => {
                    let mflops = iters_per_sec * w / 1e6;
                    (mflops, format!(" mflops={mflops:.2}"))
                }
                None => (iters_per_sec, String::new()),
            };
            print!(
                "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
                bench.name,
                mode.canonical_name(),
                cyc_per_iter,
                run.iterations,
                run.aux,
                iters_per_sec,
                guest_secs * 1000.0,
                wall_secs * 1000.0,
                rt,
            );
            // Soft reporter: tag each row against the era reference band. This is
            // observability only and never changes the exit code. After B-T9
            // calibration every row tags [in band] (the compute caps the bus model
            // cannot reach were relaxed to best-effort; see bench_reference.rs).
            println!(
                "{}{}",
                mflops_suffix,
                band_tag(bench.name, mode, band_value)
            );
        }
    }
    // Host-side perf summary (RPCS3 idea #1): decode-cache hit rate, average
    // straight-line run length, and why each run ended. Diagnostics only; lines are
    // prefixed "perf" so they never parse as a bench row. The counters are host-side
    // and do not affect cyc/iter (the guest clock metric).
    println!();
    println!("=== perf counters (host-side diagnostics; off the guest-timing path) ===");
    for (name, mode, perf) in &perf_rows {
        let instructions = perf.instructions.max(1);
        let decode_hit = 100.0 * (1.0 - perf.decode_misses as f64 / instructions as f64);
        let insns_per_run = perf.instructions as f64 / perf.straight_line_runs.max(1) as f64;
        println!(
            "perf  {:<10} {:<5} instr={:>13}  decode_hit={:>6.2}%  insns/run={:>9.1}  \
             brk[branch/step/int/cap/halt]={}/{}/{}/{}/{}  \
             data[rd d/s wr d/s]={}/{}/{}/{}  ptr[rd/wr]={}/{}  \
             page[h/m]={}/{}  fetch_page[h/m slow_refill]={}/{}/{}  \
             map_inv={}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}",
            name,
            mode.canonical_name(),
            perf.instructions,
            decode_hit,
            insns_per_run,
            perf.brk_decode_or_branch,
            perf.brk_step,
            perf.brk_interrupt,
            perf.brk_cap,
            perf.brk_halt,
            perf.data_direct_reads,
            perf.data_slow_reads,
            perf.data_direct_writes,
            perf.data_slow_writes,
            perf.direct_data_pointer_reads,
            perf.direct_data_pointer_writes,
            perf.direct_page_hits,
            perf.direct_page_misses,
            perf.fetch_page_hits,
            perf.fetch_page_misses,
            perf.slow_prefetch_refills,
            perf.direct_map_invalidations,
            perf.rep_string_fast_iterations,
            perf.rep_string_iterations,
            perf.flag_materializations,
            perf.cache_tier_lookups,
        );
    }
    Ok(())
}

fn run_bench_exe(path: &Path, hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    const BENCH_BUDGET: u64 = 50_000_000_000;
    let exe = std::fs::read(path)?;
    let mode = GswMode::Gsw586;
    let run = run_bench_one(hardware, mode, &BenchSource::DosExe(&exe), BENCH_BUDGET)?;
    let iters = u64::from(run.iterations.max(1));
    let cyc_per_iter = run.clocks as f64 / iters as f64;
    let guest_secs = run.clocks as f64 / mode.clock_hz() as f64;
    let iters_per_sec = if guest_secs > 0.0 {
        iters as f64 / guest_secs
    } else {
        0.0
    };
    let wall_secs = run.wall.as_secs_f64();
    let rt = if wall_secs > 0.0 {
        guest_secs / wall_secs
    } else {
        0.0
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("exe");
    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    println!(
        "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
        name,
        mode.canonical_name(),
        cyc_per_iter,
        run.iterations,
        run.aux,
        iters_per_sec,
        guest_secs * 1000.0,
        wall_secs * 1000.0,
        rt,
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BenchMetrics {
    cycles_per_iter: f64,
    guest_ms: f64,
    wall_ms: f64,
    rt_factor: f64,
    iters_per_sec: f64,
}

fn bench_metrics(run: &BenchRun, mode: GswMode) -> BenchMetrics {
    let iters = u64::from(run.iterations.max(1));
    let cycles_per_iter = run.clocks as f64 / iters as f64;
    let guest_secs = run.clocks as f64 / mode.clock_hz() as f64;
    let iters_per_sec = if guest_secs > 0.0 {
        iters as f64 / guest_secs
    } else {
        0.0
    };
    let wall_secs = run.wall.as_secs_f64();
    let rt_factor = if wall_secs > 0.0 {
        guest_secs / wall_secs
    } else {
        0.0
    };
    BenchMetrics {
        cycles_per_iter,
        guest_ms: guest_secs * 1000.0,
        wall_ms: wall_secs * 1000.0,
        rt_factor,
        iters_per_sec,
    }
}

fn print_single_bench_row(name: &str, mode: GswMode, run: &BenchRun) {
    let metrics = bench_metrics(run, mode);
    println!(
        "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
        name,
        mode.canonical_name(),
        metrics.cycles_per_iter,
        run.iterations,
        run.aux,
        metrics.iters_per_sec,
        metrics.guest_ms,
        metrics.wall_ms,
        metrics.rt_factor,
    );
}

fn run_profile_exe(
    path: &Path,
    json_path: Option<&Path>,
    sample_stride: u64,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    const BENCH_BUDGET: u64 = 50_000_000_000;
    let exe = std::fs::read(path)?;
    let mode = GswMode::Gsw586;
    let source = BenchSource::DosExe(&exe);
    let baseline = run_bench_one(hardware, mode, &source, BENCH_BUDGET)?;
    let profiled =
        run_bench_one_profiled(hardware, mode, &source, BENCH_BUDGET, Some(sample_stride))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("exe");

    println!("# baseline");
    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    print_single_bench_row(name, mode, &baseline);

    let baseline_metrics = bench_metrics(&baseline, mode);
    println!();
    print_machine_profile(&profiled.machine_profile);
    println!();
    print_cpu_profile(&profiled.cpu_profile);
    println!();
    println!("=== perf counters ===");
    print_perf_counter_row("profile", mode, &profiled.perf);

    if let Some(json_path) = json_path {
        write_profile_json(
            path,
            json_path,
            sample_stride,
            &baseline,
            &profiled,
            baseline_metrics,
        )?;
    }
    Ok(())
}

fn print_machine_profile(snapshot: &MachineHostProfileSnapshot) {
    let mut phases = snapshot.phases.clone();
    phases.sort_by_key(|phase| Reverse(phase.wall_ns));
    let total_ns = phases.iter().map(|phase| phase.wall_ns).sum::<u64>().max(1);
    println!("=== machine phases ===");
    println!(
        "{:<20} {:>12} {:>10} {:>8}",
        "phase", "wall_ms", "count", "share"
    );
    for phase in phases
        .iter()
        .filter(|phase| phase.count > 0 || phase.wall_ns > 0)
    {
        println!(
            "{:<20} {:>12.3} {:>10} {:>7.2}%",
            phase.name,
            phase.wall_ns as f64 / 1_000_000.0,
            phase.count,
            100.0 * phase.wall_ns as f64 / total_ns as f64,
        );
    }
}

fn print_cpu_profile(snapshot: &CpuProfileSnapshot) {
    let mut groups = snapshot.groups.clone();
    groups.sort_by_key(|group| Reverse(group.sample_wall_ns));
    let total_instructions = groups
        .iter()
        .map(|group| group.instructions)
        .sum::<u64>()
        .max(1);
    let total_guest = groups
        .iter()
        .map(|group| group.guest_core_clocks)
        .sum::<u64>()
        .max(1);
    let total_sample = groups
        .iter()
        .map(|group| group.sample_wall_ns)
        .sum::<u64>()
        .max(1);
    println!(
        "=== cpu groups (sample_stride={}) ===",
        snapshot.sample_stride
    );
    println!(
        "{:<18} {:>13} {:>8} {:>13} {:>8} {:>12} {:>8} {:>9}",
        "group", "instr", "instr%", "guest_clk", "guest%", "sample_ms", "sample%", "samples"
    );
    for group in groups
        .iter()
        .filter(|group| group.instructions > 0 || group.samples > 0)
    {
        println!(
            "{:<18} {:>13} {:>7.2}% {:>13} {:>7.2}% {:>12.3} {:>7.2}% {:>9}",
            group.name,
            group.instructions,
            100.0 * group.instructions as f64 / total_instructions as f64,
            group.guest_core_clocks,
            100.0 * group.guest_core_clocks as f64 / total_guest as f64,
            group.sample_wall_ns as f64 / 1_000_000.0,
            100.0 * group.sample_wall_ns as f64 / total_sample as f64,
            group.samples,
        );
    }

    let mut opcodes = snapshot.opcodes.clone();
    opcodes.sort_by_key(|opcode| Reverse((opcode.sample_wall_ns, opcode.instructions)));
    println!();
    println!(
        "=== cpu opcodes (top {}, sample_stride={}) ===",
        CPU_OPCODE_PROFILE_PRINT_LIMIT, snapshot.sample_stride
    );
    println!(
        "{:<8} {:<18} {:>13} {:>8} {:>13} {:>8} {:>12} {:>8} {:>9} {:>9} {:>9}",
        "opcode",
        "group",
        "instr",
        "instr%",
        "guest_clk",
        "guest%",
        "sample_ms",
        "sample%",
        "samples",
        "reg_i",
        "mem_i"
    );
    for opcode in opcodes
        .iter()
        .filter(|opcode| opcode.instructions > 0 || opcode.samples > 0)
        .take(CPU_OPCODE_PROFILE_PRINT_LIMIT)
    {
        println!(
            "{:<8} {:<18} {:>13} {:>7.2}% {:>13} {:>7.2}% {:>12.3} {:>7.2}% {:>9} {:>9} {:>9}",
            format_profile_opcode(opcode.opcode),
            opcode.group,
            opcode.instructions,
            100.0 * opcode.instructions as f64 / total_instructions as f64,
            opcode.guest_core_clocks,
            100.0 * opcode.guest_core_clocks as f64 / total_guest as f64,
            opcode.sample_wall_ns as f64 / 1_000_000.0,
            100.0 * opcode.sample_wall_ns as f64 / total_sample as f64,
            opcode.samples,
            opcode.register_instructions,
            opcode.memory_instructions,
        );
    }
}

fn format_profile_opcode(opcode: u16) -> String {
    if opcode & 0xff00 == 0x0f00 {
        format!("0F {:02X}", opcode as u8)
    } else {
        format!("{:02X}", opcode as u8)
    }
}

fn write_profile_json(
    exe_path: &Path,
    json_path: &Path,
    sample_stride: u64,
    baseline: &BenchRun,
    profiled: &BenchRun,
    baseline_metrics: BenchMetrics,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = json_path
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

    let mut machine_phases = profiled.machine_profile.phases.clone();
    machine_phases.sort_by_key(|phase| Reverse(phase.wall_ns));
    let mut cpu_groups = profiled.cpu_profile.groups.clone();
    cpu_groups.sort_by_key(|group| Reverse(group.sample_wall_ns));
    let mut cpu_opcodes = profiled.cpu_profile.opcodes.clone();
    cpu_opcodes.sort_by_key(|opcode| Reverse((opcode.sample_wall_ns, opcode.instructions)));
    let report = json!({
        "schema": "izarravm-profile-v1",
        "exe": exe_path.display().to_string(),
        "mode": "586",
        "sample_stride": sample_stride.max(1),
        "baseline": {
            "wall_ms": baseline_metrics.wall_ms,
            "guest_ms": baseline_metrics.guest_ms,
            "rt_factor": baseline_metrics.rt_factor,
            "cycles_per_iter": baseline_metrics.cycles_per_iter,
            "iters": baseline.iterations,
            "aux": baseline.aux,
        },
        "profile": {
            "wall_ms": profiled.wall.as_secs_f64() * 1000.0,
            "machine_phases": machine_phases.iter().map(|phase| json!({
                "name": phase.name,
                "wall_ns": phase.wall_ns,
                "count": phase.count,
            })).collect::<Vec<_>>(),
            "cpu_groups": cpu_groups.iter().map(|group| json!({
                "name": group.name,
                "instructions": group.instructions,
                "guest_core_clocks": group.guest_core_clocks,
                "sample_wall_ns": group.sample_wall_ns,
                "samples": group.samples,
            })).collect::<Vec<_>>(),
            "cpu_opcodes": cpu_opcodes.iter().map(|opcode| json!({
                "opcode": format_profile_opcode(opcode.opcode),
                "opcode_raw": opcode.opcode,
                "group": opcode.group,
                "instructions": opcode.instructions,
                "guest_core_clocks": opcode.guest_core_clocks,
                "sample_wall_ns": opcode.sample_wall_ns,
                "samples": opcode.samples,
                "register_instructions": opcode.register_instructions,
                "memory_instructions": opcode.memory_instructions,
                "register_samples": opcode.register_samples,
                "memory_samples": opcode.memory_samples,
            })).collect::<Vec<_>>(),
            "perf": perf_counters_json(&profiled.perf),
        },
    });
    std::fs::write(json_path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

fn perf_counters_json(perf: &PerfCounters) -> serde_json::Value {
    json!({
        "instructions": perf.instructions,
        "decode_misses": perf.decode_misses,
        "straight_line_runs": perf.straight_line_runs,
        "brk_decode_or_branch": perf.brk_decode_or_branch,
        "brk_step": perf.brk_step,
        "brk_interrupt": perf.brk_interrupt,
        "brk_cap": perf.brk_cap,
        "brk_halt": perf.brk_halt,
        "data_direct_reads": perf.data_direct_reads,
        "data_slow_reads": perf.data_slow_reads,
        "data_direct_writes": perf.data_direct_writes,
        "data_slow_writes": perf.data_slow_writes,
        "direct_page_hits": perf.direct_page_hits,
        "direct_page_misses": perf.direct_page_misses,
        "direct_data_pointer_reads": perf.direct_data_pointer_reads,
        "direct_data_pointer_writes": perf.direct_data_pointer_writes,
        "fetch_page_hits": perf.fetch_page_hits,
        "fetch_page_misses": perf.fetch_page_misses,
        "slow_prefetch_refills": perf.slow_prefetch_refills,
        "direct_map_invalidations": perf.direct_map_invalidations,
        "rep_string_iterations": perf.rep_string_iterations,
        "rep_string_fast_iterations": perf.rep_string_fast_iterations,
        "flag_materializations": perf.flag_materializations,
        "cache_tier_lookups": perf.cache_tier_lookups,
    })
}

fn print_perf_counter_row(name: &str, mode: GswMode, perf: &PerfCounters) {
    let instructions = perf.instructions.max(1);
    let decode_hit = 100.0 * (1.0 - perf.decode_misses as f64 / instructions as f64);
    let insns_per_run = perf.instructions as f64 / perf.straight_line_runs.max(1) as f64;
    println!(
        "perf  {:<10} {:<5} instr={:>13}  decode_hit={:>6.2}%  insns/run={:>9.1}  \
         brk[branch/step/int/cap/halt]={}/{}/{}/{}/{}  \
         data[rd d/s wr d/s]={}/{}/{}/{}  ptr[rd/wr]={}/{}  \
         page[h/m]={}/{}  fetch_page[h/m slow_refill]={}/{}/{}  \
         map_inv={}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}",
        name,
        mode.canonical_name(),
        perf.instructions,
        decode_hit,
        insns_per_run,
        perf.brk_decode_or_branch,
        perf.brk_step,
        perf.brk_interrupt,
        perf.brk_cap,
        perf.brk_halt,
        perf.data_direct_reads,
        perf.data_slow_reads,
        perf.data_direct_writes,
        perf.data_slow_writes,
        perf.direct_data_pointer_reads,
        perf.direct_data_pointer_writes,
        perf.direct_page_hits,
        perf.direct_page_misses,
        perf.fetch_page_hits,
        perf.fetch_page_misses,
        perf.slow_prefetch_refills,
        perf.direct_map_invalidations,
        perf.rep_string_fast_iterations,
        perf.rep_string_iterations,
        perf.flag_materializations,
        perf.cache_tier_lookups,
    );
}

/// Compare a measured `iters/sec` to the matching era reference band and return
/// a tag to append to the row: ` [in band]`, ` [LOW <ratio>]`, ` [HIGH <ratio>]`,
/// or empty when no band is encoded for this payload/mode.
fn band_tag(payload: &str, mode: GswMode, iters_per_sec: f64) -> String {
    use bench_reference::BandVerdict;
    let Some(band) = bench_reference::band_for(payload, mode) else {
        return String::new();
    };
    match band.verdict(iters_per_sec) {
        BandVerdict::InBand => " [in band]".to_string(),
        BandVerdict::Low => format!(" [LOW {:.2}]", iters_per_sec / band.target),
        BandVerdict::High => format!(" [HIGH {:.2}]", iters_per_sec / band.target),
    }
}

/// Block sizes swept by --headless-bandwidth, powers of two from 4 KB to 4 MB.
/// A block that fits the live mode's cache stays resident across passes; one that
/// exceeds it re-misses every pass. The largest block (4 MB) plus the 1 MB base
/// tops out at 5 MB, well inside the 24 MB machine.
const BANDWIDTH_BLOCKS: &[u32] = &[
    4 * 1024,
    8 * 1024,
    16 * 1024,
    32 * 1024,
    64 * 1024,
    128 * 1024,
    256 * 1024,
    512 * 1024,
    1024 * 1024,
    2 * 1024 * 1024,
    4 * 1024 * 1024,
];

/// A simplified SpeedSys-style memory-read bandwidth sweep. For each CPU mode it
/// drives the bus directly from the host (no guest program, so it can touch any
/// physical address) over a range of block sizes and prints MB/s per block, so a
/// human can see the L1/L2/RAM cache tiers as steps in the curve.
///
/// This is observability only: it never fails the process (the hard tier-ordering
/// assertions are a later task). The tier costs are CALIBRATED (B-T9): the curve
/// steps DOWN at each cache boundary (586/486: L1 > L2 > RAM; 386: L2 > RAM; 286:
/// flat RAM), and each tier sits in its (best-effort) era band.
fn run_bandwidth(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    // A fixed total budget per block: small blocks do many passes, large blocks a
    // few. 16 MB amortizes the cold first pass so the steady state dominates.
    const TOTAL: u64 = 16 * 1024 * 1024;

    let modes = [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    println!(
        "memory read bandwidth sweep (TOTAL {} MB per block, base 0x{:06X})",
        TOTAL / (1024 * 1024),
        0x10_0000u32,
    );
    println!("(tier costs calibrated: the curve steps down at each cache boundary)");

    for mode in modes {
        println!();
        println!(
            "mode {} @ {:.2} MHz  L1/L2 = {:?} KB",
            mode.canonical_name(),
            mode.clock_hz() as f64 / 1.0e6,
            mode.cache_kb(),
        );
        println!("{:>8} {:>12} {:>16}", "block", "MB/s", "band");
        for &block in BANDWIDTH_BLOCKS {
            // A fresh machine per (mode, size) so each measurement starts cold and
            // nothing carries over from the previous block.
            let mut machine = Machine::new_boot_image(
                MachineProfile::from_hardware_profile(hardware),
                izarravm_firmware::neurketa_image(),
            )?;
            machine.set_mode(mode);
            let sample = machine.measure_read_bandwidth(0x10_0000, block, TOTAL);
            let mb_per_sec = if sample.clocks > 0 {
                sample.bytes as f64 / (sample.clocks as f64 / mode.clock_hz() as f64) / 1.0e6
            } else {
                0.0
            };
            println!(
                "{:>7}K {:>12.1} {:>16}",
                block / 1024,
                mb_per_sec,
                bandwidth_band_tag(mode, block, mb_per_sec),
            );
        }
    }
    Ok(())
}

/// Soft band tag for a bandwidth row: pick the tier a block falls into for the
/// mode's cache geometry (<= L1 -> L1, <= L2 -> L2, else RAM), look up the
/// matching `bandwidth-*` era band, and tag the measured MB/s against it. Returns
/// empty when no band is encoded for that mode/tier. Observability only.
fn bandwidth_band_tag(mode: GswMode, block: u32, mb_per_sec: f64) -> String {
    use bench_reference::BandVerdict;
    let (l1_kb, l2_kb) = mode.cache_kb();
    let block_kb = block / 1024;
    let tier = if l1_kb != 0 && block_kb <= u32::from(l1_kb) {
        "bandwidth-l1"
    } else if l2_kb != 0 && block_kb <= u32::from(l2_kb) {
        "bandwidth-l2"
    } else {
        "bandwidth-ram"
    };
    let Some(band) = bench_reference::band_for(tier, mode) else {
        return String::new();
    };
    let label = tier.trim_start_matches("bandwidth-");
    match band.verdict(mb_per_sec) {
        BandVerdict::InBand => format!("{label} [in band]"),
        BandVerdict::Low => format!("{label} [LOW {:.2}]", mb_per_sec / band.target),
        BandVerdict::High => format!("{label} [HIGH {:.2}]", mb_per_sec / band.target),
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
fn run_izarra_bios(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    // The graphical POST blit and RAM sweep need more than the old 200 ms budget.
    let budget = hardware.clock_hz;
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
    Ok(())
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
fn run_boot_hdd_folder(
    dir: &Path,
    cycles: Option<u64>,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_hdd_folder(dir)?;
    let budget = cycles.unwrap_or(DEFAULT_BOOT_HDD_CYCLES);
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;

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
    Ok(())
}

/// After a headless run, report the active video mode and whether the screen
/// holds meaningful content. It renders a full frame and counts non-background
/// pixels with a small histogram of the busiest DAC indices; in text mode it
/// also prints the 80x25 page. A human reads this to confirm a booter drew its
/// title or menu rather than sitting on a blank screen.
fn print_video_summary(machine: &mut Machine) {
    use izarravm_video::VideoMode;

    let mode = machine.video().active_mode();
    let mode_name = match mode {
        VideoMode::Text => "text (03h)",
        VideoMode::Mode13h => "mode 13h (320x200x256)",
        VideoMode::Planar => "planar (EGA/VGA 16-color)",
        VideoMode::ModeX => "mode X (unchained 256-color)",
        VideoMode::Cga => "CGA graphics (320x200x4 / 640x200x2)",
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

    // Render one full frame and summarize the pixel indices (works for text and
    // graphics modes alike: render_full_frame walks the CRTC scanlines). The
    // background is DAC index 0 (black on the stock palette), so non-zero pixels
    // mean the guest drew something.
    let raster = machine.video_mut().render_full_frame();
    let total = raster.pixels.len();
    let nonzero = raster.pixels.iter().filter(|&&p| p != 0).count();
    println!(
        "framebuffer: {}x{} ({total} px)",
        raster.width, raster.height
    );
    println!(
        "non-zero pixels: {nonzero} ({:.1}%)",
        if total == 0 {
            0.0
        } else {
            100.0 * nonzero as f64 / total as f64
        }
    );
    let mut histogram = [0u32; 256];
    for &index in &raster.pixels {
        histogram[index as usize] += 1;
    }
    let mut entries: Vec<(usize, u32)> = histogram
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, count)| count > 0)
        .collect();
    entries.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    let top: Vec<String> = entries
        .iter()
        .take(8)
        .map(|(index, count)| format!("idx {index}: {count}"))
        .collect();
    println!("distinct colors: {}", entries.len());
    println!("top indices: {}", top.join(", "));
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
    config.apply_overrides(ConfigOverrides {
        cpu: cli.cpu,
        memory_mib: cli.memory_mib,
        video: cli.video,
        c_drive,
        soundfont: cli.soundfont.clone(),
        midi_backend: cli.midi_backend,
        sb_irq: cli.sb_irq,
        sb_dma: cli.sb_dma,
        sb_high_dma: cli.sb_high_dma,
    });

    Ok(config)
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

/// Map a Windows LANGID to one of the 17 guest layout indices (see the canonical
/// table in dev_docs/2026-06-26-keyboard-layout-import-design.md). Regions that
/// share a language but use different keyboards are matched on the full LANGID
/// first; everything else falls back to the primary-language default, then US.
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
mod tests {
    use super::*;

    #[test]
    fn ascii_to_set1_maps_a_letter_to_make_and_break() {
        assert_eq!(ascii_to_set1('h'), vec![0x23, 0xa3]);
        // Uppercase wraps the key in left-Shift make/break.
        assert_eq!(ascii_to_set1('H'), vec![0x2a, 0x23, 0xa3, 0xaa]);
        // Enter is the unshifted return key.
        assert_eq!(ascii_to_set1('\r'), vec![0x1c, 0x9c]);
        // A shifted number-row glyph holds Shift over the digit key.
        assert_eq!(ascii_to_set1('!'), vec![0x2a, 0x02, 0x82, 0xaa]);
        // Characters with no US-layout key produce nothing.
        assert!(ascii_to_set1('\u{00f1}').is_empty());
    }

    #[test]
    fn langid_maps_to_guest_layout_index() {
        assert_eq!(layout_index_from_langid(0x0409), 0); // en-US
        assert_eq!(layout_index_from_langid(0x0809), 1); // en-GB
        assert_eq!(layout_index_from_langid(0x1009), 0); // en-CA -> US
        assert_eq!(layout_index_from_langid(0x0c0a), 2); // es-ES
        assert_eq!(layout_index_from_langid(0x080a), 16); // es-MX -> Latin America
        assert_eq!(layout_index_from_langid(0x040c), 3); // fr-FR
        assert_eq!(layout_index_from_langid(0x0407), 4); // de-DE
        assert_eq!(layout_index_from_langid(0x0410), 5); // it-IT
        assert_eq!(layout_index_from_langid(0x0411), 0); // ja-JP -> US fallback
    }

    #[test]
    fn langid_maps_new_layouts() {
        assert_eq!(layout_index_from_langid(0x080c), 6); // fr-BE -> BE
        assert_eq!(layout_index_from_langid(0x0c0c), 7); // fr-CA -> CF
        assert_eq!(layout_index_from_langid(0x0406), 8); // da-DK -> DK
        assert_eq!(layout_index_from_langid(0x0413), 9); // nl-NL -> NL
        assert_eq!(layout_index_from_langid(0x0414), 10); // nb-NO -> NO
        assert_eq!(layout_index_from_langid(0x0816), 11); // pt-PT -> PO
        assert_eq!(layout_index_from_langid(0x100c), 12); // fr-CH -> SF
        assert_eq!(layout_index_from_langid(0x0807), 13); // de-CH -> SG
        assert_eq!(layout_index_from_langid(0x040b), 14); // fi-FI -> SU
        assert_eq!(layout_index_from_langid(0x041d), 15); // sv-SE -> SV
    }

    #[test]
    fn codepage_index_for_each_layout() {
        let want = [0u8, 0, 1, 1, 1, 1, 1, 3, 4, 1, 4, 2, 1, 1, 1, 1, 1];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(codepage_index_for_layout(i as u8), *w);
        }
    }

    #[test]
    fn katea_run_prog_name_picks_a_clean_8_3_name() {
        use std::path::Path;
        assert_eq!(katea_run_prog_name(Path::new("/x/FOO.EXE")), "PROG.EXE");
        assert_eq!(katea_run_prog_name(Path::new("bar.com")), "PROG.COM");
        assert_eq!(katea_run_prog_name(Path::new("noext")), "PROG.COM");
        assert_eq!(katea_run_prog_name(Path::new("a.longext")), "PROG.LON");
    }

    #[test]
    fn c_root_path_lives_under_dot_izarravm_when_not_portable() {
        let p = super::c_root_path(false);
        assert!(
            p.ends_with(std::path::Path::new(".izarravm").join("c_drive")),
            "default C: root should end with .izarravm/c_drive, got {p:?}"
        );
    }

    #[test]
    fn c_root_path_is_a_bare_c_drive_when_portable() {
        // Portable mode keys off the executable's own directory, so the path is
        // just <exe_dir>/c_drive — no ~/.izarravm prefix.
        let p = super::c_root_path(true);
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some("c_drive"),
            "portable C: root should be a c_drive folder, got {p:?}"
        );
        assert!(
            !p.to_string_lossy().contains(".izarravm"),
            "portable C: root must not use the ~/.izarravm prefix, got {p:?}"
        );
    }
}

#[cfg(test)]
mod tokados_smoke {
    use super::*;
    use izarravm_machine::StopReason;

    /// Boot the committed FAT32 HDD image (no floppy): INT 19h reads LBA 0 (the
    /// MBR), which chains to the partition VBR, which loads KERNEL.SYS. The kernel
    /// then mounts the FAT32 partition as C: and launches the shell.
    fn boot_hdd(cycles: u64) -> (Machine, StopReason) {
        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine.mount_hdd(izarravm_firmware::tokados_hdd_img().to_vec());
        let stop = machine
            .run_until_halt_or_cycles(cycles)
            .expect("run machine");
        (machine, stop)
    }

    // Katea-1 Milestone-0 GO/NO-GO gate: the real FreeDOS kernel must boot to
    // C:\> from the synthesized static FAT32 HDD image (no floppy) and read a file
    // off the volume. If this passes, the static-image approach is proven and the
    // lazy host-folder facade work can begin; if it cannot reach C:\>, it is a
    // NO-GO and we stop before any facade work.
    #[test]
    #[ignore = "boots a full DOS image from a FAT32 HDD (slow in debug); run with --ignored"]
    fn katea_static_hdd_boots() {
        let (mut machine, stop) = boot_hdd(500_000_000);
        if let StopReason::CpuError(msg) = &stop {
            let text = machine.screen_text().as_text();
            panic!("CPU fault during Katea HDD boot: {msg}\nstop={stop:?}\n{text}");
        }
        let text = machine.screen_text().as_text().to_ascii_lowercase();
        // The kernel must assign the FAT32 partition to C: and prompt there, NOT A:.
        assert!(
            text.contains("c:\\>"),
            "no C:\\> prompt after HDD boot (stop={stop:?}).\n{text}"
        );

        // VER: the kernel responds with its version banner.
        for ch in "ver\r".chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type ver");
        }
        machine
            .run_until_halt_or_cycles(20_000_000)
            .expect("settle ver");
        let ver_text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            ver_text.contains("c:\\>ver"),
            "VER not echoed at the C: prompt.\n{ver_text}"
        );

        // DIR C: must list the test file from the FAT32 root directory, proving the
        // kernel read the volume's filesystem.
        for ch in "dir c:\\\r".chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type dir");
        }
        machine
            .run_until_halt_or_cycles(40_000_000)
            .expect("settle dir");
        let dir_text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            dir_text.contains("hello"),
            "DIR C: did not list HELLO.TXT off the FAT32 volume.\n{dir_text}"
        );
    }

    /// The Katea host-folder facade end-to-end: mount a real host directory as C:
    /// through `mount_hdd_folder` (the lazy facade, not a flat image), boot the
    /// real FreeDOS kernel, and confirm it reaches C:\> and lists a file that lives
    /// only in the host folder. This proves the mount path — system files from the
    /// committed image + host files folded to 8.3 — boots and reads host files,
    /// the M0 deliverable.
    #[test]
    #[ignore = "boots a full DOS image from a host-folder facade (slow in debug); run with --ignored"]
    fn katea_host_folder_boots_and_lists_a_host_file() {
        // A unique scratch folder under the system temp dir, holding one host file
        // whose 8.3 name (GREETING.TXT — already a valid 8.3 name, no ~n fold) is
        // distinct from the system files, so spotting it in DIR proves the host
        // folder reached the volume.
        let dir = std::env::temp_dir().join(format!(
            "katea_folder_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("GREETING.TXT"), b"hi from the host folder\r\n")
            .expect("write host file");

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine.mount_hdd_folder(&dir).expect("mount host folder");
        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("run machine");
        if let StopReason::CpuError(msg) = &stop {
            let text = machine.screen_text().as_text();
            std::fs::remove_dir_all(&dir).ok();
            panic!("CPU fault during Katea folder boot: {msg}\nstop={stop:?}\n{text}");
        }
        let text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            text.contains("c:\\>"),
            "no C:\\> prompt after folder boot (stop={stop:?}).\n{text}"
        );

        // DIR C: must list the host file by its 8.3 name (GREETING.TXT).
        for ch in "dir c:\\\r".chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type dir");
        }
        machine
            .run_until_halt_or_cycles(40_000_000)
            .expect("settle dir");
        let dir_text = machine.screen_text().as_text().to_ascii_lowercase();

        // TYPE the host file: this drives the kernel to READ the file's data, which
        // the facade serves lazily (open+seek+read) straight from the host file on
        // disk — proving the data path, not just the synthesized directory entry.
        // The host folder must still exist while the read happens, so the cleanup
        // waits until after both screens are captured.
        for ch in "type c:\\greeting.txt\r".chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type the type command");
        }
        machine
            .run_until_halt_or_cycles(40_000_000)
            .expect("settle type");
        let type_text = machine.screen_text().as_text().to_ascii_lowercase();

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            dir_text.contains("greeti"),
            "DIR C: did not list the host file off the folder facade.\n{dir_text}"
        );
        assert!(
            type_text.contains("hi from the host folder"),
            "TYPE did not print the host file's contents read lazily through the facade.\n{type_text}"
        );
    }

    /// Regression: with the full graphical POST (the GUI default), the BIOS leaves
    /// the Margo linear framebuffer active. A real-MBR FreeDOS boot never sets a
    /// video mode, so without the izbios mode-03h reset before the boot jump the
    /// GUI stays frozen on the POST splash while the booted OS writes to the hidden
    /// VGA text buffer. Boot a host folder with `set_fast_post(false)` (so the
    /// graphical POST runs and activates Margo) and assert the display latch is
    /// cleared — i.e. the BIOS handed the screen back to VGA text — by the C:\>
    /// prompt. The headless `screen_text()` rasterizes the VGA core regardless of
    /// the latch, so the `margo_active()` check (not the screen text) is what
    /// guards the GUI-visible bug.
    #[test]
    #[ignore = "boots a full DOS image under the slow graphical POST; run with --ignored"]
    fn katea_full_post_boot_hands_display_back_to_vga() {
        let dir = std::env::temp_dir().join(format!(
            "katea_fullpost_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("GREETING.TXT"), b"hi\r\n").expect("write host file");

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        // The GUI runs the full graphical POST, which lights the Margo LFB; headless
        // defaults to fast POST (text only), so this is the path the e2e tests miss.
        machine.set_fast_post(false);
        machine.mount_hdd_folder(&dir).expect("mount host folder");
        let stop = machine
            .run_until_halt_or_cycles(600_000_000)
            .expect("run machine");
        let text = machine.screen_text().as_text().to_ascii_lowercase();
        std::fs::remove_dir_all(&dir).ok();
        if let StopReason::CpuError(msg) = &stop {
            panic!("CPU fault during full-POST Katea boot: {msg}\n{text}");
        }
        assert!(
            text.contains("c:\\>"),
            "no C:\\> prompt after full-POST folder boot (stop={stop:?}).\n{text}"
        );
        assert!(
            !machine.margo_active(),
            "the BIOS must hand the display back to VGA text before booting; the \
             Margo LFB is still active, so the GUI would show the frozen POST splash"
        );
    }

    /// The Katea-1 M1 boot-coherence gate: a host folder with a file *in a
    /// subfolder* boots the real FreeDOS kernel, the subfolder is navigable, and a
    /// file at depth reads lazily through the recursive tree facade. This is the
    /// milestone's success gate — it proves the tree volume (`KateaTreeVolume`)
    /// produces a self-consistent, bootable disk and that `CD` into a synthesized
    /// subdirectory plus a `TYPE` of a file two levels down (`C:\GAMES\HELLO\`)
    /// returns the host file's bytes, which M0's flat facade could never expose.
    #[test]
    #[ignore = "boots a full DOS image from a host-folder tree (slow in debug); run with --ignored"]
    fn katea_host_folder_tree_reads_a_file_in_a_subfolder() {
        // A unique scratch tree under the system temp dir: GAMES\HELLO\READAT.TXT
        // lives two directory levels down, so reaching it proves the subfolder
        // chain (root -> GAMES -> HELLO) is navigable and the file reads at depth.
        let dir = std::env::temp_dir().join(format!(
            "katea_tree_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let depth = dir.join("GAMES").join("HELLO");
        std::fs::create_dir_all(&depth).expect("scratch tree");
        std::fs::write(depth.join("READAT.TXT"), b"read at depth ok\r\n")
            .expect("write depth file");

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine
            .mount_hdd_folder(&dir)
            .expect("mount host folder tree");
        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("run machine");
        if let StopReason::CpuError(msg) = &stop {
            let text = machine.screen_text().as_text();
            std::fs::remove_dir_all(&dir).ok();
            panic!("CPU fault during Katea tree boot: {msg}\nstop={stop:?}\n{text}");
        }
        let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            boot_text.contains("c:\\>"),
            "no C:\\> prompt after tree boot (stop={stop:?}).\n{boot_text}"
        );

        // CD into the subfolder two levels down, then DIR it. DIR listing the file
        // proves the synthesized subdirectory chain (with its `.`/`..` entries) is
        // navigable.
        for cmd in ["cd games\\hello\r", "dir\r"] {
            for ch in cmd.chars() {
                for code in ascii_to_set1(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine
                    .run_until_halt_or_cycles(5_000_000)
                    .expect("type cmd");
            }
            machine
                .run_until_halt_or_cycles(40_000_000)
                .expect("settle cmd");
        }
        let dir_text = machine.screen_text().as_text().to_ascii_lowercase();

        // TYPE the file at depth: the kernel reads its data clusters, which the
        // tree facade serves lazily (open+seek+read) straight from the host file
        // under GAMES\HELLO — proving the lazy data path at depth, not just the
        // synthesized directory entry. The host tree must still exist while the
        // read happens, so cleanup waits until both screens are captured.
        for ch in "type readat.txt\r".chars() {
            for code in ascii_to_set1(ch) {
                machine.inject_key_scancodes(&[code]);
            }
            machine
                .run_until_halt_or_cycles(5_000_000)
                .expect("type the type command");
        }
        machine
            .run_until_halt_or_cycles(40_000_000)
            .expect("settle type");
        let type_text = machine.screen_text().as_text().to_ascii_lowercase();

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            dir_text.contains("readat"),
            "DIR in the subfolder did not list the file at depth.\n{dir_text}"
        );
        assert!(
            type_text.contains("read at depth ok"),
            "TYPE did not print the subfolder file's contents read lazily through the tree facade.\n{type_text}"
        );
    }

    /// Katea-1 M2 success gate: real FreeDOS, booted from a host folder, creates a
    /// new file, overwrites an existing one, and grows one — all in a subfolder,
    /// plus MKDIR a host subdir with a file at depth — and the changes appear
    /// correctly in the host folder (verified by host-side read-back), with the
    /// read tree + boot intact.
    #[test]
    #[ignore = "boots a full DOS image and writes through Katea (slow in debug); run with --ignored"]
    fn katea_host_folder_writes_create_overwrite_grow_in_a_subfolder() {
        let dir = std::env::temp_dir().join(format!(
            "katea_m2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let saves = dir.join("SAVES");
        std::fs::create_dir_all(&saves).expect("scratch tree");
        // Pre-seed a file to overwrite and a batch file holding the write commands
        // as exact bytes (so `>`/`>>`/`\` never go through the keyboard).
        std::fs::write(saves.join("OLD.TXT"), b"before\r\n").expect("seed OLD.TXT");
        let make_bat = b"echo created>NEW.TXT\r\n\
echo overwritten>OLD.TXT\r\n\
echo line1>GROW.TXT\r\n\
echo line2>>GROW.TXT\r\n\
mkdir SUB\r\n\
echo deep>SUB\\DEEP.TXT\r\n";
        std::fs::write(saves.join("MAKE.BAT"), make_bat).expect("seed MAKE.BAT");

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine.mount_hdd_folder(&dir).expect("mount host folder");
        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("run machine");
        if let StopReason::CpuError(msg) = &stop {
            let text = machine.screen_text().as_text();
            std::fs::remove_dir_all(&dir).ok();
            panic!("CPU fault during Katea M2 boot: {msg}\nstop={stop:?}\n{text}");
        }
        let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            boot_text.contains("c:\\>"),
            "no C:\\> prompt after boot (stop={stop:?}).\n{boot_text}"
        );

        // Only letters are typed: cd into SAVES (cheap), then run MAKE.BAT (six
        // file-writing commands, each doing disk I/O + an inline reconcile, so it
        // gets a far larger settle budget than the trivial cd).
        for (cmd, settle) in [("cd saves\r", 40_000_000u64), ("make\r", 120_000_000u64)] {
            for ch in cmd.chars() {
                for code in ascii_to_set1(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine
                    .run_until_halt_or_cycles(5_000_000)
                    .expect("type cmd");
            }
            machine
                .run_until_halt_or_cycles(settle)
                .expect("settle cmd");
        }

        // Final reconcile, then read the host folder back.
        machine.flush_hdd_folder();

        let read = |p: std::path::PathBuf| -> String {
            String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).to_string()
        };
        let new_txt = read(saves.join("NEW.TXT"));
        let old_txt = read(saves.join("OLD.TXT"));
        let grow_txt = read(saves.join("GROW.TXT"));
        let deep_txt = read(saves.join("SUB").join("DEEP.TXT"));
        let sub_is_dir = saves.join("SUB").is_dir();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            new_txt.contains("created"),
            "NEW.TXT not created: {new_txt:?}"
        );
        assert!(
            old_txt.contains("overwritten") && !old_txt.contains("before"),
            "OLD.TXT not overwritten: {old_txt:?}"
        );
        assert!(
            grow_txt.contains("line1") && grow_txt.contains("line2"),
            "GROW.TXT not grown: {grow_txt:?}"
        );
        assert!(sub_is_dir, "SUB subdir not created on host");
        assert!(
            deep_txt.contains("deep"),
            "SUB\\DEEP.TXT not created: {deep_txt:?}"
        );
    }

    /// Katea-1 M3 gate: real FreeDOS deletes a file, renames a file in place, moves
    /// a file into a subdir, RMDIRs an emptied dir, renames a subdir, and deletes a
    /// PRE-EXISTING host file — all reflected in the host folder, read back.
    #[test]
    #[ignore = "boots a full DOS image and mutates host files via Katea (slow); run with --ignored"]
    fn katea_host_folder_delete_rename_move_rmdir() {
        let dir = std::env::temp_dir().join(format!(
            "katea_m3_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ops = dir.join("OPS");
        std::fs::create_dir_all(ops.join("SUB")).expect("scratch tree");
        std::fs::create_dir_all(ops.join("EMPTYDIR")).expect("emptydir");
        std::fs::create_dir_all(ops.join("OLDDIR")).expect("olddir");
        std::fs::write(ops.join("DELME.TXT"), b"delete me\r\n").unwrap();
        std::fs::write(ops.join("KEEP.TXT"), b"rename me\r\n").unwrap();
        std::fs::write(ops.join("MOVEME.TXT"), b"move me\r\n").unwrap();
        std::fs::write(ops.join("PREEXIST.TXT"), b"existed first\r\n").unwrap();
        // Only built-in FreeCOM commands (MOVE.EXE is not on the Katea C: drive —
        // the boot payload carries only KERNEL/COMMAND/CONFIG/AUTOEXEC). The file
        // move is COPY+DEL; the directory rename is REN, which this FreeCOM's
        // cmd_rename accepts on directories (its findfirst mask includes FA_DIREC).
        // Redirection/`\` chars are exact bytes in the batch so only letters are
        // ever typed.
        let ops_bat = b"del DELME.TXT\r\n\
ren KEEP.TXT RENAMED.TXT\r\n\
copy MOVEME.TXT SUB\\\r\n\
del MOVEME.TXT\r\n\
rmdir EMPTYDIR\r\n\
ren OLDDIR NEWDIR\r\n\
del PREEXIST.TXT\r\n";
        std::fs::write(ops.join("OPS.BAT"), ops_bat).expect("seed OPS.BAT");

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine.mount_hdd_folder(&dir).expect("mount host folder");
        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("run machine");
        if let StopReason::CpuError(msg) = &stop {
            let text = machine.screen_text().as_text();
            std::fs::remove_dir_all(&dir).ok();
            panic!("CPU fault during Katea M3 boot: {msg}\nstop={stop:?}\n{text}");
        }
        let boot_text = machine.screen_text().as_text().to_ascii_lowercase();
        assert!(
            boot_text.contains("c:\\>"),
            "no C:\\> prompt (stop={stop:?}).\n{boot_text}"
        );

        // `ops` runs six commands incl. a COPY (heavier than M2's echoes) + an inline
        // reconcile each, hence a larger settle budget than the M2 e2e's 120M.
        for (cmd, settle) in [("cd ops\r", 40_000_000u64), ("ops\r", 150_000_000u64)] {
            for ch in cmd.chars() {
                for code in ascii_to_set1(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine
                    .run_until_halt_or_cycles(5_000_000)
                    .expect("type cmd");
            }
            machine
                .run_until_halt_or_cycles(settle)
                .expect("settle cmd");
        }
        machine.flush_hdd_folder();

        let exists = |p: std::path::PathBuf| p.exists();
        let del_gone = !exists(ops.join("DELME.TXT"));
        let renamed_new = exists(ops.join("RENAMED.TXT"));
        let renamed_old_gone = !exists(ops.join("KEEP.TXT"));
        let moved_to_sub = exists(ops.join("SUB").join("MOVEME.TXT"));
        let moved_from_root_gone = !exists(ops.join("MOVEME.TXT"));
        let rmdir_gone = !exists(ops.join("EMPTYDIR"));
        let dir_renamed_new = exists(ops.join("NEWDIR"));
        let dir_renamed_old_gone = !exists(ops.join("OLDDIR"));
        let preexist_gone = !exists(ops.join("PREEXIST.TXT"));
        std::fs::remove_dir_all(&dir).ok();

        assert!(del_gone, "DELME.TXT not deleted");
        assert!(renamed_new, "RENAMED.TXT not present after rename");
        assert!(renamed_old_gone, "KEEP.TXT still present after rename");
        assert!(moved_to_sub, "MOVEME.TXT did not arrive in SUB");
        assert!(
            moved_from_root_gone,
            "MOVEME.TXT still in the root (move's delete didn't apply)"
        );
        assert!(rmdir_gone, "EMPTYDIR not removed");
        assert!(dir_renamed_new, "NEWDIR not present after dir rename");
        assert!(
            dir_renamed_old_gone,
            "OLDDIR still present after dir rename"
        );
        assert!(preexist_gone, "pre-existing PREEXIST.TXT not deleted");
    }

    /// The katea-run gate: a program that exits 42, run through real FreeDOS via
    /// --katea-run, makes `katea_run` return 42 — proving boot -> AUTOEXEC -> RUNNER
    /// -> EXEC -> AH=4Dh -> unit-tester exit -> TestExit, end to end.
    #[test]
    #[ignore = "boots a full FreeDOS image to run one program (slow); run with --ignored"]
    fn katea_run_captures_a_program_exit_code() {
        // Self-cleaning dir (drops at the end of the test body, after the assert),
        // so a panic mid-test can't leak it — same guard the production katea_run uses.
        let dir = TempDir::new(std::env::temp_dir().join(format!(
            "katea_run_e2e_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
        .unwrap();
        let prog = dir.path().join("EXIT42.COM");
        std::fs::write(&prog, izarravm_firmware::exit42_com()).unwrap();

        let code =
            katea_run(&prog, MachineProfile::gsw_386(16, VideoCard::Et4000Ax)).expect("katea_run");
        assert_eq!(
            code, 42,
            "the program's DOS exit code must reach the host process"
        );
    }

    /// SP-4b M0 Task 1: prove the bespoke `.SYS` device-driver seam end to end.
    /// A minimal TOKAEMM.SYS is overlaid onto C: and loaded via `DEVICE=` in a
    /// CONFIG.SYS override. Its INIT runs at SYSINIT (real mode, no V86), prints a
    /// marker through INT 29h, and reports "no resident code" (r_endaddr = header
    /// seg:0) so DOS unloads it again. The gate: the marker reaches the screen AND
    /// FreeDOS still boots to C:\>.
    ///
    /// CONFIG.SYS and TOKAEMM.SYS are both passed as `mount_hdd_folder_with`
    /// overrides (which replace/append onto the committed system files). The host
    /// `dir` stays empty: a CONFIG.SYS written there would collide with the
    /// system CONFIG.SYS whose 8.3 name is reserved first, and lose the `~n` fold.
    #[test]
    #[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
    fn tokaemm_init_enters_v86() {
        let dir = std::env::temp_dir().join(format!(
            "tokaemm_t3a_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");

        // The stock CONFIG.SYS (from the committed image) plus a DEVICE= line for
        // the bespoke driver. Passed as an override so it replaces the system copy.
        let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\TOKAEMM.SYS\r\n\
SHELL=C:\\COMMAND.COM C:\\ /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
            .to_vec();

        let mut machine = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
            izarravm_firmware::izarra_bios(),
        )
        .expect("build machine");
        machine
            .mount_hdd_folder_with(
                &dir,
                vec![
                    ("CONFIG.SYS".to_string(), config),
                    (
                        "TOKAEMM.SYS".to_string(),
                        izarravm_firmware::tokaemm_sys().to_vec(),
                    ),
                ],
            )
            .expect("mount host folder with overrides");

        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("run machine");
        let text = machine.screen_text().as_text();
        std::fs::remove_dir_all(&dir).ok();
        // Task 3 increment A: TOKAEMM.SYS INIT builds its PM/paging env and enters
        // V86, where the stub signals 0xA5 through the unit-tester exit port. (It
        // does not yet return to DOS, so the boot stops here rather than reaching
        // C:\> — that is Task 3 increment B.)
        assert_eq!(
            stop,
            StopReason::TestExit { code: 0xA5 },
            "TOKAEMM INIT did not enter V86 and signal 0xA5 (stop={stop:?}).\n{text}"
        );
    }
}
