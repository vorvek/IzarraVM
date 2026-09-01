// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GuiApp {
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
        let running = self.session_snapshot.powered;
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
                        self.reset_session();
                    }
                    if ui
                        .add_sized(
                            [48.0, 48.0],
                            egui::Button::new(egui::RichText::new("POWER").size(13.0)),
                        )
                        .clicked()
                    {
                        if running {
                            self.power_off_session();
                        } else {
                            self.power_on_session();
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
                        .circle_stroke(c, 6.0, egui::Stroke::new(1.0_f32, BEVEL_LO));
                });
            },
        );
    }

    fn panel_body(&mut self, ui: &mut egui::Ui) {
        let running = self.session_snapshot.powered;
        let (mode, speed, idle, floppy_accesses, c_accesses, cd_accesses) = if running {
            (
                self.session_snapshot.mode,
                self.session_snapshot.speed_ratio,
                self.session_snapshot.idle,
                self.session_snapshot.floppy_accesses,
                self.session_snapshot.c_accesses,
                self.session_snapshot.cd_accesses,
            )
        } else {
            (
                None,
                0.0,
                false,
                self.floppy_access_seen,
                self.c_access_seen,
                self.cd_access_seen,
            )
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
        let mode = mode.unwrap_or(self.session_snapshot.configured_mode);
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.separator();
                let line = |ui: &mut egui::Ui, text: String| {
                    ui.label(egui::RichText::new(text).color(MUTED).size(12.0));
                };
                // CPU and mode line, with the COM1 toggle aligned to its right.
                ui.horizontal(|ui| {
                    line(ui, cpu_mode_label(mode));
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
                    let text = if idle {
                        format!("Idle - {} MB", self.session_snapshot.memory_mib)
                    } else {
                        format!(
                            "Speed {:.0}% - {} MB",
                            speed * 100.0,
                            self.session_snapshot.memory_mib
                        )
                    };
                    line(ui, text);
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
                //
                // This is the HOST's level -- the powered speakers the machine's
                // line-out feeds -- applied to the finished mix on its way to the
                // sound device. The levels inside the machine are the guest's,
                // on the card's own mixer, and SNDMIXER.COM sets them.
                //
                // The travel runs past unity to MAX_VOLUME, the way a powered
                // speaker's knob does: a title that maxes its own mixer can still
                // arrive 14 dB down (see `volume_gain`). The value box reads in
                // percent so the neutral point is named, and the 0.01 step means
                // every position the knob can hold is a whole percent -- keyboard
                // arrows and typed values land on one, and 100% is reachable
                // exactly rather than approached.
                //
                // The box is editable, so it needs a parser in the units it
                // prints as much as it needs the formatter; `volume_percent_to_
                // fraction` says what egui's default gets wrong.
                ui.horizontal(|ui| {
                    volume_icon(ui);
                    ui.add_space(4.0);
                    ui.spacing_mut().slider_width = (ui.available_width() - 56.0).max(40.0);
                    let slider = ui
                        .add(
                            egui::Slider::new(&mut self.volume, 0.0..=MAX_VOLUME)
                                .step_by(0.01)
                                // The rail is the faceplate colour, so without
                                // a trailing fill the travelled part of the
                                // track reads no differently from the rest.
                                .trailing_fill(true)
                                .custom_formatter(|value, _| format!("{:.0}%", value * 100.0))
                                .custom_parser(volume_percent_to_fraction),
                        )
                        .on_hover_text(
                            "Speaker volume. This is the host's playback level; 100% is unity and \
                             above it amplifies. The machine's own mixer levels are set in DOS \
                             with SNDMIXER.",
                        );
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
        let midi_config = self.session_snapshot.midi_config.clone();
        self.config_dialog = Some(ConfigDialog {
            page: ConfigPage::Settings,
            start_fullscreen: self.prefs.start_fullscreen,
            mouse_sensitivity: self.prefs.mouse_sensitivity,
            input_release: self.input_release.clone(),
            fullscreen: self.fullscreen_key.clone(),
            screenshot: self.screenshot_key.clone(),
            crt_style: self.crt_style,
            monitor_gamma: self.monitor_gamma,
            glide_gamma: self.glide_gamma,
            glide_texture_filter: self.glide_texture_filter,
            midi_backend: midi_config.backend,
            external_midi_port: midi_config.external_port,
            soundfont: midi_config.soundfont,
            mt32_control_rom: path_text(midi_config.mt32_control_rom.as_ref()),
            mt32_pcm_rom: path_text(midi_config.mt32_pcm_rom.as_ref()),
            midi_ports: MidiEngine::external_ports(),
            capturing: None,
        });
    }

    /// True while the dialog is waiting to capture a hotkey, so the event loop
    /// swallows the next key instead of toggling capture or forwarding to the guest.
    pub(super) fn is_capturing_bind(&self) -> bool {
        self.config_dialog
            .as_ref()
            .is_some_and(|d| d.capturing.is_some())
            || self
                .controller_setup
                .as_ref()
                .is_some_and(|setup| setup.capturing_key.is_some())
    }

    pub(super) fn is_capturing_controller_key(&self) -> bool {
        self.controller_setup
            .as_ref()
            .is_some_and(|setup| setup.capturing_key.is_some())
    }

    pub(super) fn is_editing_profile_name(&self) -> bool {
        self.controller_setup.as_ref().is_some_and(|setup| {
            matches!(
                setup.profile_prompt.as_ref(),
                Some(ControllerProfilePrompt::Add { .. })
            )
        })
    }

    pub(super) fn cancel_bind_capture(&mut self) {
        if let Some(dialog) = self.config_dialog.as_mut() {
            dialog.capturing = None;
        }
        if let Some(setup) = self.controller_setup.as_mut() {
            setup.capturing_key = None;
        }
    }

    /// Record a captured combo into the staged binding the dialog is waiting on,
    /// then stop capturing.
    pub(super) fn record_bind(
        &mut self,
        code: winit::keyboard::KeyCode,
        ctrl: bool,
        shift: bool,
        alt: bool,
        super_key: bool,
    ) {
        if let Some(dialog) = &mut self.config_dialog
            && let Some(target) = dialog.capturing.take()
        {
            let key = format!("{code:?}");
            let binding = KeyBinding::new(ctrl, shift, alt, super_key, &key);
            match target {
                BindTarget::InputRelease => dialog.input_release = binding,
                BindTarget::Fullscreen => dialog.fullscreen = binding,
                BindTarget::Screenshot => dialog.screenshot = binding,
            }
            return;
        }

        let Some(chord) = guest_chord_from_capture(code, ctrl, shift, alt) else {
            return;
        };
        if let Some(setup) = self.controller_setup.as_mut()
            && let Some(index) = setup.capturing_key
            && let Some(binding) = setup
                .staged
                .as_mut()
                .and_then(|config| config.keys.get_mut(index))
        {
            binding.guest = chord;
            setup.capturing_key = None;
        }
    }

    /// Render the settings window and its hotkey and MIDI subwindows.
    fn config_ui(&mut self, ctx: &egui::Context) {
        if self.controller_setup.is_some() {
            return;
        }
        let (wavetable_status, midi_status) = if self.session_snapshot.powered {
            (
                self.session_snapshot.wavetable_status,
                self.session_snapshot.midi_status,
            )
        } else {
            (
                MidiStatus::InitializationFailed,
                MidiStatus::InitializationFailed,
            )
        };
        let Some(mut dialog) = self.config_dialog.take() else {
            return;
        };
        let page = dialog.page;
        let mut keep_open = true;
        let mut accept = false;
        let mut apply = false;
        let mut open_controller_setup = false;
        let mut return_to_settings = false;
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
                        let title = match page {
                            ConfigPage::Settings => "SETTINGS",
                            ConfigPage::Hotkeys => "APPLICATION HOTKEYS",
                            ConfigPage::Midi => "MIDI EMULATION",
                            ConfigPage::Graphics => "GRAPHICS EMULATION",
                        };
                        ui.label(header_text(title, 18.0));
                    });
                    ui.add_space(6.0);

                    match page {
                        ConfigPage::Settings => {
                            ui.label(
                                egui::RichText::new("APPLICATION SETTINGS")
                                    .color(LABEL)
                                    .size(11.0),
                            );
                            beige_group(ui, |ui| {
                                ui.checkbox(&mut dialog.start_fullscreen, "Start in Full Screen");
                                ui.horizontal(|ui| {
                                    ui.label("Mouse sensitivity");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Slider::new(
                                                    &mut dialog.mouse_sensitivity,
                                                    crate::prefs::MIN_MOUSE_SENSITIVITY
                                                        ..=crate::prefs::MAX_MOUSE_SENSITIVITY,
                                                )
                                                .suffix("%")
                                                .logarithmic(true),
                                            );
                                        },
                                    );
                                });
                            });

                            ui.add_space(8.0);
                            let width = ui.available_width();
                            if ui
                                .add_sized(
                                    [width, 30.0],
                                    egui::Button::new("Application Hotkeys..."),
                                )
                                .clicked()
                            {
                                dialog.page = ConfigPage::Hotkeys;
                            }
                            if ui
                                .add_sized(
                                    [width, 30.0],
                                    egui::Button::new("Graphics emulation..."),
                                )
                                .clicked()
                            {
                                dialog.page = ConfigPage::Graphics;
                            }
                            if ui
                                .add_sized(
                                    [width, 30.0],
                                    egui::Button::new("Controller emulation..."),
                                )
                                .clicked()
                            {
                                open_controller_setup = true;
                            }
                            if let Some(profile) = &self.controller_profile {
                                ui.small(format!("Selected controller profile: {profile}"));
                            }
                            if !self.host_input.joystick_enabled() {
                                ui.small("Joystick input is disabled in izarravm.toml.");
                            } else if self.controllers.is_none() {
                                ui.small("Host controller input is unavailable.");
                            }
                            if ui
                                .add_sized([width, 30.0], egui::Button::new("MIDI emulation..."))
                                .clicked()
                            {
                                dialog.page = ConfigPage::Midi;
                            }
                        }
                        ConfigPage::Graphics => {
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
                                            ui.selectable_value(
                                                &mut dialog.crt_style,
                                                CrtStyle::Off,
                                                "No",
                                            );
                                        },
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Monitor gamma");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let is_raw = dialog.monitor_gamma.is_none();
                                            let mut custom = dialog
                                                .monitor_gamma
                                                .unwrap_or(crate::prefs::DEFAULT_MONITOR_GAMMA);
                                            // Greyed out and inert while Raw is selected: a
                                            // number here would otherwise claim an active
                                            // gamma that Raw does not apply.
                                            if ui
                                                .add_enabled(
                                                    !is_raw,
                                                    egui::DragValue::new(&mut custom)
                                                        .range(
                                                            crate::prefs::MIN_MONITOR_GAMMA
                                                                ..=crate::prefs::MAX_MONITOR_GAMMA,
                                                        )
                                                        .speed(0.01)
                                                        .fixed_decimals(2),
                                                )
                                                .changed()
                                            {
                                                dialog.monitor_gamma = Some(custom);
                                            }
                                            ui.selectable_value(
                                                &mut dialog.monitor_gamma,
                                                Some(2.5),
                                                "2.5",
                                            );
                                            ui.selectable_value(
                                                &mut dialog.monitor_gamma,
                                                Some(crate::prefs::DEFAULT_MONITOR_GAMMA),
                                                "2.4",
                                            );
                                            ui.selectable_value(
                                                &mut dialog.monitor_gamma,
                                                Some(2.2),
                                                "2.2",
                                            );
                                            ui.selectable_value(
                                                &mut dialog.monitor_gamma,
                                                None,
                                                "Raw",
                                            );
                                        },
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Glide gamma");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            // Applies to Distira's output only.
                                            // Compatible neutralises the Voodoo
                                            // era's gamma lift, which was meant
                                            // for the darker CRTs of the period;
                                            // Original presents it as the card
                                            // did. Neither setting alters the
                                            // guest's gamma register.
                                            ui.selectable_value(
                                                &mut dialog.glide_gamma,
                                                crate::prefs::GlideGamma::Original,
                                                "Original",
                                            );
                                            ui.selectable_value(
                                                &mut dialog.glide_gamma,
                                                crate::prefs::GlideGamma::Compatible,
                                                "Compatible",
                                            );
                                        },
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Glide texture filtering");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            // "On (original)" samples exactly as
                                            // the guest's texture_mode register
                                            // asks -- today's only behaviour.
                                            // "Disabled" forces nearest (point)
                                            // sampling for every TMU regardless.
                                            // Takes effect on the next power-on,
                                            // like a CPU, memory, or video card
                                            // change.
                                            ui.selectable_value(
                                                &mut dialog.glide_texture_filter,
                                                crate::prefs::GlideTextureFilter::Disabled,
                                                "Disabled",
                                            );
                                            ui.selectable_value(
                                                &mut dialog.glide_texture_filter,
                                                crate::prefs::GlideTextureFilter::Original,
                                                "On (original)",
                                            );
                                        },
                                    );
                                });
                            });
                            ui.small(
                                "Graphics settings take effect the next time the machine \
                                 powers on.",
                            );
                        }
                        ConfigPage::Hotkeys => {
                            beige_group(ui, |ui| {
                                for (label, target) in [
                                    ("Input release", BindTarget::InputRelease),
                                    ("Full screen", BindTarget::Fullscreen),
                                    ("Screenshot", BindTarget::Screenshot),
                                ] {
                                    ui.horizontal(|ui| {
                                        ui.label(label);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| bind_button(ui, &mut dialog, target),
                                        );
                                    });
                                }
                            });
                            ui.small(format!(
                                "Screenshots are saved in {}.",
                                self.screenshots_dir.display()
                            ));
                        }
                        ConfigPage::Midi => {
                            beige_group(ui, |ui| {
                                ui.small(
                                    "Set levels inside the machine in DOS with SNDMIXER. \
                                     The panel volume controls the host speakers.",
                                );
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.label("P300 wavetable output");
                                    ui.label("FluidSynth");
                                });
                                soundfont_picker(ui, &mut dialog.soundfont);
                                let wavetable_color = if wavetable_status == MidiStatus::Ready {
                                    INK
                                } else {
                                    egui::Color32::from_rgb(170, 62, 48)
                                };
                                ui.colored_label(
                                    wavetable_color,
                                    midi_status_text(wavetable_status),
                                );
                                ui.add_space(6.0);
                                let munt_ready = munt_roms_available(
                                    &dialog.mt32_control_rom,
                                    &dialog.mt32_pcm_rom,
                                );
                                let munt_label = if munt_ready {
                                    "Munt (MT-32)"
                                } else {
                                    "Munt (MT-32) (missing ROMs)"
                                };
                                let receiver_label = match dialog.midi_backend {
                                    MidiBackend::Off => {
                                        midi_backend_label(MidiBackend::Off).to_owned()
                                    }
                                    MidiBackend::Munt => munt_label.to_owned(),
                                    MidiBackend::External => dialog
                                        .external_midi_port
                                        .as_ref()
                                        .map(midi_port_label)
                                        .unwrap_or_else(|| "Select a host MIDI device".to_owned()),
                                };
                                ui.horizontal(|ui| {
                                    ui.label("P330 MIDI receiver");
                                    egui::ComboBox::from_id_salt("midi-backend")
                                        .selected_text(receiver_label)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut dialog.midi_backend,
                                                MidiBackend::Off,
                                                midi_backend_label(MidiBackend::Off),
                                            );
                                            ui.selectable_value(
                                                &mut dialog.midi_backend,
                                                MidiBackend::Munt,
                                                munt_label,
                                            );
                                            for port in &dialog.midi_ports {
                                                let selected = dialog.midi_backend
                                                    == MidiBackend::External
                                                    && dialog.external_midi_port.as_ref()
                                                        == Some(port);
                                                if ui
                                                    .selectable_label(
                                                        selected,
                                                        midi_port_label(port),
                                                    )
                                                    .clicked()
                                                {
                                                    dialog.midi_backend = MidiBackend::External;
                                                    dialog.external_midi_port = Some(port.clone());
                                                }
                                            }
                                        });
                                });
                                if dialog.midi_ports.is_empty() {
                                    ui.small("No host MIDI destination ports were found.");
                                }
                                if midi_rom_selection_visible(dialog.midi_backend) {
                                    midi_path_picker(
                                        ui,
                                        "MT-32 control ROM",
                                        &mut dialog.mt32_control_rom,
                                        "ROM image",
                                        &["rom", "bin"],
                                        "ROM file or the set's folder",
                                    );
                                    midi_path_picker(
                                        ui,
                                        "MT-32 PCM ROM",
                                        &mut dialog.mt32_pcm_rom,
                                        "ROM image",
                                        &["rom", "bin"],
                                        "ROM file or the set's folder",
                                    );
                                }
                                let status_color = if midi_status == MidiStatus::Ready {
                                    INK
                                } else {
                                    egui::Color32::from_rgb(170, 62, 48)
                                };
                                ui.colored_label(status_color, midi_status_text(midi_status));
                            });
                        }
                    }

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if page == ConfigPage::Settings {
                            if ui.button("Accept").clicked() {
                                accept = true;
                                keep_open = false;
                            }
                            if ui.button("Cancel").clicked() {
                                keep_open = false;
                            }
                        } else {
                            if ui.button("Apply").clicked() {
                                apply = true;
                            }
                            if ui.button("Back").clicked() {
                                return_to_settings = true;
                            }
                        }
                    });
                });
        });
        if modal.should_close() {
            if page == ConfigPage::Settings {
                keep_open = false;
            } else {
                return_to_settings = true;
            }
        }
        if return_to_settings {
            dialog.page = ConfigPage::Settings;
            dialog.capturing = None;
        }
        if accept {
            self.apply_config(&dialog);
        }
        if apply {
            self.apply_config(&dialog);
        }
        if open_controller_setup {
            dialog.capturing = None;
            self.open_controller_setup();
        }
        if keep_open {
            self.config_dialog = Some(dialog);
        }
    }

    /// Push the staged config to the live fields, session, and prefs.
    fn apply_config(&mut self, dialog: &ConfigDialog) {
        self.input_release = dialog.input_release.clone();
        self.fullscreen_key = dialog.fullscreen.clone();
        self.screenshot_key = dialog.screenshot.clone();
        self.crt_style = dialog.crt_style;
        self.monitor_gamma = dialog.monitor_gamma;
        self.glide_gamma = dialog.glide_gamma;
        self.glide_texture_filter = dialog.glide_texture_filter;
        self.session.set_glide_force_point_sampling(matches!(
            dialog.glide_texture_filter,
            crate::prefs::GlideTextureFilter::Disabled
        ));
        self.prefs.start_fullscreen = dialog.start_fullscreen;
        self.prefs.mouse_sensitivity = dialog.mouse_sensitivity;
        self.mouse_scale = crate::host_input::mouse_sensitivity_scale(dialog.mouse_sensitivity);
        self.prefs.input_release = dialog.input_release.clone();
        self.prefs.fullscreen = dialog.fullscreen.clone();
        self.prefs.screenshot = dialog.screenshot.clone();
        self.prefs.crt_style = dialog.crt_style;
        self.prefs.monitor_gamma = dialog.monitor_gamma;
        self.prefs.glide_gamma = dialog.glide_gamma;
        self.prefs.glide_texture_filter = dialog.glide_texture_filter;
        let midi_config = MidiConfig {
            backend: dialog.midi_backend,
            external_port: dialog.external_midi_port.clone(),
            soundfont: dialog.soundfont.clone(),
            mt32_control_rom: optional_path(&dialog.mt32_control_rom),
            mt32_pcm_rom: optional_path(&dialog.mt32_pcm_rom),
        };
        if midi_request_needed(
            &midi_config,
            &self.session_snapshot.midi_config,
            self.session_snapshot.powered,
            [
                self.session_snapshot.wavetable_status,
                self.session_snapshot.midi_status,
            ],
        ) {
            let _ = self.request_session(SessionRequest::MidiConfig(midi_config));
        }
        self.save_prefs();
    }

    fn open_controller_setup(&mut self) {
        self.disconnect_controller_mapper();
        let (profiles, mut profile_error) = match self.controller_profiles.list() {
            Ok(profiles) => (profiles, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let selected_profile = self.controller_profile.clone();
        let first_device = self.controllers.as_ref().and_then(|controllers| {
            controllers
                .devices()
                .first()
                .map(|device| device.matcher.clone())
        });
        let staged = selected_profile
            .as_ref()
            .and_then(|name| match self.controller_profiles.load(name) {
                Ok(config) => Some(config),
                Err(err) => {
                    profile_error = Some(err.to_string());
                    self.controller_config.clone()
                }
            })
            .or_else(|| {
                selected_profile
                    .is_none()
                    .then(|| first_device.clone().map(ControllerConfig::default_keyboard))
                    .flatten()
            });
        let selected_device = staged
            .as_ref()
            .map(|config| config.device.clone())
            .or_else(|| first_device.clone());
        self.controller_setup = Some(ControllerSetupDialog {
            staged,
            tab: ControllerSetupTab::Assignments,
            profiles,
            selected_profile,
            profile_prompt: None,
            profile_error,
            selected_device,
            capturing_key: None,
        });
    }

    fn disconnect_controller_mapper(&mut self) {
        if let Some(mapper) = self.controller_mapper.as_mut() {
            let delta = mapper.apply(HostControllerBatch {
                connected: false,
                reset: true,
                events: Vec::new(),
                final_values: Vec::new(),
            });
            self.send_controller_delta(delta);
        }
    }

    fn apply_controller_setup(
        &mut self,
        profile: Option<String>,
        config: Option<ControllerConfig>,
    ) -> Result<(), String> {
        match (&profile, &config) {
            (Some(profile), Some(config)) => self
                .controller_profiles
                .save(profile, config)
                .map_err(|err| err.to_string())?,
            (None, None) => {}
            _ => return Err("Add a controller profile before you save this mapping.".into()),
        }
        self.disconnect_controller_mapper();
        self.controller_profile = profile.clone();
        self.controller_config = config.clone();
        self.controller_mapper = config.clone().map(ControllerMapper::new);
        self.prefs.controller = None;
        self.prefs.controller_profile = profile;
        self.last_controller_gameport = None;
        self.save_prefs();
        Ok(())
    }

    fn controller_setup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut setup) = self.controller_setup.take() else {
            return;
        };
        let (devices, topology_generation) = self.controllers.as_ref().map_or_else(
            || (Vec::new(), 0),
            |controllers| {
                (
                    controllers.devices().to_vec(),
                    controllers.topology_generation(),
                )
            },
        );
        self.controller_names.refresh(topology_generation);
        let display_devices = self.controller_names.display_devices(&devices);
        for live in &devices {
            let hardware_name = self.controller_names.hardware_name(&live.matcher);
            if let Some(selected) = setup.selected_device.as_mut() {
                upgrade_controller_name(selected, &live.matcher, hardware_name);
            }
            if let Some(staged) = setup.staged.as_mut() {
                upgrade_controller_name(&mut staged.device, &live.matcher, hardware_name);
            }
        }
        let controls = controller_controls(&self.controller_values);
        let values = self.controller_values.clone();
        let mut keep_open = true;
        let mut save = false;
        let mut select_profile = None;
        let mut add_profile = None;
        let mut delete_profile = None;
        let modal = egui::Modal::new(egui::Id::new("controller-setup-modal")).show(ctx, |ui| {
            egui::Frame::new()
                .fill(PANEL_FACE)
                .inner_margin(egui::Margin::same(14))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    beige_visuals(ui);
                    ui.set_width(1040.0);
                    ui.vertical_centered(|ui| {
                        ui.label(header_text("CONTROLLER SETUP", 19.0));
                        ui.label(
                            egui::RichText::new(
                                "Map one host gamepad, joystick, or wheel to guest keys or a guest controller.",
                            )
                            .color(MUTED)
                            .size(11.0),
                        );
                    });
                    ui.add_space(8.0);

                    beige_group(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("HOST DEVICE");
                            let selected = setup.selected_device.as_ref().map_or_else(
                                || "Select a controller".to_owned(),
                                |selected| {
                                    display_devices
                                        .iter()
                                        .find(|device| {
                                            selected == &device.matcher
                                                || selected.strongly_matches(&device.matcher)
                                        })
                                        .map_or_else(
                                            || {
                                                controller_device_display_name(
                                                    &display_devices,
                                                    selected,
                                                )
                                            },
                                            |device| {
                                                controller_device_display_name(
                                                    &display_devices,
                                                    &device.matcher,
                                                )
                                            },
                                        )
                                },
                            );
                            egui::ComboBox::from_id_salt("controller-device")
                                .selected_text(selected)
                                .width(300.0)
                                .show_ui(ui, |ui| {
                                    for (device, display) in devices.iter().zip(&display_devices) {
                                        let is_selected = setup.selected_device.as_ref().is_some_and(
                                            |selected| {
                                                selected == &device.matcher
                                                    || selected.strongly_matches(&device.matcher)
                                            },
                                        );
                                        let label = controller_device_display_name(
                                            &display_devices,
                                            &display.matcher,
                                        );
                                        if ui
                                            .selectable_label(is_selected, label)
                                            .clicked()
                                            && !is_selected
                                        {
                                            setup.selected_device = Some(display.matcher.clone());
                                            setup.staged = Some(ControllerConfig::default_keyboard(
                                                display.matcher.clone(),
                                            ));
                                            setup.capturing_key = None;
                                        }
                                    }
                                });
                            ui.label("PROFILE");
                            egui::ComboBox::from_id_salt("controller-saved-profile")
                                .selected_text(
                                    setup
                                        .selected_profile
                                        .as_deref()
                                        .unwrap_or("Select a profile"),
                                )
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for profile in &setup.profiles {
                                        let selected =
                                            setup.selected_profile.as_ref() == Some(profile);
                                        if ui.selectable_label(selected, profile).clicked()
                                            && !selected
                                        {
                                            select_profile = Some(profile.clone());
                                        }
                                    }
                                });
                            if ui
                                .add_enabled(
                                    setup.staged.is_some() || setup.selected_device.is_some(),
                                    egui::Button::new("Add new profile"),
                                )
                                .clicked()
                            {
                                setup.profile_prompt = Some(ControllerProfilePrompt::Add {
                                    name: String::new(),
                                    error: None,
                                    request_focus: true,
                                });
                            }
                            if ui
                                .add_enabled(
                                    setup.selected_profile.is_some(),
                                    egui::Button::new("Delete Profile"),
                                )
                                .clicked()
                                && let Some(name) = setup.selected_profile.clone()
                            {
                                setup.profile_prompt =
                                    Some(ControllerProfilePrompt::Delete { name });
                            }
                            if ui
                                .add_enabled(
                                    setup.staged.is_some(),
                                    egui::Button::new("Clear mapping"),
                                )
                                .clicked()
                            {
                                setup.staged = None;
                                setup.selected_profile = None;
                                setup.capturing_key = None;
                            }
                        });
                        if let Some(error) = &setup.profile_error {
                            ui.colored_label(egui::Color32::from_rgb(170, 62, 48), error);
                        }
                        ui.small(
                            "Add creates a named profile. Delete asks for confirmation. Save applies mapping edits.",
                        );
                        if devices.is_empty() {
                            ui.small("No connected host controller was found. Saved mappings remain available.");
                        } else if setup.staged.is_some() && setup.selected_profile.is_none() {
                            ui.small("Add a profile before you save this mapping.");
                        }
                    });

                    if let Some(config) = setup.staged.as_mut() {
                        ui.add_space(8.0);
                        let old_profile = config.profile;
                        controller_profile_picker(ui, config);
                        if config.profile != old_profile {
                            setup.capturing_key = None;
                        }
                        ui.add_space(8.0);
                        ui.horizontal_top(|ui| {
                            controller_main_card(ui, &values);
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.set_width(800.0);
                                ui.horizontal(|ui| {
                                    ui.selectable_value(
                                        &mut setup.tab,
                                        ControllerSetupTab::Assignments,
                                        "ASSIGNMENTS",
                                    );
                                    ui.selectable_value(
                                        &mut setup.tab,
                                        ControllerSetupTab::InputTest,
                                        "INPUT TEST",
                                    );
                                });
                                ui.separator();
                                if setup.tab != ControllerSetupTab::Assignments {
                                    setup.capturing_key = None;
                                }
                                match setup.tab {
                                    ControllerSetupTab::Assignments => {
                                        controller_assignments_ui(
                                            ui,
                                            config,
                                            &controls,
                                            &mut setup.capturing_key,
                                        )
                                    }
                                    ControllerSetupTab::InputTest => {
                                        controller_input_test_ui(ui, &values)
                                    }
                                }
                            });
                        });
                    } else {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label("Choose a device or a saved profile to create a mapping.");
                        });
                    }

                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_save = controller_setup_can_save(
                            setup.selected_profile.as_deref(),
                            setup.staged.is_some(),
                        );
                        if ui
                            .add_enabled(can_save, egui::Button::new("Save"))
                            .clicked()
                        {
                            save = true;
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
        if keep_open && let Some(mut prompt) = setup.profile_prompt.take() {
            let mut keep_prompt = true;
            let prompt_modal =
                egui::Modal::new(egui::Id::new("controller-profile-prompt")).show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(PANEL_FACE)
                        .inner_margin(egui::Margin::same(14))
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            beige_visuals(ui);
                            ui.set_width(340.0);
                            match &mut prompt {
                                ControllerProfilePrompt::Add {
                                    name,
                                    error,
                                    request_focus,
                                } => {
                                    ui.vertical_centered(|ui| {
                                        ui.label(header_text("ADD CONTROLLER PROFILE", 17.0));
                                    });
                                    ui.label("Profile name");
                                    let response = ui.add(
                                        egui::TextEdit::singleline(name)
                                            .hint_text("Game or profile name")
                                            .desired_width(f32::INFINITY),
                                    );
                                    if *request_focus {
                                        response.request_focus();
                                        *request_focus = false;
                                    }
                                    if let Some(error) = error {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(170, 62, 48),
                                            error,
                                        );
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add_enabled(
                                                    !name.trim().is_empty(),
                                                    egui::Button::new("Add"),
                                                )
                                                .clicked()
                                            {
                                                add_profile = Some(name.trim().to_owned());
                                                keep_prompt = false;
                                            }
                                            if ui.button("Cancel").clicked() {
                                                keep_prompt = false;
                                            }
                                        },
                                    );
                                }
                                ControllerProfilePrompt::Delete { name } => {
                                    ui.vertical_centered(|ui| {
                                        ui.label(header_text("DELETE CONTROLLER PROFILE", 17.0));
                                    });
                                    ui.label(format!("Delete profile {name:?}?"));
                                    ui.label("You cannot undo this action.");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("Delete").clicked() {
                                                delete_profile = Some(name.clone());
                                                keep_prompt = false;
                                            }
                                            if ui.button("Cancel").clicked() {
                                                keep_prompt = false;
                                            }
                                        },
                                    );
                                }
                            }
                        });
                });
            if prompt_modal.should_close() {
                keep_prompt = false;
            }
            if keep_prompt {
                setup.profile_prompt = Some(prompt);
            }
        }
        if let Some(profile) = select_profile {
            match self.controller_profiles.load(&profile) {
                Ok(config) => {
                    setup.selected_device = Some(config.device.clone());
                    setup.staged = Some(config);
                    setup.selected_profile = Some(profile);
                    setup.profile_error = None;
                    setup.capturing_key = None;
                }
                Err(err) => setup.profile_error = Some(err.to_string()),
            }
        }
        if let Some(name) = add_profile {
            let config = setup.staged.clone().or_else(|| {
                setup
                    .selected_device
                    .clone()
                    .map(ControllerConfig::default_keyboard)
            });
            if let Some(config) = config {
                match self.controller_profiles.create_named(&name, &config) {
                    Ok(()) => {
                        match self.controller_profiles.list() {
                            Ok(profiles) => {
                                setup.profiles = profiles;
                                setup.profile_error = None;
                            }
                            Err(err) => setup.profile_error = Some(err.to_string()),
                        }
                        setup.selected_profile = Some(name);
                        setup.staged = Some(config);
                    }
                    Err(err) => {
                        setup.profile_prompt = Some(ControllerProfilePrompt::Add {
                            name,
                            error: Some(err.to_string()),
                            request_focus: true,
                        });
                    }
                }
            }
        }
        if let Some(profile) = delete_profile {
            match self.controller_profiles.delete(&profile) {
                Ok(()) => {
                    setup.profiles.retain(|candidate| candidate != &profile);
                    if setup.selected_profile.as_ref() == Some(&profile) {
                        setup.selected_profile = None;
                        setup.staged = None;
                        setup.capturing_key = None;
                    }
                    setup.profile_error = None;
                    if self.controller_profile.as_ref() == Some(&profile) {
                        let _ = self.apply_controller_setup(None, None);
                    }
                }
                Err(err) => setup.profile_error = Some(err.to_string()),
            }
        }
        if save {
            match self.apply_controller_setup(setup.selected_profile.clone(), setup.staged.clone())
            {
                Ok(()) => keep_open = false,
                Err(err) => {
                    setup.profile_error = Some(err);
                    keep_open = true;
                }
            }
        }
        if keep_open {
            self.controller_setup = Some(setup);
        }
    }

    /// The floating COM1 window: black monospace serial log on white, auto-scrolled
    /// to the bottom, inside the shared beige chrome. The window is draggable,
    /// resizable, and closable; its open state is bound to `show_com1` so the
    /// close control and the footer button stay in sync.
    fn com1_window(&mut self, ctx: &egui::Context) {
        let serial = if self.session_snapshot.powered {
            self.session_snapshot.serial.clone()
        } else {
            String::new()
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

    /// The floating License window: the full GPL-3.0-only text, black monospace on
    /// white inside the shared beige chrome. Opened from the About window.
    fn license_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_license;
        beige_window(
            ctx,
            "License (GPL-3.0-only)",
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
                        egui::RichText::new("the Izarra3000 virtual machine")
                            .color(MUTED)
                            .size(12.0),
                    );
                    ui.hyperlink_to("github.com/vorvek/IzarraVM", GITHUB_URL);
                    ui.label(
                        egui::RichText::new(
                            "\u{00A9} 2026 General Simulation Works \u{00B7} GPL-3.0-only",
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
}

fn controller_device_display_name(
    devices: &[ControllerDevice],
    matcher: &ControllerDeviceMatcher,
) -> String {
    let duplicate_count = devices
        .iter()
        .filter(|device| {
            device.matcher.backend == matcher.backend && device.matcher.name == matcher.name
        })
        .count();
    let name = if duplicate_count < 2 {
        matcher.name.clone()
    } else {
        let ordinal = devices
            .iter()
            .filter(|device| {
                device.matcher.backend == matcher.backend && device.matcher.name == matcher.name
            })
            .position(|device| &device.matcher == matcher)
            .map_or(usize::from(matcher.occurrence) + 1, |index| index + 1);
        format!("{} ({ordinal})", matcher.name)
    };
    match matcher.backend.as_str() {
        "gilrs-wgi" => format!("{name} (WGI)"),
        "xinput" => format!("{name} (XInput)"),
        _ => name,
    }
}

fn upgrade_controller_name(
    target: &mut ControllerDeviceMatcher,
    live: &ControllerDeviceMatcher,
    hardware_name: Option<&str>,
) -> bool {
    let Some(hardware_name) = hardware_name else {
        return false;
    };
    if !target.strongly_matches(live) || target.name == hardware_name {
        return false;
    }
    target.name = hardware_name.to_owned();
    true
}

fn controller_controls(values: &[HostControlValue]) -> Vec<HostControlId> {
    let mut controls = Vec::new();
    for value in values {
        if !controls
            .iter()
            .any(|control: &HostControlId| control.matches(value.control))
        {
            controls.push(value.control);
        }
    }
    for axis in [
        JoystickAxis::LeftStickX,
        JoystickAxis::LeftStickY,
        JoystickAxis::LeftZ,
        JoystickAxis::RightStickX,
        JoystickAxis::RightStickY,
        JoystickAxis::RightZ,
        JoystickAxis::DPadX,
        JoystickAxis::DPadY,
    ] {
        let control = HostControlId::semantic_axis(axis);
        if !controls.iter().any(|candidate| candidate.matches(control)) {
            controls.push(control);
        }
    }
    for button in [
        JoystickButton::South,
        JoystickButton::East,
        JoystickButton::North,
        JoystickButton::West,
        JoystickButton::C,
        JoystickButton::Z,
        JoystickButton::LeftTrigger,
        JoystickButton::LeftTrigger2,
        JoystickButton::RightTrigger,
        JoystickButton::RightTrigger2,
        JoystickButton::Select,
        JoystickButton::Start,
        JoystickButton::Mode,
        JoystickButton::LeftThumb,
        JoystickButton::RightThumb,
        JoystickButton::DPadUp,
        JoystickButton::DPadDown,
        JoystickButton::DPadLeft,
        JoystickButton::DPadRight,
    ] {
        let control = HostControlId::semantic_button(button);
        if !controls.iter().any(|candidate| candidate.matches(control)) {
            controls.push(control);
        }
    }
    controls
}

fn controller_profile_picker(ui: &mut egui::Ui, config: &mut ControllerConfig) {
    beige_group(ui, |ui| {
        ui.label(egui::RichText::new("GUEST TARGET").color(LABEL).size(11.0));
        ui.horizontal(|ui| {
            let keyboard = matches!(config.profile, GuestControllerProfile::KeyboardOnly);
            if ui.selectable_label(keyboard, "Keyboard only").clicked() && !keyboard {
                config.apply_profile_defaults(GuestControllerProfile::KeyboardOnly);
            }
            let standard = matches!(config.profile, GuestControllerProfile::Standard);
            if ui.selectable_label(standard, "Standard joystick").clicked() && !standard {
                config.apply_profile_defaults(GuestControllerProfile::Standard);
            }
            let gravis = matches!(config.profile, GuestControllerProfile::Gravis { .. });
            if ui.selectable_label(gravis, "4 button gamepad").clicked() && !gravis {
                config.apply_profile_defaults(GuestControllerProfile::Gravis {
                    mode: GravisMode::FourButton,
                    handedness: GravisHandedness::RightHanded,
                });
            }
            let wheel = matches!(config.profile, GuestControllerProfile::WheelPedals);
            if ui.selectable_label(wheel, "Wheel and pedals").clicked() && !wheel {
                config.apply_profile_defaults(GuestControllerProfile::WheelPedals);
            }
        });
    });
}

const CONTROLLER_FACE_SVG: &[u8] = include_bytes!("../assets/controller-face.svg");
const CONTROLLER_SHOULDERS_SVG: &[u8] = include_bytes!("../assets/controller-shoulders.svg");
const FACE_VIEWBOX: [f32; 2] = [240.0, 120.0];
const SHOULDERS_VIEWBOX: [f32; 2] = [240.0, 100.0];
const PREVIEW_PRESS_THRESHOLD: f32 = 0.65;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct ControllerVisualState {
    left_stick: [f32; 2],
    right_stick: [f32; 2],
    dpad: [bool; 4],
    face: [bool; 4],
    shoulders: [bool; 4],
    select: bool,
    start: bool,
    left_thumb: bool,
    right_thumb: bool,
}

impl ControllerVisualState {
    fn from_values(values: &[HostControlValue]) -> Self {
        let axis =
            |axis| control_value(values, HostControlId::semantic_axis(axis)).clamp(-1.0, 1.0);
        let button = |button| {
            control_value(values, HostControlId::semantic_button(button)) >= PREVIEW_PRESS_THRESHOLD
        };
        let dpad_x = axis(JoystickAxis::DPadX);
        let dpad_y = axis(JoystickAxis::DPadY);
        Self {
            left_stick: [
                axis(JoystickAxis::LeftStickX),
                axis(JoystickAxis::LeftStickY),
            ],
            right_stick: [
                axis(JoystickAxis::RightStickX),
                axis(JoystickAxis::RightStickY),
            ],
            dpad: [
                button(JoystickButton::DPadUp) || dpad_y >= PREVIEW_PRESS_THRESHOLD,
                button(JoystickButton::DPadDown) || dpad_y <= -PREVIEW_PRESS_THRESHOLD,
                button(JoystickButton::DPadLeft) || dpad_x <= -PREVIEW_PRESS_THRESHOLD,
                button(JoystickButton::DPadRight) || dpad_x >= PREVIEW_PRESS_THRESHOLD,
            ],
            face: [
                button(JoystickButton::South),
                button(JoystickButton::East),
                button(JoystickButton::West),
                button(JoystickButton::North),
            ],
            shoulders: [
                button(JoystickButton::LeftTrigger2),
                button(JoystickButton::LeftTrigger),
                button(JoystickButton::RightTrigger),
                button(JoystickButton::RightTrigger2),
            ],
            select: button(JoystickButton::Select),
            start: button(JoystickButton::Start),
            left_thumb: button(JoystickButton::LeftThumb),
            right_thumb: button(JoystickButton::RightThumb),
        }
    }
}

fn controller_main_card(ui: &mut egui::Ui, values: &[HostControlValue]) {
    let state = ControllerVisualState::from_values(values);
    egui::Frame::new()
        .fill(FACEPLATE)
        .stroke(egui::Stroke::new(1.0_f32, BEVEL_LO))
        .inner_margin(egui::Margin::same(6))
        .corner_radius(5.0)
        .show(ui, |ui| {
            ui.set_width(216.0);
            ui.vertical_centered(|ui| {
                let (face, _) =
                    ui.allocate_exact_size(egui::vec2(216.0, 108.0), egui::Sense::hover());
                egui::Image::from_bytes(
                    "bytes://izarravm/controller-face.svg",
                    CONTROLLER_FACE_SVG,
                )
                .paint_at(ui, face);
                paint_controller_face(ui.painter(), face, state);
                ui.add_space(6.0);
                let (shoulders, _) =
                    ui.allocate_exact_size(egui::vec2(216.0, 90.0), egui::Sense::hover());
                egui::Image::from_bytes(
                    "bytes://izarravm/controller-shoulders.svg",
                    CONTROLLER_SHOULDERS_SVG,
                )
                .paint_at(ui, shoulders);
                paint_controller_shoulders(ui.painter(), shoulders, state.shoulders);
            });
        });
}

fn paint_controller_face(painter: &egui::Painter, rect: egui::Rect, state: ControllerVisualState) {
    for (active, view_rect) in state.dpad.into_iter().zip([
        [38.0, 28.0, 50.0, 46.0],
        [38.0, 54.0, 50.0, 72.0],
        [22.0, 44.0, 40.0, 56.0],
        [48.0, 44.0, 66.0, 56.0],
    ]) {
        if active {
            painter.rect_filled(
                controller_rect(rect, FACE_VIEWBOX, view_rect),
                1.5,
                LOGO_RED,
            );
        }
    }

    let face_buttons = [
        ([196.0, 65.0], "A", state.face[0]),
        ([211.0, 50.0], "B", state.face[1]),
        ([181.0, 50.0], "X", state.face[2]),
        ([196.0, 35.0], "Y", state.face[3]),
    ];
    for (center, label, active) in face_buttons {
        controller_circle_button(painter, rect, FACE_VIEWBOX, center, 8.0, label, active);
    }

    controller_slot_button(
        painter,
        rect,
        FACE_VIEWBOX,
        [96.0, 40.0, 114.0, 48.0],
        "-",
        state.select,
    );
    controller_slot_button(
        painter,
        rect,
        FACE_VIEWBOX,
        [126.0, 40.0, 144.0, 48.0],
        "+",
        state.start,
    );
    controller_stick(
        painter,
        rect,
        FACE_VIEWBOX,
        [86.0, 82.0],
        state.left_stick,
        state.left_thumb,
    );
    controller_stick(
        painter,
        rect,
        FACE_VIEWBOX,
        [154.0, 82.0],
        state.right_stick,
        state.right_thumb,
    );
}

fn paint_controller_shoulders(painter: &egui::Painter, rect: egui::Rect, active: [bool; 4]) {
    for ((view_rect, label), active) in [
        ([20.0, 22.0, 80.0, 52.0], "LT"),
        ([22.0, 66.0, 78.0, 79.0], "LB"),
        ([162.0, 66.0, 218.0, 79.0], "RB"),
        ([160.0, 22.0, 220.0, 52.0], "RT"),
    ]
    .into_iter()
    .zip(active)
    {
        controller_slot_button(painter, rect, SHOULDERS_VIEWBOX, view_rect, label, active);
    }
}

fn control_value(values: &[HostControlValue], control: HostControlId) -> f32 {
    resolve_control_value(values, control).unwrap_or(0.0)
}

fn controller_point(rect: egui::Rect, viewbox: [f32; 2], point: [f32; 2]) -> egui::Pos2 {
    egui::pos2(
        rect.left() + point[0] * rect.width() / viewbox[0],
        rect.top() + point[1] * rect.height() / viewbox[1],
    )
}

fn controller_rect(rect: egui::Rect, viewbox: [f32; 2], value: [f32; 4]) -> egui::Rect {
    egui::Rect::from_min_max(
        controller_point(rect, viewbox, [value[0], value[1]]),
        controller_point(rect, viewbox, [value[2], value[3]]),
    )
}

fn controller_radius(rect: egui::Rect, viewbox: [f32; 2], radius: f32) -> f32 {
    radius * (rect.width() / viewbox[0]).min(rect.height() / viewbox[1])
}

fn controller_circle_button(
    painter: &egui::Painter,
    rect: egui::Rect,
    viewbox: [f32; 2],
    center: [f32; 2],
    radius: f32,
    label: &str,
    active: bool,
) {
    let center = controller_point(rect, viewbox, center);
    if active {
        painter.circle_filled(center, controller_radius(rect, viewbox, radius), LOGO_RED);
    }
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(8.0),
        if active { egui::Color32::WHITE } else { INK },
    );
}

fn controller_slot_button(
    painter: &egui::Painter,
    rect: egui::Rect,
    viewbox: [f32; 2],
    view_rect: [f32; 4],
    label: &str,
    active: bool,
) {
    let slot = controller_rect(rect, viewbox, view_rect);
    if active {
        painter.rect_filled(slot.shrink(1.0), 4.0, LOGO_RED);
    }
    painter.text(
        slot.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(8.0),
        if active { egui::Color32::WHITE } else { INK },
    );
}

fn controller_stick(
    painter: &egui::Painter,
    rect: egui::Rect,
    viewbox: [f32; 2],
    center: [f32; 2],
    axes: [f32; 2],
    pressed: bool,
) {
    let center = controller_stick_point(rect, viewbox, center, axes, 7.0);
    let radius = controller_radius(rect, viewbox, 8.0);
    painter.circle_filled(center, radius, if pressed { LOGO_RED } else { RECESS });
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0_f32, BEVEL_HI));
}

