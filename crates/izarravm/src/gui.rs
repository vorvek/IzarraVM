use crate::prefs::{self, GuiPrefs};
use izarravm_audio::{AudioPlayer, AudioSink};
use izarravm_core::GswMode;
use izarravm_dos::HostDrive;
use izarravm_input::HostKeyboard;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason};
use izarravm_video::{DISTIRA_RENDER_THREAD_CHOICES, normalize_distira_render_threads};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{error, warn};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

const OPL_NATIVE_HZ: f64 = 49_716.0;

/// Map a 0..1 master-volume slider to a linear audio gain. This is a cubic
/// perceptual curve; swap it for a proper dB map if it ever matters.
fn volume_gain(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0).powi(3)
}

/// How long the emulation thread sleeps between work slices. The wall-clock
/// catch-up pacing absorbs the coarse Windows timer granularity, so realtime
/// holds regardless of the exact wake interval as long as it stays well under
/// the clock_hz/20 budget cap (50 ms of guest time).
const EMU_SLICE: Duration = Duration::from_millis(1);

/// How long a drive-access LED stays lit after the last access, so a burst of
/// fast reads reads as a steady glow rather than an imperceptible flicker.
const LED_GLOW: Duration = Duration::from_millis(150);

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

/// Refill the pacing credit by the wall time elapsed this slice, capping the
/// backlog at `cap`. The cap forgives a long host stall (the OS starving the
/// thread) instead of banking it, so the guest never sprints above realtime to
/// repay it. The caller runs `credit.max(0)` clocks then subtracts what actually
/// ran, so a floppy read that overshoots its budget drives credit negative and
/// holds the guest until wall-clock catches up: the drive "grinds" in real time.
fn refill_credit(credit: i64, dt: f64, clock_hz: u64, cap: u64) -> i64 {
    (credit + (dt * clock_hz as f64) as i64).min(cap as i64)
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
        ActiveDisplay::Distira => machine.frame_argb(),
    }
}

/// Render OPL audio for the emulated time elapsed since the last pump. `gain` is
/// the host-side master gain (already curved), applied to each sample before it
/// is queued, independent of the guest's own CT1745 mixer.
fn pump_audio(
    machine: &mut Machine,
    sink: &AudioSink,
    clock_hz: u64,
    audio_clocks: &mut u64,
    debt: &mut f64,
    gain: f32,
) {
    let now = machine.elapsed_clocks();
    let delta = now.saturating_sub(*audio_clocks);
    *audio_clocks = now;
    *debt += delta as f64 * OPL_NATIVE_HZ / clock_hz as f64;
    let mut samples = debt.floor() as usize;
    *debt -= samples as f64;
    // A floppy stall jumps the guest clock forward by its whole duration at once,
    // so the catch-up here could ask for seconds of audio in one render. Cap it at
    // roughly the sink's buffer (~0.5 s): the surplus is the paused drive grind,
    // which carries no audio and would only be dropped at the queue anyway.
    let max_samples = OPL_NATIVE_HZ as usize / 2;
    if samples > max_samples {
        samples = max_samples;
        *debt = 0.0;
    }
    if samples == 0 {
        return;
    }
    let mut pcm = machine.render_audio(samples);
    if gain != 1.0 {
        for (l, r) in &mut pcm {
            *l = (*l as f32 * gain)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            *r = (*r as f32 * gain)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
    }
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
    floppy_accesses: u64,  // monotonic A: access count, drives the LED
    c_accesses: u64,       // monotonic C: access count, drives the LED
    cd_accesses: u64,      // monotonic CD access count, drives the LED
}

/// The guest cursor coordinate range the INT 33h mouse uses (matches the machine's
/// MOUSE_MAX_X / MOUSE_MAX_Y): x spans 0..639, y spans 0..199.
const MOUSE_GUEST_MAX_X: i32 = 639;
const MOUSE_GUEST_MAX_Y: i32 = 199;

/// UI-to-emulation-thread messages.
enum Command {
    Keys(Vec<u8>),
    /// An absolute host mouse position mapped onto the guest screen: `x` 0..639,
    /// `y` 0..199, plus the button mask. The host pointer's position over the
    /// framebuffer rect maps straight to the guest cursor (no relative drift, no
    /// confinement), which the BIOS menus read through INT 33h. Capture only.
    MouseAbsolute(i32, i32, u8),
    /// Mount a floppy image into drive A: live. `flush_path` is the source IMG to
    /// rewrite a dirty image to on eject; folder mounts pass None (read-only).
    MountFloppy {
        bytes: Vec<u8>,
        flush_path: Option<PathBuf>,
    },
    /// Eject drive A:, flushing a dirty image back to its source IMG if any.
    EjectFloppy,
    /// Mount a parsed CD image into the ATAPI drive (D:).
    MountCd(izarravm_machine::CdImage),
    /// Eject the CD.
    EjectCd,
    /// Set the host-side Distira/Glide render worker count.
    SetGlideRenderThreads(u8),
    Shutdown,
}

/// Open the host file manager at `path`. A small portable shim over the platform
/// "reveal in file manager" command, kept behind a cfg so no extra crate is
/// pulled in. Failures are logged rather than surfaced; opening a folder is a
/// convenience, not a critical path.
fn open_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";

    match std::process::Command::new(program).arg(path).spawn() {
        Ok(_) => {}
        Err(err) => error!(%err, path = %path.display(), "failed to open the file manager"),
    }
}

#[derive(Clone, Copy)]
enum DriveIcon {
    Floppy,
    Cd,
    Hdd,
}

/// Draw a small drive-type glyph inline and advance the cursor. Painter shapes
/// rather than emoji, so it renders the same regardless of the font's emoji
/// coverage.
fn drive_icon(ui: &mut egui::Ui, kind: DriveIcon) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let col = ui.visuals().text_color();
    let dim = egui::Color32::from_gray(120);
    let stroke = egui::Stroke::new(1.0, col);
    let p = ui.painter();
    let body = rect.shrink(2.0);
    match kind {
        DriveIcon::Floppy => {
            p.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Inside);
            // Metal shutter at the top, label patch at the bottom.
            let shutter = egui::Rect::from_min_size(
                body.left_top() + egui::vec2(body.width() * 0.5, 1.0),
                egui::vec2(body.width() * 0.3, body.height() * 0.35),
            );
            p.rect_filled(shutter, 0.0, dim);
            let label = egui::Rect::from_min_max(
                body.left_bottom() + egui::vec2(2.0, -body.height() * 0.4),
                body.right_bottom() + egui::vec2(-2.0, -1.0),
            );
            p.rect_filled(label, 0.0, dim);
        }
        DriveIcon::Cd => {
            let c = body.center();
            p.circle_stroke(c, body.width() * 0.45, stroke);
            p.circle_filled(c, 1.5, col);
        }
        DriveIcon::Hdd => {
            p.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            p.circle_filled(body.right_bottom() + egui::vec2(-3.5, -3.5), 1.5, col);
        }
    }
}

