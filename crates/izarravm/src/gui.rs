// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#[path = "gui_runtime.rs"]
mod runtime;
#[path = "gui_session.rs"]
mod session;
#[path = "gui_ui.rs"]
mod ui;

pub use runtime::run;
// The headless --cd-image mount goes through the same loader as the GUI mount,
// so the accepted formats and the error messages stay one list.
pub(crate) use session::load_cd_image_from_path;

use crate::host_input::HostInputPolicy;
use crate::prefs::{CrtStyle, GuiPrefs, KeyBinding, MAX_VOLUME};
use crate::startup::GuiLaunch;
use izarravm_audio::{AudioPlayer, MidiEngine};
use izarravm_core::{GswMode, MidiBackend, MidiConfig, MidiPortId, MidiStatus};
use izarravm_input::{
    GamepadManager, HostKeyboard, JoystickBinding, JoystickSample, JoystickWizard,
};
use izarravm_machine::{CdAudioState, JoystickState};
use session::{
    AppliedState, CdSource, FloppySource, GuestInput, GuiSession, PreparedCd, PreparedFloppy,
    PreparedInitialMedia, RequestId, SessionClosed, SessionEvent, SessionFailure, SessionFrame,
    SessionRequest, SessionSnapshot, SessionSpec, SharedGain, ShutdownReport,
};
use std::cell::Cell;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tracing::{error, warn};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

/// Map the master-volume slider to a linear audio gain.
///
/// The knob stands for the powered speakers the machine's line-out feeds, and
/// such a knob has travel past line level. Below the 100% detent the curve is
/// the cubic perceptual taper it has always been, so a `master_volume` saved by
/// an older build still means exactly what it meant. At and above the detent the
/// knob reads directly: 100% is unity, 300% is three times, 500% is five.
///
/// Five times is [`MAX_VOLUME`], which is +14 dB, and the arithmetic behind that
/// ceiling is in `docs/izarravm-gui/guide.md`: a title that maxes its own mixer
/// still leaves the card at CT1745 level 27 (-8 dB) under the mix's deliberate
/// -6 dB headroom reserve, so the worst well-behaved case arrives 14 dB down.
fn volume_gain(volume: f32) -> f32 {
    let volume = volume.clamp(0.0, MAX_VOLUME);
    if volume < 1.0 { volume.powi(3) } else { volume }
}

/// Read a volume back out of the slider's value box, in the units it prints.
///
/// The box is editable and it displays "80%" over a stored 0.8. egui's stock
/// parser is a plain float parse, which gets both halves of that wrong. It
/// rejects the "%" the box seeds itself with, so pressing Enter on text the user
/// never edited silently does nothing. And it reads a typed `100` as the number
/// one hundred: on a knob that used to stop at 1.0 that clamped harmlessly to
/// unity, but the travel now goes to five, so typing the neutral setting would
/// land at five times it.
///
/// Parsing in the printed units fixes both: strip one optional trailing "%",
/// read the number, divide. Anything that is not a number is rejected, which is
/// what tells egui to leave the value alone.
fn volume_percent_to_fraction(text: &str) -> Option<f64> {
    text.trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .map(|percent| percent / 100.0)
}

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
const JOYSTICK_POLL_HZ: f64 = 120.0;

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
/// The Izarra3000 logo's red, sampled from the wordmark. Used for the floating
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
        w.bg_stroke = egui::Stroke::new(1.0_f32, BEVEL_LO);
        w.fg_stroke = egui::Stroke::new(1.0_f32, INK);
    }
    v.widgets.inactive.bg_fill = FACEPLATE;
    v.widgets.inactive.weak_bg_fill = FACEPLATE;
    v.widgets.hovered.bg_fill = BEVEL_HI;
    v.widgets.hovered.weak_bg_fill = BEVEL_HI;
    v.widgets.active.bg_fill = BEVEL_LO;
    v.widgets.active.weak_bg_fill = BEVEL_LO;
    // A pressed segmented control reads as recessed.
    v.selection.bg_fill = BEVEL_LO;
    v.selection.stroke = egui::Stroke::new(1.0_f32, INK);
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
    let top = egui::Stroke::new(1.0_f32, hi);
    let bot = egui::Stroke::new(1.0_f32, lo);
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
                .stroke(egui::Stroke::new(1.5_f32, BEVEL_LO))
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
    let stroke = egui::Stroke::new(1.5_f32, INK);
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
        egui::Stroke::new(0.5_f32, BEVEL_LO),
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
        egui::Stroke::new(1.5_f32, col),
    );
    enabled && resp.clicked()
}

/// Which glyph a [`transport_button`] paints: the four standard transport
/// symbols, as solid shapes on the eject button's scale.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportIcon {
    Play,
    Pause,
    SkipNext,
    Stop,
}

