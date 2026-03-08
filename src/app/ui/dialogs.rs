use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, TextEdit};

use crate::app::MailmanApp;
use crate::domain::non_empty_trimmed;

use super::shared::{attach_text_context_menu, HandCursor};
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_share_bundle_dialogs(&mut self, ctx: &egui::Context) {
        self.render_export_bundle_dialog(ctx);
        self.render_import_bundle_dialog(ctx);
    }

    fn render_export_bundle_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_export_bundle_dialog {
            return;
        }

        let mut open = self.show_export_bundle_dialog;
        let mut should_close = false;

        egui::Window::new("Export Bundle")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(360.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Create a password-protected bundle for sharing or backup.")
                        .color(theme::MUTED)
                        .size(12.0),
                );
                ui.add_space(8.0);

                let response = ui.add(
                    TextEdit::singleline(&mut self.export_bundle_password)
                        .password(true)
                        .hint_text("Bundle password (min 8 chars)")
                        .desired_width(f32::INFINITY),
                );
                attach_text_context_menu(&response, &self.export_bundle_password, true);
                ui.add_space(4.0);
                let response = ui.add(
                    TextEdit::singleline(&mut self.export_bundle_password_confirm)
                        .password(true)
                        .hint_text("Confirm bundle password")
                        .desired_width(f32::INFINITY),
                );
                attach_text_context_menu(
                    &response,
                    &self.export_bundle_password_confirm,
                    true,
                );
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Export").cursor_hand().clicked() {
                        let password = self.export_bundle_password.trim().to_owned();
                        let confirm = self.export_bundle_password_confirm.trim().to_owned();

                        if password.len() < 8 {
                            self.status_line =
                                "Bundle password must be at least 8 characters.".to_owned();
                            return;
                        }
                        if password != confirm {
                            self.status_line =
                                "Bundle password confirmation does not match.".to_owned();
                            return;
                        }

                        let save_target = rfd::FileDialog::new()
                            .set_title("Export Mailman Bundle")
                            .set_file_name("mailman-workspace.mmbundle")
                            .add_filter(
                                "Mailman Bundle",
                                &["mmbundle", "mailmanbundle"],
                            )
                            .save_file();

                        let Some(path) = save_target else {
                            self.status_line = "Bundle export canceled.".to_owned();
                            return;
                        };

                        match self.export_workspace_bundle_to_path(&path, &password) {
                            Ok((endpoint_count, environment_count)) => {
                                self.status_line = format!(
                                    "Exported {endpoint_count} endpoints and {environment_count} environments to {}",
                                    path.display()
                                );
                                should_close = true;
                            }
                            Err(err) => {
                                self.status_line = format!("Export failed: {err}");
                            }
                        }
                    }

                    if ui.button("Cancel").cursor_hand().clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_export_bundle_dialog = open && !should_close;
    }

    fn render_import_bundle_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_import_bundle_dialog {
            return;
        }

        let mut open = self.show_import_bundle_dialog;
        let mut should_close = false;
        let selected_path = self.import_bundle_path.clone();

        egui::Window::new("Import Bundle")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(360.0)
            .show(ctx, |ui| {
                if let Some(path) = selected_path.as_ref() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("File:").color(theme::MUTED).size(11.0));
                        ui.label(
                            RichText::new(path.display().to_string())
                                .monospace()
                                .size(11.0),
                        );
                    });
                } else {
                    ui.colored_label(theme::WARNING, "No bundle file selected.");
                }
                ui.add_space(8.0);

                let response = ui.add(
                    TextEdit::singleline(&mut self.import_bundle_password)
                        .password(true)
                        .hint_text("Bundle password")
                        .desired_width(f32::INFINITY),
                );
                attach_text_context_menu(&response, &self.import_bundle_password, true);
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Import").cursor_hand().clicked() {
                        let Some(path) = selected_path.as_ref() else {
                            self.status_line = "Select a bundle file first.".to_owned();
                            return;
                        };

                        let password = self.import_bundle_password.trim().to_owned();
                        match self.import_workspace_bundle_from_path(path, &password) {
                            Ok(summary) => {
                                self.status_line = format!(
                                    "Imported: {} endpoints added, {} updated, {} environments, {} env vars merged.",
                                    summary.endpoints_added,
                                    summary.endpoints_updated,
                                    summary.environments_added,
                                    summary.environment_variables_merged,
                                );
                                self.last_mutation = Instant::now() - Duration::from_secs(1);
                                self.try_auto_save();
                                should_close = true;
                            }
                            Err(err) => {
                                self.status_line = format!("Import failed: {err}");
                            }
                        }
                    }

                    if ui.button("Cancel").cursor_hand().clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_import_bundle_dialog = open && !should_close;
        if should_close {
            self.import_bundle_path = None;
            self.import_bundle_password.clear();
        }
    }

    pub(in crate::app) fn render_postman_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_postman_import_dialog {
            return;
        }

        let mut open = self.show_postman_import_dialog;
        let mut should_close = false;

        egui::Window::new("Import From Postman")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(420.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Import requests and environments from Postman local data.")
                        .color(theme::MUTED)
                        .size(12.0),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Workspace filter")
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                    let response = ui.add(
                        TextEdit::singleline(&mut self.postman_workspace_filter)
                            .desired_width(f32::INFINITY)
                            .hint_text("optional"),
                    );
                    attach_text_context_menu(
                        &response,
                        &self.postman_workspace_filter,
                        true,
                    );
                });
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    let response = ui.add(
                        TextEdit::singleline(&mut self.postman_import_path)
                            .desired_width(f32::INFINITY)
                            .hint_text("/path/to/Postman"),
                    );
                    attach_text_context_menu(&response, &self.postman_import_path, true);
                    if ui.button("Browse").cursor_hand().clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Select Postman Directory")
                            .pick_folder()
                        {
                            self.postman_import_path = path.display().to_string();
                        }
                    }
                });
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Auto Detect").cursor_hand().clicked() {
                        let workspace_filter =
                            non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                        let summary =
                            self.import_postman_from_defaults(workspace_filter.as_deref());
                        self.status_line = summary.to_message();
                        should_close = true;
                    }

                    if ui.button("Import from Path").cursor_hand().clicked() {
                        let raw_path = self.postman_import_path.trim();
                        if raw_path.is_empty() {
                            self.status_line = "Enter a Postman path first.".to_owned();
                            return;
                        }

                        let path = std::path::PathBuf::from(raw_path);
                        let workspace_filter =
                            non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                        let summary =
                            self.import_postman_from_path(&path, workspace_filter.as_deref());
                        self.status_line = summary.to_message();
                        should_close = true;
                    }

                    if ui.button("Cancel").cursor_hand().clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_postman_import_dialog = open && !should_close;
    }
}
