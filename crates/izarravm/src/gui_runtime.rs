// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
    // Physical modifier state, tracked so the configurable hotkeys (input
    // release, fullscreen) and the rebind capture can read the live combo.
    ctrl_down: bool,
    shift_down: bool,
    alt_down: bool,
    // Whether the window is currently fullscreen, toggled by the fullscreen hotkey.
    is_fullscreen: bool,
    // Whether our window has keyboard focus. Raw device key events are global, so
    // we only forward them to the guest while focused.
    focused: bool,
    // Set once any raw DeviceEvent::Key arrives. From then the guest keyboard is
    // driven by the raw path (immune to the Windows NumLock/fake-shift mangling
    // that drops numpad releases on the cooked WindowEvent path); the cooked path
    // is the fallback only until/unless raw events appear (e.g. on Wayland).
    raw_keys: bool,
    // Host shortcuts are withheld from the guest; remember their trigger key so
    // repeats and the matching release edge are swallowed too.
    host_hotkeys_down: Vec<winit::keyboard::KeyCode>,
    window: Option<Arc<Window>>,
    wgpu: Option<WgpuState>,
    egui_ctx: egui::Context,
    egui_winit: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    // When the next frame is due. about_to_wait paces redraws to the guest
    // refresh rate with ControlFlow::WaitUntil rather than spinning at host vsync.
    next_frame: Instant,
    // When the next mouse-motion flush is due, paced independently of next_frame
    // at MOUSE_FLUSH_HZ (see its doc comment).
    next_mouse_flush: Instant,
    // Raw mouse motion the Windows WM_INPUT hook accumulates between frames; drained
    // each frame in about_to_wait. Always zero on platforms without the hook.
    raw_mouse: RawMouseAccum,
}

impl WinitApp {
    /// Draw one frame: run the egui pass and present it. Called every frame from
    /// about_to_wait, and on demand for OS-driven repaints (resize). Driving the
    /// steady-state redraw from about_to_wait rather than request_redraw matters
    /// on Windows: request_redraw posts WM_PAINT, the lowest-priority message,
    /// which a high-polling-rate mouse (8000 Hz of WM_INPUT) starves out, dropping
    /// the host frame rate. winit dispatches about_to_wait from its own loop
    /// bookkeeping, so it survives the flood.
    fn render(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(egui_winit), Some(wgpu), Some(renderer)) = (
            self.window.as_ref(),
            self.egui_winit.as_mut(),
            self.wgpu.as_mut(),
            self.egui_renderer.as_mut(),
        ) else {
            return;
        };
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

    /// Translate one physical key transition to the guest. The configurable input
    /// release and fullscreen hotkeys are intercepted (and withheld from the
    /// guest); a pending rebind capture swallows the next key; everything else
    /// goes through HostKeyboard and on to the emulation thread. Used by both the
    /// raw DeviceEvent::Key path and the cooked WindowEvent fallback.
    fn handle_guest_key(&mut self, code: winit::keyboard::KeyCode, pressed: bool, repeat: bool) {
        use winit::keyboard::KeyCode;
        // Track modifiers (still forwarded to the guest below).
        match code {
            KeyCode::ControlLeft | KeyCode::ControlRight => self.ctrl_down = pressed,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.shift_down = pressed,
            KeyCode::AltLeft | KeyCode::AltRight => self.alt_down = pressed,
            _ => {}
        }
        let is_modifier = matches!(
            code,
            KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
        );
        // The winit KeyCode debug name is the binding's key identity (e.g. "F2").
        let name = format!("{code:?}");
        let (ctrl, shift, alt) = (self.ctrl_down, self.shift_down, self.alt_down);

        if !pressed {
            if let Some(index) = self.host_hotkeys_down.iter().position(|held| *held == code) {
                self.host_hotkeys_down.swap_remove(index);
                return;
            }
        } else if self.host_hotkeys_down.contains(&code) {
            return;
        }

        // The config dialog is capturing a rebind: take the next non-modifier key
        // as the new combo and swallow it.
        if pressed && !is_modifier && self.gui.is_capturing_bind() {
            self.gui.record_bind(&name, ctrl, shift, alt);
            return;
        }
        if pressed
            && !repeat
            && self.gui.input_captured
            && self.gui.input_release.matches(&name, ctrl, shift, alt)
        {
            self.host_hotkeys_down.push(code);
            if let Some(window) = self.window.clone() {
                self.gui.toggle_capture(&window, &mut self.host_kbd);
            }
            return;
        }
        if pressed && !repeat && self.gui.fullscreen_key.matches(&name, ctrl, shift, alt) {
            self.host_hotkeys_down.push(code);
            self.toggle_fullscreen();
            return;
        }

        let codes = self.host_kbd.key_with_repeat(code, pressed, repeat);
        self.gui.send_keys_to_guest(codes);
    }

