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
    /// With --hdd-folder, print a machine-readable result block after stop:
    /// stop reason, CS:IP, full register state, and the 80x25 text page. For
    /// headless benchmark/timedemo runs whose result lands in text mode.
    #[arg(long)]
    dump_result: bool,
    /// With --hdd-folder, write the final framebuffer to this PPM (P6) path.
    /// For headless benchmark/timedemo runs whose result lands in graphics mode.
    #[arg(long)]
    result_ppm: Option<PathBuf>,
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
    // The cost-fold native-LOAD JIT path is a process-global toggle read at region emit time; set it
    // once here from `IZARRAVM_JIT_FOLD` so every entry path (bench/hdd-folder/katea/exe) sees it. Only
    // meaningful alongside `IZARRAVM_JIT`; a no-op unless built `--features jit`.
    #[cfg(feature = "jit")]
    izarravm_cpu::CpuGsw::set_jit_fold_timing(jit_fold_enabled());
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
        return run_boot_hdd_folder(
            dir,
            cli.cycles,
            &hardware,
            cli.dump_result,
            cli.result_ppm.as_deref(),
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
    // Calibration census tool (mirrors run_boot_hdd_folder):
    // IZARRAVM_CPU_PROFILE=<stride> samples the per-opcode CPU profile for
    // every bench run; the caller prints it when the env is set.
    let stride = std::env::var("IZARRAVM_CPU_PROFILE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    run_bench_one_profiled(hardware, mode, source, budget, stride)
}

/// Whether the JIT should auto-admit hot loops this run, read from `IZARRAVM_JIT` (any value other
/// than empty or "0" turns it on). Lets the headless game anchors be measured with the JIT active
/// without a dedicated flag. A no-op unless the binary was built `--features jit`.
fn jit_env_enabled() -> bool {
    std::env::var("IZARRAVM_JIT")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Whether the cost-fold native-LOAD JIT path should run this session, read from `IZARRAVM_JIT_FOLD`.
/// Off by default; only meaningful alongside `IZARRAVM_JIT` (it needs the JIT active). Makes JIT-block
/// timing approximate, so it is an opt-in A/B knob. A no-op unless the binary was built `--features jit`.
#[cfg(feature = "jit")]
fn jit_fold_enabled() -> bool {
    std::env::var("IZARRAVM_JIT_FOLD")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
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
    machine.set_jit_auto_admit(jit_env_enabled());
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
        min_mode: GswMode::Gsw386Slow,
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
        min_mode: GswMode::Gsw386Slow,
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

/// Permanent poll-loop microbench fixture (P4a Task 0.4). Reproduces the three
/// diagnosis patterns behind the P4a lazy-port-device-time initiative (see
/// dev_docs/2026-07-02-p4a-lazy-port-device-time-plan.md): a tight 0x3DA
/// vsync-wait poll (the named worst case, ~131 ns/access at 586 on the
/// original diagnosis), a VRAM write loop (memory-bound, no port I/O, isolates
/// the poll's cost from a plain hot loop), and a register-only loop (the
/// compute-only floor, no memory or port access at all). Each payload is a
/// small hand-assembled real-mode code array injected directly as the boot ROM
/// (mirrors `paced_wall_topup_lets_a_polling_guest_catch_vretrace_windows`'s
/// `rom_with_code` pattern), run for a fixed guest-clock budget via
/// `Machine::new` + `run_until_halt_or_cycles` -- NOT `Machine::new_raw_program`
/// (the DOS-loader path) and NOT a new `neurketa` selector (that payload is a
/// NASM-built, pre-baked binary blob; adding a selector there would need an
/// assembly-toolchain rebuild step this fixture should not depend on). None of
/// the three payloads ever halts or exits, so there is no iteration count to
/// self-report the way the unit-tester-backed `BENCHES` table's payloads do;
/// this fixture reports guest_ms/wall_ms/rt_factor only, the same shape the
/// original diagnosis numbers used (58M batches/guest-s, ~131 ns/access).
struct Microbench {
    name: &'static str,
    code: &'static [u8],
}

// Small enough that the slowest pattern (poll-3da, which pays a full batch
// epilogue per iteration pre-Slice-1) still finishes in a couple of seconds at
// 486/586, large enough for a stable rt_factor (thousands of guest batches
// even in the worst case).
const MICROBENCH_BUDGET: u64 = 20_000_000;

const MICROBENCHES: &[Microbench] = &[
    Microbench {
        name: "poll-3da",
        // mov dx, 0x3DA
        // wait: in al, dx
        //       test al, 0x08
        //       jz wait          (spin while the vretrace bit is clear)
        //       jmp wait         (re-poll once seen set, an unconditional spin)
        // The worst case named in the plan: 58M batches/guest-s at the original
        // diagnosis baseline.
        code: &[
            0xBA, 0xDA, 0x03, // mov dx, 0x03DA
            0xEC, // wait: in al, dx
            0xA8, 0x08, // test al, 0x08
            0x74, 0xFB, // jz wait
            0xEB, 0xF9, // jmp wait
        ],
    },
    Microbench {
        name: "poll-61",
        // wait: in al, 0x61
        //       test al, 0x10
        //       jz wait          (spin while the DRAM-refresh heartbeat bit is clear)
        //       jmp wait         (re-poll once seen set, an unconditional spin)
        // Mirrors poll-3da's shape but against port 0x61 bits 4/5 (PIT channel
        // 1/2 OUT), the P4a Task 2.3 lazy-read target: a real boot-time DRAM-
        // refresh detection loop polls exactly this bit.
        code: &[
            0xE4, 0x61, // wait: in al, 0x61
            0xA8, 0x10, // test al, 0x10
            0x74, 0xFA, // jz wait (rel8 -6: wait=0, jz ends at 6)
            0xEB, 0xF8, // jmp wait (rel8 -8: wait=0, jmp ends at 8)
        ],
    },
    Microbench {
        name: "adlib-detect",
        // The canonical AdLib detection idiom (Ralf Brown's probe, mirrored
        // from OplChip's own `adlib_detection_sequence_reports_present` unit
        // test): reset both timers, arm timer 1 to overflow in one 80us step,
        // start it, then poll the status port for the timer-1 flag. Before
        // P4a Slice 3 every one of the loop's status reads ended a CPU batch;
        // after Slice 3 only the one setup write per OUT (address port,
        // 0x388) does, so this is the lazy-status-read counterpart of
        // poll-3da/poll-61 -- 1 address write + up to 6 status reads/session
        // per the plan's idiom shape, though this fixture polls forever (no
        // halt) so the same handful of address-port writes repeats only in
        // the one-time setup, and the hot loop is pure status reads.
        //
        // Byte layout (hand-checked, see the poll-61 lesson: a IN AL,imm8's
        // 2-byte encoding vs IN AL,DX's 1-byte moved the branch target and
        // silently turned that fixture into a port-free loop for one commit):
        //   0  BA 88 03        mov dx, 0x0388
        //   3  B0 04           mov al, 0x04
        //   5  EE              out dx, al         ; latch reg 4
        //   6  42              inc dx             ; dx = 0x0389
        //   7  B0 60           mov al, 0x60
        //   9  EE              out dx, al         ; mask both timers
        //  10  4A              dec dx             ; dx = 0x0388
        //  11  B0 04           mov al, 0x04
        //  13  EE              out dx, al         ; latch reg 4
        //  14  42              inc dx             ; dx = 0x0389
        //  15  B0 80           mov al, 0x80
        //  17  EE              out dx, al         ; reset IRQ flags
        //  18  4A              dec dx             ; dx = 0x0388
        //  19  B0 02           mov al, 0x02
        //  21  EE              out dx, al         ; latch reg 2 (timer1 preset)
        //  22  42              inc dx             ; dx = 0x0389
        //  23  B0 FF           mov al, 0xff
        //  25  EE              out dx, al         ; preset 0xff: overflow in 1 step
        //  26  4A              dec dx             ; dx = 0x0388
        //  27  B0 04           mov al, 0x04
        //  29  EE              out dx, al         ; latch reg 4
        //  30  42              inc dx             ; dx = 0x0389
        //  31  B0 21           mov al, 0x21
        //  33  EE              out dx, al         ; start timer1, mask timer2
        //  34  4A              dec dx             ; dx = 0x0388 (status port)
        //  35  EC        wait: in al, dx
        //  36  A8 40           test al, 0x40      ; timer-1 flag
        //  38  74 FB           jz wait            ; rel8 = 35 - 40 = -5 (0xFB)
        //  40  EB F9           jmp wait           ; rel8 = 35 - 42 = -7 (0xF9)
        code: &[
            0xBA, 0x88, 0x03, // mov dx, 0x0388
            0xB0, 0x04, 0xEE, // mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x60, 0xEE, // inc dx ; mov al, 0x60 ; out dx, al
            0x4A, 0xB0, 0x04, 0xEE, // dec dx ; mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x80, 0xEE, // inc dx ; mov al, 0x80 ; out dx, al
            0x4A, 0xB0, 0x02, 0xEE, // dec dx ; mov al, 0x02 ; out dx, al
            0x42, 0xB0, 0xFF, 0xEE, // inc dx ; mov al, 0xff ; out dx, al
            0x4A, 0xB0, 0x04, 0xEE, // dec dx ; mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x21, 0xEE, // inc dx ; mov al, 0x21 ; out dx, al
            0x4A, // dec dx
            0xEC, // wait: in al, dx
            0xA8, 0x40, // test al, 0x40
            0x74, 0xFB, // jz wait
            0xEB, 0xF9, // jmp wait
        ],
    },
    Microbench {
        name: "vram-write",
        // mov ax, 0x0013 ; int 0x10          (mode 13h: 0xA0000 is the LFB)
        // mov ax, 0xA000 ; mov es, ax
        // xor di, di
        // write: mov [es:di], al ; inc di ; jmp write
        // No port I/O at all: an isolated memory-bound hot loop, the
        // counterpart the plan's decision 4 step 7 checks stays UNCHANGED by
        // any later port-laziness slice (it never touches a lazy port).
        code: &[
            0xB8, 0x13, 0x00, // mov ax, 0x0013
            0xCD, 0x10, // int 0x10
            0xB8, 0x00, 0xA0, // mov ax, 0xA000
            0x8E, 0xC0, // mov es, ax
            0x31, 0xFF, // xor di, di
            0x26, 0x88, 0x05, // write: mov [es:di], al
            0x47, // inc di
            0xEB, 0xFA, // jmp write
        ],
    },
    Microbench {
        name: "register-only",
        // xor ax, ax ; loop: inc ax ; jmp loop. Pure ALU + branch, the
        // compute-only floor: no memory or port access whatsoever.
        code: &[
            0x31, 0xC0, // xor ax, ax
            0x40, // loop: inc ax
            0xEB, 0xFD, // jmp loop
        ],
    },
];

/// Wrap `code` as a bare boot ROM: the payload at offset 0 (physical 0xF0000,
/// where CS:IP F000:0000 lands after reset), an IRET stub at 0xF000 (some
/// BIOS-intercepted service vectors return through it), and a far jump at the
/// reset vector (0xFFF0) into the payload. Mirrors
/// `paced_wall_topup_lets_a_polling_guest_catch_vretrace_windows`'s
/// `rom_with_code` test helper (izarravm-machine/src/lib.rs).
fn microbench_rom(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    rom[0xF000] = 0xCF; // IRET
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]); // jmp F000:0000
    rom
}

/// Run the poll-loop microbench fixture (Task 0.4): all three patterns, in
/// every GSW mode, for a fixed clock budget each (none of the payloads halts
/// or exits, so there is nothing to run "to completion"). Prints rt_factor per
/// mode per pattern in the same columns `run_bench` uses for its guest_ms/
/// wall_ms/rt_factor fields. Every row carries an ` [info]` marker (the
/// ` [approx]` suffix precedent): these rows never gate the process exit,
/// even when read without the section header in view. Banding runs through
/// `band_tag`, which no-ops for a payload name with no reference band, so no
/// microbench row can ever fail the run -- same policy as the
/// Approximate-class rows in the main table, per Phase 3 of
/// dev_docs/2026-07-01-cpu-timing-classes-plan.md. A row whose run ended on
/// anything other than the clock budget is tagged ` [early-stop: <reason>]`,
/// since its rt_factor measured a truncated run.
fn run_microbench(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    println!();
    println!("=== poll-loop microbench (informational; P4a Task 0.4) ===");
    println!(
        "{:<14} {:<5} {:>10} {:>9} {:>10}",
        "pattern", "mode", "guest_ms", "wall_ms", "rt_factor"
    );
    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];
    for bench in MICROBENCHES {
        for mode in modes {
            let profile = MachineProfile::from_hardware_profile(hardware);
            let mut machine = Machine::new(profile, microbench_rom(bench.code))?;
            machine.set_mode(mode);
            let started = std::time::Instant::now();
            let stop = machine.run_until_halt_or_cycles(MICROBENCH_BUDGET)?;
            let wall = started.elapsed();
            let guest_secs = machine.elapsed_clocks() as f64 / mode.clock_hz() as f64;
            let wall_secs = wall.as_secs_f64();
            let rt = if wall_secs > 0.0 {
                guest_secs / wall_secs
            } else {
                0.0
            };
            // The payloads never halt or exit by design, so the only clean stop
            // is the clock budget running out. Anything else (an early HLT, a
            // fault, a stray DOS/test exit) means the row measured a truncated
            // run and its rt_factor is skewed: mark it visibly so a permanent
            // fixture can never silently report corrupted numbers.
            let early_stop = match stop {
                StopReason::CycleLimit { .. } => String::new(),
                other => format!(" [early-stop: {other:?}]"),
            };
            println!(
                "{:<14} {:<5} {:>10.3} {:>9.3} {:>10.3} [info]{}{}",
                bench.name,
                mode.canonical_name(),
                guest_secs * 1000.0,
                wall_secs * 1000.0,
                rt,
                band_tag(bench.name, mode, rt),
                early_stop,
            );
        }
    }
    Ok(())
}

