use clap::Parser;
use font8x8::{BASIC_FONTS, UnicodeFonts};
use izarravm_audio::{AudioPlayer, AudioSubsystem};
use izarravm_core::{
    AppConfig, ConfigOverrides, CpuPreset, HardwareProfile, MidiBackend, VideoCard,
};
use izarravm_dos::{DosKernelServices, HostDrive};
use izarravm_firmware::{SuiteRecordStatus, boot_test_image, parse_result_block, test_rom};
use izarravm_input::InputState;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason};
use izarravm_video::{Framebuffer, MargoDisplay, PlaceholderVideoAdapter, TextFrame, VideoAdapter};
use std::error::Error;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tracing::info;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::window::{Window, WindowAttributes, WindowId};

const GLYPH_SIZE: usize = 8;
const TEXT_SCALE: usize = 2;
const OPL_NATIVE_HZ: f64 = 49_716.0;
const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 400;
const MODE13H_SCALE: usize = 2;
const MARGO_LFB_SCALE: usize = 1;
const VGA_PALETTE: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa, 0x555555,
    0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

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

    if cli.headless_test_rom {
        let mut machine =
            Machine::new(MachineProfile::from_hardware_profile(&hardware), test_rom())?;
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
        println!("{screen_text}");
        println!("stop: {stop_reason:?}");
        return Ok(());
    }

    let mut machine = Machine::new(MachineProfile::from_hardware_profile(&hardware), test_rom())?;
    if cli.margo_test_pattern {
        load_margo_test_pattern(&mut machine);
    }
    run_window(config, video, machine)?;
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
    machine: Machine,
) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let context = softbuffer::Context::new(event_loop.owned_display_handle())?;
    let rendered_screen = render_text_frame(&machine.screen_text());

    // Open the host audio device when an OPL-backed card is enabled. Failure is
    // non-fatal: the emulator keeps running silently.
    let audio = if config.audio.opl3 || config.audio.sound_blaster {
        match AudioPlayer::new() {
            Ok(player) => Some(player),
            Err(error) => {
                info!(%error, "audio output unavailable; running silently");
                None
            }
        }
    } else {
        None
    };

    let mut app = WindowApp {
        context,
        window: None,
        surface: None,
        title: format!(
            "IzarraVM - {} / {} MiB / {}",
            config.machine.cpu,
            config.machine.memory_mib,
            video.card()
        ),
        clock_hz: machine.profile().clock_hz,
        machine,
        rendered_screen,
        stop_reason: None,
        audio,
        audio_clocks: 0,
        audio_sample_debt: 0.0,
        epoch: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct WindowApp {
    context: softbuffer::Context<OwnedDisplayHandle>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>>,
    title: String,
    clock_hz: u64,
    machine: Machine,
    rendered_screen: RenderedFrame,
    stop_reason: Option<StopReason>,
    audio: Option<AudioPlayer>,
    audio_clocks: u64,             // elapsed clocks already turned into audio
    audio_sample_debt: f64,        // fractional OPL samples owed
    epoch: Option<(Instant, u64)>, // wall-clock + machine clocks at pacing start
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = WindowAttributes::default()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .expect("native window should be creatable"),
        );
        let surface = softbuffer::Surface::new(&self.context, window.clone())
            .expect("native window surface should be creatable");
        info!("live test ROM screen started");
        window.request_redraw();
        self.surface = Some(surface);
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
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_size) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.stop_reason.is_some() {
            return;
        }

        // Pace the emulation to wall-clock time so the OPL audio plays at the
        // right rate; the catch-up is capped at 50 ms to avoid a spiral.
        let now = Instant::now();
        let (epoch, epoch_clocks) = *self
            .epoch
            .get_or_insert((now, self.machine.elapsed_clocks()));
        let executed = self.machine.elapsed_clocks().saturating_sub(epoch_clocks);
        let cap = self.clock_hz / 20;
        let budget = pacing_budget(now.duration_since(epoch), self.clock_hz, executed, cap);

        if budget > 0 {
            let reason = tick_machine(&mut self.machine, budget);
            self.pump_audio();
            self.rendered_screen = self.render_current_frame();
            if let Some(reason) = reason {
                self.finish(reason);
                event_loop.set_control_flow(ControlFlow::Wait);
                return;
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }

        // Sleep briefly instead of busy-spinning until the next ~2 ms slice.
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(2)));
    }
}