/// A small square LED that glows green when a drive was just accessed.
fn access_led(ui: &mut egui::Ui, lit: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    let color = if lit {
        egui::Color32::from_rgb(48, 220, 64)
    } else {
        egui::Color32::from_rgb(28, 52, 30)
    };
    ui.painter().rect_filled(rect.shrink(1.0), 2.0, color);
}

/// Host-side master audio gain shared between the UI thread (writes it from the
/// volume slider) and the emulation thread (reads it each audio pump). The f32
/// gain is stored as its bit pattern so it can ride in a lock-free atomic on the
/// audio path.
#[derive(Clone)]
struct SharedGain(Arc<AtomicU32>);

impl SharedGain {
    fn new(gain: f32) -> Self {
        Self(Arc::new(AtomicU32::new(gain.to_bits())))
    }

    fn set(&self, gain: f32) {
        self.0.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
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
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        profile: MachineProfile,
        rom: Vec<u8>,
        c_drive: PathBuf,
        test_pattern: bool,
        sink: Option<AudioSink>,
        rtc_setup: crate::cmos::RtcSetup,
        gain: SharedGain,
        glide_render_threads: u8,
    ) -> Self {
        let frame = Arc::new(Mutex::new(Frame::default()));
        let (commands, rx) = mpsc::channel();
        let frame_thread = Arc::clone(&frame);
        let join = std::thread::Builder::new()
            .name("izarravm-emu".into())
            .spawn(move || {
                emulate(
                    profile,
                    rom,
                    c_drive,
                    test_pattern,
                    sink,
                    rtc_setup,
                    gain,
                    glide_render_threads,
                    rx,
                    frame_thread,
                )
            })
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

    fn send_mouse_absolute(&self, x: i32, y: i32, buttons: u8) {
        let _ = self.commands.send(Command::MouseAbsolute(x, y, buttons));
    }

    fn mount_floppy(&self, bytes: Vec<u8>, flush_path: Option<PathBuf>) {
        let _ = self
            .commands
            .send(Command::MountFloppy { bytes, flush_path });
    }

    fn eject_floppy(&self) {
        let _ = self.commands.send(Command::EjectFloppy);
    }

    fn mount_cd(&self, image: izarravm_machine::CdImage) {
        let _ = self.commands.send(Command::MountCd(image));
    }

    fn eject_cd(&self) {
        let _ = self.commands.send(Command::EjectCd);
    }

    fn set_glide_render_threads(&self, threads: u8) {
        let _ = self.commands.send(Command::SetGlideRenderThreads(threads));
    }

    fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Eject the A: floppy, writing a dirty image back to its source IMG. A folder
/// mount (no flush path) or a clean image is ejected without touching the host.
/// Clears the flush path so a later eject does not rewrite a stale file.
fn flush_floppy(machine: &mut Machine, flush_path: &mut Option<PathBuf>) {
    let dirty = machine.floppy_dirty();
    let Some(bytes) = machine.eject_floppy() else {
        *flush_path = None;
        return;
    };
    if dirty {
        if let Some(path) = flush_path.as_ref() {
            if let Err(err) = std::fs::write(path, &bytes) {
                error!(%err, path = %path.display(), "failed to flush floppy image");
            }
        }
    }
    *flush_path = None;
}

/// The emulation thread body: build the machine, then pace it by wall clock,
/// pump audio, and publish a frame snapshot, until told to shut down. Nothing
/// the UI thread does (input floods, slow repaints) can starve this loop.
#[allow(clippy::too_many_arguments)]
fn emulate(
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    test_pattern: bool,
    sink: Option<AudioSink>,
    rtc_setup: crate::cmos::RtcSetup,
    gain: SharedGain,
    glide_render_threads: u8,
    commands: Receiver<Command>,
    frame: Arc<Mutex<Frame>>,
) {
    let mut machine = match Machine::new(profile, &rom) {
        Ok(m) => m,
        Err(err) => {
            error!(%err, "failed to start machine");
            return;
        }
    };
    // The GUI runs near real time, so let the BIOS play the full graceful POST:
    // the ~8 s RAM count-up and the startup chime. Headless runs and tests leave
    // the default (fast) so they finish inside their cycle budgets.
    machine.set_fast_post(false);
    machine.set_distira_render_threads(glide_render_threads);
    match HostDrive::mount_c(&c_drive) {
        Ok(drive) => machine.mount_c_drive(drive),
        Err(err) => error!(%err, "failed to mount C: drive"),
    }
    // Let the BIOS boot Toka-DOS from this drive and the setup-menu Repair and
    // Format options act on it.
    machine.set_toka_c_root(c_drive.clone());
    // Bring the RTC online: load cmos.bin (or write defaults) and seed the clock
    // from the host time read on the main thread at startup.
    rtc_setup.apply(&mut machine);
    // Auto-match the guest keyboard layout to the host. Auto-detect wins each
    // boot; the setup page / KEYB still change the live layout for the session.
    if let Some(index) = crate::host_keyboard_layout_index() {
        let mut cmos = machine.cmos_bytes();
        cmos[0x10] = index;
        cmos[0x13] = crate::codepage_index_for_layout(index);
        machine.load_cmos(&cmos);
    }
    if test_pattern {
        load_margo_test_pattern(&mut machine);
    }

    let mut audio_clocks = machine.elapsed_clocks();
    let mut audio_debt = 0.0;
    let mut speed_ratio = 0.0;
    // Pacing credit (guest clocks the guest is owed). Wall time refills it; the
    // cycles run drain it. A disk read that consumes more than its slice drives
    // it negative, pausing the guest for the disk's duration.
    let mut credit: i64 = 0;
    let mut last = Instant::now();
    let mut published_seq = u64::MAX; // force the first publish

    let cmos_path = rtc_setup.cmos_path.clone();
    // The source IMG path of the mounted floppy, when it is a writable image
    // mount. A dirty image is flushed here on eject and on shutdown. Folder mounts
    // are read-only and leave this None.
    let mut floppy_flush_path: Option<PathBuf> = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Keys(codes)) => machine.inject_key_scancodes(&codes),
                Ok(Command::MouseAbsolute(x, y, buttons)) => {
                    machine.set_mouse_absolute(x, y, buttons)
                }
                Ok(Command::MountFloppy { bytes, flush_path }) => {
                    match machine.mount_floppy(bytes) {
                        Ok(()) => floppy_flush_path = flush_path,
                        Err(err) => error!(%err, "failed to mount floppy image"),
                    }
                }
                Ok(Command::EjectFloppy) => {
                    flush_floppy(&mut machine, &mut floppy_flush_path);
                }
                Ok(Command::MountCd(image)) => machine.mount_cd(image),
                Ok(Command::EjectCd) => machine.eject_cd(),
                Ok(Command::SetGlideRenderThreads(threads)) => {
                    machine.set_distira_render_threads(threads)
                }
                Ok(Command::Shutdown) => {
                    // Flush the floppy and the final CMOS state before exiting.
                    flush_floppy(&mut machine, &mut floppy_flush_path);
                    crate::cmos::save_cmos_file(&cmos_path, &machine.cmos_bytes());
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    flush_floppy(&mut machine, &mut floppy_flush_path);
                    crate::cmos::save_cmos_file(&cmos_path, &machine.cmos_bytes());
                    return;
                }
            }
        }

        // Pace by the wall time since the last slice. The credit bucket forgives a
        // host stall (capped backlog, no catch-up sprint) and, in the other
        // direction, makes a disk read that jumps the guest clock cost real
        // wall-clock time: the overshoot drives credit negative and the next
        // slices run nothing until wall time refills it.
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        // Pace against the live mode clock, which the guest can change at runtime
        // (Lotura port 0xE1). Reading it each slice keeps the credit refill in the
        // same clock domain as the cycles run and as the disk-stall jumps, so a
        // floppy read pauses for its true wall-clock duration in any GSW mode.
        let clock_hz = machine.active_mode().clock_hz();
        let cap = clock_hz / 20;
        credit = refill_credit(credit, dt, clock_hz, cap);
        let budget = credit.max(0) as u64;
        if budget > 0 {
            let before = machine.elapsed_clocks();
            let stall_before = machine.io_stall_clocks();
            let stop = tick_machine(&mut machine, budget);
            let ran = machine.elapsed_clocks().saturating_sub(before);
            // Of those clocks, some may be a device-I/O stall (a floppy seek/read)
            // that jumped the clock without executing instructions. Drain the full
            // ran from the credit so the stall still costs wall-clock time, but
            // exclude it from the speed measurement below.
            let stalled = machine.io_stall_clocks().saturating_sub(stall_before);
            credit -= i64::try_from(ran).unwrap_or(i64::MAX);
            // A halted guest (POST done, nothing to boot) stops driving the video
            // beam, so the display would freeze on whatever half-drawn frame was
            // completing when HLT ran. Keep scanning the VGA so the final, complete
            // framebuffer is presented instead.
            if matches!(stop, Some(StopReason::Halted)) {
                machine.advance_devices_clocks(budget);
            }
            if let Some(sink) = &sink {
                pump_audio(
                    &mut machine,
                    sink,
                    clock_hz,
                    &mut audio_clocks,
                    &mut audio_debt,
                    gain.get(),
                );
            }
            if dt > 0.0 {
                // Speed reflects instructions executed vs wall time; a drive stall
                // is intentional wait, not the emulator running fast.
                let executed = ran.saturating_sub(stalled);
                let ratio = (executed as f64 / (dt * clock_hz as f64)).min(1.5);
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
        let (floppy_accesses, c_accesses) = machine.drive_access_counts();
        let cd_accesses = machine.cd_access_count();
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
            f.floppy_accesses = floppy_accesses;
            f.c_accesses = c_accesses;
            f.cd_accesses = cd_accesses;
        }
        published_seq = seq;

        // Persist cmos.bin when the guest wrote an NVRAM byte (a setup-page
        // save). take_cmos_dirty clears the flag so we write only on a change.
        if machine.take_cmos_dirty() {
            crate::cmos::save_cmos_file(&cmos_path, &machine.cmos_bytes());
        }

        std::thread::sleep(EMU_SLICE);
    }
}

