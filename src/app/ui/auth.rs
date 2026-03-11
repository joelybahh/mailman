use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::app::{AppPhase, MailmanApp};

use super::shared::{HandCursor, attach_text_context_menu};
use super::theme;

const LOGO_BYTES: &[u8] = include_bytes!("../../../assets/icons/128x128@2x.png");

impl MailmanApp {
    pub(in crate::app) fn render_auth_screen(&mut self, ctx: &egui::Context) {
        // Lazy-load the logo texture once
        if self.logo_texture.is_none() {
            if let Ok(icon) = eframe::icon_data::from_png_bytes(LOGO_BYTES) {
                let size = [icon.width as usize, icon.height as usize];
                let img = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
                self.logo_texture =
                    Some(ctx.load_texture("mailman-logo", img, egui::TextureOptions::LINEAR));
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_rect_before_wrap();

            if !avail.is_finite() || avail.width() < 100.0 || avail.height() < 100.0 {
                return;
            }

            let card_w = 360.0_f32.min(avail.width() - 48.0);
            let card_x = avail.center().x - card_w / 2.0;
            let card_y = (avail.min.y + avail.height() * 0.18).max(avail.min.y + 40.0);

            let card_slot = egui::Rect::from_min_size(
                egui::pos2(card_x, card_y),
                egui::vec2(card_w, avail.max.y - card_y - 32.0),
            );

            ui.scope_builder(egui::UiBuilder::new().max_rect(card_slot), |ui| {
                let frame_resp = egui::Frame::default()
                    .inner_margin(egui::Margin::same(28))
                    // Flat top so the accent strip above sits flush; only bottom corners rounded
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 12,
                        se: 12,
                    })
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.noninteractive.bg_stroke.color,
                    ))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 18,
                        spread: 0,
                        color: Color32::from_black_alpha(50),
                    })
                    .show(ui, |ui| {
                        // Branding block
                        ui.vertical_centered(|ui| {
                            if let Some(texture) = &self.logo_texture {
                                ui.add(
                                    egui::Image::new((texture.id(), egui::vec2(72.0, 72.0)))
                                        .corner_radius(egui::CornerRadius::same(14)),
                                );
                            }
                            ui.add_space(10.0);
                            {
                                let font = egui::FontId::proportional(22.0);
                                let mut job = egui::text::LayoutJob::default();
                                job.append(
                                    "Mail",
                                    0.0,
                                    egui::text::TextFormat {
                                        font_id: font.clone(),
                                        color: ui.visuals().strong_text_color(),
                                        ..Default::default()
                                    },
                                );
                                job.append(
                                    "man",
                                    0.0,
                                    egui::text::TextFormat {
                                        font_id: font,
                                        color: ui.visuals().text_color(),
                                        ..Default::default()
                                    },
                                );
                                ui.label(job);
                            }
                            ui.add_space(3.0);
                            ui.label(
                                RichText::new("Offline-first API client")
                                    .size(12.0)
                                    .color(theme::MUTED),
                            );
                        });

                        ui.add_space(20.0);
                        ui.separator();
                        ui.add_space(16.0);

                        let pending = self.auth_pending;

                        match self.phase {
                            AppPhase::SetupPassword => {
                                ui.label(RichText::new("Set a master password").strong());
                                ui.add_space(5.0);
                                ui.label(
                                    RichText::new(
                                        "Environment files are encrypted at rest with \
                                         Argon2id + XChaCha20-Poly1305. \
                                         The password is never stored.",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.5),
                                );
                                ui.add_space(14.0);

                                let r = ui.add_enabled(
                                    !pending,
                                    TextEdit::singleline(&mut self.setup_password)
                                        .password(true)
                                        .hint_text("Master password (min 12 chars)")
                                        .desired_width(f32::INFINITY),
                                );
                                if !pending {
                                    attach_text_context_menu(&r, &self.setup_password, true);
                                }
                                ui.add_space(6.0);
                                let r = ui.add_enabled(
                                    !pending,
                                    TextEdit::singleline(&mut self.setup_password_confirm)
                                        .password(true)
                                        .hint_text("Confirm password")
                                        .desired_width(f32::INFINITY),
                                );
                                if !pending {
                                    attach_text_context_menu(
                                        &r,
                                        &self.setup_password_confirm,
                                        true,
                                    );
                                }
                                ui.add_space(16.0);

                                if pending {
                                    ui.horizontal(|ui| {
                                        let btn_w = ui.available_width();
                                        let btn = egui::Button::new(
                                            RichText::new("Configuring…")
                                                .color(Color32::from_white_alpha(120)),
                                        )
                                        .fill(theme::ACCENT.gamma_multiply(0.5))
                                        .min_size(egui::vec2(btn_w, 32.0));
                                        ui.add_enabled(false, btn);
                                    });
                                    ui.add_space(6.0);
                                    ui.vertical_centered(|ui| {
                                        ui.spinner();
                                    });
                                } else {
                                    let btn = egui::Button::new(
                                        RichText::new("Configure Encryption and Open")
                                            .color(Color32::WHITE),
                                    )
                                    .fill(theme::ACCENT)
                                    .min_size(egui::vec2(ui.available_width(), 32.0));
                                    if ui.add(btn).cursor_hand().clicked() {
                                        self.handle_setup_password_submission();
                                    }
                                }
                            }
                            AppPhase::UnlockPassword => {
                                ui.label(RichText::new("Unlock workspace").strong());
                                ui.add_space(5.0);
                                ui.label(
                                    RichText::new(
                                        "Enter your master password to decrypt \
                                         environment variables.",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.5),
                                );
                                ui.add_space(14.0);

                                let r = ui.add_enabled(
                                    !pending,
                                    TextEdit::singleline(&mut self.unlock_password)
                                        .password(true)
                                        .hint_text("Master password")
                                        .desired_width(f32::INFINITY),
                                );
                                if !pending {
                                    attach_text_context_menu(&r, &self.unlock_password, true);
                                }
                                ui.add_space(10.0);

                                // ── Session duration picker ──────────────────
                                const LABELS: [&str; 6] = [
                                    "Always ask",
                                    "1 day",
                                    "7 days",
                                    "14 days",
                                    "30 days",
                                    "Forever",
                                ];
                                const VALUES: [Option<u32>; 6] =
                                    [None, Some(1), Some(7), Some(14), Some(30), Some(0)];

                                let current_idx = VALUES
                                    .iter()
                                    .position(|v| *v == self.config.session_duration_days)
                                    .unwrap_or(0);

                                let mut selected_idx = current_idx;
                                ui.add_enabled_ui(!pending, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("Keep me signed in:")
                                                .size(12.0)
                                                .color(theme::MUTED),
                                        );
                                        egui::ComboBox::from_id_salt("session-duration")
                                            .selected_text(LABELS[selected_idx])
                                            .width(110.0)
                                            .show_ui(ui, |ui| {
                                                for (i, label) in LABELS.iter().enumerate() {
                                                    ui.selectable_value(
                                                        &mut selected_idx,
                                                        i,
                                                        *label,
                                                    );
                                                }
                                            });
                                    });
                                });

                                if selected_idx != current_idx {
                                    self.config.session_duration_days = VALUES[selected_idx];
                                    let _ = self.storage.save_config(&self.config);
                                }

                                ui.add_space(12.0);

                                if pending {
                                    ui.horizontal(|ui| {
                                        let btn_w = ui.available_width();
                                        let btn = egui::Button::new(
                                            RichText::new("Verifying…")
                                                .color(Color32::from_white_alpha(120)),
                                        )
                                        .fill(theme::ACCENT.gamma_multiply(0.5))
                                        .min_size(egui::vec2(btn_w, 32.0));
                                        ui.add_enabled(false, btn);
                                    });
                                    ui.add_space(6.0);
                                    ui.vertical_centered(|ui| {
                                        ui.spinner();
                                    });
                                } else {
                                    let btn = egui::Button::new(
                                        RichText::new("Unlock").color(Color32::WHITE),
                                    )
                                    .fill(theme::ACCENT)
                                    .min_size(egui::vec2(ui.available_width(), 32.0));
                                    if ui.add(btn).cursor_hand().clicked() {
                                        self.handle_unlock_password_submission();
                                    }
                                }
                            }
                            AppPhase::Ready => {}
                        }

                        if !self.auth_message.is_empty() {
                            ui.add_space(12.0);
                            ui.colored_label(theme::ERROR, &self.auth_message);
                        }
                    });

                // Accent bar painted on top of the card — rounded top matches the strip shape
                let card_rect = frame_resp.response.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(card_rect.min, egui::vec2(card_rect.width(), 5.0)),
                    egui::CornerRadius {
                        nw: 6,
                        ne: 6,
                        sw: 0,
                        se: 0,
                    },
                    theme::ACCENT,
                );

                // Status line below the card
                if !self.status_line.is_empty() {
                    let below_y = card_rect.max.y + 12.0;
                    if below_y < avail.max.y {
                        let status_rect = egui::Rect::from_min_size(
                            egui::pos2(card_x, below_y),
                            egui::vec2(card_w, 20.0),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(status_rect), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(&self.status_line)
                                        .color(theme::MUTED)
                                        .size(11.0),
                                );
                            });
                        });
                    }
                }
            });
        });
    }
}