    /// Toggle borderless fullscreen on the window.
    fn toggle_fullscreen(&mut self) {
        self.is_fullscreen = !self.is_fullscreen;
        if let Some(window) = &self.window {
            let mode = self
                .is_fullscreen
                .then_some(winit::window::Fullscreen::Borderless(None));
            window.set_fullscreen(mode);
        }
    }
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("IzarraVM")
            .with_window_icon(star_window_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        // Ask winit for raw device input while focused, so the guest keyboard can
        // read DeviceEvent::Key (Win32 Raw Input) instead of the cooked
        // WindowEvent path, and raw mouse motion for capture.
        event_loop.listen_device_events(winit::event_loop::DeviceEvents::WhenFocused);

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
        let mut egui_renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        egui_renderer
            .callback_resources
            .insert(crate::crt::CrtResources::new(&device, &queue, format));

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
        // The guest keyboard is driven by raw DeviceEvent::Key (see device_event),
        // which is immune to the Windows NumLock/fake-shift mangling that drops
        // numpad releases on this cooked WindowEvent path. The cooked path is the
        // fallback only until a raw key event arrives (e.g. on Wayland, where
        // device key events may not fire). Either way keys never reach egui (no
        // text widgets), so this arm consumes them.
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
        {
            if !self.raw_keys {
                if let PhysicalKey::Code(code) = key_event.physical_key {
                    self.handle_guest_key(
                        code,
                        key_event.state == ElementState::Pressed,
                        key_event.repeat,
                    );
                }
            }
            return;
        }
        if let WindowEvent::Focused(focused) = &event {
            self.focused = *focused;
            if !*focused {
                // Release everything held so a key down at the moment of an
                // alt-tab (Shift, in a game) does not stick in the guest.
                self.gui.send_keys_to_guest(self.host_kbd.release_all());
                self.ctrl_down = false;
                self.shift_down = false;
                self.alt_down = false;
                self.host_hotkeys_down.clear();
                return;
            }
            // Focused(true): fall through so egui also observes regained focus.
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
            if let WindowEvent::MouseWheel { delta, .. } = &event {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 120.0,
                };
                self.gui.forward_guest_wheel(lines);
                return;
            }
            if matches!(event, WindowEvent::CursorMoved { .. }) {
                return;
            }
        }