/// What is in drive A:, remembered so a Reset can remount the same media. Image
/// mounts replay from the source IMG (a reset flushes dirty guest writes back to
/// it first, so the re-read keeps them); folder mounts rebuild from the folder.
enum FloppySource {
    Image(PathBuf),
    Folder(PathBuf),
}

/// Pick the floppy mount to restore from saved prefs, if its source still exists.
/// A recorded image wins over a folder when both are present; a path that has
/// since been deleted or moved is skipped (the drive starts empty).
fn restore_floppy_source(prefs: &GuiPrefs) -> Option<FloppySource> {
    if let Some(path) = &prefs.last_floppy_image {
        if path.is_file() {
            return Some(FloppySource::Image(path.clone()));
        }
    }
    if let Some(dir) = &prefs.last_floppy_folder {
        if dir.is_dir() {
            return Some(FloppySource::Folder(dir.clone()));
        }
    }
    None
}

pub struct GuiApp {
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    test_pattern: bool,
    rtc_setup: crate::cmos::RtcSetup,
    title: String,
    // Input-capture state, the single source of truth for routing. When true the
    // OS cursor is confined and hidden over the window, all keyboard input goes
    // to the guest (egui does not consume it, including TAB), and host mouse
    // motion and buttons are forwarded to the VM. Ctrl+F2 releases it. Entered
    // by clicking the framebuffer image.
    input_captured: bool,
    // Guest NumLock/CapsLock/ScrollLock state, mirrored from the host. Parallel
    // to HOST_LOCK_KEYS; seeded false because the BIOS clears KB_FLAGS on boot.
    guest_locks: [bool; HOST_LOCK_KEYS.len()],
    // Set by monitor_ui when the framebuffer image is clicked, so the event loop
    // can enter capture (it owns the winit Window that monitor_ui does not).
    want_capture: bool,
    // Last button mask forwarded to the VM, so a button press or release is sent
    // even on a frame with no pointer motion.
    last_buttons: u8,
    // The framebuffer image rect from the last frame, in egui points. The capture
    // path scales host pointer motion across it into guest pixels. None until the
    // monitor has been drawn at least once.
    screen_rect: Option<egui::Rect>,
    // Accumulated guest cursor position (0..639 x 0..199) while captured: raw
    // relative motion from the locked cursor adds into it, clamped to the screen.
    // Reset to the centre on capture enter.
    abs_x: f32,
    abs_y: f32,
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
    // What is mounted in drive A:, for the label. None shows "(empty)". The
    // emulation thread owns the actual mount; this string mirrors it for display.
    floppy_label: Option<String>,
    // The source behind that mount, kept so a Reset remounts the same media
    // instead of leaving the drive empty. Cleared on Stop and Eject.
    floppy_source: Option<FloppySource>,
    // Drive-access LED state: the last access count seen from the frame snapshot
    // and when it last advanced, so the LED lights briefly on each access.
    floppy_access_seen: u64,
    c_access_seen: u64,
    floppy_access_at: Option<Instant>,
    c_access_at: Option<Instant>,
    // What is mounted in the CD-ROM drive (D:), for the label. None shows
    // "(empty)". The emulation thread owns the mount; this mirrors it.
    cd_label: Option<String>,
    cd_access_seen: u64,
    cd_access_at: Option<Instant>,
    // Whether the floating COM1 window is open. The sidebar button and the
    // window's own close control both flip this.
    show_com1: bool,
    // Master volume slider position, 0.0..1.0. Cubed into a host-side gain that
    // the emulation thread reads through `gain`.
    volume: f32,
    // The shared gain handed to the emulation thread; the UI writes it whenever
    // the slider moves so the audio path stays lock-free.
    gain: SharedGain,
    // Distira/Glide render worker count. Persisted in the GUI prefs and applied
    // live to the emulation thread.
    glide_render_threads: u8,
    // Persisted GUI prefs (volume, last mounts) and where they live on disk. The
    // file sits next to the C: root and is rewritten on a change.
    prefs: GuiPrefs,
    prefs_path: PathBuf,
}

