use eframe::egui::{self, RichText};

use crate::app::MailmanApp;

impl MailmanApp {
    pub(in crate::app) fn render_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Mail Man").strong().size(17.0));
                ui.separator();

                ui.menu_button("Import", |ui| {
                    if ui.button("From Postman...").clicked() {
                        self.show_postman_import_dialog = true;
                        ui.close();
                    }
                    if ui.button("From Bundle...").clicked() {
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

                if ui.button("Export Bundle").clicked() {
                    self.show_export_bundle_dialog = true;
                    self.export_bundle_password.clear();
                    self.export_bundle_password_confirm.clear();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.show_environment_panel && ui.button("Env Settings").clicked() {
                        self.show_environment_panel = true;
                    }

                    let selected_name = self
                        .selected_environment_index()
                        .and_then(|idx| self.environments.get(idx))
                        .map(|env| env.name.as_str())
                        .unwrap_or("None");

                    let mut selection_changed = false;
                    egui::ComboBox::from_id_salt("environment-switcher")
                        .selected_text(selected_name)
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
                    ui.label("Environment:");
                });
            });
            ui.add_space(2.0);
        });
    }
}