/// A CD front-panel transport button, wide enough for a glyph rather than for
/// a word. Returns true on a click while `enabled`. Painted for the same reason
/// [`eject_button`] is: the egui button theme cannot give a tiny glyph the
/// plastic look of the drive bay around it.
fn transport_button(ui: &mut egui::Ui, enabled: bool, icon: TransportIcon, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 18.0), egui::Sense::click());
    let held = enabled && resp.is_pointer_button_down_on();
    bevel_rect(ui.painter(), rect, FACEPLATE, !held);
    let c = rect.center();
    let col = if enabled { INK } else { BEVEL_LO };
    // Half the glyph height, set so the glyphs carry the same weight as the
    // eject triangle one row above and the buttons read as one family.
    const H: f32 = 4.5;
    let bar = |x0: f32, x1: f32, half_h: f32| {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x + x0, c.y - half_h),
                egui::pos2(c.x + x1, c.y + half_h),
            ),
            0.0,
            col,
        );
    };
    let triangle = |x0: f32, tip_x: f32| {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x + x0, c.y - H),
                egui::pos2(c.x + x0, c.y + H),
                egui::pos2(c.x + tip_x, c.y),
            ],
            col,
            egui::Stroke::NONE,
        ));
    };
    match icon {
        TransportIcon::Play => triangle(-4.25, 4.25),
        TransportIcon::Pause => {
            bar(-4.0, -1.0, H);
            bar(1.0, 4.0, H);
        }
        TransportIcon::SkipNext => {
            triangle(-4.5, 1.5);
            bar(2.6, 4.5, H);
        }
        // A square of the full glyph height outweighs the other three, so the
        // stop block takes the usual optical trim on both axes.
        TransportIcon::Stop => bar(-4.0, 4.0, 4.0),
    }
    let clicked = enabled && resp.clicked();
    resp.on_hover_text(tip);
    clicked
}