        // Let egui observe the event for its own input handling. Scope the
        // borrow so the match arms below can take &mut self to render.
        if let (Some(window), Some(egui_winit)) = (self.window.as_ref(), self.egui_winit.as_mut()) {
            let _ = egui_winit.on_window_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => {
                self.gui.shutdown_for_exit();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(wgpu) = self.wgpu.as_mut() {
                    wgpu.config.width = size.width.max(1);
                    wgpu.config.height = size.height.max(1);
                    wgpu.surface.configure(&wgpu.device, &wgpu.config);
                }
                self.render(event_loop);
            }
            WindowEvent::RedrawRequested => self.render(event_loop),
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        match event {
            // Raw keyboard (Win32 Raw Input on Windows): true hardware make/break
            // per physical key, immune to the cooked-path NumLock/fake-shift
            // mangling that drops numpad releases. This is the guest keyboard.
            DeviceEvent::Key(raw) => {
                let first_raw = !self.raw_keys;
                self.raw_keys = true;
                if self.focused {
                    if let PhysicalKey::Code(code) = raw.physical_key {
                        let pressed = raw.state == ElementState::Pressed;
                        let repeat = pressed && !first_raw && self.host_kbd.is_held(code);
                        self.handle_guest_key(code, pressed, repeat);
                    }
                }
            }
            // Raw relative pointer motion drives the captured guest cursor.
            DeviceEvent::MouseMotion { delta } if self.gui.input_captured => {
                self.gui
                    .accumulate_guest_motion(delta.0 as f32, delta.1 as f32);
            }
            _ => {}
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
        // Apply the raw mouse motion the Windows WM_INPUT hook accumulated since
        // the last pass (zero elsewhere, where DeviceEvent::MouseMotion drives
        // capture directly) every time, regardless of which deadline below is
        // due, so it is never left stranded in raw_mouse between flushes.
        let now = Instant::now();
        let (rdx, rdy) = self.raw_mouse.take();
        if self.gui.input_captured && (rdx != 0 || rdy != 0) {
            self.gui.accumulate_guest_motion(rdx as f32, rdy as f32);
        }
        // Flush mouse motion on its own, faster cadence (MOUSE_FLUSH_HZ),
        // independent of rendering: see that constant's doc comment.
        if now >= self.next_mouse_flush {
            self.gui.flush_guest_motion();
            self.next_mouse_flush = now + Duration::from_secs_f64(1.0 / MOUSE_FLUSH_HZ);
        }
        // Pace rendering to the guest refresh rate. Render directly here once the
        // deadline elapses rather than via request_redraw: winit dispatches
        // about_to_wait from its own loop, so it keeps firing under a mouse-event
        // flood that would starve the WM_PAINT request_redraw posts on Windows.
        if now >= self.next_frame {
            self.render(event_loop);
            let hz = self.gui.guest_refresh_hz().max(1.0);
            self.next_frame = now + Duration::from_secs_f64(1.0 / hz);
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            self.next_frame.min(self.next_mouse_flush),
        ));
    }
}

/// Accumulated raw mouse motion (relative counts) shared between the Windows
/// WM_INPUT message hook and the event loop. On other platforms nothing writes it
/// and captured motion still comes from winit's DeviceEvent::MouseMotion.
#[derive(Clone, Default)]
struct RawMouseAccum(Rc<Cell<(i64, i64)>>);

impl RawMouseAccum {
    /// Take the accumulated delta and reset it to zero.
    fn take(&self) -> (i64, i64) {
        self.0.replace((0, 0))
    }
}

/// Read the relative motion out of a WM_INPUT mouse packet. Returns None for
/// keyboard packets (so the caller lets winit handle them and the raw keyboard
/// path is preserved) and for absolute-pointer packets (tablets).
#[cfg(windows)]
fn read_raw_mouse_delta(
    msg: &windows_sys::Win32::UI::WindowsAndMessaging::MSG,
) -> Option<(i32, i32)> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEMOUSE,
    };
    unsafe {
        let mut data: RAWINPUT = zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let header = size_of::<RAWINPUTHEADER>() as u32;
        let read = GetRawInputData(
            msg.lParam as HRAWINPUT,
            RID_INPUT,
            &mut data as *mut _ as *mut _,
            &mut size,
            header,
        );
        if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE {
            return None;
        }
        let mouse = data.data.mouse;
        // Bit 0 of usFlags is MOUSE_MOVE_ABSOLUTE; clear means relative motion.
        if mouse.usFlags & 1 != 0 {
            return None;
        }
        Some((mouse.lLastX, mouse.lLastY))
    }
}

