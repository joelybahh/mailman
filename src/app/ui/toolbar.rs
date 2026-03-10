use eframe::egui::{self, Color32, RichText};

use crate::app::MailmanApp;

use super::shared::HandCursor;
use super::theme;

#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_PAD: f32 = 80.0;

#[cfg(target_os = "macos")]
const TOOLBAR_HEIGHT: f32 = 36.0;

#[cfg(not(target_os = "macos"))]
const TOOLBAR_HEIGHT: f32 = 32.0;

impl MailmanApp {
    pub(in crate::app) fn render_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(TOOLBAR_HEIGHT)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    #[cfg(target_os = "macos")]
                    ui.add_space(TRAFFIC_LIGHT_PAD);

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(RichText::new("Mail").strong().size(15.0));
                        ui.label(RichText::new("man").size(15.0));
                    });

                    {
                        let version_text = concat!("v", env!("CARGO_PKG_VERSION"));
                        let galley = ui.painter().layout_no_wrap(
                            version_text.to_owned(),
                            egui::FontId::proportional(10.5),
                            Color32::from_gray(160),
                        );
                        let padding = egui::vec2(7.0, 3.0);
                        let chip_size = galley.size() + padding * 2.0;
                        let (rect, _) = ui.allocate_exact_size(chip_size, egui::Sense::hover());
                        if ui.is_rect_visible(rect) {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(9),
                                Color32::from_gray(45),
                            );
                            ui.painter().galley(
                                rect.min + padding,
                                galley,
                                Color32::from_gray(160),
                            );
                        }
                    }

                    ui.separator();

                    let import_mr = ui.menu_button(RichText::new("Import").size(13.0), |ui| {
                        if ui.button("From Postman…").cursor_hand().clicked() {
                            self.show_postman_import_dialog = true;
                            ui.close();
                        }
                        if ui.button("From Bundle…").cursor_hand().clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Import Mailman Bundle")
                                .add_filter(
                                    "Mailman Bundle",
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
                    import_mr.response.cursor_hand();

                    if ui
                        .button(RichText::new("Export Bundle").size(13.0))
                        .cursor_hand()
                        .clicked()
                    {
                        self.show_export_bundle_dialog = true;
                        self.export_bundle_password.clear();
                        self.export_bundle_password_confirm.clear();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("🔒 Lock").size(13.0)).frame(false),
                            )
                            .on_hover_text("Lock workspace")
                            .cursor_hand()
                            .clicked()
                        {
                            self.lock_workspace();
                        }

                        ui.separator();

                        if !self.show_environment_panel
                            && ui
                                .button(RichText::new("Env Settings").size(13.0))
                                .cursor_hand()
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
                                    if ui
                                        .selectable_label(selected, &env.name)
                                        .cursor_hand()
                                        .clicked()
                                    {
                                        self.selected_environment_id = Some(env.id.clone());
                                        selection_changed = true;
                                    }
                                }
                            })
                            .response
                            .cursor_hand();
                        if selection_changed {
                            self.mark_dirty();
                        }

                        ui.label(RichText::new("Environment:").size(12.0).color(theme::MUTED));
                    });
                });
            });
    }
}