impl GuiApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        profile: MachineProfile,
        rom: Vec<u8>,
        c_drive: PathBuf,
        cd_image: Option<PathBuf>,
        audio_enabled: bool,
        test_pattern: bool,
        rtc_setup: crate::cmos::RtcSetup,
    ) -> Self {
        let audio = if audio_enabled {
            match AudioPlayer::new() {
                Ok(player) => Some(player),
                Err(err) => {
                    warn!(%err, "audio output unavailable; running silently");
                    None
                }
            }
        } else {
            None
        };
        // The machine details (CPU, memory) live in the controls panel; the window
        // title stays the product name.
        let title = String::from("IzarraVM");
        // Load the GUI prefs (volume, last mounts) from next to the C: root. A
        // missing or corrupt file falls back to defaults inside load().
        let prefs_path = prefs::prefs_path(&c_drive);
        let prefs = GuiPrefs::load(&prefs_path);
        let volume = prefs.master_volume.clamp(0.0, 1.0);
        let glide_render_threads = prefs.glide_render_threads;
        let gain = SharedGain::new(volume_gain(volume));
        // Restore the last mount if the source still exists on disk. An image
        // takes priority over a folder when both are recorded.
        let floppy_source = restore_floppy_source(&prefs);
        let mut app = Self {
            profile,
            rom,
            c_drive,
            test_pattern,
            rtc_setup,
            title,
            input_captured: false,
            guest_locks: [false; HOST_LOCK_KEYS.len()],
            want_capture: false,
            last_buttons: 0,
            screen_rect: None,
            abs_x: 0.0,
            abs_y: 0.0,
            audio,
            emu: None,
            texture: None,
            frame_seq: 0,
            metrics_mark: None,
            frames_since: 0,
            events_since: 0,
            host_fps: 0.0,
            input_rate: 0.0,
            floppy_label: None,
            floppy_source,
            floppy_access_seen: 0,
            c_access_seen: 0,
            floppy_access_at: None,
            c_access_at: None,
            cd_label: None,
            cd_access_seen: 0,
            cd_access_at: None,
            show_com1: false,
            volume,
            gain,
            glide_render_threads,
            prefs,
            prefs_path,
        };
        app.start();
        // Mount a config-provided CD image once the emulation thread is up.
        if let Some(path) = cd_image {
            match load_cd_image_from_path(&path) {
                Ok(image) => {
                    if let Some(emu) = &app.emu {
                        emu.mount_cd(image);
                    }
                    app.cd_label = path.file_name().map(|n| n.to_string_lossy().into_owned());
                }
                Err(err) => error!(%err, path = %path.display(), "failed to mount config CD image"),
            }
        }
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
            self.rtc_setup.clone(),
            self.gain.clone(),
            self.glide_render_threads,
        ));
        self.texture = None;
        self.frame_seq = 0;
        self.guest_locks = [false; HOST_LOCK_KEYS.len()];
        // A fresh machine boots with an empty drive A:, then we remount whatever
        // was in it so a Reset keeps the disk in the drive (no race to re-mount
        // before the BIOS boots).
        self.floppy_label = None;
        if let Some(source) = self.floppy_source.take() {
            self.mount_floppy_source(source);
        }
    }

    fn stop(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            emu.shutdown();
        }
        self.texture = None;
        self.frame_seq = 0;
        self.floppy_label = None;
        self.floppy_source = None;
        self.guest_locks = [false; HOST_LOCK_KEYS.len()];
    }

    /// Save prefs and stop the emulation thread on window close.
    fn shutdown_for_exit(&mut self) {
        self.save_prefs();
        self.stop();
    }
}

