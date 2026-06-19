use eframe::egui;
use izarravm_audio::{AudioPlayer, AudioSink};
use izarravm_core::GswMode;
use izarravm_dos::HostDrive;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason};
use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{error, info};

const OPL_NATIVE_HZ: f64 = 49_716.0;

/// How long the emulation thread sleeps between work slices. The wall-clock
/// catch-up pacing absorbs the coarse Windows timer granularity, so realtime
/// holds regardless of the exact wake interval as long as it stays well under
/// the clock_hz/20 budget cap (50 ms of guest time).
const EMU_SLICE: Duration = Duration::from_millis(1);

/// Pack 0x00RRGGBB words into an opaque egui image.
fn words_to_color_image(words: &[u32], width: usize, height: usize) -> egui::ColorImage {
    let mut rgba = vec![0u8; width * height * 4];
    for (i, &color) in words.iter().enumerate().take(width * height) {
        let o = i * 4;
        rgba[o] = ((color >> 16) & 0xff) as u8;
        rgba[o + 1] = ((color >> 8) & 0xff) as u8;
        rgba[o + 2] = (color & 0xff) as u8;
        rgba[o + 3] = 0xff;
    }
    egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba)
}

/// Map palette-indexed pixels (mode 13h, the VGA raster core) to 0x00RRGGBB words.
fn palette_words(pixels: &[u8], palette: &[u32; 256]) -> Vec<u32> {
    pixels.iter().map(|&i| palette[i as usize]).collect()
}

/// Nearest-neighbour integer upscale per axis, as large as fits the target
/// without exceeding it. The caller then lets egui stretch the small remainder
/// with bilinear filtering, which gives a sharp-bilinear look without a shader.
fn sharp_prescale(image: &egui::ColorImage, target_w: usize, target_h: usize) -> egui::ColorImage {
    let [source_w, source_h] = image.size;
    if source_w == 0 || source_h == 0 {
        return image.clone();
    }
    let factor_x = (target_w / source_w).max(1);
    let factor_y = (target_h / source_h).max(1);
    if factor_x == 1 && factor_y == 1 {
        return image.clone();
    }
    let dest_w = source_w * factor_x;
    let dest_h = source_h * factor_y;
    let mut pixels = Vec::with_capacity(dest_w * dest_h);
    for y in 0..dest_h {
        let source_row = (y / factor_y) * source_w;
        for x in 0..dest_w {
            pixels.push(image.pixels[source_row + x / factor_x]);
        }
    }
    egui::ColorImage::new([dest_w, dest_h], pixels)
}

/// Emulated clocks to run for a slice of `dt` wall seconds, clamped to `cap`.
/// Clamping forgives a long stall (host busy, OS starving the thread) instead
/// of banking it, so the emulator does not sprint above 100% to repay a backlog
/// once the host recovers, which read as a catch-up spike before.
fn slice_budget(dt: f64, clock_hz: u64, cap: u64) -> u64 {
    ((dt * clock_hz as f64) as u64).min(cap)
}

fn tick_machine(machine: &mut Machine, cycles: u64) -> Option<StopReason> {
    match machine.run_cycles(cycles) {
        Ok(StopReason::CycleLimit { .. }) => None,
        Ok(reason) => Some(reason),
        Err(err) => Some(StopReason::CpuError(err.to_string())),
    }
}

/// The active display as native-resolution 0x00RRGGBB words plus its size.
fn render_words(machine: &mut Machine) -> (Vec<u32>, usize, usize) {
    match machine.active_display() {
        ActiveDisplay::VgaRaster => {
            let palette = machine.palette_argb();
            match machine.vga_raster() {
                Some(raster) => (
                    palette_words(&raster.pixels, &palette),
                    raster.width as usize,
                    raster.height as usize,
                ),
                None => (vec![0x0000_0000], 1, 1),
            }
        }
        ActiveDisplay::MargoLfb => {
            let palette = machine.palette_argb();
            let margo = machine.margo();
            let display = margo.display();
            (
                margo.scanout_argb(&palette),
                display.width as usize,
                display.height as usize,
            )
        }
    }
}