fn controller_stick_point(
    rect: egui::Rect,
    viewbox: [f32; 2],
    center: [f32; 2],
    axes: [f32; 2],
    travel: f32,
) -> egui::Pos2 {
    controller_point(
        rect,
        viewbox,
        [
            center[0] + axes[0].clamp(-1.0, 1.0) * travel,
            center[1] - axes[1].clamp(-1.0, 1.0) * travel,
        ],
    )
}

fn controller_assignments_ui(
    ui: &mut egui::Ui,
    config: &mut ControllerConfig,
    controls: &[HostControlId],
    capturing_key: &mut Option<usize>,
) {
    config.normalize_profile_bindings();
    if matches!(config.profile, GuestControllerProfile::KeyboardOnly) {
        controller_keyboard_keys_ui(ui, config, capturing_key);
        return;
    }
    ui.columns(2, |columns| {
        let (axis_columns, detail_columns) = columns.split_at_mut(1);
        controller_axes_ui(&mut axis_columns[0], config, controls);

        let detail = &mut detail_columns[0];
        if matches!(config.profile, GuestControllerProfile::Gravis { .. }) {
            controller_gamepad_switches_ui(detail, config);
            detail.add_space(6.0);
        }
        controller_buttons_ui(detail, config, controls);
        detail.add_space(6.0);
        controller_keys_ui(detail, config, controls, capturing_key);
    });
}

