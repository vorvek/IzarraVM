mod gui;

use clap::Parser;
use izarravm_audio::AudioSubsystem;
use izarravm_core::{
    AppConfig, ConfigOverrides, CpuPreset, HardwareProfile, MidiBackend, VideoCard,
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
    cpu: Option<CpuPreset>,
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
    headless_config_check: bool,
    #[arg(long)]
    headless_test_rom: bool,
    #[arg(long)]
    headless_boot_suite: bool,
    #[arg(long)]
    headless_run_com: Option<PathBuf>,
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
    config.validate()?;
    let hardware = HardwareProfile::from_config(&config.machine)?;
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

    if cli.headless_boot_suite {
        let mut machine = Machine::new_boot_image(
            MachineProfile::from_hardware_profile(&hardware),
            boot_test_image(),
        )?;
        let stop_reason = machine.run_until_halt_or_cycles(5_000_000)?;
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
        return Ok(());
    }

    if let Some(path) = &cli.headless_run_com {
        // Compute the exit code in an inner scope so the machine and the loaded
        // image drop before process::exit (which does not unwind). The DOS exit
        // code becomes the process status so a .COM result is scriptable.
        let code = {
            let image = std::fs::read(path)?;
            let mut machine =
                Machine::new_dos_com(MachineProfile::from_hardware_profile(&hardware), &image)?;
            // Mount the configured C: drive so INT 21h file calls resolve.
            machine.mount_c_drive(dos.c_drive.clone());
            if let Some(text) = &cli.stdin_text {
                machine.set_dos_stdin(text.as_bytes());
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

    if cli.headless_test_rom {
        let rom = select_rom(cli.bios.as_deref())?;
        let mut machine = Machine::new(MachineProfile::from_hardware_profile(&hardware), &rom)?;
        let budget = cli.cycles.unwrap_or(DEFAULT_TEST_ROM_CYCLES);
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
        println!("post: {:#04x}", machine.io_port(0x80).unwrap_or(0));
        println!("stop: {stop_reason:?}");
        return Ok(());
    }

    let rom = select_rom(cli.bios.as_deref())?;
    let audio_enabled = config.audio.opl3 || config.audio.sound_blaster;
    gui::run(
        MachineProfile::from_hardware_profile(&hardware),
        rom,
        config.dos.c_drive.clone(),
        audio_enabled,
        cli.margo_test_pattern,
    )?;
    Ok(())
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
    });

    Ok(config)
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
    fn select_rom_without_bios_returns_the_builtin_test_rom() {
        assert_eq!(select_rom(None).unwrap(), test_rom().to_vec());
    }
}