/// The CD level fader. The same slider as the speaker level on the audio
/// panel: a trailing fill so the travelled part of the track reads the level,
/// and an editable value box in percent on the right.
///
/// The box is editable, so it needs a parser in the units it prints. The
/// travel is already whole percent, so unlike the speaker knob the parser only
/// has to accept the "%" the formatter writes.
fn cd_fader(ui: &mut egui::Ui, enabled: bool, percent: u8) -> Option<u8> {
    let mut level = percent;
    // Leave the value box the same room the speaker row leaves it.
    ui.spacing_mut().slider_width = (ui.available_width() - 56.0).max(40.0);
    let moved = ui
        .add_enabled(
            enabled,
            egui::Slider::new(&mut level, 0..=100)
                .trailing_fill(true)
                .custom_formatter(|value, _| format!("{value:.0}%"))
                .custom_parser(|text| text.trim().trim_end_matches('%').trim().parse().ok()),
        )
        .on_hover_text("CD audio level")
        .changed();
    moved.then_some(level)
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
    let stroke = egui::Stroke::new(1.2_f32, LABEL);
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
    for (src, dst) in raw
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
    {
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

pub struct GuiApp {
    session: GuiSession,
    session_snapshot: SessionSnapshot,
    session_frame: Option<SessionFrame>,
    host_input: HostInputPolicy,
    gamepads: Option<GamepadManager>,
    joystick_binding: Option<JoystickBinding>,
    last_joystick_sent: Option<Option<JoystickSample>>,
    title: String,
    // Input-capture state, the single source of truth for routing. When true the
    // OS cursor is confined and hidden over the window, all keyboard input goes
    // to the guest (egui does not consume it, including TAB), and host mouse
    // motion and buttons are forwarded to the VM. The input-release hotkey
    // (Super+F2 by default) releases it. Entered by clicking the framebuffer
    // image.
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
    // emulation thread gets a Send sink cloned from it. Polled each frame so a
    // stream killed by a device change is rebuilt on the same queue rather than
    // leaving the machine playing to nothing for the rest of the session.
    audio: AudioPlayer,
    // Guest frame counter of the texture currently uploaded, so we rebuild it
    // only when a new frame is presented rather than on every update().
    frame_seq: u64,
    // Host render rate, recomputed once a second and surfaced in the panel.
    metrics_mark: Option<Instant>,
    frames_since: u32,
    host_fps: f64,
    // Drive-access LED state: the last access count seen from the frame snapshot
    // and when it last advanced, so the LED lights briefly on each access.
    floppy_access_seen: u64,
    c_access_seen: u64,
    floppy_access_at: Option<Instant>,
    c_access_at: Option<Instant>,
    cd_access_seen: u64,
    cd_access_at: Option<Instant>,
    // Whether the floating COM1 window is open. The footer button and the
    // window's own close control both flip this.
    show_com1: bool,
    // Whether the floating About window is open. The footer info button and the
    // window's own close control both flip this.
    show_about: bool,
    // Whether the floating License (GPL-3.0-only) window is open. The About window's
    // "View license" button and the window's own close control flip this.
    show_license: bool,
    // Master volume slider position, 0.0..MAX_VOLUME, where 1.0 is unity.
    // Mapped by `volume_gain` into a host-side gain that the emulation thread
    // reads through `gain`.
    volume: f32,
    // The shared master gain (curved volume slider), read each audio pump. This
    // is the HOST's level: the powered speakers the machine's line-out feeds,
    // applied after everything the machine renders. Every level inside the
    // chain belongs to the guest and is set with SNDMIXER.COM.
    gain: SharedGain,
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
    joystick_binding: Option<JoystickBinding>,
    joystick_wizard: Option<JoystickWizard>,
    crt_style: CrtStyle,
    midi_backend: MidiBackend,
    external_midi_port: Option<MidiPortId>,
    soundfont: Option<PathBuf>,
    mt32_control_rom: String,
    mt32_pcm_rom: String,
    midi_ports: Vec<MidiPortId>,
    // The binding awaiting a key press, set when the user clicks a rebind button.
    capturing: Option<BindTarget>,
}

fn apply_joystick_binding(
    live: &mut Option<JoystickBinding>,
    prefs: &mut GuiPrefs,
    staged: &Option<JoystickBinding>,
    last_sent: &mut Option<Option<JoystickSample>>,
) {
    if staged != live {
        *live = staged.clone();
        *last_sent = None;
    }
    prefs.joystick_binding = staged.clone();
}

fn path_text(path: Option<&PathBuf>) -> String {
    path.map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn optional_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn cpu_mode_label(mode: GswMode) -> String {
    let clock = mode.clock_rate();
    let mhz = clock.as_hz_f64() / 1_000_000.0;
    let precision = if clock.denominator() == 1 { 0 } else { 2 };
    format!(
        "GSW-586 - {} mode - {mhz:.precision$} MHz",
        mode.canonical_name()
    )
}

fn midi_backend_label(backend: MidiBackend) -> &'static str {
    match backend {
        MidiBackend::Off => "Off",
        MidiBackend::External => "External MIDI",
        MidiBackend::Munt => "Munt (MT-32)",
    }
}

/// Whether the two ROM boxes name something the loader can work with.
///
/// `exists`, not `is_file`: either box may name the FOLDER a ROM set lives in,
/// which is how a set of split half-images (and any set whose files are named
/// something other than `MT32_*.ROM`) is loaded. Requiring a file here is what
/// left the Munt entry greyed out for a user whose ROMs were perfectly good.
fn munt_roms_available(control: &str, pcm: &str) -> bool {
    [control, pcm].into_iter().all(|path| {
        let path = path.trim();
        !path.is_empty() && Path::new(path).exists()
    })
}

fn midi_port_label(port: &MidiPortId) -> String {
    format!("{} #{}", port.name, u32::from(port.ordinal) + 1)
}

/// Whether pressing Accept should hand the session a MIDI configuration.
///
/// A changed configuration obviously has to be sent. So does an UNCHANGED one
/// when an engine is not Ready, and that half is the fix for a panel that could
/// show an error forever: the engines only re-open on a settings change their
/// own role cares about, and the wavetable's role cares about exactly one
/// setting, so an engine that failed mid-session had no path back and no way to
/// clear its message. With this, Accept is a retry.
///
/// `MissingSoundFont` counts as not-Ready on purpose: it means the user's own
/// SoundFont failed and the embedded bank is standing in, so Accept should try
/// theirs again rather than leave the fallback until the next restart.
fn midi_request_needed(
    staged: &MidiConfig,
    live: &MidiConfig,
    powered: bool,
    statuses: [MidiStatus; 2],
) -> bool {
    // A CHANGED configuration is always sent, running or not. There is no
    // engine to reconfigure while the machine is off, but the session still has
    // to be told: with no worker it applies the change to the spec and the
    // snapshot itself and emits the Applied event that carries it into
    // izarravm.conf, so the next power-on boots what the user chose. Swallowing
    // it here lost the setting in total silence -- the panel reseeds from the
    // snapshot, so Accept looked like it had worked.
    //
    // The RETRY half is what needs the machine running: a status can only be
    // stale, and an engine can only be re-opened, when there is a worker
    // holding one.
    staged != live
        || (powered
            && statuses
                .into_iter()
                .any(|status| status != MidiStatus::Ready))
}

fn midi_status_text(status: MidiStatus) -> &'static str {
    match status {
        MidiStatus::Ready => "Ready",
        MidiStatus::MissingPort => "The selected host MIDI destination is not available.",
        MidiStatus::MissingSoundFont => "The custom SoundFont failed. The embedded bank is active.",
        MidiStatus::MissingRoms => "Select both MT-32 ROMs. P330 output is silent.",
        MidiStatus::RomPathMissing => "A selected MT-32 ROM path does not exist.",
        MidiStatus::RomControlMissing => {
            "No MT-32 control ROM was recognised. Point either box at the ROM set's folder."
        }
        MidiStatus::RomPcmMissing => {
            "The control ROM loaded but no PCM ROM was recognised. Add the PCM image to the set."
        }
        MidiStatus::RomsNotPairable => {
            "The control and PCM ROMs are from different machines. Use one matched set."
        }
        MidiStatus::InitializationFailed => "The MIDI output could not be initialized.",
    }
}

fn midi_path_picker(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    filter: &str,
    extensions: &[&str],
    hint: &str,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 140.0).max(120.0);
        ui.add_sized(
            [width, 22.0],
            egui::TextEdit::singleline(text).hint_text(hint),
        );
        if ui.button("Browse").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(filter, extensions)
                .pick_file()
        {
            *text = path.to_string_lossy().into_owned();
        }
        // A ROM set is a folder as often as it is two files a user can name,
        // and the loader takes either. Without this button the only way to load
        // a set whose files are split halves is to type the folder in by hand.
        if ui.button("Folder").clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            *text = path.to_string_lossy().into_owned();
        }
    });
}

