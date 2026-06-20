use crate::prefs::{self, GuiPrefs};
use eframe::egui;
use izarravm_audio::{AudioPlayer, AudioSink};
use izarravm_core::GswMode;
use izarravm_dos::HostDrive;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{error, warn};

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
    match HostDrive::mount_c(&c_drive) {
        Ok(drive) => machine.mount_c_drive(drive),
        Err(err) => error!(%err, "failed to mount C: drive"),
    }
    // Bring the RTC online: load cmos.bin (or write defaults) and seed the clock
    // from the host time read on the main thread at startup.
    rtc_setup.apply(&mut machine);
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
        ));
        self.texture = None;
        self.frame_seq = 0;
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
        // Clicking the screen grabs the input: the OS cursor is confined and
        // hidden, and keyboard/mouse route to the guest until Ctrl+F2 releases.
        let response = ui.interact(rect, ui.id().with("monitor-capture"), egui::Sense::click());
        if response.clicked() && !self.input_captured {
            self.set_capture(ctx, true);
        }
    }

    /// Enter or leave input capture. While captured we lock and hide the OS cursor
    /// (winit Locked: pinned in place, cannot move on screen or leave the window)
    /// and route keyboard and mouse to the guest, which draws its own cursor.
    /// Ctrl+F2 releases. Locked delivers motion as raw relative deltas, which we
    /// accumulate into the guest cursor position (clamped to the screen), so there
    /// is nothing for the OS cursor to escape and no warp to fight.
    fn set_capture(&mut self, ctx: &egui::Context, capture: bool) {
        if self.input_captured == capture {
            return;
        }
        self.input_captured = capture;
        self.last_buttons = 0;
        if capture {
            // Start the guest cursor centred; raw motion accumulates from there.
            self.abs_x = MOUSE_GUEST_MAX_X as f32 / 2.0;
            self.abs_y = MOUSE_GUEST_MAX_Y as f32 / 2.0;
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        }
        self.title = capture_title(capture);
    }

    /// Drain the captured input out of `raw_input` before egui sees it, routing it
    /// to the guest instead. Runs from `raw_input_hook`, ahead of egui's own
    /// processing, so a captured TAB never reaches the sidebar focus traversal and
    /// the sidebar stays pointer-inert while the screen holds the grab.
    ///
    /// Ctrl+F2 is checked first and releases the grab without forwarding anything,
    /// so the combo never lands in the guest. Otherwise every keyboard and pointer
    /// event is consumed: keys become Set 1 scancodes, pointer motion becomes a
    /// relative guest-pixel delta scaled across the framebuffer rect, and the
    /// button mask is rebuilt from the pointer-button events.
    fn process_captured_raw_input(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Ctrl+F2 releases the grab. Handle it before anything else and forward
        // nothing this frame so the combo does not reach the guest.
        let release_combo = raw_input.modifiers.ctrl
            && raw_input.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::F2,
                        pressed: true,
                        ..
                    }
                )
            });
        if release_combo {
            self.set_capture(ctx, false);
            // Drop the captured keys, the F2 press, and any pointer events so none
            // of them slip through to egui or the guest on the release frame.
            raw_input.events.retain(|e| !is_captured_input_event(e));
            return;
        }

        // Collect Set 1 scancodes, raw relative mouse motion, and the button mask,
        // then strip every keyboard and pointer event so egui never acts on them
        // (TAB never moves sidebar focus; the sidebar stays pointer-inert).
        let mut codes: Vec<u8> = Vec::new();
        let mut buttons = self.last_buttons;
        let mut moved = false;
        let ppp = ctx.pixels_per_point().max(0.01);
        for event in &raw_input.events {
            match event {
                egui::Event::Key { key, pressed, .. } => {
                    if let Some(make) = egui_key_to_set1(*key) {
                        codes.push(if *pressed { make } else { make | 0x80 });
                    }
                }
                // Raw relative motion from the locked, hidden cursor (winit
                // DeviceEvent::MouseMotion, surfaced as MouseMoved). The cursor is
                // pinned and cannot leave, so we accumulate the deltas into the guest
                // position ourselves, scaled so a full sweep across the video-output
                // rect covers the full guest range, and clamped to the screen.
                egui::Event::MouseMoved(delta) => {
                    if let Some(rect) = self.screen_rect {
                        let sx = MOUSE_GUEST_MAX_X as f32 / (rect.width() * ppp).max(1.0);
                        let sy = MOUSE_GUEST_MAX_Y as f32 / (rect.height() * ppp).max(1.0);
                        self.abs_x =
                            (self.abs_x + delta.x * sx).clamp(0.0, MOUSE_GUEST_MAX_X as f32);
                        self.abs_y =
                            (self.abs_y + delta.y * sy).clamp(0.0, MOUSE_GUEST_MAX_Y as f32);
                        moved = true;
                    }
                }
                egui::Event::PointerButton {
                    button, pressed, ..
                } => {
                    let bit = match button {
                        egui::PointerButton::Primary => 0x01,
                        egui::PointerButton::Secondary => 0x02,
                        egui::PointerButton::Middle => 0x04,
                        _ => 0,
                    };
                    if *pressed {
                        buttons |= bit;
                    } else {
                        buttons &= !bit;
                    }
                }
                _ => {}
            }
        }
        raw_input.events.retain(|e| !is_captured_input_event(e));

        if !codes.is_empty() {
            if let Some(emu) = &self.emu {
                emu.send_keys(codes);
            }
        }

        // Send the accumulated absolute guest position when it or the buttons
        // changed. The cursor is locked (pinned and hidden) so there is nothing to
        // confine or warp: the guest cursor is purely the accumulated motion clamped
        // to the screen, and the BIOS menus read it via INT 33h.
        if moved || buttons != self.last_buttons {
            self.last_buttons = buttons;
            let gx = self.abs_x.round() as i32;
            let gy = self.abs_y.round() as i32;
            if let Some(emu) = &self.emu {
                emu.send_mouse_absolute(gx, gy, buttons);
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

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the title in sync with the capture state every frame, so a
        // capture toggle is reflected even if set_capture ran mid-frame.
        self.title = capture_title(self.input_captured);
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
        // While captured, all keyboard and pointer input was already drained and
        // routed to the guest in raw_input_hook, ahead of egui's processing. Only
        // the non-captured path runs here: forward keys to the guest when no egui
        // widget wants the keyboard, otherwise yield so the user can type in the UI.
        if !self.input_captured && !ctx.wants_keyboard_input() {
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
        // The COM1 console floats over the central panel when toggled open.
        if self.show_com1 {
            self.com1_window(ctx);
        }
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

    /// Intercept input before egui processes the frame. While captured this strips
    /// the keyboard and pointer events out of the raw input and routes them to the
    /// guest, so egui's focus traversal (TAB) and the sidebar never see them and
    /// input stays trapped in the screen until Ctrl+F2 releases it.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.input_captured {
            self.process_captured_raw_input(ctx, raw_input);
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

/// Is this an input event the capture path swallows so egui never sees it?
/// Covers keyboard and every pointer event; everything else (window focus,
/// screenshots, paste, and so on) is left for egui to handle.
fn is_captured_input_event(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::PointerMoved(_)
            | egui::Event::MouseMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Zoom(_)
            | egui::Event::Touch { .. }
    )
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
    cd_image: Option<PathBuf>,
    audio_enabled: bool,
    test_pattern: bool,
    rtc_setup: crate::cmos::RtcSetup,
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
                cd_image,
                audio_enabled,
                test_pattern,
                rtc_setup,
            )))
        }),
    )?;
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
