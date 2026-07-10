#[path = "gui_runtime.rs"]
mod runtime;

pub use runtime::run;

use crate::prefs::{self, CrtStyle, GuiPrefs, KeyBinding};
use izarravm_audio::{AudioPlayer, AudioSink};
use izarravm_core::{GswMode, TimingClass};
use izarravm_input::HostKeyboard;
use izarravm_machine::{ActiveDisplay, Machine, MachineProfile, StopReason, VRETRACE_PEEK_CLOCKS};
use std::cell::Cell;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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

/// The ReSonique 2 output amp gain as a linear multiplier, from the config's
/// tenths encoding (30 -> 3.0). Models the card's analog output stage (line
/// driver / power amp) that the digital mixer model does not represent: a game
/// like Doom that never programs the CT1745 volume runs on the power-on default
/// (master and voice both -14 dB), so its digitized voice path lands at -28 dB
/// and is inaudible played straight out of a host DAC with no analog gain. The
/// user tunes it from the config menu; it is folded into the shared master gain.
fn amp_multiplier(amp_gain: u32) -> f32 {
    amp_gain as f32 / 10.0
}

/// The PC speaker volume as a linear gain, from the config's percent (100 -> 1.0,
/// 0 -> muted). Applied host-side to the speaker only, independent of the card amp.
fn speaker_multiplier(pc_speaker_volume: u32) -> f32 {
    pc_speaker_volume as f32 / 100.0
}

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

/// Ceiling on how often accumulated mouse motion is flushed into the guest,
/// independent of (and generally faster than) the video refresh rate that
/// paces rendering. A real PS/2 mouse samples at well under this; it just
/// keeps a violent host flick's motion arriving in small, frequent packets
/// rather than one huge coalesced delta that the guest can only convey as a
/// long train of catch-up packets (see `Machine::inject_mouse_relative`).
///
/// Must stay below the keyboard controller's own drain rate or the aux queue
/// grows without bound under sustained motion even though no single flush is
/// ever large: `AUX_BYTE_SETTLE_US` (keyboard.rs) paces aux bytes out of the
/// 8042 at 1/ms, and a TOKAMOUS-driven IntelliMouse packet is 4 bytes, so the
/// guest can never drain faster than 250 packets/s. 200 Hz matches the highest
/// standard PS/2 sample rate while leaving room for the aux byte pacing.
const MOUSE_FLUSH_HZ: f64 = 200.0;

/// How long a drive-access LED stays lit after the last access, so a burst of
/// fast reads reads as a steady glow rather than an imperceptible flicker.
const LED_GLOW: Duration = Duration::from_millis(150);

/// The beige front-panel palette. One warm-beige family, dark-brown ink, and
/// the LED greens. Shared by the panel, the drive bays, and the config modal so
/// the whole interface reads as one moulded plastic face.
const PANEL_FACE: egui::Color32 = egui::Color32::from_rgb(0xCD, 0xC3, 0xA4);
const FACEPLATE: egui::Color32 = egui::Color32::from_rgb(0xC4, 0xBA, 0x99);
const BEVEL_HI: egui::Color32 = egui::Color32::from_rgb(0xDE, 0xD6, 0xBD);
const BEVEL_LO: egui::Color32 = egui::Color32::from_rgb(0x9B, 0x91, 0x76);
const RECESS: egui::Color32 = egui::Color32::from_rgb(0x22, 0x1F, 0x18);
const INK: egui::Color32 = egui::Color32::from_rgb(0x4A, 0x43, 0x32);
const LABEL: egui::Color32 = egui::Color32::from_rgb(0x6B, 0x62, 0x48);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x5C, 0x53, 0x40);
const LED_ON: egui::Color32 = egui::Color32::from_rgb(0x46, 0xE0, 0x5A);
const LED_OFF: egui::Color32 = egui::Color32::from_rgb(0x2D, 0x4A, 0x2E);
/// The Izarra 3000 logo's red, sampled from the wordmark. Used for the floating
/// window headers so they read as branded and contrast on the beige frame.
const LOGO_RED: egui::Color32 = egui::Color32::from_rgb(0xC7, 0x44, 0x46);
/// A darker blue for hyperlinks, legible on the beige panel (egui's default
/// link blue is too light against it).
const LINK_BLUE: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x47, 0xA1);

/// The panel face as f32 RGB, for the logo recolor unmix target.
const PANEL_FACE_F32: [f32; 3] = [205.0, 195.0, 164.0];

const GITHUB_URL: &str = "https://github.com/vorvek/IzarraVM";

/// The embedded logo as pre-decoded straight RGBA (off-white background). It is
/// recoloured to the panel beige at load. Regenerate with the PowerShell recipe
/// in the design doc if the source art changes.
const LOGO_RGBA: &[u8] = include_bytes!("../assets/izarra3000_logo.rgba");
const LOGO_W: usize = 94;
const LOGO_H: usize = 53;
/// The embedded blob must be exactly LOGO_W x LOGO_H RGBA, or building the
/// texture would panic. This catches a wrongly regenerated asset at compile time.
const _: () = assert!(LOGO_RGBA.len() == LOGO_W * LOGO_H * 4);
/// The source PNG's flat background colour, the unmix origin.
const LOGO_BG_F32: [f32; 3] = [236.0, 230.0, 223.0];

/// Pack 0x00RRGGBB words into a tightly-packed opaque RGBA8 buffer for upload.
fn words_to_rgba(words: &[u32], width: usize, height: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for (i, &color) in words.iter().enumerate().take(width * height) {
        let o = i * 4;
        rgba[o] = ((color >> 16) & 0xff) as u8;
        rgba[o + 1] = ((color >> 8) & 0xff) as u8;
        rgba[o + 2] = (color & 0xff) as u8;
        rgba[o + 3] = 0xff;
    }
    rgba
}