fn soundfont_picker(ui: &mut egui::Ui, soundfont: &mut Option<PathBuf>) {
    ui.label("P300 SoundFont");
    ui.horizontal(|ui| {
        if ui
            .selectable_label(soundfont.is_none(), "FluidR3Mono GM (Internal)")
            .clicked()
        {
            *soundfont = None;
        }
        if ui
            .selectable_label(soundfont.is_some(), "External...")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("SoundFont", &["sf2", "sf3"])
                .pick_file()
        {
            *soundfont = Some(path);
        }
    });
    if let Some(path) = soundfont {
        ui.small(format!("External: {}", path.display()));
    }
}

fn initial_cd_source(cd_image: Option<PathBuf>, prefs: &GuiPrefs) -> Option<CdSource> {
    cd_image.map(CdSource::Image).or_else(|| {
        prefs
            .last_cd_image
            .clone()
            .filter(|path| path.is_file())
            .map(CdSource::Image)
            .or_else(|| {
                prefs
                    .last_cd_folder
                    .clone()
                    .filter(|path| path.is_dir())
                    .map(CdSource::Folder)
            })
    })
}

fn prepare_initial_media(cd_image: Option<PathBuf>, prefs: &GuiPrefs) -> PreparedInitialMedia {
    let floppy = prefs
        .last_floppy_image
        .clone()
        .filter(|path| path.is_file())
        .and_then(|path| {
            PreparedFloppy::from_source(FloppySource(path))
                .inspect_err(|err| error!(%err, "failed to prepare startup floppy"))
                .ok()
        });
    let cd_source = initial_cd_source(cd_image, prefs);
    let cd = cd_source.and_then(|source| {
        PreparedCd::from_source(source)
            .inspect_err(|err| error!(%err, "failed to prepare startup CD"))
            .ok()
    });
    PreparedInitialMedia { floppy, cd }
}

fn remember_initial_media(prefs: &mut GuiPrefs, snapshot: &SessionSnapshot) -> bool {
    let mut changed = false;
    if let Some(source) = &snapshot.floppy_source
        && prefs.last_floppy_image.as_ref() != Some(&source.0)
    {
        prefs.last_floppy_image = Some(source.0.clone());
        changed = true;
    }
    if let Some(source) = &snapshot.cd_source {
        match source {
            CdSource::Image(path) => {
                changed |=
                    prefs.last_cd_image.as_ref() != Some(path) || prefs.last_cd_folder.is_some();
                prefs.last_cd_image = Some(path.clone());
                prefs.last_cd_folder = None;
            }
            CdSource::Folder(path) => {
                changed |=
                    prefs.last_cd_folder.as_ref() != Some(path) || prefs.last_cd_image.is_some();
                prefs.last_cd_folder = Some(path.clone());
                prefs.last_cd_image = None;
            }
        }
    }
    changed
}

fn apply_session_preference(prefs: &mut GuiPrefs, state: AppliedState) -> bool {
    match state {
        AppliedState::Floppy(source) => {
            let path = source.map(|source| source.0);
            let changed = prefs.last_floppy_image != path;
            prefs.last_floppy_image = path;
            changed
        }
        AppliedState::Cd(source) => match source {
            Some(CdSource::Image(path)) => {
                let changed =
                    prefs.last_cd_image.as_ref() != Some(&path) || prefs.last_cd_folder.is_some();
                prefs.last_cd_image = Some(path);
                prefs.last_cd_folder = None;
                changed
            }
            Some(CdSource::Folder(path)) => {
                let changed =
                    prefs.last_cd_folder.as_ref() != Some(&path) || prefs.last_cd_image.is_some();
                prefs.last_cd_folder = Some(path);
                prefs.last_cd_image = None;
                changed
            }
            None => {
                let image = prefs.last_cd_image.take().is_some();
                let folder = prefs.last_cd_folder.take().is_some();
                image || folder
            }
        },
        AppliedState::Midi(config) => {
            let changed = prefs.midi != config;
            prefs.midi = config;
            changed
        }
        AppliedState::Other => false,
    }
}