impl WindowApp {
    /// Render OPL audio for the emulated time elapsed since the last pump and
    /// queue it for playback. Paced by emulated time, so the ring absorbs jitter
    /// between the emulation rate and the audio clock.
    fn pump_audio(&mut self) {
        if self.audio.is_none() {
            return;
        }
        let now = self.machine.elapsed_clocks();
        let delta = now.saturating_sub(self.audio_clocks);
        self.audio_clocks = now;
        self.audio_sample_debt += delta as f64 * OPL_NATIVE_HZ / self.clock_hz as f64;
        let samples = self.audio_sample_debt.floor() as usize;
        self.audio_sample_debt -= samples as f64;
        if samples == 0 {
            return;
        }
        let pcm = self.machine.render_audio(samples);
        if let Some(player) = &self.audio {
            player.queue(&pcm);
        }
    }

    fn finish(&mut self, reason: StopReason) {
        info!(
            ?reason,
            clocks = self.machine.elapsed_clocks(),
            bus_cycles = self.machine.bus_trace().cycles().len(),
            first_line = %self.machine.screen_text().line_string(0),
            "live test ROM stopped"
        );
        if let Some(window) = &self.window {
            window.set_title(&format!("{} - {reason:?}", self.title));
        }
        self.stop_reason = Some(reason);
    }

    fn render_current_frame(&self) -> RenderedFrame {
        match self.machine.active_display() {
            ActiveDisplay::MargoLfb => {
                let margo = self.machine.margo();
                render_margo_lfb(
                    margo.display(),
                    margo.visible_surface(),
                    &self.machine.palette_argb(),
                )
            }
            ActiveDisplay::Mode13h => render_mode13h(
                self.machine.mode13h_framebuffer(),
                &self.machine.palette_argb(),
            ),
            ActiveDisplay::Text => render_text_frame(&self.machine.screen_text()),
        }
    }

    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let Some(width) = NonZeroU32::new(size.width.max(1)) else {
            return;
        };
        let Some(height) = NonZeroU32::new(size.height.max(1)) else {
            return;
        };

        surface
            .resize(width, height)
            .expect("native window surface should be resizable");
        let mut buffer = surface
            .buffer_mut()
            .expect("native window buffer should be writable");
        blit_centered(
            &self.rendered_screen,
            &mut buffer,
            width.get() as usize,
            height.get() as usize,
        );
        buffer
            .present()
            .expect("native window buffer should be presentable");
    }
}

/// Emulated clocks to run this tick so the machine tracks wall-clock time: the
/// shortfall between the clocks real time calls for and those already run since
/// the epoch, capped so a host that cannot keep up does not spiral.
fn pacing_budget(wall_elapsed: Duration, clock_hz: u64, executed: u64, cap: u64) -> u64 {
    let target = (wall_elapsed.as_secs_f64() * clock_hz as f64) as u64;
    target.saturating_sub(executed).min(cap)
}