/// Map palette-indexed pixels (mode 13h, the VGA raster core) to 0x00RRGGBB words.
fn palette_words(pixels: &[u8], palette: &[u32; 256]) -> Vec<u32> {
    pixels.iter().map(|&i| palette[i as usize]).collect()
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

/// Result of `top_up_shortfall`: `topped_up` is device+master-clock time created
/// without executing instructions (wall-backed by construction); `peeked` is
/// REAL executed guest time the vretrace peeks ran. The caller drains credit for
/// both, folding `peeked` into its executed-clock accounting.
struct TopUp {
    topped_up: u64,
    peeked: u64,
}

/// Consume an Approximate-class wall-pacing shortfall, edge-aware.
///
/// advance_wall_shortfall stops at each VGA vertical-retrace start edge inside
/// the span (a bare top-up sweeps the beam across a whole mode-13h frame with
/// zero instructions executing, so a guest polling 0x3DA caught only 12.8
/// percent of the windows), and the CPU gets a small peek quantum at every stop
/// so the window is observable. The slice may thus consume slightly more than
/// its budget (at most a few peeks' worth), which the credit bucket absorbs.
/// This applies in text modes too: uniform behavior, and a non-polling guest
/// pays only ~70 edge-stops + peeks per wall second (~0.2 percent of a 486
/// slice).
///
/// Termination: advance_wall_shortfall consumes >= 1 clock per call, and a
/// defensive stop cap bounds the peek work. Normal VGA frames are 60-70 Hz, so
/// a slice's shortfall (<= 50ms of clocks) sees a handful of edges; the cap
/// budgets one stop per 10ms of shortfall plus slack, and a pathological
/// guest-programmed CRTC (kHz-rate frames) then degrades to an unclamped
/// top-up instead of livelocking the emulate thread in peeks. A peek that
/// halts or faults aborts the top-up; the unrun remainder stays as credit for
/// the next slice, which sees the new machine state per its own stop handling.
fn top_up_shortfall(machine: &mut Machine, mut remaining: u64) -> TopUp {
    let clock_hz = machine.active_mode().clock_hz();
    let mut stops_left = remaining / (clock_hz / 100).max(1) + 2;
    let mut topped_up = 0u64;
    let mut peeked = 0u64;
    while remaining > 0 {
        let consumed = machine.advance_wall_shortfall(remaining);
        topped_up += consumed;
        remaining = remaining.saturating_sub(consumed.max(1));
        if remaining == 0 {
            break;
        }
        if stops_left == 0 {
            machine.advance_wall_clocks(remaining);
            topped_up += remaining;
            break;
        }
        stops_left -= 1;
        // Stopped at a vretrace start edge (bit 3 of 0x3DA already reads set):
        // let the guest run a peek so a polling loop observes the window before
        // the top-up sweeps past it.
        let peek_before = machine.elapsed_clocks();
        let peek_stop = tick_machine(machine, VRETRACE_PEEK_CLOCKS);
        peeked += machine.elapsed_clocks().saturating_sub(peek_before);
        if peek_stop.is_some() {
            break;
        }
    }
    TopUp { topped_up, peeked }
}

/// The active display as native-resolution 0x00RRGGBB words plus its size.
fn render_words(machine: &mut Machine) -> (Vec<u32>, usize, usize) {
    match machine.active_display() {
        ActiveDisplay::VgaRaster => {
            let palette = machine.palette_argb();
            match machine.vga_raster() {
                Some(raster) => {
                    // Present only the active visible region. `height` is the full
                    // beam frame (vtotal) including the vertical retrace/border the
                    // monitor never shows; cropping to the top `display_height`
                    // (vdisp_end) rows is what makes the aspect-fill correct — it
                    // drops the black bottom bar a 320x200 mode would otherwise bake
                    // into the stretched image.
                    let w = raster.width as usize;
                    let h = if raster.display_height == 0 {
                        raster.height as usize
                    } else {
                        raster.display_height as usize
                    };
                    let visible = &raster.pixels[..(w * h).min(raster.pixels.len())];
                    (palette_words(visible, &palette), w, h)
                }
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
/// is queued, independent of the guest's own CT1745 mixer. `amp` is the ReSonique
/// 2 analog output-stage gain; the machine applies it to the card's own sources
/// (not the PC speaker) inside `render_audio`, so it is set on the machine here
/// rather than multiplied into every sample.
fn pump_audio(
    machine: &mut Machine,
    sink: &AudioSink,
    wall_dt: f64,
    debt: &mut f64,
    gain: f32,
    amp: f32,
    speaker_vol: f32,
) {
    machine.set_card_amp(amp);
    machine.set_speaker_volume(speaker_vol);
    // Produce audio for the WALL time elapsed, not the guest clocks elapsed. The
    // sound device consumes at real time, so tying production to guest execution
    // meant a below-real-time guest (a 386 at 22 MHz, or a 486/586 the host cannot
    // emulate at full speed) fed the ring slower than it drained -> starvation ->
    // crackle. A real OPL/DAC runs on its own clock regardless of CPU speed: the
    // music just changes notes more slowly. Driving synthesis at real time
    // reproduces that (smooth audio, slower tempo) and keeps the ring fed. When the
    // guest DOES keep up, wall time and guest time advance together, so this is
    // identical to the old behavior. The SB DMA ring is still filled at the guest
    // rate, so effects degrade to gaps (not crackle) rather than being stretched.
    *debt += wall_dt * OPL_NATIVE_HZ;
    let mut samples = debt.floor() as usize;
    *debt -= samples as f64;
    // A long host hitch could ask for a huge render in one call. Cap it at roughly
    // the sink's buffer (~0.5 s); the surplus would only be dropped at the queue.
    let max_samples = OPL_NATIVE_HZ as usize / 2;
    if samples > max_samples {
        samples = max_samples;
        *debt = 0.0;
    }
    if samples == 0 {
        return;
    }
    // The card amp was applied inside render_audio (card sources only); `gain` here
    // is just the host master volume, applied to the whole mix (card + speaker).
    let mut pcm = machine.render_audio(samples);
    for (l, r) in &mut pcm {
        *l = (*l as f32 * gain)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        *r = (*r as f32 * gain)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
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

/// UI-to-emulation-thread messages.
enum Command {
    Keys(Vec<u8>),
    /// A coalesced frame of relative mouse motion (raw mickey counts) plus the
    /// button mask. The guest driver applies its mickey ratio and clamps the cursor
    /// to the active video mode's range; the host just forwards the counts, so the
    /// cursor is never confined to a stale virtual range. Capture only.
    MouseRelative(i32, i32, u8),
    /// One scroll-wheel detent from the host, forwarded to the emulated mouse.
    /// Positive is scroll-up, negative is scroll-down. Capture only.
    MouseWheel(i32),
    /// Mount a floppy image into drive A: live. `flush_path` is the source IMG to
    /// rewrite a dirty image to on eject.
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

/// Apply the beige theme to a ui subtree: dark ink text and faceplate-coloured
/// widgets with bevel-toned borders, so standard egui buttons, sliders, and
/// selectable labels inside it read as plastic without bespoke widgets.
fn beige_visuals(ui: &mut egui::Ui) {
    let v = ui.visuals_mut();
    v.override_text_color = Some(INK);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.bg_stroke = egui::Stroke::new(1.0, BEVEL_LO);
        w.fg_stroke = egui::Stroke::new(1.0, INK);
    }
    v.widgets.inactive.bg_fill = FACEPLATE;
    v.widgets.inactive.weak_bg_fill = FACEPLATE;
    v.widgets.hovered.bg_fill = BEVEL_HI;
    v.widgets.hovered.weak_bg_fill = BEVEL_HI;
    v.widgets.active.bg_fill = BEVEL_LO;
    v.widgets.active.weak_bg_fill = BEVEL_LO;
    // A pressed segmented control reads as recessed.
    v.selection.bg_fill = BEVEL_LO;
    v.selection.stroke = egui::Stroke::new(1.0, INK);
}

/// Draw the four bevel edges over `rect`: highlight on the top and left, shadow
/// on the bottom and right (raised), or swapped (recessed). The fill is drawn
/// separately by the caller (a Frame or `rect_filled`).
fn bevel_edges(painter: &egui::Painter, rect: egui::Rect, raised: bool) {
    let (hi, lo) = if raised {
        (BEVEL_HI, BEVEL_LO)
    } else {
        (BEVEL_LO, BEVEL_HI)
    };
    let top = egui::Stroke::new(1.0, hi);
    let bot = egui::Stroke::new(1.0, lo);
    painter.line_segment([rect.left_top(), rect.right_top()], top);
    painter.line_segment([rect.left_top(), rect.left_bottom()], top);
    painter.line_segment([rect.left_bottom(), rect.right_bottom()], bot);
    painter.line_segment([rect.right_top(), rect.right_bottom()], bot);
}

/// Fill `rect` and bevel it in one call, for slots and standalone plates.
fn bevel_rect(painter: &egui::Painter, rect: egui::Rect, fill: egui::Color32, raised: bool) {
    painter.rect_filled(rect, 2.0, fill);
    bevel_edges(painter, rect, raised);
}

/// A raised beige faceplate wrapping `add`, bevelled on all four edges.
fn beige_group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let res = egui::Frame::new()
        .fill(FACEPLATE)
        .inner_margin(egui::Margin::same(9))
        .corner_radius(2.0)
        .show(ui, |ui| {
            beige_visuals(ui);
            add(ui)
        });
    bevel_edges(ui.painter(), res.response.rect, true);
    res.inner
}

/// The shared red, bold header style for dialog and floating-window titles, so
/// the brand red lives in one place (window titles and the config header).
fn header_text(text: &str, size: f32) -> egui::RichText {
    egui::RichText::new(text)
        .color(LOGO_RED)
        .strong()
        .size(size)
}

/// The shared beige look for IzarraVM's floating windows (COM1, About,
/// License): PANEL_FACE fill, a dark-beige border, beige inner padding, a bold
/// logo-red header, no collapse button, draggable + closable. The caller
/// supplies the title, the open flag (the window's own close control flips it),
/// whether the window shows a resize grip, a default size, and the body.
fn beige_window(
    ctx: &egui::Context,
    title: &str,
    open: &mut bool,
    resizable: bool,
    default_size: [f32; 2],
    add: impl FnOnce(&mut egui::Ui),
) {
    // egui paints the title bar (title text + close button) from the global
    // style before the body runs, so darken the interactive glyphs (the close
    // X) to read on the beige frame here, then restore. The title text itself
    // is a bold logo-red RichText below.
    let saved_widgets = ctx.style().visuals.widgets.clone();
    ctx.style_mut(|s| {
        s.visuals.widgets.inactive.fg_stroke.color = INK;
        s.visuals.widgets.hovered.fg_stroke.color = INK;
        s.visuals.widgets.active.fg_stroke.color = INK;
        s.visuals.widgets.hovered.weak_bg_fill = BEVEL_HI;
        s.visuals.widgets.active.weak_bg_fill = BEVEL_LO;
    });
    egui::Window::new(header_text(title, 15.0))
        .open(open)
        .resizable(resizable)
        .collapsible(false)
        .default_size(default_size)
        .frame(
            egui::Frame::new()
                .fill(PANEL_FACE)
                .stroke(egui::Stroke::new(1.5, BEVEL_LO))
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 12,
                    bottom: 12,
                })
                .corner_radius(4.0),
        )
        .show(ctx, |ui| {
            beige_visuals(ui);
            add(ui);
        });
    ctx.style_mut(|s| {
        s.visuals.widgets = saved_widgets;
    });
}

/// A small painted "i in a circle" info-icon button, since the default font
/// lacks the U+1F6C8 glyph. Matches the adjacent buttons' footprint; returns
/// the click response so callers can add hover text and handle clicks.
fn info_button(ui: &mut egui::Ui) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    let resp = ui.add_sized([h, h], egui::Button::new(""));
    let rect = resp.rect;
    let c = rect.center();
    let r = (h * 0.32).round();
    let stroke = egui::Stroke::new(1.5, INK);
    let p = ui.painter();
    p.circle_stroke(c, r, stroke);
    // The dot and stem of the lowercase "i".
    p.circle_filled(c - egui::vec2(0.0, r * 0.45), 1.1, INK);
    p.line_segment(
        [c - egui::vec2(0.0, r * 0.05), c + egui::vec2(0.0, r * 0.5)],
        stroke,
    );
    resp
}

/// Render multi-line attribution text, turning any embedded http(s) URL into a
/// clickable hyperlink (link color comes from the ui's `hyperlink_color`). One
/// label per source line so each stays on its own line in a wide-enough window
/// and centers cleanly in a centered layout; keeps the NOTICE file as the
/// single source of truth.
fn notice_block(ui: &mut egui::Ui, text: &str, color: egui::Color32, size: f32) {
    ui.spacing_mut().item_spacing.y = 1.0;
    for line in text.lines() {
        let Some(start) = line.find("http") else {
            ui.label(egui::RichText::new(line).color(color).size(size));
            continue;
        };
        // The URL runs until whitespace or a closing paren.
        let len = line[start..]
            .find(|c: char| c.is_whitespace() || c == ')')
            .unwrap_or(line.len() - start);
        let (url, before, after) = (
            &line[start..start + len],
            &line[..start],
            &line[start + len..],
        );
        // A plain horizontal takes the full width and left-biases in a centered
        // layout, so measure the line and allocate a row exactly that wide; the
        // centered layout then centers the whole row.
        let mut row = ui.fonts(|f| {
            f.layout_no_wrap(
                format!("{before}{url}{after}"),
                egui::FontId::proportional(size),
                color,
            )
            .size()
        });
        row.x += 2.0;
        ui.allocate_ui_with_layout(
            row,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if !before.is_empty() {
                    ui.label(egui::RichText::new(before).color(color).size(size));
                }
                ui.hyperlink_to(egui::RichText::new(url).size(size), url);
                if !after.is_empty() {
                    ui.label(egui::RichText::new(after).color(color).size(size));
                }
            },
        );
    }
}

/// A small square drive-activity LED.
fn activity_led(ui: &mut egui::Ui, lit: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    let color = if lit { LED_ON } else { LED_OFF };
    ui.painter().rect_filled(rect, 1.0, color);
    ui.painter().rect_stroke(
        rect,
        1.0,
        egui::Stroke::new(0.5, BEVEL_LO),
        egui::StrokeKind::Inside,
    );
}

/// A physical eject button (up-triangle over a bar). Returns true on a click
/// while `enabled`. Painted, so it keeps the plastic look the egui button theme
/// cannot give a tiny glyph.
fn eject_button(ui: &mut egui::Ui, enabled: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 18.0), egui::Sense::click());
    bevel_rect(ui.painter(), rect, FACEPLATE, true);
    let c = rect.center();
    let col = if enabled { INK } else { BEVEL_LO };
    let tri = vec![
        c + egui::vec2(0.0, -3.5),
        c + egui::vec2(-4.0, 1.5),
        c + egui::vec2(4.0, 1.5),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(tri, col, egui::Stroke::NONE));
    ui.painter().line_segment(
        [c + egui::vec2(-4.0, 4.0), c + egui::vec2(4.0, 4.0)],
        egui::Stroke::new(1.5, col),
    );
    enabled && resp.clicked()
}