impl GuiApp {
    fn new(launch: GuiLaunch) -> Result<Self, SessionFailure> {
        let GuiLaunch {
            profile,
            rom,
            c_drive,
            cd_image,
            midi_config,
            glide_ovl,
            test_pattern,
            rtc_setup,
            host_input,
            mut prefs,
            prefs_path,
        } = launch;
        // Always built, even with no sound device on the host: the queue is
        // what the emulation thread writes to, and it has to exist before a
        // stream does for a device plugged in later to be picked up at all.
        let audio = AudioPlayer::new();
        if !audio.is_playing() {
            warn!("no audio output device; the machine will play to one if it appears");
        }
        let volume = prefs.master_volume.clamp(0.0, MAX_VOLUME);
        let gain = SharedGain::new(volume_gain(volume));
        let initial_media = prepare_initial_media(cd_image, &prefs);
        let spec = SessionSpec {
            profile,
            rom,
            c_drive,
            midi_config,
            glide_ovl,
            test_pattern,
            sink: Some(audio.sink()),
            rtc_setup,
            gain: gain.clone(),
            #[cfg(test)]
            finalization_probe: None,
        };
        let mut session = GuiSession::start(spec, initial_media)?;
        let initial_update = session.poll();
        let initial_prefs_changed = remember_initial_media(&mut prefs, &initial_update.snapshot);
        let crt_style = prefs.crt_style;
        let input_release = prefs.input_release.clone();
        let fullscreen_key = prefs.fullscreen.clone();
        let joystick_binding = prefs.joystick_binding.clone();
        let gamepads = host_input
            .joystick_enabled()
            .then(GamepadManager::new)
            .and_then(|result| match result {
                Ok(manager) => Some(manager),
                Err(err) => {
                    warn!(%err, "host controller input unavailable");
                    None
                }
            });
        let panel_open = prefs.panel_open;
        let app = Self {
            session,
            session_snapshot: initial_update.snapshot,
            session_frame: initial_update.newest_frame,
            host_input,
            gamepads,
            joystick_binding,
            last_joystick_sent: None,
            title: String::from("IzarraVM"),
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
            frame_seq: u64::MAX,
            metrics_mark: None,
            frames_since: 0,
            host_fps: 0.0,
            floppy_access_seen: 0,
            c_access_seen: 0,
            floppy_access_at: None,
            c_access_at: None,
            cd_access_seen: 0,
            cd_access_at: None,
            show_com1: false,
            show_about: false,
            show_license: false,
            volume,
            gain,
            crt_style,
            input_release,
            fullscreen_key,
            config_dialog: None,
            prefs,
            prefs_path,
            panel_open,
            logo: None,
        };
        if initial_prefs_changed {
            app.save_prefs();
        }
        Ok(app)
    }

    fn poll_session(&mut self) {
        let update = self.session.poll();
        self.session_snapshot = update.snapshot;
        if let Some(frame) = update.newest_frame {
            // FOLD, do not overwrite. The frame already sitting here was polled
            // and never painted -- egui may have discarded the pass, or two
            // polls may fall between two paints -- and its scanline runs are the
            // only record that those rows moved. Dropping it would leave them
            // stale on the texture until something happened to repaint them.
            self.session_frame = Some(match self.session_frame.take() {
                Some(unpainted) => merge_session_frames(unpainted, frame),
                None => frame,
            });
        }
        let mut prefs_changed = false;
        for event in update.events {
            match event {
                SessionEvent::Applied { state, .. } => {
                    prefs_changed |= apply_session_preference(&mut self.prefs, state);
                }
                SessionEvent::Rejected {
                    request_id,
                    kind,
                    message,
                } => {
                    error!(?request_id, ?kind, %message, "session request rejected");
                }
                SessionEvent::WorkerFailed {
                    generation,
                    message,
                } => {
                    error!(generation, %message, "emulation worker failed");
                }
                SessionEvent::FinalizationFailed {
                    generation,
                    message,
                } => {
                    error!(generation, %message, "session finalization failed");
                }
            }
        }
        if !self.session_snapshot.powered {
            self.session_frame = None;
            self.frame_seq = u64::MAX;
        }
        if prefs_changed {
            self.save_prefs();
        }
    }

    fn request_session(&mut self, request: SessionRequest) -> Result<RequestId, SessionClosed> {
        self.session.request(request)
    }

    fn reset_presentation_state(&mut self) {
        self.frame_seq = u64::MAX;
        self.session_frame = None;
        self.last_joystick_sent = None;
        self.guest_locks = [false; HOST_LOCK_KEYS.len()];
    }

    fn log_shutdown_report(action: &'static str, report: ShutdownReport) {
        if !report.was_running {
            return;
        }
        for failure in report.failures {
            error!(%failure, generation = ?report.generation, action, "session finalization failure");
        }
    }

    fn reset_session(&mut self) {
        self.reset_presentation_state();
        match self.session.reset() {
            Ok(report) => {
                for failure in report.failures {
                    error!(%failure, generation = report.generation, "session reset media failure");
                }
            }
            Err(err) => error!(%err, "failed to reset GUI session"),
        }
        self.poll_session();
    }

