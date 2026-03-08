use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::app::{AppPhase, MailmanApp};

use super::shared::attach_text_context_menu;
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
                // Drop shadow painted before the card
                let shadow = card_slot
                    .expand2(egui::vec2(6.0, 4.0))
                    .translate(egui::vec2(0.0, 4.0));
                ui.painter().rect_filled(
                    shadow,
                    egui::CornerRadius::same(14),
                    Color32::from_black_alpha(28),
                );

                let frame_resp = egui::Frame::default()
                    .inner_margin(egui::Margin::same(28))
                    .corner_radius(egui::CornerRadius::same(12))
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.noninteractive.bg_stroke.color,
                    ))
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
                            ui.label(RichText::new("Mail Man").size(22.0).strong());
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

                                let r = ui.add(
                                    TextEdit::singleline(&mut self.setup_password)
                                        .password(true)
                                        .hint_text("Master password (min 12 chars)")
                                        .desired_width(f32::INFINITY),
                                );
                                attach_text_context_menu(&r, &self.setup_password, true);
                                ui.add_space(6.0);
                                let r = ui.add(
                                    TextEdit::singleline(&mut self.setup_password_confirm)
                                        .password(true)
                                        .hint_text("Confirm password")
                                        .desired_width(f32::INFINITY),
                                );
                                attach_text_context_menu(
                                    &r,
                                    &self.setup_password_confirm,
                                    true,
                                );
                                ui.add_space(16.0);

                                let btn = egui::Button::new(
                                    RichText::new("Configure Encryption and Open")
                                        .color(Color32::WHITE),
                                )
                                .fill(theme::ACCENT)
                                .min_size(egui::vec2(ui.available_width(), 32.0));
                                if ui.add(btn).clicked() {
                                    self.handle_setup_password_submission();
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

                                let r = ui.add(
                                    TextEdit::singleline(&mut self.unlock_password)
                                        .password(true)
                                        .hint_text("Master password")
                                        .desired_width(f32::INFINITY),
                                );
                                attach_text_context_menu(&r, &self.unlock_password, true);
                                ui.add_space(16.0);

                                let btn = egui::Button::new(
                                    RichText::new("Unlock").color(Color32::WHITE),
                                )
                                .fill(theme::ACCENT)
                                .min_size(egui::vec2(ui.available_width(), 32.0));
                                if ui.add(btn).clicked() {
                                    self.handle_unlock_password_submission();
                                }
                            }
                            AppPhase::Ready => {}
                        }

                        if !self.auth_message.is_empty() {
                            ui.add_space(12.0);
                            ui.colored_label(theme::ERROR, &self.auth_message);
                        }
                    });

                // Accent bar painted on top of the card border
                let card_rect = frame_resp.response.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(card_rect.min, egui::vec2(card_rect.width(), 5.0)),
                    egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 },
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