/// A small speaker icon (back box, flared cone, and two sound waves) drawn at
/// the left of the volume row in place of a text label.
fn volume_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 14.0), egui::Sense::hover());
    let cy = rect.center().y;
    let left = rect.left();
    // Speaker back box.
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(left + 1.0, cy - 3.0),
            egui::pos2(left + 6.0, cy + 3.0),
        ),
        0.0,
        LABEL,
    );
    // Speaker cone, a trapezoid flaring to the right.
    let cone = vec![
        egui::pos2(left + 6.0, cy - 3.0),
        egui::pos2(left + 12.0, cy - 6.0),
        egui::pos2(left + 12.0, cy + 6.0),
        egui::pos2(left + 6.0, cy + 3.0),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(cone, LABEL, egui::Stroke::NONE));
    // Two sound-wave chevrons to the right.
    let stroke = egui::Stroke::new(1.2, LABEL);
    ui.painter().line_segment(
        [
            egui::pos2(left + 14.0, cy - 2.5),
            egui::pos2(left + 15.5, cy),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(left + 15.5, cy),
            egui::pos2(left + 14.0, cy + 2.5),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(left + 16.5, cy - 4.0),
            egui::pos2(left + 18.5, cy),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(left + 18.5, cy),
            egui::pos2(left + 16.5, cy + 4.0),
        ],
        stroke,
    );
}

/// Recolour the logo's flat off-white background to `beige` with a per-pixel
/// unmix. For each pixel, `w` is how much of it is background
/// (`min(r/bg, g/bg, b/bg)`, clamped); the pixel is shifted by `w * (beige -
/// bg)`. Pure background maps exactly to beige, ink stays ink, and the
/// anti-aliased edges blend into beige with no halo. Alpha is preserved.
fn recolor_logo(raw: &[u8], beige: [f32; 3]) -> Vec<u8> {
    let bg = LOGO_BG_F32;
    let mut out = vec![0u8; raw.len()];
    for (src, dst) in raw.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let p = [src[0] as f32, src[1] as f32, src[2] as f32];
        let w = (p[0] / bg[0])
            .min(p[1] / bg[1])
            .min(p[2] / bg[2])
            .clamp(0.0, 1.0);
        for c in 0..3 {
            let v = (p[c] + w * (beige[c] - bg[c])).round().clamp(0.0, 255.0);
            dst[c] = v as u8;
        }
        dst[3] = src[3];
    }
    out
}

/// Rasterize a solid five-pointed star into `size` x `size` straight RGBA,
/// `color` inside and transparent outside. The classic star uses an inner /
/// outer radius ratio of 0.382, with the top point up.
fn render_star_icon(size: u32, color: [u8; 3]) -> Vec<u8> {
    let n = size as f32;
    let (cx, cy) = (n / 2.0, n / 2.0);
    let ro = n * 0.46;
    let ri = ro * 0.382;
    let mut pts = Vec::with_capacity(10);
    for k in 0..5 {
        let ao = (-90.0 + k as f32 * 72.0).to_radians();
        pts.push((cx + ro * ao.cos(), cy + ro * ao.sin()));
        let ai = (-90.0 + 36.0 + k as f32 * 72.0).to_radians();
        pts.push((cx + ri * ai.cos(), cy + ri * ai.sin()));
    }
    let inside = |px: f32, py: f32| -> bool {
        // Ray-casting point-in-polygon, valid for this concave star.
        let mut hit = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) {
                let x_cross = (xj - xi) * (py - yi) / (yj - yi) + xi;
                if px < x_cross {
                    hit = !hit;
                }
            }
            j = i;
        }
        hit
    };
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            if inside(x as f32 + 0.5, y as f32 + 0.5) {
                let o = ((y * size + x) * 4) as usize;
                rgba[o] = color[0];
                rgba[o + 1] = color[1];
                rgba[o + 2] = color[2];
                rgba[o + 3] = 0xFF;
            }
        }
    }
    rgba
}

