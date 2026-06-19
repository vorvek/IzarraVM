use eframe::egui;
use izarravm_audio::AudioPlayer;
use izarravm_dos::HostDrive;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason};
use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{error, info};

const OPL_NATIVE_HZ: f64 = 49_716.0;

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

/// Palette-indexed pixels (mode 13h, the VGA raster core) to an image.
fn indexed_to_color_image(
    pixels: &[u8],
    width: usize,
    height: usize,
    palette: &[u32; 256],
) -> egui::ColorImage {
    let words: Vec<u32> = pixels.iter().map(|&i| palette[i as usize]).collect();
    words_to_color_image(&words, width, height)
}

/// The Very Slow / Slow / Fast readout. The fiction's three modes map to these
/// clocks; the actual mode is switched from Toka, which is not wired yet, so
/// this reflects the machine's current clock. Anything unmapped shows its MHz.
fn speed_label(clock_hz: u64) -> String {
    match clock_hz {
        25_000_000 => "Very Slow".to_string(),
        66_000_000 => "Slow".to_string(),
        233_000_000 => "Fast".to_string(),
        other => format!("{} MHz", other / 1_000_000),
    }
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

/// Emulated clocks to run this tick so the machine tracks wall-clock time,
/// capped so a host that cannot keep up does not spiral.
fn pacing_budget(wall_elapsed: Duration, clock_hz: u64, executed: u64, cap: u64) -> u64 {
    let target = (wall_elapsed.as_secs_f64() * clock_hz as f64) as u64;
    target.saturating_sub(executed).min(cap)
}

fn tick_machine(machine: &mut Machine, cycles: u64) -> Option<StopReason> {
    match machine.run_cycles(cycles) {
        Ok(StopReason::CycleLimit { .. }) => None,
        Ok(reason) => Some(reason),
        Err(err) => Some(StopReason::CpuError(err.to_string())),
    }
}

/// The current frame for the active display, native resolution. Takes `&mut`
/// because the VGA raster path consumes the last presented frame.
fn current_image(machine: &mut Machine) -> egui::ColorImage {
    match machine.active_display() {
        ActiveDisplay::VgaRaster => {
            let palette = machine.palette_argb();
            match machine.vga_raster() {
                Some(raster) => indexed_to_color_image(
                    &raster.pixels,
                    raster.width as usize,
                    raster.height as usize,
                    &palette,
                ),
                None => egui::ColorImage::new([1, 1], vec![egui::Color32::BLACK]),
            }
        }
        ActiveDisplay::MargoLfb => {
            let palette = machine.palette_argb();
            let margo = machine.margo();
            let display = margo.display();
            words_to_color_image(
                &margo.scanout_argb(&palette),
                display.width as usize,
                display.height as usize,
            )
        }
    }
}

pub struct GuiApp {
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    test_pattern: bool,
    title: String,
    machine: Option<Machine>,
    audio: Option<AudioPlayer>,
    audio_clocks: u64,
    audio_sample_debt: f64,
    epoch: Option<(Instant, u64)>,
    // Emulation speed as a fraction of real time: emulated clocks executed per
    // wall second divided by the configured clock. EMA-smoothed, clamped at 1.5.
    speed_ratio: f64,
    last_pace: Option<(Instant, u64)>,
    texture: Option<egui::TextureHandle>,
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
            machine: None,
            audio,
            audio_clocks: 0,
            audio_sample_debt: 0.0,
            epoch: None,
            speed_ratio: 0.0,
            last_pace: None,
            texture: None,
        };
        app.start();
        app
    }

    /// Build a fresh machine, mount C:, and reset the pacing and audio state.
    fn start(&mut self) {
        let mut machine = match Machine::new(self.profile.clone(), &self.rom) {
            Ok(m) => m,
            Err(err) => {
                error!(%err, "failed to start machine");
                return;
            }
        };
        match HostDrive::mount_c(&self.c_drive) {
            Ok(drive) => machine.mount_c_drive(drive),
            Err(err) => error!(%err, "failed to mount C: drive"),
        }
        if self.test_pattern {
            load_margo_test_pattern(&mut machine);
        }
        self.audio_clocks = 0;
        self.audio_sample_debt = 0.0;
        self.epoch = None;
        self.speed_ratio = 0.0;
        self.last_pace = None;
        self.machine = Some(machine);
    }

    fn stop(&mut self) {
        self.machine = None;
    }

    /// Render OPL audio for the emulated time elapsed since the last pump.
    fn pump_audio(&mut self) {
        let (Some(machine), Some(player)) = (&mut self.machine, &self.audio) else {
            return;
        };
        let now = machine.elapsed_clocks();
        let delta = now.saturating_sub(self.audio_clocks);
        self.audio_clocks = now;
        self.audio_sample_debt += delta as f64 * OPL_NATIVE_HZ / self.profile.clock_hz as f64;
        let samples = self.audio_sample_debt.floor() as usize;
        self.audio_sample_debt -= samples as f64;
        if samples == 0 {
            return;
        }
        let pcm = machine.render_audio(samples);
        player.queue(&pcm);
    }

    /// Advance the machine to track wall-clock time and update the speed-ratio EMA.
    fn run_frame(&mut self) {
        let clock_hz = self.profile.clock_hz;
        let Some(machine) = &mut self.machine else {
            return;
        };
        let now = Instant::now();
        let (epoch, epoch_clocks) = *self.epoch.get_or_insert((now, machine.elapsed_clocks()));
        let executed = machine.elapsed_clocks().saturating_sub(epoch_clocks);
        let cap = clock_hz / 20;
        let budget = pacing_budget(now.duration_since(epoch), clock_hz, executed, cap);
        if budget == 0 {
            return;
        }
        tick_machine(machine, budget);
        let ran_to = machine.elapsed_clocks();
        self.pump_audio();
        if let Some((then, then_clocks)) = self.last_pace {
            let wall = now.duration_since(then).as_secs_f64();
            if wall > 0.0 {
                let ran = ran_to.saturating_sub(then_clocks) as f64;
                let ratio = (ran / (wall * clock_hz as f64)).min(1.5);
                self.speed_ratio = self.speed_ratio * 0.9 + ratio * 0.1;
            }
        }
        self.last_pace = Some((now, ran_to));
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
        let Some(machine) = &mut self.machine else {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            return;
        };
        let native = current_image(machine);
        let image = sharp_prescale(
            &native,
            rect.width().round() as usize,
            rect.height().round() as usize,
        );
        let options = egui::TextureOptions::LINEAR;
        let texture = match &mut self.texture {
            Some(t) => {
                t.set(image, options);
                &*t
            }
            None => self
                .texture
                .insert(ctx.load_texture("monitor", image, options)),
        };
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .image(texture.id(), rect, uv, egui::Color32::WHITE);
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.machine.is_some();

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
        ui.label(format!("CPU class: {}", speed_label(self.profile.clock_hz)));
        ui.label(format!("Emulation speed: {:.0}%", self.speed_ratio * 100.0));
        ui.label(format!("Memory: {} MB", self.profile.memory_mib));

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
        let log = self
            .machine
            .as_ref()
            .map(|m| m.serial_text())
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.monospace(log);
            });
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title.clone()));
        if !ctx.wants_keyboard_input() {
            let events: Vec<(egui::Key, bool)> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed, .. } => Some((*key, *pressed)),
                        _ => None,
                    })
                    .collect()
            });
            if let Some(machine) = &mut self.machine {
                for (key, pressed) in events {
                    if let Some(make) = egui_key_to_set1(key) {
                        let code = if pressed { make } else { make | 0x80 };
                        machine.inject_key_scancodes(&[code]);
                    }
                }
            }
        }
        self.run_frame();
        egui::SidePanel::right("controls")
            .exact_width(320.0)
            .resizable(false)
            .show(ctx, |ui| self.controls_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.monitor_ui(ui, ctx));
        ctx.request_repaint();
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
    fn indexed_image_maps_through_palette() {
        let pixels = [0u8, 1, 0, 1];
        let mut palette = [0u32; 256];
        palette[1] = 0x00AB_CDEF;
        let image = indexed_to_color_image(&pixels, 2, 2, &palette);
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

    #[test]
    fn speed_label_maps_known_clocks() {
        assert_eq!(speed_label(25_000_000), "Very Slow");
        assert_eq!(speed_label(66_000_000), "Slow");
        assert_eq!(speed_label(233_000_000), "Fast");
        // Unmapped clocks fall back to a raw MHz label.
        assert_eq!(speed_label(133_000_000), "133 MHz");
    }
}