fn controller_gamepad_switches_ui(ui: &mut egui::Ui, config: &mut ControllerConfig) {
    if let GuestControllerProfile::Gravis { mode, handedness } = config.profile {
        let mut new_mode = mode;
        let mut new_handedness = handedness;
        beige_group(ui, |ui| {
            ui.label(
                egui::RichText::new("4 BUTTON GAMEPAD SWITCHES")
                    .color(LABEL)
                    .size(11.0),
            );
            ui.horizontal(|ui| {
                ui.label("Control side");
                ui.selectable_value(
                    &mut new_handedness,
                    GravisHandedness::RightHanded,
                    "Right-handed",
                );
                ui.selectable_value(
                    &mut new_handedness,
                    GravisHandedness::LeftHanded,
                    "Left-handed",
                );
            });
            ui.horizontal(|ui| {
                ui.label("Button mode");
                ui.selectable_value(&mut new_mode, GravisMode::FourButton, "4 buttons");
                ui.selectable_value(
                    &mut new_mode,
                    GravisMode::TwoButtonTurbo,
                    "A/B + C/D autofire",
                );
            });
            ui.small(match new_mode {
                GravisMode::FourButton => "A, B, C, and D drive the four gameport button lines.",
                GravisMode::TwoButtonTurbo => {
                    "A and B are normal. C autofires A, and D autofires B."
                }
            });
        });
        if new_handedness != handedness {
            config.axes[0].transform.inverted = !config.axes[0].transform.inverted;
            config.axes[1].transform.inverted = !config.axes[1].transform.inverted;
        }
        config.profile = GuestControllerProfile::Gravis {
            mode: new_mode,
            handedness: new_handedness,
        };
    }
}