impl Drop for GuiApp {
    fn drop(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            emu.shutdown();
        }
        // Save-on-exit as a backstop; changes are already persisted as they
        // happen, so this just catches anything not yet flushed.
        self.save_prefs();
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
        // Record the image rect so the capture path can scale host pointer motion
        // across it into guest pixels.
        self.screen_rect = Some(rect);
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
        // Clicking the screen requests input capture. The actual cursor grab and
        // hide happen later in toggle_capture (run from the event loop, which owns
        // the winit Window); here we only flag the intent.
        let response = ui.interact(rect, ui.id().with("monitor-capture"), egui::Sense::click());
        if response.clicked() && !self.input_captured {
            self.want_capture = true;
        }
    }

    /// Forward already-translated Set 1 bytes to the emulation thread. Empty
    /// slices (an unmapped key, nothing held) are dropped.
    fn send_keys_to_guest(&self, codes: Vec<u8>) {
        if codes.is_empty() {
            return;
        }
        if let Some(emu) = &self.emu {
            emu.send_keys(codes);
        }
    }

    /// The guest's published vertical refresh rate, used to pace the host
    /// redraw. Falls back to 60 Hz when no machine is running or the guest has
    /// not reported a rate yet.
    fn guest_refresh_hz(&self) -> f64 {
        self.emu.as_ref().map_or(60.0, |emu| {
            let hz = emu
                .frame
                .lock()
                .expect("frame snapshot poisoned")
                .refresh_hz;
            if hz > 0.0 { hz } else { 60.0 }
        })
    }

    /// Whether monitor_ui flagged a click-to-capture this frame, clearing it.
    fn take_want_capture(&mut self) -> bool {
        std::mem::take(&mut self.want_capture)
    }

    /// Enter or leave input capture. While captured we lock and hide the OS cursor
    /// (winit Locked: pinned in place, cannot move on screen or leave the window)
    /// and route keyboard and mouse to the guest, which draws its own cursor.
    /// Ctrl+F2 releases. Locked delivers motion as raw relative deltas, which we
    /// accumulate into the guest cursor position (clamped to the screen), so there
    /// is nothing for the OS cursor to escape and no warp to fight. On release we
    /// flush any held keys so nothing sticks down in the guest.
    fn toggle_capture(&mut self, window: &winit::window::Window, kbd: &mut HostKeyboard) {
        self.input_captured = !self.input_captured;
        self.last_buttons = 0;
        if self.input_captured {
            // Start the guest cursor centred; raw motion accumulates from there.
            self.abs_x = MOUSE_GUEST_MAX_X as f32 / 2.0;
            self.abs_y = MOUSE_GUEST_MAX_Y as f32 / 2.0;
            self.sync_guest_locks();
            let _ = window
                .set_cursor_grab(winit::window::CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(winit::window::CursorGrabMode::Confined));
            window.set_cursor_visible(false);
        } else {
            self.send_keys_to_guest(kbd.release_all());
            let _ = window.set_cursor_grab(winit::window::CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
        self.title = capture_title(self.input_captured);
    }

    /// Update the guest button mask from a pointer button edge and resend the
    /// current absolute position with the new mask.
    fn set_guest_button(&mut self, bit: u8, pressed: bool) {
        if pressed {
            self.last_buttons |= bit;
        } else {
            self.last_buttons &= !bit;
        }
        if let Some(emu) = &self.emu {
            emu.send_mouse_absolute(self.abs_x as i32, self.abs_y as i32, self.last_buttons);
        }
    }

    /// Add raw relative motion (winit DeviceEvent::MouseMotion) into the guest
    /// cursor position, scaled across the framebuffer rect into guest pixels and
    /// clamped to the screen, then send the new absolute position.
    fn accumulate_guest_motion(&mut self, dx: f32, dy: f32, ppp: f32) {
        let Some(rect) = self.screen_rect else {
            return;
        };
        let sx = MOUSE_GUEST_MAX_X as f32 / (rect.width() * ppp).max(1.0);
        let sy = MOUSE_GUEST_MAX_Y as f32 / (rect.height() * ppp).max(1.0);
        self.abs_x = (self.abs_x + dx * sx).clamp(0.0, MOUSE_GUEST_MAX_X as f32);
        self.abs_y = (self.abs_y + dy * sy).clamp(0.0, MOUSE_GUEST_MAX_Y as f32);
        if let Some(emu) = &self.emu {
            emu.send_mouse_absolute(self.abs_x as i32, self.abs_y as i32, self.last_buttons);
        }
    }

    /// Mirror the host's NumLock/CapsLock/ScrollLock onto the guest. Each lock
    /// that differs gets a make+break injected, which the BIOS INT 09h handler
    /// toggles once (guarded by its held-flag). Runs every frame, so it also
    /// catches the host toggling a lock mid-session, not just the load.
    fn sync_guest_locks(&mut self) {
        let Some(emu) = &self.emu else {
            return;
        };
        for (i, (vk, make)) in HOST_LOCK_KEYS.iter().enumerate() {
            let Some(host_on) = host_lock_on(*vk) else {
                return;
            };
            if host_on != self.guest_locks[i] {
                emu.send_keys(vec![*make, *make | 0x80]);
                self.guest_locks[i] = host_on;
            }
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.emu.is_some();
        let (mode, speed, floppy_accesses, c_accesses, cd_accesses) = match &self.emu {
            Some(emu) => {
                let f = emu.frame.lock().expect("frame snapshot poisoned");
                (
                    f.mode,
                    f.speed_ratio,
                    f.floppy_accesses,
                    f.c_accesses,
                    f.cd_accesses,
                )
            }
            None => (
                None,
                0.0,
                self.floppy_access_seen,
                self.c_access_seen,
                self.cd_access_seen,
            ),
        };
        // Light a drive LED whenever its access count advanced since last frame.
        let now = Instant::now();
        if floppy_accesses != self.floppy_access_seen {
            self.floppy_access_seen = floppy_accesses;
            self.floppy_access_at = Some(now);
        }
        if c_accesses != self.c_access_seen {
            self.c_access_seen = c_accesses;
            self.c_access_at = Some(now);
        }
        if cd_accesses != self.cd_access_seen {
            self.cd_access_seen = cd_accesses;
            self.cd_access_at = Some(now);
        }

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
        ui.heading("Glide");
        ui.horizontal(|ui| {
            ui.label("Render threads");
            let before = self.glide_render_threads;
            for threads in DISTIRA_RENDER_THREAD_CHOICES {
                ui.selectable_value(&mut self.glide_render_threads, threads, threads.to_string());
            }
            if self.glide_render_threads != before {
                self.set_glide_render_threads(self.glide_render_threads);
            }
        });

        ui.separator();
        ui.heading("Audio");
        ui.horizontal(|ui| {
            ui.label("Volume");
            // Show 0..100% but drive a 0..1 value. On a change, recompute the
            // host-side gain and persist the new volume to izarravm.conf.
            let slider = ui.add(
                egui::Slider::new(&mut self.volume, 0.0..=1.0)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                    .custom_parser(|s| {
                        s.trim_end_matches('%')
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .map(|p| (p / 100.0).clamp(0.0, 1.0))
                    }),
            );
            if slider.changed() {
                self.gain.set(volume_gain(self.volume));
                self.prefs.master_volume = self.volume;
                self.save_prefs();
            }
        });

        ui.separator();
        ui.heading("Drives");
        self.drives_ui(ui, running);

        // The serial console lives in its own floating window now; a button at the
        // bottom of the sidebar toggles it.
        ui.separator();
        let com1_label = if self.show_com1 { "Hide COM1" } else { "COM1" };
        if ui.button(com1_label).clicked() {
            self.show_com1 = !self.show_com1;
        }
    }

    /// The floating COM1 window: black monospace serial log on white, auto-scrolled
    /// to the bottom, the same console look the sidebar used to carry inline. The
    /// window is draggable, resizable, and closable; its open state is bound to
    /// `show_com1` so the close control and the sidebar button stay in sync.
    fn com1_window(&mut self, ctx: &egui::Context) {
        let serial = match &self.emu {
            Some(emu) => emu
                .frame
                .lock()
                .expect("frame snapshot poisoned")
                .serial
                .clone(),
            None => String::new(),
        };
        let mut open = self.show_com1;
        egui::Window::new("COM1")
            .open(&mut open)
            .resizable(true)
            .default_size([480.0, 320.0])
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.style_mut().spacing.scroll.bar_width = 6.0;
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(serial)
                                        .monospace()
                                        .color(egui::Color32::BLACK),
                                ));
                            });
                    });
            });
        self.show_com1 = open;
    }

    /// Write the current prefs to disk. Best-effort: GuiPrefs::save logs and
    /// swallows any IO error, so this never interrupts the UI.
    fn save_prefs(&self) {
        self.prefs.save(&self.prefs_path);
    }

    fn set_glide_render_threads(&mut self, threads: u8) {
        let threads = normalize_distira_render_threads(threads);
        self.glide_render_threads = threads;
        self.prefs.glide_render_threads = threads;
        self.save_prefs();
        if let Some(emu) = &self.emu {
            emu.set_glide_render_threads(threads);
        }
    }

    /// The three drive rows: A: floppy (load IMG/folder, eject), CD-ROM (the same
    /// pair, disabled for now), and C: (open the host folder, no mount). `running`
    /// gates the floppy actions on a live emulation thread to send commands to.
    fn drives_ui(&mut self, ui: &mut egui::Ui, running: bool) {
        let lit = |at: Option<Instant>| at.is_some_and(|t| t.elapsed() < LED_GLOW);
        let floppy_lit = lit(self.floppy_access_at);
        let c_lit = lit(self.c_access_at);
        let cd_lit = lit(self.cd_access_at);

        // A: floppy. Icon, name, then the access LED on the header row; the drive
        // letter in the header makes the status line below it letter-free.
        ui.horizontal(|ui| {
            drive_icon(ui, DriveIcon::Floppy);
            ui.label("A: floppy");
            access_led(ui, floppy_lit);
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(running, egui::Button::new("Load IMG..."))
                .clicked()
            {
                self.load_floppy_img();
            }
            if ui
                .add_enabled(running, egui::Button::new("Load folder..."))
                .clicked()
            {
                self.load_floppy_folder();
            }
            let mounted = self.floppy_label.is_some();
            if ui
                .add_enabled(running && mounted, egui::Button::new("Eject"))
                .clicked()
            {
                if let Some(emu) = &self.emu {
                    emu.eject_floppy();
                }
                self.floppy_label = None;
                self.floppy_source = None;
                // Forget the mount so it is not restored next launch.
                self.prefs.last_floppy_image = None;
                self.prefs.last_floppy_folder = None;
                self.save_prefs();
            }
        });
        ui.label(self.floppy_label.as_deref().unwrap_or("(empty)"));

        ui.add_space(4.0);

        // CD-ROM (D:): mount an ISO or a CUE/BIN into the ATAPI drive live.
        ui.horizontal(|ui| {
            drive_icon(ui, DriveIcon::Cd);
            ui.label("D: CD-ROM");
            access_led(ui, cd_lit);
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(running, egui::Button::new("Load ISO/CUE..."))
                .clicked()
            {
                self.load_cd_image();
            }
            let mounted = self.cd_label.is_some();
            if ui
                .add_enabled(running && mounted, egui::Button::new("Eject"))
                .clicked()
            {
                if let Some(emu) = &self.emu {
                    emu.eject_cd();
                }
                self.cd_label = None;
            }
        });
        ui.label(self.cd_label.as_deref().unwrap_or("(empty)"));

        ui.add_space(4.0);

        // C: drive. Auto-mounted; no mount button, just open the host folder.
        ui.horizontal(|ui| {
            drive_icon(ui, DriveIcon::Hdd);
            ui.label("C: drive");
            access_led(ui, c_lit);
        });
        if ui.button("Open C: folder").clicked() {
            open_in_file_manager(&self.c_drive);
        }
        ui.label(self.c_drive.display().to_string());
    }

    /// Pick a floppy IMG and mount it live. The image is writable in memory and
    /// flushed back to this file on eject, so the source path travels with it.
    fn load_floppy_img(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Floppy image", &["img", "ima", "flp"])
            .pick_file()
        else {
            return;
        };
        self.mount_floppy_source(FloppySource::Image(path));
    }

    /// Pick a host folder, synthesize a FAT12 image from it, and mount it live.
    /// Folder mounts are read-only, so there is no flush path back to the host.
    fn load_floppy_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.mount_floppy_source(FloppySource::Folder(dir));
    }

    /// Read or build the image for `source`, mount it into the live emulation
    /// thread, and remember it so a Reset can remount the same media. Errors are
    /// logged and leave the drive unchanged. Used by both the Load buttons and
    /// the remount on Reset.
    fn mount_floppy_source(&mut self, source: FloppySource) {
        let (bytes, flush_path, label) = match &source {
            FloppySource::Image(path) => {
                let bytes = match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        error!(%err, path = %path.display(), "failed to read floppy image");
                        return;
                    }
                };
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                (bytes, Some(path.clone()), label)
            }
            FloppySource::Folder(dir) => {
                let bytes = match izarravm_machine::build_fat12(dir) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        error!(%err, dir = %dir.display(), "failed to build a FAT12 image from the folder");
                        return;
                    }
                };
                let label = format!(
                    "{} (folder)",
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dir.display().to_string())
                );
                (bytes, None, label)
            }
        };
        let Some(emu) = &self.emu else {
            return;
        };
        emu.mount_floppy(bytes, flush_path);
        self.floppy_label = Some(label);
        // Remember the mount in prefs so it is restored next launch. An image and
        // a folder are mutually exclusive in drive A:, so recording one clears the
        // other.
        match &source {
            FloppySource::Image(path) => {
                self.prefs.last_floppy_image = Some(path.clone());
                self.prefs.last_floppy_folder = None;
            }
            FloppySource::Folder(dir) => {
                self.prefs.last_floppy_folder = Some(dir.clone());
                self.prefs.last_floppy_image = None;
            }
        }
        self.save_prefs();
        self.floppy_source = Some(source);
    }

    /// Pick a CD image (an `.iso` or a `.cue`) and mount it into the ATAPI drive.
    /// A `.cue` is parsed against its companion `.bin`; an `.iso` mounts as a
    /// single data track. Errors are logged and leave the drive unchanged.
    fn load_cd_image(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CD image", &["iso", "cue", "bin"])
            .pick_file()
        else {
            return;
        };
        let image = match load_cd_image_from_path(&path) {
            Ok(image) => image,
            Err(err) => {
                error!(%err, path = %path.display(), "failed to load CD image");
                return;
            }
        };
        let Some(emu) = &self.emu else {
            return;
        };
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        emu.mount_cd(image);
        self.cd_label = Some(label);
    }
}

