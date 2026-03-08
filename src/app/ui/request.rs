use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::app::{MailmanApp, RequestEditorTab};
use crate::domain::{method_color, resolve_endpoint_url};
use crate::models::{BODY_MODE_OPTIONS, KeyValue, METHOD_OPTIONS};
use crate::request_body::normalize_body_mode;

use super::shared::attach_text_context_menu;
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_request_editor(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // ── Empty state ──────────────────────────────────────────────────────
            let Some(index) = self.selected_endpoint_index() else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Select a request from the sidebar.")
                            .color(theme::MUTED)
                            .size(14.0),
                    );
                });
                return;
            };

            // We defer side-effect calls that need full `&mut self` until after
            // the endpoint borrow is released.
            let mut do_send = false;
            let mut do_copy_curl = false;
            let mut changed = false;
            let mut remove_param_index: Option<usize> = None;
            let mut remove_header_index: Option<usize> = None;
            let mut request_editor_tab = self.request_editor_tab;

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let endpoint = &mut self.endpoints[index];

                    // ── Request name (editable heading) ──────────────────────────
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            TextEdit::singleline(&mut endpoint.name)
                                .frame(false)
                                .font(egui::FontId::proportional(18.0))
                                .desired_width(f32::INFINITY)
                                .hint_text("Untitled Request"),
                        );
                        attach_text_context_menu(&r, &endpoint.name, true);
                        if r.changed() {
                            changed = true;
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Copy cURL").color(theme::MUTED).size(12.0),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                do_copy_curl = true;
                            }
                        });
                    });

                    // ── Collection / Folder breadcrumb (compact, editable) ───────
                    ui.horizontal(|ui| {
                        ui.add_space(2.0);
                        let r = ui.add(
                            TextEdit::singleline(&mut endpoint.collection)
                                .frame(false)
                                .font(egui::FontId::proportional(11.0))
                                .desired_width(110.0)
                                .hint_text("Collection"),
                        );
                        if r.changed() {
                            changed = true;
                        }
                        ui.label(RichText::new("/").color(theme::MUTED).size(11.0));
                        let r = ui.add(
                            TextEdit::singleline(&mut endpoint.folder_path)
                                .frame(false)
                                .font(egui::FontId::proportional(11.0))
                                .desired_width(150.0)
                                .hint_text("Folder"),
                        );
                        if r.changed() {
                            changed = true;
                        }
                    });

                    ui.add_space(10.0);

                    // ── Method + URL + Send — the hero row ───────────────────────
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("method-picker")
                            .selected_text(
                                RichText::new(&endpoint.method)
                                    .color(method_color(&endpoint.method))
                                    .strong()
                                    .size(13.0),
                            )
                            .width(72.0)
                            .show_ui(ui, |ui| {
                                for method in METHOD_OPTIONS {
                                    if ui
                                        .selectable_label(
                                            endpoint.method == method,
                                            RichText::new(method)
                                                .color(method_color(method))
                                                .strong(),
                                        )
                                        .clicked()
                                    {
                                        endpoint.method = method.to_owned();
                                        changed = true;
                                    }
                                }
                            });

                        // URL takes all remaining space except the Send button.
                        let send_w = 70.0;
                        let spacing = ui.spacing().item_spacing.x;
                        let url_w =
                            (ui.available_width() - send_w - spacing).max(80.0);
                        let r = ui.add_sized(
                            [url_w, 0.0],
                            TextEdit::singleline(&mut endpoint.url)
                                .hint_text("https://${api_host}/resource"),
                        );
                        attach_text_context_menu(&r, &endpoint.url, true);
                        if r.changed() {
                            crate::domain::normalize_endpoint_url_and_query_params(endpoint);
                            changed = true;
                        }

                        // On-brand Send CTA
                        let send_label = if self.in_flight { "Sending…" } else { "Send" };
                        let send_btn = egui::Button::new(
                            RichText::new(send_label)
                                .color(Color32::WHITE)
                                .strong()
                                .size(13.0),
                        )
                        .fill(theme::ACCENT)
                        .min_size(egui::vec2(send_w, 0.0));
                        if ui.add_enabled(!self.in_flight, send_btn).clicked() {
                            do_send = true;
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();

                    // ── Tab bar (Params / Headers / Body) ────────────────────────
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

                    // ── Tab content ───────────────────────────────────────────────
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
                                    let (key_w, val_w, rm_w) = kv_row_widths(ui);
                                    let r = ui.add_sized(
                                        [key_w, 0.0],
                                        TextEdit::singleline(&mut param.key).hint_text("key"),
                                    );
                                    attach_text_context_menu(&r, &param.key, true);
                                    if r.changed() {
                                        changed = true;
                                    }
                                    let r = ui.add_sized(
                                        [val_w, 0.0],
                                        TextEdit::singleline(&mut param.value)
                                            .hint_text("value"),
                                    );
                                    attach_text_context_menu(&r, &param.value, true);
                                    if r.changed() {
                                        changed = true;
                                    }
                                    if ui
                                        .add_sized([rm_w, 0.0], egui::Button::new("×"))
                                        .clicked()
                                    {
                                        remove_param_index = Some(param_index);
                                    }
                                });
                            }
                            if let Some(pi) = remove_param_index {
                                endpoint.query_params.remove(pi);
                                changed = true;
                            }
                            if ui.button("+ Add Param").clicked() {
                                endpoint.query_params.push(KeyValue::default());
                                changed = true;
                            }
                        }

                        RequestEditorTab::Headers => {
                            ui.label(
                                RichText::new("Example: Authorization = Bearer ${token}")
                                    .color(theme::MUTED)
                                    .size(11.0),
                            );
                            ui.add_space(4.0);
                            for (header_index, header) in
                                endpoint.headers.iter_mut().enumerate()
                            {
                                ui.horizontal(|ui| {
                                    let (key_w, val_w, rm_w) = kv_row_widths(ui);
                                    let r = ui.add_sized(
                                        [key_w, 0.0],
                                        TextEdit::singleline(&mut header.key)
                                            .hint_text("Header name"),
                                    );
                                    attach_text_context_menu(&r, &header.key, true);
                                    if r.changed() {
                                        changed = true;
                                    }
                                    let r = ui.add_sized(
                                        [val_w, 0.0],
                                        TextEdit::singleline(&mut header.value)
                                            .hint_text("Header value"),
                                    );
                                    attach_text_context_menu(&r, &header.value, true);
                                    if r.changed() {
                                        changed = true;
                                    }
                                    if ui
                                        .add_sized([rm_w, 0.0], egui::Button::new("×"))
                                        .clicked()
                                    {
                                        remove_header_index = Some(header_index);
                                    }
                                });
                            }
                            if let Some(hi) = remove_header_index {
                                endpoint.headers.remove(hi);
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
                                                    normalize_body_mode(&endpoint.body_mode)
                                                        == mode,
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
                            let r = ui.add(
                                TextEdit::multiline(&mut endpoint.body)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(12)
                                    .code_editor()
                                    .hint_text("{\n  \"key\": \"${value}\"\n}"),
                            );
                            attach_text_context_menu(&r, &endpoint.body, true);
                            if r.changed() {
                                changed = true;
                            }
                        }
                    }

                    // ── Resolved URL preview ──────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    let env_vars = self.selected_environment_variables();
                    let resolved = resolve_endpoint_url(&self.endpoints[index], &env_vars);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Resolved URL:")
                                .color(theme::MUTED)
                                .size(11.0),
                        );
                        ui.label(RichText::new(&resolved).monospace().size(11.0));
                    });
                    ui.label(
                        RichText::new(
                            "Use ${variable_name} in URL, params, headers, and body.",
                        )
                        .color(theme::MUTED)
                        .size(11.0),
                    );
                });

            if changed {
                self.mark_dirty();
            }
            self.request_editor_tab = request_editor_tab;

            if do_send {
                self.send_selected_request();
            }
            if do_copy_curl {
                self.copy_curl_for_selected_request(ctx);
            }
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