    fn power_off_session(&mut self) {
        self.reset_presentation_state();
        let report = self.session.power_off();
        Self::log_shutdown_report("power off", report);
        self.poll_session();
    }

    fn power_on_session(&mut self) {
        self.reset_presentation_state();
        if let Err(err) = self.session.power_on() {
            error!(%err, "failed to power on GUI session");
        }
        self.poll_session();
    }

    fn shutdown_for_exit(&mut self) {
        let report = self.session.shutdown();
        Self::log_shutdown_report("shutdown", report);
        self.poll_session();
        self.save_prefs();
        self.reset_presentation_state();
    }
}

impl Drop for GuiApp {
    fn drop(&mut self) {
        let report = self.session.shutdown();
        Self::log_shutdown_report("drop", report);
        self.poll_session();
        // Save-on-exit as a backstop; changes are already persisted as they
        // happen, so this just catches anything not yet flushed.
        self.save_prefs();
    }
}

/// Fold an unpainted frame into the one that superseded it.
///
/// The newer frame's pixels are already the whole current picture, so only the
/// damage has to be carried: the result spans both publications and reports the
/// union of their runs. Frames from different worker generations do not compose
/// -- a machine reset republishes from a fresh screen -- so those take the newer
/// frame alone, and the publication gap makes the consumer upload it whole.
fn merge_session_frames(unpainted: SessionFrame, newer: SessionFrame) -> SessionFrame {
    if unpainted.generation != newer.generation
        || unpainted.width != newer.width
        || unpainted.height != newer.height
        || unpainted.update_to.wrapping_add(1) != newer.update_from
    {
        return newer;
    }
    SessionFrame {
        changed_rows: merge_row_runs(&unpainted.changed_rows, &newer.changed_rows),
        update_from: unpainted.update_from,
        ..newer
    }
}