/// Build the winit window icon: a brand-red star. Logged and dropped on the
/// rare `BadIcon`, so a bad buffer never blocks the window.
fn star_window_icon() -> Option<winit::window::Icon> {
    let size = 64u32;
    let rgba = render_star_icon(size, [0xC7, 0x44, 0x46]);
    match winit::window::Icon::from_rgba(rgba, size, size) {
        Ok(icon) => Some(icon),
        Err(err) => {
            warn!(%err, "could not build the window icon");
            None
        }
    }
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
        amp: SharedGain,
        speaker_vol: SharedGain,
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
                    amp,
                    speaker_vol,
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

    fn send_mouse_relative(&self, dx: i32, dy: i32, buttons: u8) {
        let _ = self.commands.send(Command::MouseRelative(dx, dy, buttons));
    }

    fn send_mouse_wheel(&self, dz: i32) {
        let _ = self.commands.send(Command::MouseWheel(dz));
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

/// Eject the A: floppy, writing a dirty image back to its source IMG. A clean
/// image is ejected without touching the host. Clears the flush path so a
/// later eject does not rewrite a stale file.
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
    amp: SharedGain,
    speaker_vol: SharedGain,
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
    // Boot real FreeDOS from this host folder via Katea: the controller presents
    // the folder as a real ATA disk and the kernel does its own FAT / INT 21h.
    // mount_hdd_folder seeds the user-owned CONFIG.SYS/AUTOEXEC.BAT (which loads
    // TOKAMOUS and SET BLASTER) and overlays the OS binaries (TOKAMOUS.COM ships
    // on the payload), so the mouse and Sound Blaster work and the user owns the
    // config. "Repair Toka-DOS" in the BIOS setup menu resets it.
    if let Err(err) = machine.mount_hdd_folder(&c_drive) {
        error!(%err, "failed to mount C: host folder");
    }
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

    let mut audio_debt = 0.0;
    let mut speed_ratio = 0.0;
    // Pacing credit (guest clocks the guest is owed). Wall time refills it; the
    // cycles run drain it. A disk read that consumes more than its slice drives
    // it negative, pausing the guest for the disk's duration.
    let mut credit: i64 = 0;
    let mut last = Instant::now();
    let mut published_seq = u64::MAX; // force the first publish
    // Dirty-framebuffer cache (graphics modes only, v1): the content-generation key
    // of the last frame we palette-mapped + published. The guest's vsync counter
    // (`seq`) advances every retrace even on a totally static mode-13h screen, which
    // would re-run the 64 KB palette map (`render_words`) ~70x/s for nothing. When
    // `frame_generation()` returns `Some(k)` and k is unchanged, the graphics output
    // cannot have changed, so we skip the render + publish: `f.seq` stays put, so the
    // UI's existing per-seq texture-upload guard skips the upload too. `None` (text
    // mode / Margo / Distira) always renders, today's behavior (text-cursor blink).
    let mut last_frame_gen: Option<u64> = None;

    let cmos_path = rtc_setup.cmos_path.clone();
    // The source IMG path of the mounted floppy. A dirty image is flushed here
    // on eject and on shutdown.
    let mut floppy_flush_path: Option<PathBuf> = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Keys(codes)) => machine.inject_key_scancodes(&codes),
                Ok(Command::MouseRelative(dx, dy, buttons)) => {
                    machine.inject_mouse_relative(dx, dy, buttons)
                }
                Ok(Command::MouseWheel(dz)) => machine.inject_mouse_wheel(dz),
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
                    // Flush the Katea host folder, the floppy, and the final CMOS
                    // state before exiting (this arm also runs on Reset, which
                    // shuts the thread down and respawns).
                    machine.flush_hdd_folder();
                    flush_floppy(&mut machine, &mut floppy_flush_path);
                    crate::cmos::save_cmos_file(&cmos_path, &machine.cmos_bytes());
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Channel closed (the GUI dropped the sender on exit) — same
                    // flush sequence as Shutdown before the thread ends.
                    machine.flush_hdd_folder();
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
            let approximate = machine.active_mode().timing_class() == TimingClass::Approximate;
            let stop = if approximate {
                // Approximate class (486/586): time is already an approximation, so
                // sub-slice the budget in ~1ms quanta and stop issuing quanta once
                // the slice's wall deadline passes. On a host too slow to emulate
                // the mode at full speed, the CPU retires what it can and the unrun
                // remainder is topped up below, so audio/PIT hold realtime instead
                // of starving behind an ever-longer slice. The Accurate class
                // (286/386) keeps the single full-budget call: sub-slicing changes
                // device-advance boundaries and its cadence is frozen.
                let quantum = (clock_hz / 1000).max(1);
                let deadline = now + Duration::from_secs_f64(budget as f64 / clock_hz as f64);
                let mut stop = None;
                loop {
                    let ran_so_far = machine.elapsed_clocks().saturating_sub(before);
                    let remaining = budget.saturating_sub(ran_so_far);
                    if remaining == 0 {
                        break;
                    }
                    stop = tick_machine(&mut machine, quantum.min(remaining));
                    if stop.is_some() || Instant::now() >= deadline {
                        break;
                    }
                }
                stop
            } else {
                tick_machine(&mut machine, budget)
            };
            let mut ran = machine.elapsed_clocks().saturating_sub(before);
            // Of those clocks, some may be a device-I/O stall (a floppy seek/read)
            // that jumped the clock without executing instructions. Drain the full
            // ran from the credit so the stall still costs wall-clock time, but
            // exclude it from the speed measurement below.
            let mut stalled = machine.io_stall_clocks().saturating_sub(stall_before);
            let mut topped_up = 0u64;
            // A halted guest (POST done, nothing to boot) stops driving the video
            // beam, so the display would freeze on whatever half-drawn frame was
            // completing when HLT ran. Keep scanning the VGA so the final, complete
            // framebuffer is presented instead.
            if matches!(stop, Some(StopReason::Halted)) {
                machine.advance_devices_clocks(budget);
            } else if approximate {
                if stalled > 0 {
                    // A device I/O stall (floppy/CD seek) jumped the master clock
                    // without advancing the devices, so fill the device gap to match
                    // and audio keeps playing through a disk grind instead of the
                    // sink starving. The master clock is NOT bumped here: stall_for
                    // already advanced it by the stall, and re-adding it gave the
                    // audio pump a cumulative lead over wall time. The Accurate
                    // class (286/386) keeps exact main behavior (devices resume from
                    // the next instruction's own advance).
                    machine.advance_devices_clocks(stalled);
                }
                if stop.is_none() {
                    // The host ran out of wall time before the budget was consumed
                    // (the deadline bail-out above). Top up devices AND the master
                    // clock by the genuinely unrun remainder so guest time keeps
                    // tracking wall time; the shortfall is wall-backed by
                    // construction, so draining credit for it below is correct.
                    // Stall jumps already count as progress in `ran`, so they are
                    // not double-counted here. Fatal stops (CpuError/DosExit/
                    // TestExit) skip the top-up and keep devices frozen.
                    //
                    // The top-up stops at VGA vretrace start edges and peeks the
                    // CPU there (see top_up_shortfall). The peek is REAL executed
                    // guest time backed by the wall time spent running it, so it
                    // folds into `ran` and drains credit like any other execution.
                    let top_up = top_up_shortfall(&mut machine, budget.saturating_sub(ran));
                    topped_up += top_up.topped_up;
                    ran += top_up.peeked;
                    // A peek can itself hit a device-I/O stall (a guest kicking
                    // off a disk read at the edge): fill the device gap the same
                    // way the slice fill above did, and fold it into `stalled` so
                    // the speed metric keeps excluding intentional waits.
                    let stalled_total = machine.io_stall_clocks().saturating_sub(stall_before);
                    if stalled_total > stalled {
                        machine.advance_devices_clocks(stalled_total - stalled);
                        stalled = stalled_total;
                    }
                }
            }
            credit -= i64::try_from(ran.saturating_add(topped_up)).unwrap_or(i64::MAX);
            if let Some(sink) = &sink {
                pump_audio(
                    &mut machine,
                    sink,
                    dt,
                    &mut audio_debt,
                    gain.get(),
                    amp.get(),
                    speaker_vol.get(),
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
        // Dirty-framebuffer guard: in a graphics mode whose content key is unchanged
        // since the last published frame, the output is bit-identical, so skip the
        // palette map + publish even though `seq` advanced. `None` (text/Margo/
        // Distira) never short-circuits, preserving today's per-vsync render.
        let frame_gen = machine.frame_generation();
        let content_unchanged = matches!((frame_gen, last_frame_gen), (Some(k), Some(p)) if k == p);
        let new_frame = seq != published_seq && !content_unchanged;
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
                // Remember the published frame's content key so the next vsync with the
                // same key (static screen) is short-circuited above.
                last_frame_gen = frame_gen;
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

/// What is in drive A:, remembered so a Reset can remount the same media. A
/// reset flushes dirty guest writes back to the source IMG first, so the
/// re-read keeps them.
struct FloppySource(PathBuf);

/// Pick the floppy mount to restore from saved prefs, if its source still
/// exists. A path that has since been deleted or moved is skipped (the drive
/// starts empty).
fn restore_floppy_source(prefs: &GuiPrefs) -> Option<FloppySource> {
    let path = prefs.last_floppy_image.as_ref()?;
    path.is_file().then(|| FloppySource(path.clone()))
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
    // Raw relative mouse motion (mickeys) accumulated since the last frame flush
    // while captured. The guest driver owns the cursor position, range, and mickey
    // ratio, so the host only forwards these counts, coalesced once per frame.
    mouse_rel_x: f32,
    mouse_rel_y: f32,
    // Set on motion, cleared by the once-per-frame flush in about_to_wait.
    // An 8000 Hz mouse fires ~130 events per frame; sending one guest packet each
    // floods the emulation thread with guest IRQ12s and stalls the UI thread.
    mouse_dirty: bool,
    // Fractional scroll-wheel carry (trackpads/pixel-delta) so only whole detents
    // are forwarded to the guest. A full notch sends exactly one +/-1 wheel command.
    wheel_accum: f32,
    // The cpal stream is !Send, so it stays here on the UI thread; the
    // emulation thread gets a Send sink cloned from it.
    audio: Option<AudioPlayer>,
    emu: Option<Emulator>,
    // Guest frame counter of the texture currently uploaded, so we rebuild it
    // only when a new frame is presented rather than on every update().
    frame_seq: u64,
    // Host render rate, recomputed once a second and surfaced in the panel.
    metrics_mark: Option<Instant>,
    frames_since: u32,
    host_fps: f64,
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
    // Whether the floating COM1 window is open. The footer button and the
    // window's own close control both flip this.
    show_com1: bool,
    // Whether the floating About window is open. The footer info button and the
    // window's own close control both flip this.
    show_about: bool,
    // Whether the floating License (GPL-3.0) window is open. The About window's
    // "View license" button and the window's own close control flip this.
    show_license: bool,
    // Master volume slider position, 0.0..1.0. Cubed into a host-side gain that
    // the emulation thread reads through `gain`.
    volume: f32,
    // ReSonique 2 output amp gain, in tenths (120 = 12.0x). Edited in the config
    // modal, persisted in prefs. The multiplier form rides `amp` to the emu thread.
    amp_gain: u32,
    // PC speaker volume as a percent (100 = full, 0 = muted). Edited in the config
    // modal, persisted in prefs. The gain form rides `speaker_vol` to the emu thread.
    pc_speaker_volume: u32,
    // The shared master gain (curved volume slider), read each audio pump.
    gain: SharedGain,
    // The shared ReSonique 2 amp multiplier (amp_gain / 10), read each audio pump
    // and applied to the card's sources only (not the PC speaker). Separate atomic
    // from `gain` so the two stay lock-free and independently updatable.
    amp: SharedGain,
    // The shared PC speaker gain (pc_speaker_volume / 100), read each audio pump
    // and applied to the speaker only.
    speaker_vol: SharedGain,
    // CRT presentation style (off / subtle / Ye Olde). Persisted; read by
    // monitor_ui each frame and mapped to the shader's style uniform.
    crt_style: CrtStyle,
    // Live hotkeys for releasing captured input and toggling fullscreen. The
    // event loop matches physical keys against these; the config dialog edits
    // staged copies and writes them back on Accept.
    input_release: KeyBinding,
    fullscreen_key: KeyBinding,
    // The configuration modal, when open. Holds a staged copy of the settings it
    // edits so Cancel discards and Accept applies.
    config_dialog: Option<ConfigDialog>,
    // Persisted GUI prefs (volume, last mounts) and where they live on disk. The
    // file sits next to the C: root and is rewritten on a change.
    prefs: GuiPrefs,
    prefs_path: PathBuf,
    // Whether the beige control panel is expanded. Mirrors prefs.panel_open and
    // is persisted on toggle.
    panel_open: bool,
    // The recoloured logo texture, loaded once on the first frame.
    logo: Option<egui::TextureHandle>,
}

/// Which hotkey the config dialog is currently waiting to capture.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BindTarget {
    InputRelease,
    Fullscreen,
}

/// Staged settings edited by the configuration modal. Seeded from the live
/// values when opened; applied on Accept, discarded on Cancel.
struct ConfigDialog {
    input_release: KeyBinding,
    fullscreen: KeyBinding,
    crt_style: CrtStyle,
    // ReSonique 2 amp gain in tenths (120 = 12.0x); see GuiApp::amp_gain.
    amp_gain: u32,
    // PC speaker volume percent (100 = full, 0 = muted); see GuiApp::pc_speaker_volume.
    pc_speaker_volume: u32,
    // The binding awaiting a key press, set when the user clicks a rebind button.
    capturing: Option<BindTarget>,
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
        let amp_gain = prefs.amp_gain;
        let crt_style = prefs.crt_style;
        let input_release = prefs.input_release.clone();
        let fullscreen_key = prefs.fullscreen.clone();
        let gain = SharedGain::new(volume_gain(volume));
        let amp = SharedGain::new(amp_multiplier(amp_gain));
        let pc_speaker_volume = prefs.pc_speaker_volume;
        let speaker_vol = SharedGain::new(speaker_multiplier(pc_speaker_volume));
        // Restore the last floppy mount if the source still exists on disk.
        let floppy_source = restore_floppy_source(&prefs);
        let panel_open = prefs.panel_open;
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
            mouse_rel_x: 0.0,
            mouse_rel_y: 0.0,
            mouse_dirty: false,
            wheel_accum: 0.0,
            audio,
            emu: None,
            frame_seq: u64::MAX,
            metrics_mark: None,
            frames_since: 0,
            host_fps: 0.0,
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
            show_about: false,
            show_license: false,
            volume,
            amp_gain,
            pc_speaker_volume,
            gain,
            amp,
            speaker_vol,
            crt_style,
            input_release,
            fullscreen_key,
            config_dialog: None,
            prefs,
            prefs_path,
            panel_open,
            logo: None,
        };
        app.start();
        // Mount a CD once the emulation thread is up. An explicit config-file
        // cd_image wins; otherwise fall back to whichever remembered prefs
        // mount is set (an ISO/CUE path or a folder), preferring the image if
        // both are somehow set since the two are meant to be mutually
        // exclusive. A remembered source that no longer exists on disk is
        // silently skipped (like the floppy restore), leaving the drive empty.
        if let Some(path) = cd_image {
            app.mount_cd_from_path(&path);
        } else if let Some(path) = app
            .prefs
            .last_cd_image
            .clone()
            .filter(|path| path.is_file())
        {
            app.mount_cd_from_path(&path);
        } else if let Some(dir) = app.prefs.last_cd_folder.clone().filter(|dir| dir.is_dir()) {
            app.mount_cd_from_folder(&dir);
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
            self.amp.clone(),
            self.speaker_vol.clone(),
        ));
        self.frame_seq = u64::MAX;
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
        self.frame_seq = u64::MAX;
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
    fn monitor_ui(&mut self, ui: &mut egui::Ui) {
        let rect = fit_4_3(ui.max_rect());
        // Record the image rect so the capture path can scale host pointer motion
        // across it into guest pixels.
        self.screen_rect = Some(rect);
        let Some(emu) = &self.emu else {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            return;
        };
        // Pull a fresh framebuffer only when the guest frame counter advanced;
        // otherwise the persistent GPU texture is reused. The lock is held only
        // for the copy.
        let frame = {
            let f = emu.frame.lock().expect("frame snapshot poisoned");
            if f.width > 0 && f.seq != self.frame_seq {
                self.frame_seq = f.seq;
                Some(crate::crt::CrtFrame {
                    rgba: words_to_rgba(&f.words, f.width, f.height),
                    width: f.width as u32,
                    height: f.height as u32,
                })
            } else {
                None
            }
        };
        // Paint the guest screen through the wgpu shader pass: aspect-fill to the
        // 4:3 rect, sharp upscale, and the CRT model for the chosen style. The Ye
        // Olde grain animates, so keep repainting while it is active.
        let style = self.crt_style.as_u32();
        let time = ui.input(|i| i.time) as f32;
        if self.crt_style == CrtStyle::YeOlde {
            ui.ctx().request_repaint();
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            crate::crt::CrtCallback { frame, style, time },
        ));
        // Clicking the screen requests input capture (handled later by the event
        // loop, which owns the winit Window).
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
            // Drop any motion accumulated before capture; the guest driver owns the
            // cursor position from here.
            self.mouse_rel_x = 0.0;
            self.mouse_rel_y = 0.0;
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
        // Set the OS title bar directly: viewport commands are not applied in this
        // bespoke winit loop (no eframe), so the lock hint has to go on the window.
        self.title = capture_title(self.input_captured, &self.input_release.display());
        window.set_title(&self.title);
    }

    /// Update the guest button mask from a pointer button edge and send it with any
    /// motion still pending this frame, so a click lands at the cursor's spot.
    fn set_guest_button(&mut self, bit: u8, pressed: bool) {
        if pressed {
            self.last_buttons |= bit;
        } else {
            self.last_buttons &= !bit;
        }
        let dx = self.mouse_rel_x as i32;
        let dy = self.mouse_rel_y as i32;
        self.mouse_rel_x = 0.0;
        self.mouse_rel_y = 0.0;
        self.mouse_dirty = false;
        if let Some(emu) = &self.emu {
            emu.send_mouse_relative(dx, dy, self.last_buttons);
        }
    }

    /// Forward host scroll-wheel motion to the guest. `lines` is signed notches
    /// (positive = scroll-up); fractional pixel-delta accumulates so only whole
    /// detents are sent, one +/-1 command per notch.
    fn forward_guest_wheel(&mut self, lines: f32) {
        self.wheel_accum += lines;
        if let Some(emu) = &self.emu {
            while self.wheel_accum >= 1.0 {
                emu.send_mouse_wheel(1); // scroll-up = +1
                self.wheel_accum -= 1.0;
            }
            while self.wheel_accum <= -1.0 {
                emu.send_mouse_wheel(-1);
                self.wheel_accum += 1.0;
            }
        }
    }

    /// Accumulate raw relative mouse motion (mickeys) for the next per-frame flush.
    /// The guest driver applies its ratio and clamps to the video mode's range, so
    /// the host forwards the raw counts unscaled and unclamped.
    fn accumulate_guest_motion(&mut self, dx: f32, dy: f32) {
        self.mouse_rel_x += dx;
        self.mouse_rel_y += dy;
        self.mouse_dirty = true;
    }

    /// Send the motion accumulated since the last flush as one coalesced relative
    /// packet, if any. The caller paces this separately from rendering so an 8000
    /// Hz mouse drives the guest at MOUSE_FLUSH_HZ, not at the host polling rate.
    fn flush_guest_motion(&mut self) {
        if !self.mouse_dirty {
            return;
        }
        self.mouse_dirty = false;
        let dx = self.mouse_rel_x as i32;
        let dy = self.mouse_rel_y as i32;
        self.mouse_rel_x = 0.0;
        self.mouse_rel_y = 0.0;
        if let Some(emu) = &self.emu {
            emu.send_mouse_relative(dx, dy, self.last_buttons);
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

    /// Toggle the panel and persist the new state.
    fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
        self.prefs.panel_open = self.panel_open;
        self.save_prefs();
    }

    /// The close tab while the panel is open: the full-height left edge of the
    /// panel is clickable, the same beige as the background so it reads as the
    /// border, with a small triangle icon. It highlights on hover. Clicking
    /// collapses the panel.
    fn open_handle(&mut self, ui: &mut egui::Ui) {
        let h = ui.available_height().max(40.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, h), egui::Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, BEVEL_HI);
        }
        // Triangle icon pointing inward (collapse the panel).
        let c = rect.center();
        let tri = vec![
            c + egui::vec2(-2.5, -5.0),
            c + egui::vec2(-2.5, 5.0),
            c + egui::vec2(3.5, 0.0),
        ];
        ui.painter()
            .add(egui::Shape::convex_polygon(tri, LABEL, egui::Stroke::NONE));
        if resp.clicked() {
            self.toggle_panel();
        }
    }

    /// The collapsed strip pinned to the window's right edge: the whole strip is
    /// the clickable reopen tab, flat with a small triangle icon. Clicking
    /// expands the panel.
    fn collapsed_tab(&mut self, ui: &mut egui::Ui) {
        let size = egui::vec2(ui.available_width(), ui.available_height());
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let fill = if resp.hovered() { BEVEL_HI } else { PANEL_FACE };
        ui.painter().rect_filled(rect, 0.0, fill);
        // Triangle icon pointing outward (pull the panel out).
        let c = rect.center();
        let tri = vec![
            c + egui::vec2(2.5, -5.0),
            c + egui::vec2(2.5, 5.0),
            c + egui::vec2(-3.5, 0.0),
        ];
        ui.painter()
            .add(egui::Shape::convex_polygon(tri, LABEL, egui::Stroke::NONE));
        if resp.clicked() {
            self.toggle_panel();
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            self.open_handle(ui);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.add_space(12.0);
                beige_visuals(ui);
                self.panel_body(ui);
            });
        });
    }

    /// The top row: the logo aligned left, then the power LED and the square
    /// Power and Reset buttons (Reset smaller) aligned to the right, all sharing
    /// one bottom baseline. The logo texture is built once and cached.
    fn panel_header(&mut self, ui: &mut egui::Ui) {
        let tex = self.logo.get_or_insert_with(|| {
            let rgba = recolor_logo(LOGO_RGBA, PANEL_FACE_F32);
            let image = egui::ColorImage::from_rgba_unmultiplied([LOGO_W, LOGO_H], &rgba);
            ui.ctx()
                .load_texture("izarra-logo", image, egui::TextureOptions::LINEAR)
        });
        let id = tex.id();
        let scale = 34.0 / LOGO_H as f32;
        let size = egui::vec2(LOGO_W as f32 * scale, LOGO_H as f32 * scale);
        let running = self.emu.is_some();
        // A fixed-height row, bottom-aligned, so the logo, LED, and buttons
        // share one baseline (the Power button's). The explicit height stops the
        // Align::Max layout from expanding to fill the whole panel.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 48.0),
            egui::Layout::left_to_right(egui::Align::Max),
            |ui| {
                ui.image((id, size));
                // Right side, added right to left so it reads LED, Power, Reset.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                    let reset = ui
                        .add_enabled_ui(running, |ui| {
                            ui.add_sized(
                                [36.0, 36.0],
                                egui::Button::new(egui::RichText::new("RESET").size(10.0)),
                            )
                        })
                        .inner;
                    if reset.clicked() {
                        self.start();
                    }
                    if ui
                        .add_sized(
                            [48.0, 48.0],
                            egui::Button::new(egui::RichText::new("POWER").size(13.0)),
                        )
                        .clicked()
                    {
                        if running {
                            self.stop();
                        } else {
                            self.start();
                        }
                    }
                    // A tall box so the LED centres vertically against the Power button.
                    let (led, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 48.0), egui::Sense::hover());
                    let c = led.center();
                    ui.painter()
                        .circle_filled(c, 6.0, if running { LED_ON } else { LED_OFF });
                    if running {
                        ui.painter().circle_filled(
                            c,
                            2.5,
                            egui::Color32::from_rgb(0xC8, 0xFF, 0xCE),
                        );
                    }
                    ui.painter()
                        .circle_stroke(c, 6.0, egui::Stroke::new(1.0, BEVEL_LO));
                });
            },
        );
    }

    fn panel_body(&mut self, ui: &mut egui::Ui) {
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

        self.panel_header(ui);
        ui.separator();
        self.drives_ui(ui, running);

        // Push the readout, volume, COM1, and vents to the bottom of the panel.
        let mode = mode.unwrap_or(self.profile.cpu);
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.separator();
                let line = |ui: &mut egui::Ui, text: String| {
                    ui.label(egui::RichText::new(text).color(MUTED).size(12.0));
                };
                // CPU and mode line, with the COM1 toggle aligned to its right.
                ui.horizontal(|ui| {
                    line(
                        ui,
                        format!(
                            "GSW-586 - {} mode - {} MHz",
                            mode.canonical_name(),
                            mode.clock_hz() / 1_000_000
                        ),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if info_button(ui).on_hover_text("About").clicked() {
                            self.show_about = true;
                        }
                        if ui
                            .button("\u{2699}")
                            .on_hover_text("Configuration")
                            .clicked()
                        {
                            self.open_config_dialog();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    line(
                        ui,
                        format!(
                            "Speed {:.0}% - {} MB",
                            speed * 100.0,
                            self.profile.memory_mib
                        ),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let com1_label = if self.show_com1 { "Hide COM1" } else { "COM1" };
                        if ui.button(com1_label).clicked() {
                            self.show_com1 = !self.show_com1;
                        }
                    });
                });
                line(ui, format!("Host {:.0} fps", self.host_fps));

                ui.add_space(6.0);
                // Volume row: the classic ascending-bars icon and a slider that
                // stretches to fill the remaining width.
                ui.horizontal(|ui| {
                    volume_icon(ui);
                    ui.add_space(4.0);
                    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(40.0);
                    let slider =
                        ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false));
                    if slider.changed() {
                        self.gain.set(volume_gain(self.volume));
                        self.prefs.master_volume = self.volume;
                        self.save_prefs();
                    }
                });

                ui.add_space(8.0);
                // Vent grille: four rows, kept clear of the right border.
                let cols = 5;
                let rows = 4;
                let row_h = 3.0;
                let row_gap = 3.0;
                let col_gap = 4.0;
                let right_margin = 8.0;
                let grille_w = (ui.available_width() - right_margin).max(20.0);
                let grille_h = rows as f32 * row_h + (rows as f32 - 1.0) * row_gap;
                let (grille, _) =
                    ui.allocate_exact_size(egui::vec2(grille_w, grille_h), egui::Sense::hover());
                let slot_w = (grille_w - col_gap * (cols as f32 - 1.0)) / cols as f32;
                let p = ui.painter();
                for r in 0..rows {
                    for col in 0..cols {
                        let x = grille.left() + col as f32 * (slot_w + col_gap);
                        let y = grille.top() + r as f32 * (row_h + row_gap);
                        let slot =
                            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(slot_w, row_h));
                        p.rect_filled(slot, 1.0, RECESS);
                    }
                }
            });
        });
    }

    /// Open the configuration modal, seeding its staged settings from the live
    /// values so Cancel can discard cleanly.
    fn open_config_dialog(&mut self) {
        self.config_dialog = Some(ConfigDialog {
            input_release: self.input_release.clone(),
            fullscreen: self.fullscreen_key.clone(),
            crt_style: self.crt_style,
            amp_gain: self.amp_gain,
            pc_speaker_volume: self.pc_speaker_volume,
            capturing: None,
        });
    }

    /// True while the dialog is waiting to capture a hotkey, so the event loop
    /// swallows the next key instead of toggling capture or forwarding to the guest.
    fn is_capturing_bind(&self) -> bool {
        self.config_dialog
            .as_ref()
            .is_some_and(|d| d.capturing.is_some())
    }

    /// Record a captured combo into the staged binding the dialog is waiting on,
    /// then stop capturing. `key` is the winit `KeyCode` debug name.
    fn record_bind(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool) {
        if let Some(dialog) = &mut self.config_dialog {
            if let Some(target) = dialog.capturing.take() {
                let binding = KeyBinding::new(ctrl, shift, alt, key);
                match target {
                    BindTarget::InputRelease => dialog.input_release = binding,
                    BindTarget::Fullscreen => dialog.fullscreen = binding,
                }
            }
        }
    }

    /// Render the configuration modal. Accept applies the staged settings and
    /// closes; Cancel, the backdrop, or Esc discards and closes.
    fn config_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.config_dialog.take() else {
            return;
        };
        let mut keep_open = true;
        let mut accept = false;
        let modal = egui::Modal::new(egui::Id::new("config-modal")).show(ctx, |ui| {
            egui::Frame::new()
                .fill(PANEL_FACE)
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 12,
                    bottom: 12,
                })
                .corner_radius(4.0)
                .show(ui, |ui| {
                    beige_visuals(ui);
                    ui.set_width(440.0);
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);

                    ui.vertical_centered(|ui| {
                        ui.label(header_text("Configuration", 18.0));
                    });
                    ui.add_space(6.0);

                    ui.label(egui::RichText::new("INPUT").color(LABEL).size(11.0));
                    beige_group(ui, |ui| {
                        egui::Grid::new("config-keys")
                            .num_columns(2)
                            .spacing([16.0, 10.0])
                            .show(ui, |ui| {
                                ui.label("Input release");
                                bind_button(ui, &mut dialog, BindTarget::InputRelease);
                                ui.end_row();
                                ui.label("Full screen");
                                bind_button(ui, &mut dialog, BindTarget::Fullscreen);
                                ui.end_row();
                            });
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("DISPLAY").color(LABEL).size(11.0));
                    beige_group(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("CRT emulation");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.selectable_value(
                                        &mut dialog.crt_style,
                                        CrtStyle::YeOlde,
                                        "Ye Olde Screene",
                                    );
                                    ui.selectable_value(
                                        &mut dialog.crt_style,
                                        CrtStyle::Subtle,
                                        "Subtle",
                                    );
                                    ui.selectable_value(&mut dialog.crt_style, CrtStyle::Off, "No");
                                },
                            );
                        });
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("AUDIO").color(LABEL).size(11.0));
                    beige_group(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("ReSonique 2 amp gain");
                            ui.add(
                                egui::Slider::new(&mut dialog.amp_gain, 0..=prefs::AMP_GAIN_MAX)
                                    .custom_formatter(|n, _| format!("{:.1}x", n / 10.0)),
                            )
                            .on_hover_text(
                                "Output gain for the sound card's analog stage. Raise if a \
                                 game's sound is too quiet, lower if it clips.",
                            );
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("PC speaker volume");
                            ui.add(
                                egui::Slider::new(&mut dialog.pc_speaker_volume, 0..=100)
                                    .custom_formatter(|n, _| {
                                        if n <= 0.0 {
                                            "Muted".to_string()
                                        } else {
                                            format!("{n:.0}%")
                                        }
                                    }),
                            )
                            .on_hover_text(
                                "Volume of the motherboard PC speaker (the beeps), separate \
                                 from the sound card. Set to 0 to mute it.",
                            );
                        });
                    });

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Accept").clicked() {
                            accept = true;
                            keep_open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                });
        });
        if modal.should_close() {
            keep_open = false;
        }
        if accept {
            self.apply_config(&dialog);
        }
        if keep_open {
            self.config_dialog = Some(dialog);
        }
    }

    /// Push the staged config to the live fields, the emulation thread, and prefs.
    fn apply_config(&mut self, dialog: &ConfigDialog) {
        self.input_release = dialog.input_release.clone();
        self.fullscreen_key = dialog.fullscreen.clone();
        self.crt_style = dialog.crt_style;
        self.prefs.input_release = dialog.input_release.clone();
        self.prefs.fullscreen = dialog.fullscreen.clone();
        self.prefs.crt_style = dialog.crt_style;
        // Amp gain: update the live value + prefs and push the new multiplier to
        // the shared amp atomic so the emulation thread's audio pump picks it up
        // without a restart.
        if dialog.amp_gain != self.amp_gain {
            self.amp_gain = dialog.amp_gain;
            self.prefs.amp_gain = dialog.amp_gain;
            self.amp.set(amp_multiplier(self.amp_gain));
        }
        // PC speaker volume: same live-update path as the amp gain.
        if dialog.pc_speaker_volume != self.pc_speaker_volume {
            self.pc_speaker_volume = dialog.pc_speaker_volume;
            self.prefs.pc_speaker_volume = dialog.pc_speaker_volume;
            self.speaker_vol
                .set(speaker_multiplier(self.pc_speaker_volume));
        }
        self.save_prefs();
    }

    /// The floating COM1 window: black monospace serial log on white, auto-scrolled
    /// to the bottom, inside the shared beige chrome. The window is draggable,
    /// resizable, and closable; its open state is bound to `show_com1` so the
    /// close control and the footer button stay in sync.
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
        beige_window(ctx, "COM1", &mut open, true, [480.0, 320.0], |ui| {
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

    /// The floating License window: the full GPL-3.0 text, black monospace on
    /// white inside the shared beige chrome. Opened from the About window.
    fn license_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_license;
        beige_window(
            ctx,
            "License (GPL-3.0)",
            &mut open,
            true,
            [640.0, 520.0],
            |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.style_mut().spacing.scroll.bar_width = 6.0;
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(include_str!("../../../LICENSE"))
                                        .monospace()
                                        .color(egui::Color32::BLACK),
                                ));
                            });
                    });
            },
        );
        self.show_license = open;
    }

    /// The floating About window: product/version/copyright and a GitHub link
    /// first, then the bundled third-party attribution (verbatim NOTICE), then
    /// a button to open the full license.
    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        let mut open_license = self.show_license;
        beige_window(
            ctx,
            "About IzarraVM",
            &mut open,
            false,
            [540.0, 420.0],
            |ui| {
                ui.vertical_centered(|ui| {
                    ui.visuals_mut().hyperlink_color = LINK_BLUE;
                    ui.label(
                        egui::RichText::new(concat!("IzarraVM ", env!("CARGO_PKG_VERSION")))
                            .color(INK)
                            .size(18.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("the Izarra 3000 virtual machine")
                            .color(MUTED)
                            .size(12.0),
                    );
                    ui.hyperlink_to("github.com/vorvek/IzarraVM", GITHUB_URL);
                    ui.label(
                        egui::RichText::new(
                            "\u{00A9} 2026 General Simulation Works \u{00B7} GPL-3.0",
                        )
                        .color(MUTED)
                        .size(12.0),
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Bundled software")
                            .color(LABEL)
                            .size(11.0)
                            .strong(),
                    );
                    notice_block(ui, include_str!("../../../NOTICE"), MUTED, 11.0);
                    ui.add_space(8.0);
                    if ui.button("View license").clicked() {
                        open_license = true;
                    }
                });
            },
        );
        self.show_about = open;
        self.show_license = open_license;
    }

    /// Write the current prefs to disk. Best-effort: GuiPrefs::save logs and
    /// swallows any IO error, so this never interrupts the UI.
    fn save_prefs(&self) {
        self.prefs.save(&self.prefs_path);
    }

    /// The three drive bays. `running` gates the media actions on a live
    /// emulation thread to send commands to.
    fn drives_ui(&mut self, ui: &mut egui::Ui, running: bool) {
        let lit = |at: Option<Instant>| at.is_some_and(|t| t.elapsed() < LED_GLOW);
        let floppy_lit = lit(self.floppy_access_at);
        let c_lit = lit(self.c_access_at);
        let cd_lit = lit(self.cd_access_at);

        // Floppy A:
        beige_group(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("FLOPPY  A:").color(LABEL).size(11.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    activity_led(ui, floppy_lit);
                });
            });
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 30.0).max(20.0);
                let (slot, _) = ui.allocate_exact_size(egui::vec2(w, 10.0), egui::Sense::hover());
                bevel_rect(ui.painter(), slot, RECESS, false);
                let mounted = self.floppy_label.is_some();
                if eject_button(ui, running && mounted) {
                    self.eject_floppy_action();
                }
            });
            ui.label(
                egui::RichText::new(self.floppy_label.as_deref().unwrap_or("(empty)"))
                    .color(MUTED)
                    .italics()
                    .size(11.0),
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(running, egui::Button::new("Load IMG"))
                    .clicked()
                {
                    self.load_floppy_img();
                }
            });
        });

        ui.add_space(8.0);

        // CD-ROM D:
        beige_group(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("CD-ROM  D:").color(LABEL).size(11.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    activity_led(ui, cd_lit);
                });
            });
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 30.0).max(20.0);
                let (slot, _) = ui.allocate_exact_size(egui::vec2(w, 18.0), egui::Sense::hover());
                bevel_rect(ui.painter(), slot, RECESS, false);
                // Tray seam.
                let seam = slot.center().y;
                ui.painter().line_segment(
                    [
                        egui::pos2(slot.left() + 5.0, seam),
                        egui::pos2(slot.right() - 5.0, seam),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(0x3D, 0x38, 0x2D)),
                );
                let mounted = self.cd_label.is_some();
                if eject_button(ui, running && mounted) {
                    self.eject_cd_action();
                }
            });
            ui.label(
                egui::RichText::new(self.cd_label.as_deref().unwrap_or("(empty)"))
                    .color(MUTED)
                    .italics()
                    .size(11.0),
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(running, egui::Button::new("Load ISO"))
                    .clicked()
                {
                    self.load_cd_image();
                }
                if ui
                    .add_enabled(running, egui::Button::new("Load folder"))
                    .clicked()
                {
                    self.load_cd_folder();
                }
            });
        });

        ui.add_space(8.0);

        // Hard Disk C:
        beige_group(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("HARD DISK  C:").color(LABEL).size(11.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    activity_led(ui, c_lit);
                });
            });
            if ui.button("Open C: folder").clicked() {
                open_in_file_manager(&self.c_drive);
            }
            // Blank line holds the box at its prior height now that the path
            // label is gone.
            ui.label(egui::RichText::new(" ").size(11.0));
        });
    }

    /// Eject drive A: and forget the mount so it is not restored next launch.
    fn eject_floppy_action(&mut self) {
        if let Some(emu) = &self.emu {
            emu.eject_floppy();
        }
        self.floppy_label = None;
        self.floppy_source = None;
        self.prefs.last_floppy_image = None;
        self.save_prefs();
    }

    /// Eject the CD and forget the mount so it is not restored next launch.
    fn eject_cd_action(&mut self) {
        if let Some(emu) = &self.emu {
            emu.eject_cd();
        }
        self.cd_label = None;
        self.prefs.last_cd_image = None;
        self.prefs.last_cd_folder = None;
        self.save_prefs();
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
        self.mount_floppy_source(FloppySource(path));
    }

    /// Read the image for `source`, mount it into the live emulation thread,
    /// and remember it so a Reset can remount the same media. Errors are
    /// logged and leave the drive unchanged. Used by both the Load IMG button
    /// and the remount on Reset.
    fn mount_floppy_source(&mut self, source: FloppySource) {
        let FloppySource(path) = &source;
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
        let Some(emu) = &self.emu else {
            return;
        };
        emu.mount_floppy(bytes, Some(path.clone()));
        self.floppy_label = Some(label);
        // Remember the mount in prefs so it is restored next launch.
        self.prefs.last_floppy_image = Some(path.clone());
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
        self.mount_cd_from_path(&path);
    }

    /// Read or build the CD image at `path`, mount it into the live emulation
    /// thread, and remember it in prefs so it is restored next launch. Errors
    /// are logged and leave the drive unchanged. Used by the Load ISO button,
    /// the config-file `cd_image` mount, and the prefs restore on startup.
    fn mount_cd_from_path(&mut self, path: &Path) {
        let image = match load_cd_image_from_path(path) {
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
        // An ISO/CUE mount and a folder mount are mutually exclusive in the
        // CD drive, so recording one clears the other.
        self.prefs.last_cd_image = Some(path.to_path_buf());
        self.prefs.last_cd_folder = None;
        self.save_prefs();
    }

    /// Pick a host folder and mount it as a CD-ROM: an ISO9660 image is built
    /// in memory (metadata only; file contents are read from the host folder
    /// lazily as the guest requests sectors, so a large folder does not get
    /// copied in up front).
    fn load_cd_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.mount_cd_from_folder(&dir);
    }

    /// Build and mount the folder at `dir` as a CD-ROM, and remember it in
    /// prefs so it is restored (rebuilt) next launch. Errors -- including the
    /// ~650 MB CD-ROM capacity guard -- are logged the same way a bad ISO/CUE
    /// mount is, and leave the drive unchanged. Used by the Load folder button
    /// and the prefs restore on startup.
    fn mount_cd_from_folder(&mut self, dir: &Path) {
        let built = match izarravm_machine::build_cd_folder(dir) {
            Ok(built) => built,
            Err(err) => {
                error!(%err, dir = %dir.display(), "failed to build a CD image from the folder");
                return;
            }
        };
        let image = match izarravm_machine::CdImage::from_folder(built) {
            Ok(image) => image,
            Err(err) => {
                error!(%err, dir = %dir.display(), "failed to mount the folder as a CD image");
                return;
            }
        };
        let Some(emu) = &self.emu else {
            return;
        };
        let label = format!(
            "{} (folder)",
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string())
        );
        emu.mount_cd(image);
        self.cd_label = Some(label);
        self.prefs.last_cd_folder = Some(dir.to_path_buf());
        self.prefs.last_cd_image = None;
        self.save_prefs();
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
        // The window title (capture-lock hint) is set directly on the winit window
        // from the event loop now; viewport commands are not applied without eframe.
        // Host render rate: count this frame, roll the rate up once a second.
        let now = Instant::now();
        self.frames_since += 1;
        let mark = *self.metrics_mark.get_or_insert(now);
        let window = now.duration_since(mark).as_secs_f64();
        if window >= 1.0 {
            self.host_fps = self.frames_since as f64 / window;
            self.frames_since = 0;
            self.metrics_mark = Some(now);
        }
        // Mirror the host lock keys onto the guest each frame.
        self.sync_guest_locks();
        if self.panel_open {
            // No left/top/bottom margin so the close tab is flush to the left
            // edge and spans the full height; the body adds its own padding.
            let open_frame = egui::Frame::new()
                .fill(PANEL_FACE)
                .inner_margin(egui::Margin {
                    left: 0,
                    right: 12,
                    top: 0,
                    bottom: 0,
                });
            egui::SidePanel::right("controls")
                .exact_width(320.0)
                .resizable(false)
                .frame(open_frame)
                .show(ctx, |ui| self.controls_ui(ui));
        } else {
            egui::SidePanel::right("controls-tab")
                .exact_width(18.0)
                .resizable(false)
                .frame(egui::Frame::new().fill(PANEL_FACE))
                .show(ctx, |ui| self.collapsed_tab(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show(ctx, |ui| self.monitor_ui(ui));
        // The COM1 console floats over the central panel when toggled open.
        if self.show_com1 {
            self.com1_window(ctx);
        }
        // The configuration modal renders on top of everything when open.
        self.config_ui(ctx);
        // About must dispatch before License: its "View license" button sets
        // show_license, so this order opens the License window the same frame.
        if self.show_about {
            self.about_window(ctx);
        }
        if self.show_license {
            self.license_window(ctx);
        }
    }
}

/// Bump every UI text style up a couple of points for legibility. Applied once
/// to the egui context at startup, so it persists across frames.
fn enlarge_ui_fonts(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        for font_id in style.text_styles.values_mut() {
            font_id.size += 2.0;
        }
    });
}