fn controller_axes_ui(
    ui: &mut egui::Ui,
    config: &mut ControllerConfig,
    controls: &[HostControlId],
) {
    beige_group(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            egui::RichText::new("GUEST GAMEPORT AXES")
                .color(LABEL)
                .size(11.0),
        );
        let axis_count = if matches!(config.profile, GuestControllerProfile::WheelPedals) {
            4
        } else {
            2
        };
        for axis in 0..axis_count {
            let label = guest_axis_label(config.profile, axis);
            let binding = &mut config.axes[axis];
            ui.horizontal(|ui| {
                ui.add_sized(
                    [78.0, 18.0],
                    egui::Label::new(egui::RichText::new(label).strong().color(INK)),
                );
                control_combo(
                    ui,
                    ("axis-control", axis),
                    &mut binding.host,
                    controls,
                    94.0,
                );
                egui::ComboBox::from_id_salt(("axis-span", axis))
                    .selected_text(match binding.transform.span {
                        AxisSpan::Full => "Full travel",
                        AxisSpan::PositiveHalf => "Center to + edge",
                        AxisSpan::NegativeHalf => "Center to - edge",
                    })
                    .width(108.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut binding.transform.span,
                            AxisSpan::Full,
                            "Full travel",
                        );
                        ui.selectable_value(
                            &mut binding.transform.span,
                            AxisSpan::PositiveHalf,
                            "Center to + edge",
                        );
                        ui.selectable_value(
                            &mut binding.transform.span,
                            AxisSpan::NegativeHalf,
                            "Center to - edge",
                        );
                    });
                ui.checkbox(&mut binding.transform.inverted, "Invert");
            });
            ui.horizontal(|ui| {
                ui.add_space(78.0);
                ui.label("Min");
                ui.add(
                    egui::DragValue::new(&mut binding.transform.calibration.minimum)
                        .range(-1.0..=1.0)
                        .speed(0.01),
                );
                ui.label("Center");
                ui.add(
                    egui::DragValue::new(&mut binding.transform.calibration.center)
                        .range(-1.0..=1.0)
                        .speed(0.01),
                );
                ui.label("Max");
                ui.add(
                    egui::DragValue::new(&mut binding.transform.calibration.maximum)
                        .range(-1.0..=1.0)
                        .speed(0.01),
                );
            });
            ui.horizontal(|ui| {
                ui.add_space(78.0);
                ui.label("Dead");
                ui.add(
                    egui::DragValue::new(&mut binding.transform.calibration.deadzone)
                        .range(0.0..=0.5)
                        .speed(0.01),
                );
                ui.label("Sat");
                ui.add(
                    egui::DragValue::new(&mut binding.transform.calibration.saturation)
                        .range(0.5..=1.0)
                        .speed(0.01),
                );
            });
        }
    });
}