/// Rank the modes from slowest to fastest so a benchmark's min_mode gates which
/// modes it runs in.
fn mode_rank(mode: GswMode) -> u8 {
    match mode {
        GswMode::Gsw386Slow => 0,
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
        GswMode::Gsw386Slow,
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
    // Only an Accurate-class (286/386) out-of-band row fails the process; the
    // Approximate fast modes (486/586) are informational (see TimingClass and
    // bench_reference.rs), so their verdicts print but never flip this flag.
    let mut accurate_out_of_band = false;
    for bench in BENCHES {
        for mode in modes {
            if mode_rank(mode) < mode_rank(bench.min_mode) {
                continue;
            }
            let run = run_bench_one(hardware, mode, &bench.source, BENCH_BUDGET)?;
            if std::env::var("IZARRAVM_CPU_PROFILE")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .is_some()
            {
                println!("--- census {} {} ---", bench.name, mode.canonical_name());
                print_cpu_profile(&run.cpu_profile);
            }
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
            // Soft reporter: tag each row against the era reference band. Accurate
            // modes (286/386) gate the process exit on an out-of-band verdict; the
            // Approximate fast modes (486/586) are informational only (their bands
            // were widened for this in bench_reference.rs), so they always print
            // their tag but never fail the run. See TimingClass.
            if mode.timing_class() == izarravm_core::TimingClass::Accurate
                && bench_reference::band_for(bench.name, mode).is_some_and(|band| {
                    band.verdict(band_value) != bench_reference::BandVerdict::InBand
                })
            {
                accurate_out_of_band = true;
            }
            println!(
                "{}{}",
                mflops_suffix,
                band_tag(bench.name, mode, band_value)
            );
        }
    }
    run_microbench(hardware)?;
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
             map_inv={}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}  \
         jit[entries/insns/nativeld]={}/{}/{}  paged_tlb_success={}",
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
            perf.jit_region_entries,
            perf.jit_region_insns,
            perf.jit_native_load_hits,
            perf.jit_paged_tlb_successes,
        );
        // TEMPORARY: split the brk_decode_or_branch attribution for the decode-cache
        // miss investigation.
        println!(
            "  brk_attrib[decode_miss/not_continuable/page_cross]={}/{}/{}",
            perf.brk_cont_decode_miss, perf.brk_cont_not_continuable, perf.brk_cont_page_cross,
        );
    }
    if accurate_out_of_band {
        return Err(
            "an Accurate-class (286/386) bench row is out of its era reference band"
                .to_string()
                .into(),
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

    if !snapshot.hot_addrs.is_empty() {
        let total_samples: u64 = snapshot
            .hot_addrs
            .iter()
            .map(|&(_, s)| s)
            .sum::<u64>()
            .max(1);
        println!();
        println!(
            "=== hot sampled addresses (top {}, sample_stride={}) ===",
            snapshot.hot_addrs.len(),
            snapshot.sample_stride
        );
        println!("{:<10} {:>9} {:>8}", "linear", "samples", "top64%");
        for &(lin, samples) in &snapshot.hot_addrs {
            println!(
                "{lin:08X}   {samples:>9} {:>7.2}%",
                100.0 * samples as f64 / total_samples as f64
            );
        }
    }

    if !snapshot.smc_flush_blocks.is_empty() {
        println!();
        println!(
            "=== smc flush sources (top {}, 64-byte physical blocks) ===",
            snapshot.smc_flush_blocks.len()
        );
        println!("{:<10} {:>9}", "physical", "flushes");
        for &(block, flushes) in &snapshot.smc_flush_blocks {
            println!("{block:08X}   {flushes:>9}");
        }
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
        "smc_narrow_kills": perf.smc_narrow_kills,
        "jit_region_entries": perf.jit_region_entries,
        "jit_region_insns": perf.jit_region_insns,
    })
}