/// Build the event loop. On Windows it installs a WM_INPUT hook that drains mouse
/// raw input into `raw_mouse` and swallows those messages, so an 8000 Hz mouse
/// never reaches winit's per-report handler (three DeviceEvents each, which
/// starves the loop). Keyboard raw input falls through to winit unchanged.
#[cfg(windows)]
fn build_event_loop(
    raw_mouse: RawMouseAccum,
) -> Result<EventLoop<()>, winit::error::EventLoopError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, MSG, WM_INPUT, WM_MOUSEMOVE,
    };
    use winit::platform::windows::EventLoopBuilderExtWindows;
    let mut builder = EventLoop::builder();
    // Last legacy mouse-move we let through, to throttle the flood below.
    let last_move = Cell::new(None::<Instant>);
    builder.with_msg_hook(move |ptr| {
        let msg = unsafe { &*(ptr as *const MSG) };
        match msg.message {
            WM_INPUT => match read_raw_mouse_delta(msg) {
                Some((dx, dy)) => {
                    let (ax, ay) = raw_mouse.0.get();
                    raw_mouse.0.set((ax + dx as i64, ay + dy as i64));
                    // Clean up the raw input the way the default handler would have.
                    unsafe { DefWindowProcW(msg.hwnd, msg.message, msg.wParam, msg.lParam) };
                    true
                }
                None => false,
            },
            // Legacy WM_MOUSEMOVE still arrives (RIDEV_NOLEGACY would break window
            // dragging and resizing). Each one DefWindowProc'd synchronously
            // re-enters the window proc for WM_NCHITTEST + WM_SETCURSOR, and an
            // 8000 Hz mouse makes ~1000 of those a second, which halves the frame
            // rate while the cursor is visible. egui only needs the latest cursor
            // position per frame, so let one through every 8 ms (for hover and
            // clicks) and drop the rest WITHOUT DefWindowProc, so the hit-test and
            // set-cursor chain never fires for them. The OS still moves the visible
            // cursor sprite regardless; these messages are only notifications.
            WM_MOUSEMOVE => {
                let now = Instant::now();
                let recent = last_move
                    .get()
                    .is_some_and(|t| now.duration_since(t) < Duration::from_millis(8));
                if recent {
                    true
                } else {
                    last_move.set(Some(now));
                    false
                }
            }
            _ => false,
        }
    });
    builder.build()
}

#[cfg(not(windows))]
fn build_event_loop(
    _raw_mouse: RawMouseAccum,
) -> Result<EventLoop<()>, winit::error::EventLoopError> {
    EventLoop::builder().build()
}

/// Open the window and run the emulator. Returns when the user closes it.
#[allow(clippy::too_many_arguments)]
pub fn run(
    profile: MachineProfile,
    rom: Vec<u8>,
    c_drive: PathBuf,
    cd_image: Option<PathBuf>,
    midi_config: MidiConfig,
    glide_ovl: Option<Vec<u8>>,
    test_pattern: bool,
    rtc_setup: crate::cmos::RtcSetup,
) -> Result<(), Box<dyn Error>> {
    let raw_mouse = RawMouseAccum::default();
    let event_loop = build_event_loop(raw_mouse.clone())?;
    let gui = GuiApp::new(
        profile,
        rom,
        c_drive,
        cd_image,
        midi_config,
        glide_ovl,
        test_pattern,
        rtc_setup,
    );
    let egui_ctx = egui::Context::default();
    enlarge_ui_fonts(&egui_ctx);
    apply_black_theme(&egui_ctx);
    let mut app = WinitApp {
        gui,
        host_kbd: HostKeyboard::default(),
        ctrl_down: false,
        shift_down: false,
        alt_down: false,
        is_fullscreen: false,
        focused: true,
        raw_keys: false,
        host_hotkeys_down: Vec::new(),
        window: None,
        wgpu: None,
        egui_ctx,
        egui_winit: None,
        egui_renderer: None,
        next_frame: Instant::now(),
        next_mouse_flush: Instant::now(),
        raw_mouse,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
