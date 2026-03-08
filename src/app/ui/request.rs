use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::app::{MailmanApp, RequestEditorTab};
use crate::domain::{method_color, resolve_endpoint_url};
use crate::models::{BODY_MODE_OPTIONS, KeyValue, METHOD_OPTIONS, ResponseScript};
use crate::request_body::normalize_body_mode;

use super::shared::{attach_text_context_menu, HandCursor};
use super::theme;

/// Right-side inner margin kept throughout the panel so nothing bleeds to the
/// window edge. Applied both to the panel frame and re-used in width math.
const RIGHT_PAD: f32 = 12.0;

impl MailmanApp {
    pub(in crate::app) fn render_request_editor(&mut self, ctx: &egui::Context) {
        // Give the central panel a small inner margin so widgets never sit flush
        // against the window edge.
        let panel_frame = egui::Frame::default()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(egui::Margin {
                left: 8,
                right: RIGHT_PAD as i8,
                top: 4,
                bottom: 0,
            });

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // ── Empty state ──────────────────────────────────────────────
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

                let mut do_send = false;
                let mut do_copy_curl = false;
                let mut changed = false;
                let mut remove_param_index: Option<usize> = None;
                let mut remove_header_index: Option<usize> = None;
                let mut remove_script_index: Option<usize> = None;
                let mut request_editor_tab = self.request_editor_tab;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // The vertical scrollbar overlays the right edge of the
                        // scroll area but ui.available_width() doesn't subtract
                        // it. Clamp content width now so nothing overflows.
                        let sb = ui.spacing().scroll.bar_outer_margin
                            + ui.spacing().scroll.bar_width;
                        ui.set_max_width((ui.available_width() - sb).max(200.0));

                        let endpoint = &mut self.endpoints[index];

                        // ── Title row: name + collection/folder + Copy cURL ──
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            // Reserve space for the right-side metadata so the
                            // name field doesn't expand past it.
                            let meta_w = 300.0_f32.min(ui.available_width() * 0.45);
                            let name_w = (ui.available_width() - meta_w).max(120.0);

                            let r = ui.add(
                                TextEdit::singleline(&mut endpoint.name)
                                    .frame(false)
                                    .font(egui::FontId::proportional(18.0))
                                    .desired_width(name_w)
                                    .hint_text("Untitled Request"),
                            );
                            attach_text_context_menu(&r, &endpoint.name, true);
                            if r.changed() {
                                changed = true;
                            }

                            // Right side (right-to-left): Copy cURL | folder / collection
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // Copy cURL — visible button
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("Copy cURL")
                                                    .size(12.0)
                                                    .color(theme::MUTED),
                                            )
                                            .stroke(egui::Stroke::new(
                                                1.0,
                                                theme::MUTED.gamma_multiply(0.5),
                                            )),
                                        )
                                        .cursor_hand()
                                        .clicked()
                                    {
                                        do_copy_curl = true;
                                    }

                                    ui.separator();

                                    // Folder (right-to-left so it appears left of cURL)
                                    let r = ui.add(
                                        TextEdit::singleline(&mut endpoint.folder_path)
                                            .frame(false)
                                            .font(egui::FontId::proportional(11.0))
                                            .desired_width(90.0)
                                            .hint_text("Folder"),
                                    );
                                    if r.changed() {
                                        changed = true;
                                    }

                                    ui.label(
                                        RichText::new("/").color(theme::MUTED).size(11.0),
                                    );

                                    // Collection
                                    let r = ui.add(
                                        TextEdit::singleline(&mut endpoint.collection)
                                            .frame(false)
                                            .font(egui::FontId::proportional(11.0))
                                            .desired_width(90.0)
                                            .hint_text("Collection"),
                                    );
                                    if r.changed() {
                                        changed = true;
                                    }
                                },
                            );
                        });

                        ui.add_space(10.0);

                        // ── Method + URL + Send ─────────────────────────────
                        // Use desired_width (not add_sized with height=0) so all
                        // three widgets share the same natural row height and
                        // align correctly in the horizontal layout.
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
                                        .cursor_hand()
                                        .clicked()
                                    {
                                        endpoint.method = method.to_owned();
                                        changed = true;
                                    }
                                }
                            })
                            .response
                            .cursor_hand();

                            let send_w = 70.0;
                            let spacing = ui.spacing().item_spacing.x;
                            // desired_width lets egui use the widget's natural
                            // height, keeping it aligned with the Send button.
                            let url_w =
                                (ui.available_width() - send_w - spacing).max(80.0);
                            let r = ui.add(
                                TextEdit::singleline(&mut endpoint.url)
                                    .desired_width(url_w)
                                    .hint_text("https://${api_host}/resource"),
                            );
                            attach_text_context_menu(&r, &endpoint.url, true);
                            if r.changed() {
                                crate::domain::normalize_endpoint_url_and_query_params(
                                    endpoint,
                                );
                                changed = true;
                            }

                            let send_label =
                                if self.in_flight { "Sending…" } else { "Send" };
                            let send_btn = egui::Button::new(
                                RichText::new(send_label)
                                    .color(Color32::WHITE)
                                    .strong()
                                    .size(13.0),
                            )
                            .fill(theme::ACCENT)
                            .min_size(egui::vec2(send_w, 0.0));
                        if ui.add_enabled(!self.in_flight, send_btn).cursor_hand().clicked() {
                            do_send = true;
                        }
                        });

                        ui.add_space(8.0);
                        ui.separator();

                        // ── Tabs ────────────────────────────────────────────
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

                        let non_empty_script_count = endpoint
                            .scripts
                            .iter()
                            .filter(|s| !s.extract_key.trim().is_empty())
                            .count();

                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Params,
                                format!("Params ({non_empty_param_count})"),
                            )
                            .cursor_hand();
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Headers,
                                format!("Headers ({non_empty_header_count})"),
                            )
                            .cursor_hand();
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Body,
                                "Body",
                            )
                            .cursor_hand();
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Scripts,
                                if non_empty_script_count > 0 {
                                    format!("Scripts ({non_empty_script_count})")
                                } else {
                                    "Scripts".to_owned()
                                },
                            )
                            .cursor_hand();
                        });
                        ui.separator();

                        // ── Tab content ─────────────────────────────────────
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
                                        // desired_width instead of add_sized so
                                        // all three items share natural row height.
                                        let r = ui.add(
                                            TextEdit::singleline(&mut param.key)
                                                .desired_width(key_w)
                                                .hint_text("key"),
                                        );
                                        attach_text_context_menu(
                                            &r, &param.key, true,
                                        );
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            TextEdit::singleline(&mut param.value)
                                                .desired_width(val_w)
                                                .hint_text("value"),
                                        );
                                        attach_text_context_menu(
                                            &r, &param.value, true,
                                        );
                                        if r.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new("×")
                                                    .min_size(egui::vec2(rm_w, 0.0)),
                                            )
                                            .cursor_hand()
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
                                ui.add_space(4.0);
                                if ui.button("+ Add Param").cursor_hand().clicked() {
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
                                        let (key_w, val_w, rm_w) = kv_row_widths(ui);
                                        let r = ui.add(
                                            TextEdit::singleline(&mut header.key)
                                                .desired_width(key_w)
                                                .hint_text("Header name"),
                                        );
                                        attach_text_context_menu(
                                            &r, &header.key, true,
                                        );
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            TextEdit::singleline(&mut header.value)
                                                .desired_width(val_w)
                                                .hint_text("Header value"),
                                        );
                                        attach_text_context_menu(
                                            &r, &header.value, true,
                                        );
                                        if r.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new("×")
                                                    .min_size(egui::vec2(rm_w, 0.0)),
                                            )
                                            .cursor_hand()
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
                                ui.add_space(4.0);
                                if ui.button("+ Add Header").cursor_hand().clicked() {
                                    endpoint.headers.push(KeyValue::default());
                                    changed = true;
                                }
                            }

                            RequestEditorTab::Scripts => {
                                ui.label(
                                    RichText::new(
                                        "After a 2xx response, each rule extracts a value \
                                         from the JSON body and writes it to an env variable. \
                                         Use dot notation for nested keys, e.g. data.access_token",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.0),
                                );
                                ui.add_space(4.0);

                                for (script_index, script) in
                                    endpoint.scripts.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let (key_w, val_w, rm_w) = kv_row_widths(ui);
                                        let r = ui.add(
                                            TextEdit::singleline(&mut script.extract_key)
                                                .desired_width(key_w)
                                                .hint_text("data.access_token"),
                                        );
                                        attach_text_context_menu(
                                            &r, &script.extract_key, true,
                                        );
                                        if r.changed() { changed = true; }

                                        ui.label(
                                            RichText::new("→")
                                                .color(theme::MUTED)
                                                .size(12.0),
                                        );

                                        let r = ui.add(
                                            TextEdit::singleline(&mut script.env_var)
                                                .desired_width(val_w - 20.0)
                                                .hint_text("token"),
                                        );
                                        attach_text_context_menu(
                                            &r, &script.env_var, true,
                                        );
                                        if r.changed() { changed = true; }

                                        if ui
                                            .add(
                                                egui::Button::new("×")
                                                    .min_size(egui::vec2(rm_w, 0.0)),
                                            )
                                            .cursor_hand()
                                            .clicked()
                                        {
                                            remove_script_index = Some(script_index);
                                        }
                                    });
                                }

                                if let Some(si) = remove_script_index {
                                    endpoint.scripts.remove(si);
                                    changed = true;
                                }

                                ui.add_space(4.0);
                                if ui.button("+ Add Rule").cursor_hand().clicked() {
                                    endpoint.scripts.push(ResponseScript::default());
                                    changed = true;
                                }
                            }

                            RequestEditorTab::Body => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Mode")
                                            .color(theme::MUTED)
                                            .size(11.0),
                                    );
                                    egui::ComboBox::from_id_salt("body-mode-picker")
                                        .selected_text(normalize_body_mode(
                                            &endpoint.body_mode,
                                        ))
                                        .show_ui(ui, |ui| {
                                            for mode in BODY_MODE_OPTIONS {
                                                if ui
                                                    .selectable_label(
                                                        normalize_body_mode(
                                                            &endpoint.body_mode,
                                                        ) == mode,
                                                        mode,
                                                    )
                                                    .cursor_hand()
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

                        // ── Resolved URL footer ─────────────────────────────
                        ui.add_space(8.0);
                        ui.separator();
                        let env_vars = self.selected_environment_variables();
                        let resolved =
                            resolve_endpoint_url(&self.endpoints[index], &env_vars);
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
    let remove_width = 28.0_f32;
    let key_width = (row_width * 0.35).clamp(100.0, 240.0);
    let value_width =
        (row_width - key_width - remove_width - spacing * 2.0).max(100.0);
    (key_width, value_width, remove_width)
}