fn print_perf_counter_row(name: &str, mode: GswMode, perf: &PerfCounters) {
    let instructions = perf.instructions.max(1);
    let decode_hit = 100.0 * (1.0 - perf.decode_misses as f64 / instructions as f64);
    let insns_per_run = perf.instructions as f64 / perf.straight_line_runs.max(1) as f64;
    println!(
        "perf  {:<10} {:<5} instr={:>13}  decode_hit={:>6.2}%  insns/run={:>9.1}  \
         brk[branch/step/int/cap/halt]={}/{}/{}/{}/{}  \
         inval[cs/smc/other]={}/{}/{} narrow={}  \
         data[rd d/s wr d/s]={}/{}/{}/{}  ptr[rd/wr]={}/{}  \
         page[h/m]={}/{}  fetch_page[h/m slow_refill]={}/{}/{}  \
         map_inv={}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}  \
         jit[entries/insns/nativeld]={}/{}/{}  paged_tlb={}",
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
        perf.decode_inval_cs_load,
        perf.decode_inval_smc,
        perf.decode_inval_other,
        perf.smc_narrow_kills,
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
        perf.jit_region_entries,
        perf.jit_region_insns,
        perf.jit_native_load_hits,
        perf.jit_paged_tlb_successes,
    );
    // TEMPORARY: split the brk_decode_or_branch attribution for the decode-cache
    // miss investigation.
    println!(
        "  brk_attrib[decode_miss/not_continuable/page_cross]={}/{}/{}",
        perf.brk_cont_decode_miss, perf.brk_cont_not_continuable, perf.brk_cont_page_cross,
    );
}