/// Union two ascending, non-overlapping run lists into one of the same shape.
fn merge_row_runs(
    left: &[std::ops::Range<usize>],
    right: &[std::ops::Range<usize>],
) -> Vec<std::ops::Range<usize>> {
    let mut runs: Vec<std::ops::Range<usize>> = left.iter().chain(right).cloned().collect();
    runs.sort_by_key(|run| run.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(runs.len());
    for run in runs {
        match merged.last_mut() {
            // Touching runs coalesce as well as overlapping ones: two adjacent
            // runs are one upload, and one upload is cheaper than two.
            Some(last) if run.start <= last.end => last.end = last.end.max(run.end),
            _ => merged.push(run),
        }
    }
    merged
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
        if !self.session_snapshot.powered {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            return;
        }
        let frame = self.session_frame.take().and_then(|frame| {
            if frame.width == 0 || frame.seq == self.frame_seq {
                return None;
            }
            self.frame_seq = frame.seq;
            Some(crate::crt::CrtFrame {
                words: frame.words,
                changed_rows: frame.changed_rows,
                width: frame.width as u32,
                height: frame.height as u32,
                update_from: frame.update_from,
                update_to: frame.update_to,
            })
        });
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
        if self
            .host_input
            .mouse_capture_requested(response.clicked(), self.input_captured)
        {
            self.want_capture = true;
        }
    }

    /// Forward already-translated Set 1 bytes to the emulation thread. Empty
    /// slices (an unmapped key, nothing held) are dropped.
    fn send_keys_to_guest(&self, codes: Vec<u8>) {
        if !self.host_input.keyboard_enabled() || codes.is_empty() {
            return;
        }
        let _ = self.session.send_input(GuestInput::Keys(codes));
    }

    /// The guest's published vertical refresh rate, used to pace the host
    /// redraw. Falls back to 60 Hz when no machine is running or the guest has
    /// not reported a rate yet.
    fn guest_refresh_hz(&self) -> f64 {
        let hz = self.session_snapshot.refresh_hz;
        if self.session_snapshot.powered && hz > 0.0 {
            hz
        } else {
            60.0
        }
    }

    /// Whether monitor_ui flagged a click-to-capture this frame, clearing it.
    fn take_want_capture(&mut self) -> bool {
        let requested = std::mem::take(&mut self.want_capture);
        self.host_input.mouse_enabled() && requested
    }

    fn guest_mouse_active(&self) -> bool {
        self.host_input.mouse_active(self.input_captured)
    }

    /// Enter or leave input capture. While captured we lock and hide the OS cursor
    /// (winit Locked: pinned in place, cannot move on screen or leave the window)
    /// and route keyboard and mouse to the guest, which draws its own cursor.
    /// The input-release hotkey (Super+F2 by default) releases it. The host
    /// keyboard hook holds the Super keys away from the shell for as long as
    /// capture lasts, so a stray Super press cannot open the Start menu and
    /// take the focus off the guest.
    /// Locked delivers motion as raw relative deltas, which we
    /// accumulate into the guest cursor position (clamped to the screen), so there
    /// is nothing for the OS cursor to escape and no warp to fight. On release we
    /// flush any held keys so nothing sticks down in the guest.
    fn toggle_capture(&mut self, window: &winit::window::Window, kbd: &mut HostKeyboard) {
        if !self.input_captured && !self.host_input.mouse_enabled() {
            self.want_capture = false;
            return;
        }
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
            let releases = self.host_input.release_scancodes(kbd);
            self.send_keys_to_guest(releases);
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
        if !self.guest_mouse_active() {
            self.last_buttons = 0;
            return;
        }
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
        let _ = self
            .session
            .send_input(GuestInput::MouseRelative(dx, dy, self.last_buttons));
    }

    /// Forward host scroll-wheel motion to the guest. `lines` is signed notches
    /// (positive = scroll-up); fractional pixel-delta accumulates so only whole
    /// detents are sent, one +/-1 command per notch.
    fn forward_guest_wheel(&mut self, lines: f32) {
        if !self.guest_mouse_active() {
            self.wheel_accum = 0.0;
            return;
        }
        self.wheel_accum += lines;
        while self.wheel_accum >= 1.0 {
            let _ = self.session.send_input(GuestInput::MouseWheel(1));
            self.wheel_accum -= 1.0;
        }
        while self.wheel_accum <= -1.0 {
            let _ = self.session.send_input(GuestInput::MouseWheel(-1));
            self.wheel_accum += 1.0;
        }
    }

    /// Accumulate raw relative mouse motion (mickeys) for the next per-frame flush.
    /// The guest driver applies its ratio and clamps to the video mode's range, so
    /// the host forwards the raw counts unscaled and unclamped.
    fn accumulate_guest_motion(&mut self, dx: f32, dy: f32) {
        if !self.guest_mouse_active() {
            self.mouse_rel_x = 0.0;
            self.mouse_rel_y = 0.0;
            self.mouse_dirty = false;
            return;
        }
        self.mouse_rel_x += dx;
        self.mouse_rel_y += dy;
        self.mouse_dirty = true;
    }

    /// Send the motion accumulated since the last flush as one coalesced relative
    /// packet, if any. The caller paces this separately from rendering so an 8000
    /// Hz mouse drives the guest at MOUSE_FLUSH_HZ, not at the host polling rate.
    fn flush_guest_motion(&mut self) {
        if !self.guest_mouse_active() {
            self.mouse_rel_x = 0.0;
            self.mouse_rel_y = 0.0;
            self.mouse_dirty = false;
            return;
        }
        if !self.mouse_dirty {
            return;
        }
        self.mouse_dirty = false;
        let dx = self.mouse_rel_x as i32;
        let dy = self.mouse_rel_y as i32;
        self.mouse_rel_x = 0.0;
        self.mouse_rel_y = 0.0;
        let _ = self
            .session
            .send_input(GuestInput::MouseRelative(dx, dy, self.last_buttons));
    }

    /// Drain host-controller events and forward only changed 8-bit gameport samples.
    fn poll_joystick(&mut self) {
        let completed = if let Some(gamepads) = &mut self.gamepads {
            let wizard = self
                .config_dialog
                .as_mut()
                .and_then(|dialog| dialog.joystick_wizard.as_mut());
            gamepads.poll_wizard(wizard);
            self.config_dialog
                .as_ref()
                .and_then(|dialog| dialog.joystick_wizard.as_ref())
                .and_then(JoystickWizard::binding)
        } else {
            None
        };
        if let Some(binding) = completed
            && let Some(dialog) = &mut self.config_dialog
        {
            dialog.joystick_binding = Some(binding);
            dialog.joystick_wizard = None;
        }

        let sample = if self.host_input.joystick_enabled() {
            self.gamepads
                .as_ref()
                .zip(self.joystick_binding.as_ref())
                .and_then(|(gamepads, binding)| gamepads.sample(binding))
        } else {
            None
        };
        if self.last_joystick_sent.as_ref() == Some(&sample) {
            return;
        }
        self.last_joystick_sent = Some(sample);
        let _ = self
            .session
            .send_input(GuestInput::Joystick(sample.map(|sample| JoystickState {
                x: sample.x,
                y: sample.y,
                buttons: sample.buttons,
            })));
    }

    /// Mirror the host's NumLock/CapsLock/ScrollLock onto the guest. Each lock
    /// that differs gets a make+break injected, which the BIOS INT 09h handler
    /// toggles once (guarded by its held-flag). Runs every frame, so it also
    /// catches the host toggling a lock mid-session, not just the load.
    fn sync_guest_locks(&mut self) {
        if !self.host_input.keyboard_enabled() {
            self.guest_locks = [false; HOST_LOCK_KEYS.len()];
            return;
        }
        if !self.session_snapshot.powered {
            return;
        }
        for (i, (vk, make)) in HOST_LOCK_KEYS.iter().enumerate() {
            let Some(host_on) = host_lock_on(*vk) else {
                return;
            };
            if host_on != self.guest_locks[i] {
                let _ = self
                    .session
                    .send_input(GuestInput::Keys(vec![*make, *make | 0x80]));
                self.guest_locks[i] = host_on;
            }
        }
    }
}