/// Build a `CdImage` from a host path. A `.cue` is read as text and parsed
/// against the BIN its `FILE` line names (resolved next to the CUE); any other
/// extension is treated as a raw ISO. Returns a human-readable error string.
fn load_cd_image_from_path(path: &Path) -> Result<izarravm_machine::CdImage, String> {
    let is_cue = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    if is_cue {
        let cue = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let bin_path = cue_bin_path(path, &cue);
        let bin = std::fs::read(&bin_path)
            .map_err(|e| format!("reading BIN {}: {e}", bin_path.display()))?;
        izarravm_machine::CdImage::from_cue(&cue, bin)
    } else {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        izarravm_machine::CdImage::from_iso(bytes)
    }
}

/// Resolve the BIN file a CUE references. The `FILE "name" BINARY` line names it
/// relative to the CUE's directory; if no FILE line is found, fall back to the
/// CUE's own stem with a `.bin` extension.
fn cue_bin_path(cue_path: &Path, cue: &str) -> PathBuf {
    let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    for line in cue.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("FILE ").or_else(|| {
            trimmed
                .strip_prefix("file ")
                .or_else(|| trimmed.strip_prefix("File "))
        }) {
            // The name is the quoted token, or the first whitespace token.
            let name = rest
                .split('"')
                .nth(1)
                .or_else(|| rest.split_whitespace().next())
                .unwrap_or("");
            if !name.is_empty() {
                return dir.join(name);
            }
        }
    }
    dir.join(format!(
        "{}.bin",
        cue_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ))
}

