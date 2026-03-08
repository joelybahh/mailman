use eframe::egui::{self, RichText, TextEdit};

use crate::app::{MailmanApp, RequestEditorTab};
use crate::domain::resolve_endpoint_url;
use crate::models::{BODY_MODE_OPTIONS, KeyValue, METHOD_OPTIONS};
use crate::request_body::normalize_body_mode;

use super::shared::attach_text_context_menu;
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_request_editor(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Request");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Copy cURL").clicked() {
                                self.copy_curl_for_selected_request(ctx);
                            }
                            let send_button = ui.add_enabled(
                                !self.in_flight,
                                egui::Button::new(if self.in_flight {
                                    "Sending…"
                                } else {
                                    "Send"
                                }),
                            );
                            if send_button.clicked() {
                                self.send_selected_request();
                            }
                        });
                    });
                    ui.separator();

                    let Some(index) = self.selected_endpoint_index() else {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("Select a request from the sidebar.")
                                    .color(theme::MUTED),
                            );
                        });
                        return;
                    };

                    let mut changed = false;
                    let mut remove_param_index: Option<usize> = None;
                    let mut remove_header_index: Option<usize> = None;
                    let mut request_editor_tab = self.request_editor_tab;

                    {
                        let endpoint = &mut self.endpoints[index];

                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Name").color(theme::MUTED).size(11.0));
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.name)
                                    .desired_width(f32::INFINITY),
                            );
                            attach_text_context_menu(&response, &endpoint.name, true);
                            if response.changed() {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Collection").color(theme::MUTED).size(11.0),
                            );
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.collection)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("General"),
                            );
                            attach_text_context_menu(&response, &endpoint.collection, true);
                            if response.changed() {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Folder").color(theme::MUTED).size(11.0));
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.folder_path)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Auth / Users"),
                            );
                            attach_text_context_menu(&response, &endpoint.folder_path, true);
                            if response.changed() {
                                changed = true;
                            }
                        });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("method-picker")
                                .selected_text(
                                    RichText::new(&endpoint.method)
                                        .color(crate::domain::method_color(&endpoint.method))
                                        .strong(),
                                )
                                .show_ui(ui, |ui| {
                                    for method in METHOD_OPTIONS {
                                        if ui
                                            .selectable_label(endpoint.method == method, method)
                                            .clicked()
                                        {
                                            endpoint.method = method.to_owned();
                                            changed = true;
                                        }
                                    }
                                });

                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.url)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("https://${api_host}/resource"),
                            );
                            attach_text_context_menu(&response, &endpoint.url, true);
                            if response.changed() {
                                crate::domain::normalize_endpoint_url_and_query_params(endpoint);
                                changed = true;
                            }
                        });

                        ui.add_space(6.0);
                        ui.separator();

                        let non_empty_param_count = endpoint
                            .query_params
                            .iter()
                            .filter(|p| !p.key.trim().is_empty())
                            .count();
                        let non_empty_header_count = endpoint
                            .headers
                            .iter()
                            .filter(|h| !h.key.trim().is_empty())
                            .count();

                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Params,
                                format!("Params ({non_empty_param_count})"),
                            );
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Headers,
                                format!("Headers ({non_empty_header_count})"),
                            );
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Body,
                                "Body",
                            );
                        });
                        ui.separator();

                        match request_editor_tab {
                            RequestEditorTab::Params => {
                                ui.label(
                                    RichText::new(
                                        "Query params are appended to the URL when sending.",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.0),
                                );
                                ui.add_space(4.0);
                                for (param_index, param) in
                                    endpoint.query_params.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let (key_width, value_width, remove_width) =
                                            kv_row_widths(ui);
                                        let response = ui.add_sized(
                                            [key_width, 0.0],
                                            TextEdit::singleline(&mut param.key)
                                                .hint_text("key"),
                                        );
                                        attach_text_context_menu(&response, &param.key, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        let response = ui.add_sized(
                                            [value_width, 0.0],
                                            TextEdit::singleline(&mut param.value)
                                                .hint_text("value"),
                                        );
                                        attach_text_context_menu(
                                            &response,
                                            &param.value,
                                            true,
                                        );
                                        if response.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add_sized(
                                                [remove_width, 0.0],
                                                egui::Button::new("×"),
                                            )
                                            .clicked()
                                        {
                                            remove_param_index = Some(param_index);
                                        }
                                    });
                                }
                                if let Some(param_index) = remove_param_index {
                                    endpoint.query_params.remove(param_index);
                                    changed = true;
                                }
                                if ui.button("+ Add Param").clicked() {
                                    endpoint.query_params.push(KeyValue::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Headers => {
                                ui.label(
                                    RichText::new(
                                        "Example: Authorization = Bearer ${token}",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.0),
                                );
                                ui.add_space(4.0);
                                for (header_index, header) in
                                    endpoint.headers.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let (key_width, value_width, remove_width) =
                                            kv_row_widths(ui);
                                        let response = ui.add_sized(
                                            [key_width, 0.0],
                                            TextEdit::singleline(&mut header.key)
                                                .hint_text("Header name"),
                                        );
                                        attach_text_context_menu(&response, &header.key, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        let response = ui.add_sized(
                                            [value_width, 0.0],
                                            TextEdit::singleline(&mut header.value)
                                                .hint_text("Header value"),
                                        );
                                        attach_text_context_menu(
                                            &response,
                                            &header.value,
                                            true,
                                        );
                                        if response.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add_sized(
                                                [remove_width, 0.0],
                                                egui::Button::new("×"),
                                            )
                                            .clicked()
                                        {
                                            remove_header_index = Some(header_index);
                                        }
                                    });
                                }
                                if let Some(header_index) = remove_header_index {
                                    endpoint.headers.remove(header_index);
                                    changed = true;
                                }
                                if ui.button("+ Add Header").clicked() {
                                    endpoint.headers.push(KeyValue::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Body => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Mode").color(theme::MUTED).size(11.0),
                                    );
                                    egui::ComboBox::from_id_salt("body-mode-picker")
                                        .selected_text(normalize_body_mode(&endpoint.body_mode))
                                        .show_ui(ui, |ui| {
                                            for mode in BODY_MODE_OPTIONS {
                                                if ui
                                                    .selectable_label(
                                                        normalize_body_mode(
                                                            &endpoint.body_mode,
                                                        ) == mode,
                                                        mode,
                                                    )
                                                    .clicked()
                                                {
                                                    endpoint.body_mode = mode.to_owned();
                                                    changed = true;
                                                }
                                            }
                                        });
                                });
                                ui.add_space(4.0);
                                let response = ui.add(
                                    TextEdit::multiline(&mut endpoint.body)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(12)
                                        .code_editor()
                                        .hint_text(
                                            "{\n  \"key\": \"${value}\"\n}",
                                        ),
                                );
                                attach_text_context_menu(&response, &endpoint.body, true);
                                if response.changed() {
                                    changed = true;
                                }
                            }
                        }
                    }
                    self.request_editor_tab = request_editor_tab;

                    if changed {
                        self.mark_dirty();
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    let env_vars = self.selected_environment_variables();
                    let resolved_url = resolve_endpoint_url(&self.endpoints[index], &env_vars);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Resolved URL:").color(theme::MUTED).size(11.0),
                        );
                        ui.label(RichText::new(&resolved_url).monospace().size(11.0));
                    });
                    ui.label(
                        RichText::new("Use ${variable_name} in URL, params, headers, and body.")
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                });
        });
    }
}

fn kv_row_widths(ui: &egui::Ui) -> (f32, f32, f32) {
    let row_width = ui.available_width();
    let spacing = ui.spacing().item_spacing.x;
    let remove_width = 24.0_f32;
    let key_width = (row_width * 0.35).clamp(120.0, 260.0);
    let value_width = (row_width - key_width - remove_width - spacing * 2.0).max(120.0);
    (key_width, value_width, remove_width)
}
