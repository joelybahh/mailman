use eframe::egui::{self, Color32, RichText};

use crate::app::MailmanApp;

use super::theme;

/// Horizontal space (px) reserved on macOS for the traffic-light buttons.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_PAD: f32 = 80.0;

/// Height of the toolbar panel. On macOS this must be at least the title-bar
/// height (~28 px) so our content overlaps the transparent native bar cleanly.
#[cfg(target_os = "macos")]
const TOOLBAR_HEIGHT: f32 = 36.0;

#[cfg(not(target_os = "macos"))]
const TOOLBAR_HEIGHT: f32 = 32.0;

impl MailmanApp {
    pub(in crate::app) fn render_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(TOOLBAR_HEIGHT)
            // Make the panel draggable on macOS – users can grab the toolbar to
            // move the window, just as they would a native title bar.
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // On macOS leave room for the traffic-light buttons (close /
                    // minimise / full-screen) that the OS renders at the left.
                    #[cfg(target_os = "macos")]
                    ui.add_space(TRAFFIC_LIGHT_PAD);

                    // Brand word-mark
                    ui.label(
                        RichText::new("Mail Man")
                            .strong()
                            .size(15.0)
                            .color(theme::ACCENT),
                    );

                    // Thin divider
                    ui.separator();

                    // Import sub-menu
                    ui.menu_button(RichText::new("Import").size(13.0), |ui| {
                        if ui.button("From Postman…").clicked() {
                            self.show_postman_import_dialog = true;
                            ui.close();
                        }
                        if ui.button("From Bundle…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Import Mail Man Bundle")
                                .add_filter(
                                    "Mail Man Bundle",
                                    &["mmbundle", "mailmanbundle", "json"],
                                )
                                .pick_file()
                            {
                                self.import_bundle_path = Some(path);
                                self.import_bundle_password.clear();
                                self.show_import_bundle_dialog = true;
                            }
                            ui.close();
                        }
                    });

                    // Export
                    if ui
                        .button(RichText::new("Export Bundle").size(13.0))
                        .clicked()
                    {
                        self.show_export_bundle_dialog = true;
                        self.export_bundle_password.clear();
                        self.export_bundle_password_confirm.clear();
                    }

                    // Environment switcher — right-aligned
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.show_environment_panel
                            && ui
                                .button(RichText::new("Env Settings").size(13.0))
                                .clicked()
                        {
                            self.show_environment_panel = true;
                        }

                        let selected_name = self
                            .selected_environment_index()
                            .and_then(|idx| self.environments.get(idx))
                            .map(|env| env.name.as_str())
                            .unwrap_or("None");

                        let mut selection_changed = false;
                        egui::ComboBox::from_id_salt("environment-switcher")
                            .selected_text(
                                RichText::new(selected_name)
                                    .size(13.0)
                                    .color(Color32::from_gray(200)),
                            )
                            .show_ui(ui, |ui| {
                                for env in &self.environments {
                                    let selected = self.selected_environment_id.as_deref()
                                        == Some(env.id.as_str());
                                    if ui.selectable_label(selected, &env.name).clicked() {
                                        self.selected_environment_id = Some(env.id.clone());
                                        selection_changed = true;
                                    }
                                }
                            });
                        if selection_changed {
                            self.mark_dirty();
                        }

                        ui.label(RichText::new("Environment:").size(12.0).color(theme::MUTED));
                    });
                });
            });
    }
}