/// Render OPL audio for the emulated time elapsed since the last pump.
fn pump_audio(
    machine: &mut Machine,
    sink: &AudioSink,
    clock_hz: u64,
    audio_clocks: &mut u64,
    debt: &mut f64,
) {
    let now = machine.elapsed_clocks();
    let delta = now.saturating_sub(*audio_clocks);
    *audio_clocks = now;
    *debt += delta as f64 * OPL_NATIVE_HZ / clock_hz as f64;
    let samples = debt.floor() as usize;
    *debt -= samples as f64;
    if samples == 0 {
        return;
    }
    let pcm = machine.render_audio(samples);
    sink.queue(&pcm);
}

/// What the emulation thread publishes for the UI to render and label. The UI
/// re-uploads the framebuffer only when `seq` advances, so a static screen
/// costs a lock and a few scalars rather than a full upload.
#[derive(Default)]
struct Frame {
    words: Vec<u32>, // native 0x00RRGGBB framebuffer
    width: usize,
    height: usize,
    seq: u64,              // guest frame counter
    serial: String,        // COM1 log
    speed_ratio: f64,      // EMA, fraction of real time
    mode: Option<GswMode>, // live CPU mode for the label
    refresh_hz: f64,       // guest vertical refresh, paces the UI repaint
}

/// UI-to-emulation-thread messages.
enum Command {
    Keys(Vec<u8>),
    Shutdown,
}

/// Handle to the emulation thread: the command channel, the published frame,
/// and the join handle so it can be shut down cleanly.
struct Emulator {
    commands: Sender<Command>,
    frame: Arc<Mutex<Frame>>,
    join: Option<JoinHandle<()>>,
}

impl Emulator {
    /// Spawn the emulation thread for a fresh machine.
    fn spawn(
        profile: MachineProfile,
        rom: Vec<u8>,
        c_drive: PathBuf,
        test_pattern: bool,
        sink: Option<AudioSink>,
    ) -> Self {
        let frame = Arc::new(Mutex::new(Frame::default()));
        let (commands, rx) = mpsc::channel();
        let frame_thread = Arc::clone(&frame);
        let join = std::thread::Builder::new()
            .name("izarravm-emu".into())
            .spawn(move || emulate(profile, rom, c_drive, test_pattern, sink, rx, frame_thread))
            .expect("spawn emulation thread");
        Self {
            commands,
            frame,
            join: Some(join),
        }
    }

    fn send_keys(&self, codes: Vec<u8>) {
        let _ = self.commands.send(Command::Keys(codes));
    }

    fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The emulation thread body: build the machine, then pace it by wall clock,
/// pump audio, and publish a frame snapshot, until told to shut down. Nothing
/// the UI thread does (input floods, slow repaints) can starve this loop.
fn emulate(
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    test_pattern: bool,
    sink: Option<AudioSink>,
    commands: Receiver<Command>,
    frame: Arc<Mutex<Frame>>,
) {
    let clock_hz = profile.clock_hz;
    let mut machine = match Machine::new(profile, &rom) {
        Ok(m) => m,
        Err(err) => {
            error!(%err, "failed to start machine");
            return;
        }
    };
    match HostDrive::mount_c(&c_drive) {
        Ok(drive) => machine.mount_c_drive(drive),
        Err(err) => error!(%err, "failed to mount C: drive"),
    }
    if test_pattern {
        load_margo_test_pattern(&mut machine);
    }

    let cap = clock_hz / 20;
    let mut audio_clocks = machine.elapsed_clocks();
    let mut audio_debt = 0.0;
    let mut speed_ratio = 0.0;
    let mut last = Instant::now();
    let mut published_seq = u64::MAX; // force the first publish

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Keys(codes)) => machine.inject_key_scancodes(&codes),
                Ok(Command::Shutdown) => return,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Pace by the wall time since the last slice, clamped. A delayed slice
        // forgives the lost time rather than banking it, so the guest never
        // sprints above 100% to repay a backlog once the host frees up.
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        let budget = slice_budget(dt, clock_hz, cap);
        if budget > 0 {
            let before = machine.elapsed_clocks();
            let _ = tick_machine(&mut machine, budget);
            let ran = machine.elapsed_clocks().saturating_sub(before);
            if let Some(sink) = &sink {
                pump_audio(
                    &mut machine,
                    sink,
                    clock_hz,
                    &mut audio_clocks,
                    &mut audio_debt,
                );
            }
            if dt > 0.0 {
                let ratio = (ran as f64 / (dt * clock_hz as f64)).min(1.5);
                speed_ratio = speed_ratio * 0.9 + ratio * 0.1;
            }
        }

