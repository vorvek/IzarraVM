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
use izarravm_dos::{DosKernelServices, HostDrive};
use izarravm_firmware::{
    SuiteRecordStatus, boot_test_image, neurketa_image, parse_result_block, test_rom,
};
use izarravm_input::InputState;
use izarravm_machine::{Machine, MachineProfile, StopReason};
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
    #[arg(long)]
    headless_keyboard: bool,
    #[arg(long)]
    headless_izarra_bios: bool,
    #[arg(long)]
    headless_toka: bool,
    #[arg(long)]
    headless_boot_floppy: Option<PathBuf>,
    #[arg(long)]
    headless_run: Option<PathBuf>,
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
    // config left at its "." default), auto-mount from the per-user
    // ~/.izarravm/c_drive (or, with --portable, a c_drive beside the executable)
    // and lay down Toka-DOS if it is missing.
    if cli.c_drive.is_none() && cli.dosroot.is_none() && config.dos.c_drive == Path::new(".") {
        let c_root = izarravm_dos::resolve_c_root(cli.portable);
        let files = izarravm_firmware::toka_dos_system_files();
        izarravm_dos::toka_dos_install(
            &c_root,
            &files,
            izarravm_dos::InstallMode::EnsureIfMissing,
        )?;
        config.dos.c_drive = c_root;
    }
    let hardware = HardwareProfile::from_config(&config)?;
    let dos = DosKernelServices::new(HostDrive::mount_c(&config.dos.c_drive)?);
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
        c_drive = %dos.c_drive.root().display(),
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

    if let Some(path) = &cli.headless_run {
        return run_headless_program(path, &hardware, &dos, cli.stdin_text.as_deref());
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

    if cli.headless_toka {
        return run_headless_toka(&hardware, &dos, cli.stdin_text.as_deref());
    }

    if let Some(path) = &cli.headless_boot_floppy {
        return run_boot_floppy(path, cli.cycles, &hardware);
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

/// One Neurketa run: boot the image in `mode`, preload `selector`, run to the
/// guest's CMD_EXIT, and read back the charged guest clocks, the reported
/// result primitives, and the host wall time.
struct BenchRun {
    clocks: u64,
    iterations: u32,
    aux: u32,
    wall: std::time::Duration,
}

fn run_bench_one(
    hardware: &HardwareProfile,
    mode: GswMode,
    selector: u8,
    budget: u64,
) -> Result<BenchRun, Box<dyn Error>> {
    let mut machine = Machine::new_boot_image(
        MachineProfile::from_hardware_profile(hardware),
        neurketa_image(),
    )?;
    machine.set_mode(mode);
    machine.set_bench_selector(selector);
    let started = std::time::Instant::now();
    let stop = machine.run_until_halt_or_cycles(budget)?;
    let wall = started.elapsed();
    if !matches!(stop, StopReason::TestExit { .. }) {
        return Err(format!(
            "neurketa {} selector {selector} did not exit cleanly: {stop:?}",
            mode.canonical_name()
        )
        .into());
    }
    Ok(BenchRun {
        clocks: machine.elapsed_clocks(),
        iterations: machine.bench_iterations(),
        aux: machine.bench_aux(),
        wall,
    })
}

/// Run the Neurketa payloads in every CPU mode and print, per mode, the guest
/// cycles per iteration and the host real-time factor. Phase 0 runs the Sieve
/// (selector 1) against the empty baseline (selector 0); later phases add the C
/// payloads and the era-reference comparison.
fn run_bench(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    // The run stops at the guest's CMD_EXIT, so this is only a safety cap.
    const BENCH_BUDGET: u64 = 50_000_000_000;
    const SEL_BASELINE: u8 = 0;
    const SEL_SIEVE: u8 = 1;

    let modes = [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    println!("mode    cyc/iter      iters   primes   guest_ms   wall_ms   rt_factor");
    for mode in modes {
        let base = run_bench_one(hardware, mode, SEL_BASELINE, BENCH_BUDGET)?;
        let sieve = run_bench_one(hardware, mode, SEL_SIEVE, BENCH_BUDGET)?;
        let work = sieve.clocks.saturating_sub(base.clocks);
        let iters = u64::from(sieve.iterations.max(1));
        let cyc_per_iter = work as f64 / iters as f64;
        let guest_secs = work as f64 / mode.clock_hz() as f64;
        let wall_secs = sieve.wall.as_secs_f64();
        let rt = if wall_secs > 0.0 {
            guest_secs / wall_secs
        } else {
            0.0
        };
        println!(
            "{:<6} {:>10.2} {:>10} {:>8} {:>10.3} {:>9.3} {:>10.3}",
            mode.canonical_name(),
            cyc_per_iter,
            sieve.iterations,
            sieve.aux,
            guest_secs * 1000.0,
            wall_secs * 1000.0,
            rt,
        );
    }
    Ok(())
}

/// Load and run a DOS .COM/.EXE headless, then exit with its DOS exit code.
fn run_headless_program(
    path: &Path,
    hardware: &HardwareProfile,
    dos: &DosKernelServices,
    stdin_text: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    // Compute the exit code in an inner scope so the machine and the loaded
    // image drop before process::exit (which does not unwind). The DOS exit
    // code becomes the process status so a .COM result is scriptable.
    let code = {
        let image = std::fs::read(path)?;
        let mut machine =
            Machine::new_dos_program(MachineProfile::from_hardware_profile(hardware), &image)?;
        // Mount the configured C: drive so INT 21h file calls resolve.
        machine.mount_c_drive(dos.c_drive.clone());
        if let Some(text) = stdin_text {
            // Type the text through the real keyboard path so it reaches the
            // program via INT 09h and the BDA ring. Typeable ASCII only
            // (lowercase, digits, space) until ascii_to_set1 is extended.
            for ch in text.chars() {
                for code in ascii_to_set1(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
            }
        }
        let stop_reason = machine.run_until_halt_or_cycles(50_000_000)?;
        print!("{}", String::from_utf8_lossy(machine.dos_output()));
        println!("stop: {stop_reason:?}");
        match stop_reason {
            StopReason::DosExit { code } | StopReason::TestExit { code } => i32::from(code),
            _ => 1,
        }
    };
    std::process::exit(code);
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

/// Boot the Izarra 3000 BIOS into Toka-DOS, type an optional script at the
/// IZCMD prompt, and print the VGA text screen. With no floppy mounted the
/// BIOS falls through to the hard-disk boot, which brings up Toka-DOS from C:.
fn run_headless_toka(
    hardware: &HardwareProfile,
    dos: &DosKernelServices,
    stdin_text: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let mut machine = Machine::new(
        MachineProfile::from_hardware_profile(hardware),
        izarravm_firmware::izarra_bios(),
    )?;
    machine.mount_c_drive(dos.c_drive.clone());
    machine.set_toka_c_root(dos.c_drive.root().to_path_buf());

    // Boot through POST and into the IZCMD prompt. POST is fast (fast_post),
    // so roughly a second of cycles is ample to reach the prompt.
    machine.run_until_halt_or_cycles(hardware.clock_hz)?;

    // Feed the script character by character. The BDA keyboard ring holds only
    // 15 entries, so injecting a whole line at once overflows it on a long
    // command. Typing one character and running briefly lets IZCMD's line
    // input drain the ring before the next character, so any line length works.
    if let Some(text) = stdin_text {
        let per_char = 200_000u64;
        let line_budget = hardware.clock_hz / 2;
        for raw in text.split('\n') {
            let line = raw.trim_end_matches('\r');
            for ch in line.chars() {
                for code in ascii_to_set1(ch) {
                    machine.inject_key_scancodes(&[code]);
                }
                machine.run_until_halt_or_cycles(per_char)?;
            }
            for code in ascii_to_set1('\r') {
                machine.inject_key_scancodes(&[code]);
            }
            machine.run_until_halt_or_cycles(line_budget)?;
        }
    }

    println!("{}", machine.screen_text().as_text());
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
}