fn controller_buttons_ui(
    ui: &mut egui::Ui,
    config: &mut ControllerConfig,
    controls: &[HostControlId],
) {
    beige_group(ui, |ui| {
        ui.label(
            egui::RichText::new("GUEST GAMEPORT BUTTONS")
                .color(LABEL)
                .size(11.0),
        );
        let button_count = config.profile.button_count();
        for action in 0..button_count {
            if let Some(binding) = config
                .buttons
                .iter_mut()
                .find(|binding| usize::from(binding.action) == action)
            {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [55.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new(guest_button_label(config.profile, action))
                                .strong(),
                        ),
                    );
                    control_combo(
                        ui,
                        ("button-control", action),
                        &mut binding.host.host,
                        controls,
                        105.0,
                    );
                    ui.selectable_value(
                        &mut binding.host.direction,
                        DigitalDirection::Positive,
                        "+ direction",
                    );
                    ui.selectable_value(
                        &mut binding.host.direction,
                        DigitalDirection::Negative,
                        "- direction",
                    );
                });
            }
        }
    });
}

fn controller_keys_ui(
    ui: &mut egui::Ui,
    config: &mut ControllerConfig,
    controls: &[HostControlId],
    capturing_key: &mut Option<usize>,
) {
    beige_group(ui, |ui| {
        ui.label(
            egui::RichText::new("ADDITIONAL GUEST KEYS")
                .color(LABEL)
                .size(11.0),
        );
        let mut remove = None;
        for (index, binding) in config.keys.iter_mut().enumerate() {
            if controller_key_row(ui, index, binding, controls, capturing_key) {
                remove = Some(index);
            }
        }
        if let Some(index) = remove {
            config.keys.remove(index);
            match *capturing_key {
                Some(capturing) if capturing == index => *capturing_key = None,
                Some(capturing) if capturing > index => *capturing_key = Some(capturing - 1),
                _ => {}
            }
        }
        if ui.button("+ Add guest key").clicked() {
            config.keys.push(new_guest_key_binding());
            *capturing_key = Some(config.keys.len() - 1);
        }
    });
}