        // Publish: clone the framebuffer only when the guest presents a new
        // frame; refresh the light fields every pass so the readout stays live.
        let seq = machine.video().frames_completed();
        let new_frame = seq != published_seq;
        let rendered = new_frame.then(|| render_words(&mut machine));
        let serial = new_frame.then(|| machine.serial_text());
        let mode = machine.active_mode();
        let refresh_hz = machine.display_refresh_hz();
        {
            let mut f = frame.lock().expect("frame snapshot poisoned");
            if let Some((words, width, height)) = rendered {
                f.words = words;
                f.width = width;
                f.height = height;
                f.seq = seq;
            }
            if let Some(serial) = serial {
                f.serial = serial;
            }
            f.mode = Some(mode);
            f.refresh_hz = refresh_hz;
            f.speed_ratio = speed_ratio;
        }
        published_seq = seq;

        std::thread::sleep(EMU_SLICE);
    }
}

pub struct GuiApp {
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    test_pattern: bool,
    title: String,
    // The cpal stream is !Send, so it stays here on the UI thread; the
    // emulation thread gets a Send sink cloned from it.
    audio: Option<AudioPlayer>,
    emu: Option<Emulator>,
    texture: Option<egui::TextureHandle>,
    // Guest frame counter of the texture currently uploaded, so we rebuild it
    // only when a new frame is presented rather than on every update().
    frame_seq: u64,
    // Host-loop diagnostics, recomputed once a second: update() calls per second
    // and egui input events per second. Surfaced in the panel to tell a vsync-
    // capped loop from an unbounded spin under an input flood.
    metrics_mark: Option<Instant>,
    frames_since: u32,
    events_since: u32,
    host_fps: f64,
    input_rate: f64,
}

impl GuiApp {
    fn new(
        profile: MachineProfile,
        rom: Vec<u8>,
        c_drive: PathBuf,
        audio_enabled: bool,
        test_pattern: bool,
    ) -> Self {
        let audio = if audio_enabled {
            match AudioPlayer::new() {
                Ok(player) => Some(player),
                Err(err) => {
                    info!(%err, "audio output unavailable; running silently");
                    None
                }
            }
        } else {
            None
        };
        let title = format!(
            "IzarraVM - {} / {} MiB / {}",
            profile.cpu, profile.memory_mib, profile.video
        );
        let mut app = Self {
            profile,
            rom,
            c_drive,
            test_pattern,
            title,
            audio,
            emu: None,
            texture: None,
            frame_seq: 0,
            metrics_mark: None,
            frames_since: 0,
            events_since: 0,
            host_fps: 0.0,
            input_rate: 0.0,
        };
        app.start();
        app
    }

    /// Spawn a fresh emulation thread, replacing any running one.
    fn start(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            emu.shutdown();
        }
        let sink = self.audio.as_ref().map(AudioPlayer::sink);
        self.emu = Some(Emulator::spawn(
            self.profile.clone(),
            self.rom.clone(),
            self.c_drive.clone(),
            self.test_pattern,
            sink,
        ));
        self.texture = None;
        self.frame_seq = 0;
    }

    fn stop(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            emu.shutdown();
        }
        self.texture = None;
        self.frame_seq = 0;
    }
}

impl Drop for GuiApp {
    fn drop(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            emu.shutdown();
        }
    }
}

/// The largest 4:3 rectangle that fits `area`, centred.
fn fit_4_3(area: egui::Rect) -> egui::Rect {
    let (width, height) = if area.width() / area.height() > 4.0 / 3.0 {
        (area.height() * 4.0 / 3.0, area.height())
    } else {
        (area.width(), area.width() * 3.0 / 4.0)
    };
    egui::Rect::from_center_size(area.center(), egui::vec2(width, height))
}

