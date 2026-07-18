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
                        .circle_stroke(c, 6.0, egui::Stroke::new(1.0_f32, BEVEL_LO));
                });
            },
        );
    }

    fn panel_body(&mut self, ui: &mut egui::Ui) {
        let running = self.emu.is_some();
        let (mode, speed, idle, floppy_accesses, c_accesses, cd_accesses) = match &self.emu {
            Some(emu) => {
                let f = emu.frame.lock().expect("frame snapshot poisoned");
                (
                    f.mode,
                    f.speed_ratio,
                    f.idle,
                    f.floppy_accesses,
                    f.c_accesses,
                    f.cd_accesses,
                )
            }
            None => (
                None,
                0.0,
                false,
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
                        format!("Idle - {} MB", self.profile.memory_mib)
                    } else {
                        format!(
                            "Speed {:.0}% - {} MB",
                            speed * 100.0,
                            self.profile.memory_mib
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
            midi_backend: self.midi_config.backend,
            external_midi_port: self.midi_config.external_port.clone(),
            soundfont: self.midi_config.soundfont.clone(),
            mt32_control_rom: path_text(self.midi_config.mt32_control_rom.as_ref()),
            mt32_pcm_rom: path_text(self.midi_config.mt32_pcm_rom.as_ref()),
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
    }

    /// Record a captured combo into the staged binding the dialog is waiting on,
    /// then stop capturing. `key` is the winit `KeyCode` debug name.
    pub(super) fn record_bind(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool) {
        if let Some(dialog) = &mut self.config_dialog
            && let Some(target) = dialog.capturing.take()
        {
            let binding = KeyBinding::new(ctrl, shift, alt, key);
            match target {
                BindTarget::InputRelease => dialog.input_release = binding,
                BindTarget::Fullscreen => dialog.fullscreen = binding,
            }
        }
    }

    /// Render the configuration modal. Accept applies the staged settings and
    /// closes; Cancel, the backdrop, or Esc discards and closes.
    fn config_ui(&mut self, ctx: &egui::Context) {
        let (wavetable_status, midi_status) = self
            .emu
            .as_ref()
            .map(|emu| {
                let frame = emu.frame.lock().expect("frame snapshot poisoned");
                (frame.wavetable_status, frame.midi_status)
            })
            .unwrap_or((
                MidiStatus::InitializationFailed,
                MidiStatus::InitializationFailed,
            ));
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
                        ui.separator();
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
                        ui.colored_label(wavetable_color, midi_status_text(wavetable_status));
                        ui.add_space(6.0);
                        let munt_ready =
                            munt_roms_available(&dialog.mt32_control_rom, &dialog.mt32_pcm_rom);
                        let munt_label = if munt_ready {
                            "Munt (MT-32)"
                        } else {
                            "Munt (MT-32) (missing ROMs)"
                        };
                        let receiver_label = match dialog.midi_backend {
                            MidiBackend::Off => midi_backend_label(MidiBackend::Off).to_owned(),
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
                                    ui.add_enabled_ui(munt_ready, |ui| {
                                        ui.selectable_value(
                                            &mut dialog.midi_backend,
                                            MidiBackend::Munt,
                                            munt_label,
                                        );
                                    });
                                    for port in &dialog.midi_ports {
                                        let selected = dialog.midi_backend == MidiBackend::External
                                            && dialog.external_midi_port.as_ref() == Some(port);
                                        if ui
                                            .selectable_label(selected, midi_port_label(port))
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
                        if dialog.midi_backend == MidiBackend::Munt || !munt_ready {
                            midi_path_picker(
                                ui,
                                "MT-32 control ROM",
                                &mut dialog.mt32_control_rom,
                                "ROM image",
                                &["rom", "bin"],
                                "Required",
                            );
                            midi_path_picker(
                                ui,
                                "MT-32 PCM ROM",
                                &mut dialog.mt32_pcm_rom,
                                "ROM image",
                                &["rom", "bin"],
                                "Required",
                            );
                        }
                        let status_color = if midi_status == MidiStatus::Ready {
                            INK
                        } else {
                            egui::Color32::from_rgb(170, 62, 48)
                        };
                        ui.colored_label(status_color, midi_status_text(midi_status));
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
        let midi_config = MidiConfig {
            backend: dialog.midi_backend,
            external_port: dialog.external_midi_port.clone(),
            soundfont: dialog.soundfont.clone(),
            mt32_control_rom: optional_path(&dialog.mt32_control_rom),
            mt32_pcm_rom: optional_path(&dialog.mt32_pcm_rom),
        };
        if midi_config != self.midi_config {
            self.midi_config = midi_config.clone();
            if let Some(emu) = &self.emu {
                emu.configure_midi(midi_config.clone());
            }
        }
        self.prefs.midi = midi_config;
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
                        egui::RichText::new("the Izarra 3000 virtual machine")
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

impl GuiApp {
    /// Build one egui frame: the title, the sidebar, the monitor, and the optional
    /// COM1 window. Keyboard, mouse capture, and focus loss are handled in the
    /// winit event loop now, not here, so the guest reads raw physical keys.
    pub(super) fn ui(&mut self, ctx: &egui::Context) {
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