fn controller_keyboard_keys_ui(
    ui: &mut egui::Ui,
    config: &mut ControllerConfig,
    capturing_key: &mut Option<usize>,
) {
    beige_group(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("HOST CONTROL TO GUEST KEY")
                    .color(LABEL)
                    .size(11.0),
            );
            ui.label(
                egui::RichText::new(
                    "Click a field, then type a key or Ctrl/Shift/Alt combination.",
                )
                .color(MUTED)
                .size(10.0),
            );
        });
        let rows = keyboard_controls()
            .iter()
            .filter_map(|control| {
                config
                    .keys
                    .iter()
                    .position(|binding| binding.host == control.host)
                    .map(|index| (index, control.label))
            })
            .collect::<Vec<_>>();
        ui.columns(3, |columns| {
            for (position, (index, label)) in rows.into_iter().enumerate() {
                let column = position / 8;
                controller_keyboard_key_row(
                    &mut columns[column],
                    label,
                    index,
                    &mut config.keys[index].guest,
                    capturing_key,
                );
            }
        });
    });
}

fn controller_keyboard_key_row(
    ui: &mut egui::Ui,
    label: &str,
    index: usize,
    guest: &mut GuestKeyChord,
    capturing_key: &mut Option<usize>,
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [92.0, 18.0],
            egui::Label::new(egui::RichText::new(label).color(INK).size(10.0)),
        );
        guest_key_capture(ui, index, guest, capturing_key, 102.0);
    });
}