impl GuiApp {
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
        let cd_audio = self.session_snapshot.cd_audio;

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
                let mounted = self.session_snapshot.floppy_source.is_some();
                if eject_button(ui, running && mounted) {
                    self.eject_floppy_action();
                }
            });
            ui.label(
                egui::RichText::new(
                    self.session_snapshot
                        .floppy_label
                        .as_deref()
                        .unwrap_or("(empty)"),
                )
                .color(MUTED)
                .italics()
                .size(11.0),
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(running, egui::Button::new("Load Floppy Image"))
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
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x3D, 0x38, 0x2D)),
                );
                if eject_button(ui, cd_eject_enabled(running, cd_audio)) {
                    self.eject_cd_action();
                }
            });
            ui.label(
                egui::RichText::new(
                    self.session_snapshot
                        .cd_label
                        .as_deref()
                        .unwrap_or("(empty)"),
                )
                .color(MUTED)
                .italics()
                .size(11.0),
            );
            // Transport first: the audio controls sit directly under the tray,
            // and the media buttons below them.
            ui.horizontal(|ui| {
                // One button toggles play and pause, the way a drive's own
                // front panel does, so playing no longer leaves a dead control.
                let (icon, tip, request) = if cd_audio.playing {
                    (TransportIcon::Pause, "Pause", SessionRequest::CdPause)
                } else {
                    (TransportIcon::Play, "Play", SessionRequest::CdPlay)
                };
                if transport_button(ui, cd_transport_enabled(running, cd_audio), icon, tip) {
                    let _ = self.request_session(request);
                }
                if transport_button(
                    ui,
                    cd_skip_enabled(running, cd_audio),
                    TransportIcon::SkipNext,
                    "Next track",
                ) {
                    let _ = self.request_session(SessionRequest::CdNextTrack);
                }
                if transport_button(
                    ui,
                    cd_stop_enabled(running, cd_audio),
                    TransportIcon::Stop,
                    "Stop",
                ) {
                    let _ = self.request_session(SessionRequest::CdStop);
                }
                ui.add_space(2.0);
                volume_icon(ui);
                ui.add_space(4.0);
                let percent = cd_level_percent(cd_audio.left_level, cd_audio.right_level);
                if let Some(moved) = cd_fader(ui, running, percent) {
                    let _ = self
                        .request_session(SessionRequest::CdLinkedLevel(cd_percent_level(moved)));
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(running, egui::Button::new("Load CD Image"))
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
                open_in_file_manager(&self.session_snapshot.c_drive);
            }
            // Blank line holds the box at its prior height now that the path
            // label is gone.
            ui.label(egui::RichText::new(" ").size(11.0));
        });
    }

    /// Eject drive A: and forget the mount so it is not restored next launch.
    fn eject_floppy_action(&mut self) {
        let _ = self.request_session(SessionRequest::EjectFloppy);
    }

    /// Eject the CD and forget the mount so it is not restored next launch.
    fn eject_cd_action(&mut self) {
        let _ = self.request_session(SessionRequest::EjectCd);
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
        match PreparedFloppy::from_source(source) {
            Ok(floppy) => {
                let _ = self.request_session(SessionRequest::MountFloppy(floppy));
            }
            Err(err) => error!(%err, "failed to prepare floppy image"),
        }
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
        self.mount_cd_source(CdSource::Image(path.to_path_buf()));
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
        self.mount_cd_source(CdSource::Folder(dir.to_path_buf()));
    }

    fn mount_cd_source(&mut self, source: CdSource) {
        match PreparedCd::from_source(source) {
            Ok(cd) => {
                let _ = self.request_session(SessionRequest::MountCd(cd));
            }
            Err(err) => error!(%err, "failed to prepare CD media"),
        }
    }
}

fn cd_level_percent(left: u8, right: u8) -> u8 {
    let sum = u16::from(left.min(31)) + u16::from(right.min(31));
    ((sum * 100 + 31) / 62) as u8
}

fn cd_eject_enabled(running: bool, state: CdAudioState) -> bool {
    running && state.media_present
}

/// Play/pause is live whenever the drive holds a disc with an audio track. It
/// stays live while playing, because the same button then pauses.
fn cd_transport_enabled(running: bool, state: CdAudioState) -> bool {
    running && state.media_present && state.audio_capable
}

/// Skip needs a track to skip to, so it also waits on live playback.
fn cd_skip_enabled(running: bool, state: CdAudioState) -> bool {
    cd_transport_enabled(running, state) && state.has_next_track
}

/// Stop is live only while there is playback to stop.
fn cd_stop_enabled(running: bool, state: CdAudioState) -> bool {
    running && (state.playing || state.paused)
}

fn cd_percent_level(percent: u8) -> u8 {
    ((u16::from(percent.min(100)) * 31 + 50) / 100) as u8
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

#[cfg(test)]
#[path = "gui_test.rs"]
mod tests;