impl GuiApp {
    fn monitor_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let rect = fit_4_3(ui.max_rect());
        let Some(emu) = &self.emu else {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            return;
        };
        // Build a new native image only when the guest frame counter advanced,
        // and only hold the snapshot lock for that copy. The prescale (which
        // depends on the live rect) and the GPU upload happen after the unlock.
        let rebuilt = {
            let f = emu.frame.lock().expect("frame snapshot poisoned");
            if (self.texture.is_none() || f.seq != self.frame_seq) && f.width > 0 {
                self.frame_seq = f.seq;
                Some(words_to_color_image(&f.words, f.width, f.height))
            } else {
                None
            }
        };
        if let Some(native) = rebuilt {
            let image = sharp_prescale(
                &native,
                rect.width().round() as usize,
                rect.height().round() as usize,
            );
            let options = egui::TextureOptions::LINEAR;
            match &mut self.texture {
                Some(t) => t.set(image, options),
                None => {
                    self.texture = Some(ctx.load_texture("monitor", image, options));
                }
            }
        }
        match &self.texture {
            Some(texture) => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter()
                    .image(texture.id(), rect, uv, egui::Color32::WHITE);
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            }
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.emu.is_some();
        let (mode, speed, serial) = match &self.emu {
            Some(emu) => {
                let f = emu.frame.lock().expect("frame snapshot poisoned");
                (f.mode, f.speed_ratio, f.serial.clone())
            }
            None => (None, 0.0, String::new()),
        };

        ui.heading("Machine");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Start"))
                .clicked()
            {
                self.start();
            }
            if ui
                .add_enabled(running, egui::Button::new("Reset"))
                .clicked()
            {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                self.stop();
            }
        });

        ui.separator();
        let mode = mode.unwrap_or(self.profile.cpu);
        ui.label(format!(
            "CPU: GSW-586 ({} mode, {} MHz)",
            mode.canonical_name(),
            mode.clock_hz() / 1_000_000
        ));
        ui.label(format!("Emulation speed: {:.0}%", speed * 100.0));
        ui.label(format!("Memory: {} MB", self.profile.memory_mib));
        ui.label(format!(
            "Host: {:.0} fps, {:.0} input/s",
            self.host_fps, self.input_rate
        ));

        ui.separator();
        ui.heading("Drives");
        ui.horizontal(|ui| {
            if ui.button("Mount C: folder...").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.c_drive = dir;
                }
            }
        });
        ui.label(format!("C: {}", self.c_drive.display()));
        ui.add_enabled(false, egui::Button::new("CD-ROM: not emulated yet"));
        ui.add_enabled(false, egui::Button::new("Floppy: not emulated yet"));

        ui.separator();
        ui.heading("COM1");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.monospace(serial);
            });
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title.clone()));
        // Host-loop diagnostics: count this update() and the input events egui
        // saw, rolling the rates up once a second.
        let now = Instant::now();
        self.frames_since += 1;
        self.events_since += ctx.input(|i| i.events.len()) as u32;
        let mark = *self.metrics_mark.get_or_insert(now);
        let window = now.duration_since(mark).as_secs_f64();
        if window >= 1.0 {
            self.host_fps = self.frames_since as f64 / window;
            self.input_rate = self.events_since as f64 / window;
            self.frames_since = 0;
            self.events_since = 0;
            self.metrics_mark = Some(now);
        }
        // Forward key presses to the emulation thread as Set 1 scancodes.
        if !ctx.wants_keyboard_input() {
            let codes: Vec<u8> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed, .. } => egui_key_to_set1(*key)
                            .map(|make| if *pressed { make } else { make | 0x80 }),
                        _ => None,
                    })
                    .collect()
            });
            if !codes.is_empty() {
                if let Some(emu) = &self.emu {
                    emu.send_keys(codes);
                }
            }
        }
        egui::SidePanel::right("controls")
            .exact_width(320.0)
            .resizable(false)
            .show(ctx, |ui| self.controls_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.monitor_ui(ui, ctx));
        // Repaint at the guest's refresh rate rather than busy-looping at the
        // host vsync. Input still triggers extra repaints, but the emulation
        // runs on its own thread now, so they cannot slow it down.
        let refresh_hz = self.emu.as_ref().map_or(60.0, |emu| {
            let hz = emu
                .frame
                .lock()
                .expect("frame snapshot poisoned")
                .refresh_hz;
            if hz > 0.0 { hz } else { 60.0 }
        });
        ctx.request_repaint_after(Duration::from_secs_f64(1.0 / refresh_hz));
    }
}