fn controller_key_row(
    ui: &mut egui::Ui,
    index: usize,
    binding: &mut ControllerKeyBinding,
    controls: &[HostControlId],
    capturing_key: &mut Option<usize>,
) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        control_combo(
            ui,
            ("key-control", index),
            &mut binding.host.host,
            controls,
            105.0,
        );
        ui.selectable_value(&mut binding.host.direction, DigitalDirection::Positive, "+");
        ui.selectable_value(&mut binding.host.direction, DigitalDirection::Negative, "-");
        ui.label("to");
        guest_key_capture(ui, index, &mut binding.guest, capturing_key, 90.0);
        remove = ui
            .small_button("X")
            .on_hover_text("Remove mapping")
            .clicked();
    });
    remove
}

fn new_guest_key_binding() -> ControllerKeyBinding {
    ControllerKeyBinding {
        host: HostDigitalBinding {
            host: HostControlId::semantic_button(JoystickButton::South),
            direction: DigitalDirection::Positive,
        },
        guest: GuestKeyChord::default(),
    }
}

fn control_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    selected: &mut HostControlId,
    controls: &[HostControlId],
    width: f32,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected.display())
        .width(width)
        .show_ui(ui, |ui| {
            for control in controls {
                if ui
                    .selectable_label(*selected == *control, control.display())
                    .clicked()
                {
                    *selected = *control;
                }
            }
        });
}