/// Set the dark base theme with a pure-black canvas, so the area around the
/// monitor and the 4:3 letterbox are black rather than the default grey-blue.
/// The beige panel and modal override their own fills, so this does not leak
/// into them. Applied once at startup.
fn apply_black_theme(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        let mut v = egui::Visuals::dark();
        v.panel_fill = egui::Color32::BLACK;
        v.extreme_bg_color = egui::Color32::BLACK;
        v.window_fill = egui::Color32::from_rgb(0x1A, 0x1A, 0x1A);
        style.visuals = v;
    });
}

/// The window title for the current capture state. While captured it tells the
/// user which key releases the grab; otherwise it is just the product name.
fn capture_title(captured: bool, release_key: &str) -> String {
    if captured {
        format!("IzarraVM - [Input locked, press {release_key} to release]")
    } else {
        String::from("IzarraVM")
    }
}

/// A config-dialog button showing a binding's label, or "press a key…" while it
/// is the one being captured. Clicking toggles capture for that binding.
fn bind_button(ui: &mut egui::Ui, dialog: &mut ConfigDialog, target: BindTarget) {
    let capturing = dialog.capturing == Some(target);
    let label = if capturing {
        "press a key\u{2026}".to_string()
    } else {
        match target {
            BindTarget::InputRelease => dialog.input_release.display(),
            BindTarget::Fullscreen => dialog.fullscreen.display(),
        }
    };
    if ui.selectable_label(capturing, label).clicked() {
        dialog.capturing = if capturing { None } else { Some(target) };
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

#[cfg(test)]
#[path = "gui_test.rs"]
mod tests;