impl GuiApp {
    /// Build one egui frame: the title, the sidebar, the monitor, and the optional
    /// COM1 window. Keyboard, mouse capture, and focus loss are handled in the
    /// winit event loop now, not here, so the guest reads raw physical keys.
    fn ui(&mut self, ctx: &egui::Context) {
        // Keep the title in sync with the capture state every frame.
        self.title = capture_title(self.input_captured);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title.clone()));
        // Host-loop diagnostics: count this frame and the input events egui saw,
        // rolling the rates up once a second.
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
        // Mirror the host lock keys onto the guest each frame.
        self.sync_guest_locks();
        egui::SidePanel::right("controls")
            .exact_width(320.0)
            .resizable(false)
            .show(ctx, |ui| self.controls_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.monitor_ui(ui, ctx));
        // The COM1 console floats over the central panel when toggled open.
        if self.show_com1 {
            self.com1_window(ctx);
        }
    }
}

/// The window title for the current capture state. While captured it tells the
/// user how to release the grab; otherwise it is just the product name.
fn capture_title(captured: bool) -> String {
    if captured {
        String::from("IzarraVM - [Press Ctrl+F2 to release the input]")
    } else {
        String::from("IzarraVM")
    }
}

const VK_NUMLOCK: i32 = 0x90;
const VK_CAPITAL: i32 = 0x14;
const VK_SCROLL: i32 = 0x91;
/// Host lock keys mirrored to the guest, as (host virtual-key, Set 1 make).
/// Break is make | 0x80. Order is parallel to `GuiApp::guest_locks`.
const HOST_LOCK_KEYS: [(i32, u8); 3] = [(VK_NUMLOCK, 0x45), (VK_CAPITAL, 0x3a), (VK_SCROLL, 0x46)];

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "GetKeyState"]
    fn get_key_state(v_key: i32) -> i16;
}

#[cfg(target_os = "windows")]
fn host_lock_on(vk: i32) -> Option<bool> {
    Some((unsafe { get_key_state(vk) } & 1) != 0)
}

