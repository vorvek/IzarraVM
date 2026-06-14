use clap::Parser;
use std::error::Error;
use std::path::PathBuf;
use tracing::info;
use virtualdos_audio::AudioSubsystem;
use virtualdos_core::{
    AppConfig, ConfigOverrides, CpuPreset, HardwareProfile, MidiBackend, VideoCard,
};
use virtualdos_dos::{DosKernelServices, HostDrive};
use virtualdos_firmware::{boot_test_image, parse_result_block, test_rom};
use virtualdos_input::InputState;
use virtualdos_machine::{Machine, MachineProfile};
use virtualdos_video::{PlaceholderVideoAdapter, VideoAdapter};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

#[derive(Debug, Parser)]
#[command(version, about = "VirtualDOS emulator scaffold")]
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
    #[arg(long, env = "VIRTUALDOS_DOSROOT")]
    dosroot: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "virtualdos=info".into()),
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
    let video = PlaceholderVideoAdapter::new(config.machine.video);

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
        let results = parse_result_block(machine.memory().as_slice())?;
        println!("{}", machine.serial_text());
        println!("records: {}", results.records.len());
        println!("stop: {stop_reason:?}");
        return Ok(());
    }

    let mut machine = Machine::new(MachineProfile::from_hardware_profile(&hardware), test_rom())?;
    let stop_reason = machine.run_until_halt_or_cycles(5_000_000)?;
    let screen = machine.screen_text();
    let screen_text = screen.as_text();

    info!(
        ?stop_reason,
        clocks = machine.elapsed_clocks(),
        bus_cycles = machine.bus_trace().cycles().len(),
        first_line = %screen.line_string(0),
        "test ROM completed"
    );

    if cli.headless_test_rom {
        println!("{screen_text}");
        println!("stop: {stop_reason:?}");
        return Ok(());
    }

    run_window(config, video, screen_text)?;
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

fn run_window(
    config: AppConfig,
    video: PlaceholderVideoAdapter,
    screen_text: String,
) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = WindowApp {
        window: None,
        title: format!(
            "VirtualDOS - {} / {} MiB / {}",
            config.machine.cpu,
            config.machine.memory_mib,
            video.card()
        ),
        screen_text,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct WindowApp {
    window: Option<Window>,
    title: String,
    screen_text: String,
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = WindowAttributes::default()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = event_loop
            .create_window(attributes)
            .expect("native window should be creatable");
        info!(screen = %self.screen_text, "initial text-mode screen");
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {}
            WindowEvent::Resized(_size) => {}
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
