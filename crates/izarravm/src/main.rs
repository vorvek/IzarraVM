mod gui;

use clap::Parser;
use izarravm_audio::AudioSubsystem;
use izarravm_core::{
    AppConfig, ConfigOverrides, GswMode, HardwareProfile, MidiBackend, SbDma8, SbDma16, SbIrq,
    VideoCard,
};
use izarravm_dos::{DosKernelServices, HostDrive};
use izarravm_firmware::{SuiteRecordStatus, boot_test_image, parse_result_block, test_rom};
use izarravm_input::InputState;
use izarravm_machine::{Machine, MachineProfile, StopReason};
use std::error::Error;
use std::path::{Path, PathBuf};
use tracing::info;

/// Default cycle budget for --headless-test-rom. Large enough that test386.bin
/// reaches its POST-0x03 fault out of the box; halting ROMs return at their HLT
/// well before this, and --cycles tunes it down for quick runs.
const DEFAULT_TEST_ROM_CYCLES: u64 = 200_000_000;

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
    headless_keyboard: bool,
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
    let config = load_config(&cli)?;
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

    if let Some(path) = &cli.headless_run {
        return run_headless_program(path, &hardware, &dos, cli.stdin_text.as_deref());
    }

    if cli.headless_test_rom {
        return run_test_rom(cli.bios.as_deref(), cli.cycles, &hardware);
    }

    if cli.headless_keyboard {
        return run_keyboard_demo(&hardware, cli.stdin_text.as_deref());
    }

    let rom = match cli.bios.as_deref() {
        Some(path) => std::fs::read(path)?,
        None => izarravm_firmware::kbd_bios().to_vec(),
    };
    // The PC speaker is always-present motherboard hardware, so the host audio
    // output is opened regardless of which sound cards are enabled. AudioPlayer
    // falls back to silent if the host has no usable device.
    let audio_enabled = true;
    gui::run(
        MachineProfile::from_hardware_profile(&hardware),
        rom,
        config.dos.c_drive.clone(),
        audio_enabled,
        cli.margo_test_pattern,
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
            StopReason::DosExit { code } => i32::from(code),
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

/// Minimal ASCII to Set 1 make+break for the demo (lowercase letters, digits,
/// space). Extend if the demo needs more than typing words.
fn ascii_to_set1(ch: char) -> Vec<u8> {
    let make = match ch {
        'a' => 0x1e,
        'b' => 0x30,
        'c' => 0x2e,
        'd' => 0x20,
        'e' => 0x12,
        'f' => 0x21,
        'g' => 0x22,
        'h' => 0x23,
        'i' => 0x17,
        'j' => 0x24,
        'k' => 0x25,
        'l' => 0x26,
        'm' => 0x32,
        'n' => 0x31,
        'o' => 0x18,
        'p' => 0x19,
        'q' => 0x10,
        'r' => 0x13,
        's' => 0x1f,
        't' => 0x14,
        'u' => 0x16,
        'v' => 0x2f,
        'w' => 0x11,
        'x' => 0x2d,
        'y' => 0x15,
        'z' => 0x2c,
        ' ' => 0x39,
        '1' => 0x02,
        '2' => 0x03,
        '3' => 0x04,
        '4' => 0x05,
        '5' => 0x06,
        '6' => 0x07,
        '7' => 0x08,
        '8' => 0x09,
        '9' => 0x0a,
        '0' => 0x0b,
        _ => return Vec::new(),
    };
    vec![make, make | 0x80]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_to_set1_maps_a_letter_to_make_and_break() {
        assert_eq!(ascii_to_set1('h'), vec![0x23, 0xa3]);
        assert!(ascii_to_set1('!').is_empty());
    }
}