fn guest_key_capture(
    ui: &mut egui::Ui,
    index: usize,
    chord: &mut GuestKeyChord,
    capturing_key: &mut Option<usize>,
    width: f32,
) {
    let capturing = *capturing_key == Some(index);
    let label = if capturing {
        "Press keys...".to_owned()
    } else {
        chord.display()
    };
    if ui
        .add_sized(
            [width, 20.0],
            egui::Button::new(
                egui::RichText::new(label)
                    .color(if capturing {
                        egui::Color32::WHITE
                    } else {
                        BEVEL_HI
                    })
                    .size(10.0),
            )
            .fill(if capturing { LOGO_RED } else { RECESS }),
        )
        .on_hover_text("Click, then type the guest key or key combination")
        .clicked()
    {
        *capturing_key = Some(index);
    }
    if ui
        .small_button("X")
        .on_hover_text("Clear guest key")
        .clicked()
    {
        *chord = GuestKeyChord::default();
        if capturing {
            *capturing_key = None;
        }
    }
}

fn guest_chord_from_capture(
    code: winit::keyboard::KeyCode,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<GuestKeyChord> {
    use winit::keyboard::KeyCode;

    let mut keys = Vec::with_capacity(4);
    if ctrl && !matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight) {
        keys.push(GuestKey::from_key_code(KeyCode::ControlLeft)?);
    }
    if shift && !matches!(code, KeyCode::ShiftLeft | KeyCode::ShiftRight) {
        keys.push(GuestKey::from_key_code(KeyCode::ShiftLeft)?);
    }
    if alt && !matches!(code, KeyCode::AltLeft | KeyCode::AltRight) {
        keys.push(GuestKey::from_key_code(KeyCode::AltLeft)?);
    }
    keys.push(GuestKey::from_key_code(code)?);
    Some(GuestKeyChord::new(keys))
}

fn guest_axis_label(profile: GuestControllerProfile, axis: usize) -> &'static str {
    match (profile, axis) {
        (GuestControllerProfile::WheelPedals, 0) => "Steering",
        (GuestControllerProfile::WheelPedals, 1) => "Accelerator",
        (GuestControllerProfile::WheelPedals, 2) => "Brake",
        (GuestControllerProfile::WheelPedals, 3) => "Clutch / spare",
        (_, 0) => "Horizontal",
        (_, 1) => "Vertical",
        _ => "Unused",
    }
}

fn guest_button_label(profile: GuestControllerProfile, action: usize) -> &'static str {
    match profile {
        GuestControllerProfile::Gravis { .. } => ["A", "B", "C", "D"][action],
        _ => ["Button 1", "Button 2", "Button 3", "Button 4"][action],
    }
}

fn controller_input_test_ui(ui: &mut egui::Ui, values: &[HostControlValue]) {
    beige_group(ui, |ui| {
        ui.label(
            egui::RichText::new("RAW CAPABILITIES")
                .color(LABEL)
                .size(11.0),
        );
        if values.is_empty() {
            ui.label("Move a control once, then return it to neutral.");
            return;
        }
        let rows = values.len().div_ceil(3);
        ui.columns(3, |columns| {
            for (column, ui) in columns.iter_mut().enumerate() {
                for value in values.iter().skip(column * rows).take(rows) {
                    ui.horizontal(|ui| {
                        let name = value.control.display();
                        ui.add_sized([92.0, 18.0], egui::Label::new(&name).truncate())
                            .on_hover_text(name);
                        let normalized = input_test_progress(*value);
                        ui.scope(|ui| {
                            ui.visuals_mut().extreme_bg_color = BEVEL_HI;
                            ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
                            ui.add(
                                egui::ProgressBar::new(normalized)
                                    .desired_width(72.0)
                                    .fill(BEVEL_LO)
                                    .text(
                                        egui::RichText::new(format!("{:+.2}", value.value))
                                            .strong()
                                            .color(egui::Color32::BLACK),
                                    ),
                            );
                        });
                    });
                }
            }
        });
    });
}

fn input_test_progress(value: HostControlValue) -> f32 {
    let unipolar_axis = matches!(
        value.control.semantic,
        Some(izarravm_input::HostSemanticControl::Axis(
            JoystickAxis::LeftZ | JoystickAxis::RightZ
        ))
    );
    let normalized = match value.control.kind {
        izarravm_input::HostControlKind::Axis if unipolar_axis => value.value,
        izarravm_input::HostControlKind::Axis => (value.value + 1.0) * 0.5,
        izarravm_input::HostControlKind::Button => value.value,
    };
    normalized.clamp(0.0, 1.0)
}

impl GuiApp {
    /// Build one egui frame: the title, the sidebar, the monitor, and the optional
    /// COM1 window. Keyboard, mouse capture, and focus loss are handled in the
    /// winit event loop now, not here, so the guest reads raw physical keys.
    pub(super) fn ui(&mut self, ctx: &egui::Context) {
        self.poll_session();
        // Cheap unless there is no working stream: one atomic load.
        self.audio.poll_recover();
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
        self.controller_setup_ui(ctx);
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

#[cfg(test)]
#[path = "gui_ui_test.rs"]
mod controller_ui_tests;