#[cfg(not(target_os = "windows"))]
fn host_lock_on(_vk: i32) -> Option<bool> {
    None
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

/// The wgpu surface, device, queue, and surface config for the one window.
struct WgpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

/// Owns the winit window and the egui-on-wgpu plumbing. The GUI logic lives in
/// `GuiApp`; this struct routes raw winit events to it and drives the render.
struct WinitApp {
    gui: GuiApp,
    host_kbd: HostKeyboard,
    // Physical Ctrl state, tracked for the Ctrl+F2 capture toggle.
    ctrl_down: bool,
    window: Option<Arc<Window>>,
    wgpu: Option<WgpuState>,
    egui_ctx: egui::Context,
    egui_winit: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    // When the next frame is due. about_to_wait paces redraws to the guest
    // refresh rate with ControlFlow::WaitUntil rather than spinning at host vsync.
    next_frame: Instant,
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("IzarraVM")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        // Standard wgpu init for the surface, adapter, and device.
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("device");
        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats[0];
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let egui_winit = egui_winit::State::new(
            self.egui_ctx.clone(),
            self.egui_ctx.viewport_id(),
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);

        self.egui_renderer = Some(egui_renderer);
        self.egui_winit = Some(egui_winit);
        self.wgpu = Some(WgpuState {
            surface,
            device,
            queue,
            config,
        });
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Keyboard goes straight to the guest, never to egui (the GUI has no text
        // widgets). Handle it at the top so its borrows of self.gui/host_kbd/
        // ctrl_down stay disjoint from the window/egui/wgpu destructure below.
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
        {
            use winit::keyboard::KeyCode;
            if let PhysicalKey::Code(code) = key_event.physical_key {
                let pressed = key_event.state == ElementState::Pressed;
                if matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight) {
                    self.ctrl_down = pressed;
                }
                // Ctrl+F2 toggles input capture and is withheld from the guest.
                if pressed && code == KeyCode::F2 && self.ctrl_down {
                    if let Some(window) = self.window.clone() {
                        self.gui.toggle_capture(&window, &mut self.host_kbd);
                    }
                    return;
                }
                let codes = self.host_kbd.key(code, pressed, key_event.repeat);
                self.gui.send_keys_to_guest(codes);
            }
            return;
        }
        // On focus loss, release everything held so a key down at the moment of an
        // alt-tab (Shift, in a game) does not stick in the guest. This returns
        // before egui sees the event on purpose: focus loss is only a guest-key
        // flush, so unlike Focused(true) it is not forwarded to egui.
        if let WindowEvent::Focused(false) = &event {
            self.gui.send_keys_to_guest(self.host_kbd.release_all());
            self.ctrl_down = false;
            return;
        }
        // While captured, pointer buttons go to the guest and egui is skipped;
        // motion comes from DeviceEvent::MouseMotion instead. When not captured,
        // fall through so the sidebar and the click-to-capture still work.
        if self.gui.input_captured {
            if let WindowEvent::MouseInput { state, button, .. } = &event {
                let bit = match button {
                    MouseButton::Left => 0x01,
                    MouseButton::Right => 0x02,
                    MouseButton::Middle => 0x04,
                    _ => 0,
                };
                let pressed = *state == ElementState::Pressed;
                self.gui.set_guest_button(bit, pressed);
                return;
            }
            if matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseWheel { .. }
            ) {
                return;
            }
        }

        let (Some(window), Some(egui_winit), Some(wgpu), Some(renderer)) = (
            self.window.as_ref(),
            self.egui_winit.as_mut(),
            self.wgpu.as_mut(),
            self.egui_renderer.as_mut(),
        ) else {
            return;
        };

        let _ = egui_winit.on_window_event(window, &event);
        match event {
            WindowEvent::CloseRequested => {
                self.gui.shutdown_for_exit();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                wgpu.config.width = size.width.max(1);
                wgpu.config.height = size.height.max(1);
                wgpu.surface.configure(&wgpu.device, &wgpu.config);
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Clone the Context (Arc-backed, cheap) so the run() closure only
                // borrows self.gui, not self.egui_ctx as well.
                let egui_ctx = self.egui_ctx.clone();
                let raw_input = egui_winit.take_egui_input(window);
                let full = egui_ctx.run(raw_input, |ctx| self.gui.ui(ctx));
                egui_winit.handle_platform_output(window, full.platform_output);
                let tris = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
                let desc = egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [wgpu.config.width, wgpu.config.height],
                    pixels_per_point: full.pixels_per_point,
                };
                let mut encoder = wgpu.device.create_command_encoder(&Default::default());
                for (id, delta) in &full.textures_delta.set {
                    renderer.update_texture(&wgpu.device, &wgpu.queue, *id, delta);
                }
                renderer.update_buffers(&wgpu.device, &wgpu.queue, &mut encoder, &tris, &desc);
                let frame = match wgpu.surface.get_current_texture() {
                    Ok(f) => f,
                    // The surface changed or was lost: rebuild it and skip this
                    // frame; the next redraw draws to the fresh surface.
                    Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                        wgpu.surface.configure(&wgpu.device, &wgpu.config);
                        return;
                    }
                    // A transient timeout: just skip the frame, no reconfigure.
                    Err(wgpu::SurfaceError::Timeout) => return,
                    // Fatal: log and exit rather than spin on a dead surface.
                    Err(err @ (wgpu::SurfaceError::OutOfMemory | wgpu::SurfaceError::Other)) => {
                        error!(?err, "fatal surface error; exiting");
                        event_loop.exit();
                        return;
                    }
                };
                let view = frame.texture.create_view(&Default::default());
                {
                    let mut pass = encoder
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("egui"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        })
                        .forget_lifetime();
                    renderer.render(&mut pass, &tris, &desc);
                }
                for id in &full.textures_delta.free {
                    renderer.free_texture(id);
                }
                wgpu.queue.submit(Some(encoder.finish()));
                frame.present();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if !self.gui.input_captured {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event {
            let ppp = self
                .window
                .as_ref()
                .map_or(1.0, |w| w.scale_factor() as f32);
            self.gui
                .accumulate_guest_motion(delta.0 as f32, delta.1 as f32, ppp);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Enter capture if the monitor image was clicked this frame; the event
        // loop owns the winit Window that monitor_ui does not.
        if self.gui.take_want_capture() {
            if let Some(window) = &self.window {
                self.gui.toggle_capture(window, &mut self.host_kbd);
            }
        }
        // Pace redraws to the guest refresh rate. Only request a redraw once the
        // deadline has elapsed; requesting unconditionally would defeat the
        // WaitUntil below and spin the UI thread at host vsync.
        let now = Instant::now();
        if now >= self.next_frame {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            let hz = self.gui.guest_refresh_hz().max(1.0);
            self.next_frame = now + Duration::from_secs_f64(1.0 / hz);
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Open the window and run the emulator. Returns when the user closes it.
pub fn run(
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    cd_image: Option<PathBuf>,
    audio_enabled: bool,
    test_pattern: bool,
    rtc_setup: crate::cmos::RtcSetup,
) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let gui = GuiApp::new(
        profile,
        rom,
        c_drive,
        cd_image,
        audio_enabled,
        test_pattern,
        rtc_setup,
    );
    let mut app = WinitApp {
        gui,
        host_kbd: HostKeyboard::default(),
        ctrl_down: false,
        window: None,
        wgpu: None,
        egui_ctx: egui::Context::default(),
        egui_winit: None,
        egui_renderer: None,
        next_frame: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_gain_is_cubic_and_clamped() {
        // Endpoints are exact: silence at 0, unity at full.
        assert_eq!(volume_gain(0.0), 0.0);
        assert_eq!(volume_gain(1.0), 1.0);
        // Halfway on the slider is 0.5^3 = 0.125 of linear gain.
        assert!((volume_gain(0.5) - 0.125).abs() < 1e-6);
        // 0.8 (the default) cubes to 0.512.
        assert!((volume_gain(0.8) - 0.512).abs() < 1e-6);
        // Out-of-range input is clamped before cubing.
        assert_eq!(volume_gain(-1.0), 0.0);
        assert_eq!(volume_gain(2.0), 1.0);
    }

    #[test]
    fn refill_credit_clamps_a_stall() {
        let clock = 266_000_000u64;
        let cap = clock / 20; // 50 ms of guest time
        // From empty, a normal ~15 ms slice yields its full wall-time worth.
        assert_eq!(
            refill_credit(0, 0.015, clock, cap),
            (0.015 * clock as f64) as i64
        );
        // A long stall is clamped to the cap, so the backlog is forgiven, not banked.
        assert_eq!(refill_credit(0, 0.5, clock, cap), cap as i64);
    }

    #[test]
    fn disk_overshoot_holds_the_guest() {
        let clock = 266_000_000u64;
        let cap = clock / 20;
        // A read that ran ~190 ms past its budget leaves credit deep in debt.
        let mut credit: i64 = -(clock as i64) / 5;
        // One short slice cannot lift it out of debt, so the guest's budget stays
        // zero: it waits in wall-clock time.
        credit = refill_credit(credit, 0.001, clock, cap);
        assert!(credit < 0);
        assert_eq!(credit.max(0) as u64, 0, "no budget while in disk debt");
        // After enough wall time the debt clears and the guest runs again.
        credit = refill_credit(credit, 0.5, clock, cap);
        assert!(credit > 0, "debt repaid once wall-clock catches up");
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
