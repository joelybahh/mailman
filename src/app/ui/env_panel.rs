use eframe::egui::{self, RichText, TextEdit};

use crate::app::MailmanApp;
use crate::models::KeyValue;

use super::shared::{HandCursor, attach_text_context_menu};
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_environment_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("environments")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("Environments");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").cursor_hand().clicked() {
                            self.show_environment_panel = false;
                        }
                    });
                });
                ui.label(
                    RichText::new("Each environment is stored in its own encrypted file.")
                        .color(theme::MUTED)
                        .size(11.0),
                );
                ui.separator();

                ui.horizontal(|ui| {
                    let response = ui.add(
                        TextEdit::singleline(&mut self.new_environment_name)
                            .desired_width(150.0)
                            .hint_text("new env (qa, prod)"),
                    );
                    attach_text_context_menu(&response, &self.new_environment_name, true);
                    if ui.button("Add").cursor_hand().clicked() {
                        let name = self.new_environment_name.trim().to_owned();
                        if !name.is_empty() {
                            self.add_environment(name);
                            self.new_environment_name.clear();
                        }
                    }
                    if ui
                        .button(RichText::new("Delete").color(theme::MUTED))
                        .cursor_hand()
                        .clicked()
                    {
                        self.delete_selected_environment();
                    }
                });
                ui.separator();

                let Some(index) = self.selected_environment_index() else {
                    ui.label(
                        RichText::new("No environment selected.")
                            .color(theme::MUTED)
                            .italics(),
                    );
                    return;
                };

                let mut changed = false;

                // Fixed header: env name + file path
                {
                    let env = &mut self.environments[index];
                    let response = ui.add(
                        TextEdit::singleline(&mut env.name)
                            .desired_width(f32::INFINITY)
                            .hint_text("Environment name"),
                    );
                    attach_text_context_menu(&response, &env.name, true);
                    if response.changed() {
                        changed = true;
                    }
                    ui.label(
                        RichText::new(format!("File: {}", env.file_name))
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                }
                ui.separator();

                // Scrollable variable list
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut remove_index: Option<usize> = None;
                        let env = &mut self.environments[index];

                        for (variable_index, variable) in env.variables.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                let row_width = ui.available_width();
                                let spacing = ui.spacing().item_spacing.x;
                                let remove_width = 22.0_f32;
                                let key_width = ((row_width - remove_width - spacing * 2.0) * 0.38)
                                    .clamp(80.0, 180.0);
                                let value_width =
                                    (row_width - key_width - remove_width - spacing * 2.0)
                                        .max(80.0);

                                let response = ui.add_sized(
                                    [key_width, 0.0],
                                    TextEdit::singleline(&mut variable.key).hint_text("key"),
                                );
                                attach_text_context_menu(&response, &variable.key, true);
                                if response.changed() {
                                    changed = true;
                                }
                                let response = ui.add_sized(
                                    [value_width, 0.0],
                                    TextEdit::singleline(&mut variable.value).hint_text("value"),
                                );
                                attach_text_context_menu(&response, &variable.value, true);
                                if response.changed() {
                                    changed = true;
                                }
                                if ui
                                    .add_sized([remove_width, 0.0], egui::Button::new("×"))
                                    .cursor_hand()
                                    .clicked()
                                {
                                    remove_index = Some(variable_index);
                                }
                            });
                        }

                        if let Some(variable_index) = remove_index {
                            env.variables.remove(variable_index);
                            changed = true;
                        }

                        ui.add_space(4.0);
                        if ui.button("+ Add Variable").cursor_hand().clicked() {
                            env.variables.push(KeyValue::default());
                            changed = true;
                        }
                    });

                if changed {
                    self.mark_dirty();
                }
            });
    }
}