/// egui Key to Set 1 scancode (make code). Covers the keys a user types at a
/// DOS prompt; extend as the setup page needs more.
fn egui_key_to_set1(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    Some(match key {
        A => 0x1e,
        B => 0x30,
        C => 0x2e,
        D => 0x20,
        E => 0x12,
        F => 0x21,
        G => 0x22,
        H => 0x23,
        I => 0x17,
        J => 0x24,
        K => 0x25,
        L => 0x26,
        M => 0x32,
        N => 0x31,
        O => 0x18,
        P => 0x19,
        Q => 0x10,
        R => 0x13,
        S => 0x1f,
        T => 0x14,
        U => 0x16,
        V => 0x2f,
        W => 0x11,
        X => 0x2d,
        Y => 0x15,
        Z => 0x2c,
        Num0 => 0x0b,
        Num1 => 0x02,
        Num2 => 0x03,
        Num3 => 0x04,
        Num4 => 0x05,
        Num5 => 0x06,
        Num6 => 0x07,
        Num7 => 0x08,
        Num8 => 0x09,
        Num9 => 0x0a,
        Space => 0x39,
        Enter => 0x1c,
        Backspace => 0x0e,
        Escape => 0x01,
        Tab => 0x0f,
        ArrowUp => 0x48,
        ArrowDown => 0x50,
        ArrowLeft => 0x4b,
        ArrowRight => 0x4d,
        Delete => 0x53,
        F1 => 0x3b,
        F2 => 0x3c,
        F10 => 0x44,
        _ => return None,
    })
}

/// Fill the Margo LFB with a diagonal gradient. Debug aid behind
/// --margo-test-pattern; not reapplied automatically other than on Start/Reset.
fn load_margo_test_pattern(machine: &mut Machine) {
    machine.set_margo_mode_640x480x8();
    let display = machine.margo().display();
    let width = display.width as usize;
    let height = display.height as usize;
    let pitch = display.pitch as usize;
    let vram = machine.margo_mut().vram_mut();
    for y in 0..height {
        for x in 0..width {
            vram[y * pitch + x] = ((x + y) & 0xff) as u8;
        }
    }
}

/// Open the window and run the emulator. Returns when the user closes it.
pub fn run(
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    audio_enabled: bool,
    test_pattern: bool,
) -> Result<(), Box<dyn Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([1280.0, 720.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "IzarraVM",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(GuiApp::new(
                profile,
                rom,
                c_drive,
                audio_enabled,
                test_pattern,
            )))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_budget_clamps_a_stall() {
        let clock = 266_000_000u64;
        let cap = clock / 20; // 50 ms of guest time
        // A normal ~15 ms slice runs its full wall-time worth, under the cap.
        assert_eq!(
            slice_budget(0.015, clock, cap),
            (0.015 * clock as f64) as u64
        );
        // A long stall is clamped to the cap, so the backlog is forgiven, not banked.
        assert_eq!(slice_budget(0.5, clock, cap), cap);
    }

    #[test]
    fn palette_maps_indices_to_words() {
        let pixels = [0u8, 1, 0, 1];
        let mut palette = [0u32; 256];
        palette[1] = 0x00AB_CDEF;
        let words = palette_words(&pixels, &palette);
        assert_eq!(words.len(), 4);
        assert_eq!(words[1], 0x00AB_CDEF);
        let image = words_to_color_image(&words, 2, 2);
        assert_eq!(image.size, [2, 2]);
        let p = image.pixels[1];
        assert_eq!((p.r(), p.g(), p.b()), (0xAB, 0xCD, 0xEF));
    }

    #[test]
    fn prescale_uses_per_axis_integer_factor() {
        // 2x1 source, target 6x6: x factor 3, y factor 6.
        let src = egui::ColorImage::new(
            [2, 1],
            vec![
                egui::Color32::from_rgb(10, 0, 0),
                egui::Color32::from_rgb(0, 20, 0),
            ],
        );
        let out = sharp_prescale(&src, 6, 6);
        assert_eq!(out.size, [6, 6]);
        // First source pixel fills the left 3 columns, second fills the right 3.
        assert_eq!(out.pixels[0], egui::Color32::from_rgb(10, 0, 0));
        assert_eq!(out.pixels[2], egui::Color32::from_rgb(10, 0, 0));
        assert_eq!(out.pixels[3], egui::Color32::from_rgb(0, 20, 0));
        // Second output row repeats the first (vertical factor applied).
        assert_eq!(out.pixels[6], egui::Color32::from_rgb(10, 0, 0));
    }

    #[test]
    fn prescale_is_identity_when_target_smaller() {
        let src = egui::ColorImage::new([4, 4], vec![egui::Color32::BLACK; 16]);
        let out = sharp_prescale(&src, 3, 3);
        assert_eq!(out.size, [4, 4]);
    }
}
