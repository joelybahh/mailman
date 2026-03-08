use eframe::egui::{self, RichText, TextEdit};

use crate::app::{AppPhase, MailmanApp};

use super::shared::attach_text_context_menu;
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_auth_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);

                ui.label(RichText::new("Mail Man").size(28.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Offline-first API client")
                        .size(13.0)
                        .color(theme::MUTED),
                );
                ui.add_space(32.0);

                match self.phase {
                    AppPhase::SetupPassword => {
                        ui.label(RichText::new("Set a master password").strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Environment files are encrypted at rest with Argon2id + XChaCha20-Poly1305.\nThe password is never stored and cannot be recovered if lost.",
                            )
                            .color(theme::MUTED)
                            .size(12.0),
                        );
                        ui.add_space(14.0);

                        let response = ui.add(
                            TextEdit::singleline(&mut self.setup_password)
                                .password(true)
                                .hint_text("Master password (min 12 chars)"),
                        );
                        attach_text_context_menu(&response, &self.setup_password, true);
                        ui.add_space(6.0);
                        let response = ui.add(
                            TextEdit::singleline(&mut self.setup_password_confirm)
                                .password(true)
                                .hint_text("Confirm password"),
                        );
                        attach_text_context_menu(
                            &response,
                            &self.setup_password_confirm,
                            true,
                        );
                        ui.add_space(14.0);

                        if ui.button("Configure Encryption and Open").clicked() {
                            self.handle_setup_password_submission();
                        }
                    }
                    AppPhase::UnlockPassword => {
                        ui.label(RichText::new("Unlock workspace").strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Enter your master password to decrypt environment variables.",
                            )
                            .color(theme::MUTED)
                            .size(12.0),
                        );
                        ui.add_space(14.0);

                        let response = ui.add(
                            TextEdit::singleline(&mut self.unlock_password)
                                .password(true)
                                .hint_text("Master password"),
                        );
                        attach_text_context_menu(&response, &self.unlock_password, true);
                        ui.add_space(14.0);

                        if ui.button("Unlock").clicked() {
                            self.handle_unlock_password_submission();
                        }
                    }
                    AppPhase::Ready => {}
                }

                if !self.auth_message.is_empty() {
                    ui.add_space(12.0);
                    ui.colored_label(theme::ERROR, &self.auth_message);
                }

                if !self.status_line.is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(&self.status_line)
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                }
            });
        });
    }
}