/// Compare a measured `iters/sec` to the matching era reference band and return
/// a tag to append to the row: ` [in band]`, ` [LOW <ratio>]`, ` [HIGH <ratio>]`,
/// or empty when no band is encoded for this payload/mode. Approximate-class
/// modes (486/586; see TimingClass) carry an extra trailing ` [approx]` marker,
/// since their band is informational rather than a gate.
fn band_tag(payload: &str, mode: GswMode, iters_per_sec: f64) -> String {
    use bench_reference::BandVerdict;
    let Some(band) = bench_reference::band_for(payload, mode) else {
        return String::new();
    };
    let verdict = match band.verdict(iters_per_sec) {
        BandVerdict::InBand => " [in band]".to_string(),
        BandVerdict::Low => format!(" [LOW {:.2}]", iters_per_sec / band.target),
        BandVerdict::High => format!(" [HIGH {:.2}]", iters_per_sec / band.target),
    };
    if mode.timing_class() == izarravm_core::TimingClass::Approximate {
        format!("{verdict} [approx]")
    } else {
        verdict
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
        GswMode::Gsw386Slow,
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
    machine.set_jit_auto_admit(jit_env_enabled());
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
    machine.set_jit_auto_admit(jit_env_enabled());
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
    dump_result: bool,
    result_ppm: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_hdd_folder(dir)?;
    machine.set_jit_auto_admit(jit_env_enabled());
    // Calibration census tool: IZARRAVM_CPU_PROFILE=<stride> turns on the same
    // sampled per-opcode CPU profile the bench harness uses, dumped after the
    // run. Reads the guest-clock attribution of e.g. the x87 opcode rows
    // (D8-DF) for a timedemo without touching guest-visible state.
    let cpu_profile_stride = std::env::var("IZARRAVM_CPU_PROFILE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(stride) = cpu_profile_stride {
        machine.enable_host_profiling(stride);
    }
    let budget = cycles.unwrap_or(DEFAULT_BOOT_HDD_CYCLES);
    let start_wall = std::time::Instant::now();
    let stop_reason = machine.run_until_halt_or_cycles(budget)?;
    if cpu_profile_stride.is_some() {
        let snapshot = machine.cpu().profile_snapshot();
        print_cpu_profile(&snapshot);
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
    // Run-shape diagnostics (insns/run + break reasons). Unconditional: the counters are
    // always maintained, so unlike the sampled profile above this print costs nothing.
    print_perf_counter_row("hdd-folder", hardware.cpu, machine.cpu().perf_counters());
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
    let wall = start_wall.elapsed();
    println!("wall: {:.3}s", wall.as_secs_f64());
    let screen = machine.screen_text().as_text();
    if let Some((gametics, realtics)) = extract_timedemo_realtics(&screen) {
        println!("timed {} gametics in {} realtics", gametics, realtics);
    }

    Ok(())
}

/// Parse Doom-style timedemo output from the guest text screen.
/// Looks for lines like "timed 2134 gametics in 907 realtics".
fn extract_timedemo_realtics(text: &str) -> Option<(u32, u32)> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(after_timed) = line.strip_prefix("timed ") {
            if let Some((g_str, rest)) = after_timed.split_once(" gametics in ") {
                if let Some(r_str) = rest.strip_suffix(" realtics") {
                    if let (Ok(g), Ok(r)) = (g_str.parse::<u32>(), r_str.parse::<u32>()) {
                        return Some((g, r));
                    }
                }
            }
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
/// lands in. Resolves DAC indices through the 6-bit VGA palette to 8-bit RGB.
fn write_framebuffer_ppm(machine: &mut Machine, path: &Path) -> Result<(), Box<dyn Error>> {
    use std::io::Write;

    let raster = machine.video_mut().render_full_frame();
    let height = raster.display_height.min(raster.height);
    let width = raster.width;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{width} {height}\n255\n")?;
    for row in 0..height as usize {
        let start = row * width as usize;
        let end = start + width as usize;
        for &index in &raster.pixels[start..end] {
            let [r, g, b] = machine.video().dac_entry(index);
            // 6-bit VGA DAC component (0..=63) to 8-bit (0..=255).
            out.write_all(&[r << 2 | r >> 4, g << 2 | g >> 4, b << 2 | b >> 4])?;
        }
    }
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
#[path = "main_test.rs"]
mod tests;

#[cfg(test)]
#[path = "tokados_test.rs"]
mod tokados_smoke;