fn tick_machine(machine: &mut Machine, cycles: u64) -> Option<StopReason> {
    match machine.run_cycles(cycles) {
        Ok(StopReason::CycleLimit { .. }) => None,
        Ok(reason) => Some(reason),
        Err(error) => Some(StopReason::CpuError(error.to_string())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedFrame {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

fn render_text_frame(frame: &TextFrame) -> RenderedFrame {
    let width = frame.columns * GLYPH_SIZE * TEXT_SCALE;
    let height = frame.rows * GLYPH_SIZE * TEXT_SCALE;
    let mut pixels = vec![VGA_PALETTE[0]; width * height];

    for (cell_index, cell) in frame.cells.iter().enumerate() {
        let column = cell_index % frame.columns;
        let row = cell_index / frame.columns;
        if row >= frame.rows {
            break;
        }

        let character = match cell.character {
            0 => ' ',
            byte => char::from(byte),
        };
        let glyph = BASIC_FONTS.get(character).unwrap_or([0; GLYPH_SIZE]);
        let foreground = VGA_PALETTE[usize::from(cell.attribute & 0x0f)];
        let background = VGA_PALETTE[usize::from((cell.attribute >> 4) & 0x0f)];
        let cell_x = column * GLYPH_SIZE * TEXT_SCALE;
        let cell_y = row * GLYPH_SIZE * TEXT_SCALE;

        for (glyph_y, bits) in glyph.iter().copied().enumerate() {
            for glyph_x in 0..GLYPH_SIZE {
                let color = if bits & (1 << glyph_x) != 0 {
                    foreground
                } else {
                    background
                };
                for scale_y in 0..TEXT_SCALE {
                    for scale_x in 0..TEXT_SCALE {
                        let x = cell_x + glyph_x * TEXT_SCALE + scale_x;
                        let y = cell_y + glyph_y * TEXT_SCALE + scale_y;
                        pixels[y * width + x] = color;
                    }
                }
            }
        }
    }

    RenderedFrame {
        width,
        height,
        pixels,
    }
}

fn render_mode13h(framebuffer: &Framebuffer, palette: &[u32; 256]) -> RenderedFrame {
    let source_width = framebuffer.width as usize;
    let width = source_width * MODE13H_SCALE;
    let height = framebuffer.height as usize * MODE13H_SCALE;
    let mut pixels = vec![palette[0]; width * height];

    for (pixel_index, &index) in framebuffer.indexed_pixels.iter().enumerate() {
        let source_x = pixel_index % source_width;
        let source_y = pixel_index / source_width;
        let color = palette[usize::from(index)];
        for scale_y in 0..MODE13H_SCALE {
            for scale_x in 0..MODE13H_SCALE {
                let x = source_x * MODE13H_SCALE + scale_x;
                let y = source_y * MODE13H_SCALE + scale_y;
                pixels[y * width + x] = color;
            }
        }
    }

    RenderedFrame {
        width,
        height,
        pixels,
    }
}

fn load_margo_test_pattern(machine: &mut Machine) {
    machine.set_margo_mode_640x480x8();
    let display = machine.margo().display();
    let width = display.width as usize;
    let height = display.height as usize;
    let pitch = display.pitch as usize;
    let vram = machine.margo_mut().vram_mut();
    for y in 0..height {
        for x in 0..width {
            // A diagonal gradient across the palette so every entry is exercised.
            vram[y * pitch + x] = ((x + y) & 0xff) as u8;
        }
    }
}

fn render_margo_lfb(display: MargoDisplay, vram: &[u8], palette: &[u32; 256]) -> RenderedFrame {
    let width = display.width as usize;
    let height = display.height as usize;
    let pitch = display.pitch as usize;
    let out_width = width * MARGO_LFB_SCALE;
    let out_height = height * MARGO_LFB_SCALE;
    let mut pixels = vec![palette[0]; out_width * out_height];

    for source_y in 0..height {
        for source_x in 0..width {
            let index = vram.get(source_y * pitch + source_x).copied().unwrap_or(0);
            let color = palette[usize::from(index)];
            for scale_y in 0..MARGO_LFB_SCALE {
                for scale_x in 0..MARGO_LFB_SCALE {
                    let x = source_x * MARGO_LFB_SCALE + scale_x;
                    let y = source_y * MARGO_LFB_SCALE + scale_y;
                    pixels[y * out_width + x] = color;
                }
            }
        }
    }

    RenderedFrame {
        width: out_width,
        height: out_height,
        pixels,
    }
}

fn blit_centered(
    source: &RenderedFrame,
    target: &mut [u32],
    target_width: usize,
    target_height: usize,
) {
    target.fill(VGA_PALETTE[0]);
    let copy_width = source.width.min(target_width);
    let copy_height = source.height.min(target_height);
    let source_x = (source.width - copy_width) / 2;
    let source_y = (source.height - copy_height) / 2;
    let target_x = (target_width - copy_width) / 2;
    let target_y = (target_height - copy_height) / 2;

    for row in 0..copy_height {
        let source_start = (source_y + row) * source.width + source_x;
        let target_start = (target_y + row) * target_width + target_x;
        target[target_start..target_start + copy_width]
            .copy_from_slice(&source.pixels[source_start..source_start + copy_width]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use izarravm_video::TextCell;

    #[test]
    fn pacing_tracks_wall_clock_and_caps_catch_up() {
        let hz = 25_000_000;
        let cap = hz / 20;
        // No wall time elapsed yet: nothing to run.
        assert_eq!(pacing_budget(Duration::ZERO, hz, 0, cap), 0);
        // 1 ms at 25 MHz calls for 25_000 clocks.
        assert_eq!(pacing_budget(Duration::from_millis(1), hz, 0, cap), 25_000);
        // Already ahead of wall time: nothing to run.
        assert_eq!(pacing_budget(Duration::from_millis(1), hz, 30_000, cap), 0);
        // Far behind: catch-up is capped, not unbounded.
        assert_eq!(pacing_budget(Duration::from_secs(10), hz, 0, cap), cap);
    }

    #[test]
    fn text_renderer_draws_foreground_pixels() {
        let mut cells = vec![TextCell::default(); 80 * 25];
        cells[0] = TextCell {
            character: b'X',
            attribute: 0x0f,
        };
        let frame = TextFrame {
            columns: 80,
            rows: 25,
            cells,
            cursor_offset: 0,
        };

        let rendered = render_text_frame(&frame);

        assert_eq!(rendered.width, WINDOW_WIDTH as usize);
        assert_eq!(rendered.height, WINDOW_HEIGHT as usize);
        assert!(
            rendered
                .pixels
                .iter()
                .any(|pixel| *pixel == VGA_PALETTE[15])
        );
    }

    #[test]
    fn mode13h_renderer_maps_indices_through_palette() {
        let mut framebuffer = Framebuffer::mode13h();
        framebuffer.indexed_pixels[0] = 1;
        let mut palette = [0u32; 256];
        palette[1] = 0x00AB_CDEF;

        let rendered = render_mode13h(&framebuffer, &palette);

        assert_eq!(rendered.width, 320 * MODE13H_SCALE);
        assert_eq!(rendered.height, 200 * MODE13H_SCALE);
        // Source pixel 0 fans out to a MODE13H_SCALE by MODE13H_SCALE block.
        assert_eq!(rendered.pixels[0], 0x00AB_CDEF);
        assert_eq!(rendered.pixels[1], 0x00AB_CDEF);
        assert_eq!(rendered.pixels[rendered.width], 0x00AB_CDEF);
        assert_eq!(rendered.pixels[rendered.width + 1], 0x00AB_CDEF);
        // The next source pixel keeps the background color.
        assert_eq!(rendered.pixels[MODE13H_SCALE], palette[0]);
    }

    #[test]
    fn margo_lfb_renderer_maps_indices_through_palette() {
        let display = izarravm_video::MargoDisplay {
            mode: 0x0101,
            width: 4,
            height: 2,
            bpp: 8,
            pitch: 4,
            start: 0,
        };
        let vram = [0u8, 1, 0, 0, 0, 0, 0, 0];
        let mut palette = [0u32; 256];
        palette[1] = 0x0012_3456;

        let rendered = render_margo_lfb(display, &vram, &palette);

        assert_eq!(rendered.width, 4 * MARGO_LFB_SCALE);
        assert_eq!(rendered.height, 2 * MARGO_LFB_SCALE);
        // Source pixel (1,0) has index 1.
        assert_eq!(rendered.pixels[MARGO_LFB_SCALE], 0x0012_3456);
    }

    #[test]
    fn test_pattern_fills_the_lfb_and_selects_margo() {
        let mut machine = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            test_rom(),
        )
        .unwrap();

        load_margo_test_pattern(&mut machine);

        assert_eq!(machine.active_display(), ActiveDisplay::MargoLfb);
        let display = machine.margo().display();
        // Bottom-right visible pixel was written (not left at zero).
        let last = (display.pitch * (display.height - 1) + (display.width - 1)) as usize;
        assert_ne!(machine.margo().vram()[last], 0);
    }

    #[test]
    fn live_tick_advances_machine_and_renders_screen() {
        let mut machine = Machine::new(
            MachineProfile::i386dx25(16, VideoCard::Et4000Ax),
            test_rom(),
        )
        .unwrap();

        let _ = tick_machine(&mut machine, 50_000);
        let rendered = render_text_frame(&machine.screen_text());

        assert_eq!(rendered.width, WINDOW_WIDTH as usize);
        assert_eq!(rendered.height, WINDOW_HEIGHT as usize);
        assert_eq!(rendered.pixels.len(), rendered.width * rendered.height);
    }
}
