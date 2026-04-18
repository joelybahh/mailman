use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::app::{MailmanApp, PendingRequestAction};
use crate::domain::{method_color, resolve_endpoint_url};
use crate::models::{
    BODY_MODE_OPTIONS, KeyValue, METHOD_OPTIONS, RequestEditorTab, ResponseScript,
};
use crate::request_body::{
    normalize_body_mode, parse_body_fields_lossless, serialize_body_fields_lossless,
};

use super::shared::{
    HandCursor, REQUEST_HEADER_BAR_HEIGHT, REQUEST_HEADER_CONTENT_PAD_Y, attach_text_context_menu,
};
use super::theme;

const RIGHT_PAD: f32 = 12.0;

impl MailmanApp {
    pub(in crate::app) fn render_request_editor(&mut self, ctx: &egui::Context) {
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
                self.render_request_tab_strip(ui);
                ui.add_space(4.0);

                let Some(index) = self.active_request_tab_index() else {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Open or create a request tab.")
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
                let mut request_editor_tab = self.open_request_tabs[index].editor_tab;
                let env_vars = self.selected_environment_variables();
                let is_in_flight = self
                    .active_request_tab()
                    .map(|tab| self.is_request_in_flight(&tab.id))
                    .unwrap_or(false);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let sb = ui.spacing().scroll.bar_outer_margin
                            + ui.spacing().scroll.bar_width;
                        ui.set_max_width((ui.available_width() - sb).max(200.0));

                        let tab = &mut self.open_request_tabs[index];
                        let endpoint = &mut tab.draft;

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
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

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
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

                                    ui.label(RichText::new("/").color(theme::MUTED).size(11.0));

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

                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(format!("method-picker-{}", endpoint.id))
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
                            let url_w = (ui.available_width() - send_w - spacing).max(80.0);
                            let r = ui.add(
                                TextEdit::singleline(&mut endpoint.url)
                                    .desired_width(url_w)
                                    .hint_text("https://${api_host}/resource"),
                            );
                            attach_text_context_menu(&r, &endpoint.url, true);
                            if r.changed() {
                                changed = true;
                            }

                            let send_label = if is_in_flight { "Sending..." } else { "Send" };
                            let send_btn = egui::Button::new(
                                RichText::new(send_label)
                                    .color(Color32::WHITE)
                                    .strong()
                                    .size(13.0),
                            )
                            .fill(theme::ACCENT)
                            .min_size(egui::vec2(send_w, 0.0));
                            if ui
                                .add_enabled(self.in_flight_tab_id.is_none(), send_btn)
                                .cursor_hand()
                                .clicked()
                            {
                                do_send = true;
                            }
                        });

                        ui.add_space(8.0);
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

                        match request_editor_tab {
                            RequestEditorTab::Params => {
                                let (key_w, val_w, rm_w) = kv_row_widths(
                                    ui.available_width(),
                                    ui.spacing().item_spacing.x,
                                );
                                ui.label(
                                    RichText::new(
                                        "Query params are appended to the URL when sending.",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.0),
                                );
                                ui.add_space(4.0);
                                for (param_index, param) in endpoint.query_params.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let r = ui.add(
                                            TextEdit::singleline(&mut param.key)
                                                .desired_width(key_w)
                                                .hint_text("key"),
                                        );
                                        attach_text_context_menu(&r, &param.key, true);
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            TextEdit::singleline(&mut param.value)
                                                .desired_width(val_w)
                                                .hint_text("value"),
                                        );
                                        attach_text_context_menu(&r, &param.value, true);
                                        if r.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add(egui::Button::new("x").min_size(egui::vec2(rm_w, 0.0)))
                                            .cursor_hand()
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
                                ui.add_space(4.0);
                                if ui.button("+ Add Param").cursor_hand().clicked() {
                                    endpoint.query_params.push(KeyValue::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Headers => {
                                let (key_w, val_w, rm_w) = kv_row_widths(
                                    ui.available_width(),
                                    ui.spacing().item_spacing.x,
                                );
                                ui.label(
                                    RichText::new("Example: Authorization = Bearer ${token}")
                                        .color(theme::MUTED)
                                        .size(11.0),
                                );
                                ui.add_space(4.0);
                                for (header_index, header) in endpoint.headers.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let r = ui.add(
                                            TextEdit::singleline(&mut header.key)
                                                .desired_width(key_w)
                                                .hint_text("Header name"),
                                        );
                                        attach_text_context_menu(&r, &header.key, true);
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            TextEdit::singleline(&mut header.value)
                                                .desired_width(val_w)
                                                .hint_text("Header value"),
                                        );
                                        attach_text_context_menu(&r, &header.value, true);
                                        if r.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add(egui::Button::new("x").min_size(egui::vec2(rm_w, 0.0)))
                                            .cursor_hand()
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
                                ui.add_space(4.0);
                                if ui.button("+ Add Header").cursor_hand().clicked() {
                                    endpoint.headers.push(KeyValue::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Scripts => {
                                let (key_w, val_w, rm_w) = kv_row_widths(
                                    ui.available_width(),
                                    ui.spacing().item_spacing.x,
                                );
                                ui.label(
                                    RichText::new(
                                        "After a 2xx response, each rule extracts a value from the JSON body and writes it to an env variable.",
                                    )
                                    .color(theme::MUTED)
                                    .size(11.0),
                                );
                                ui.add_space(4.0);
                                for (script_index, script) in endpoint.scripts.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        let r = ui.add(
                                            TextEdit::singleline(&mut script.extract_key)
                                                .desired_width(key_w)
                                                .hint_text("data.access_token"),
                                        );
                                        attach_text_context_menu(&r, &script.extract_key, true);
                                        if r.changed() {
                                            changed = true;
                                        }

                                        ui.label(RichText::new("->").color(theme::MUTED).size(12.0));

                                        let r = ui.add(
                                            TextEdit::singleline(&mut script.env_var)
                                                .desired_width(val_w - 20.0)
                                                .hint_text("token"),
                                        );
                                        attach_text_context_menu(&r, &script.env_var, true);
                                        if r.changed() {
                                            changed = true;
                                        }

                                        if ui
                                            .add(egui::Button::new("x").min_size(egui::vec2(rm_w, 0.0)))
                                            .cursor_hand()
                                            .clicked()
                                        {
                                            remove_script_index = Some(script_index);
                                        }
                                    });
                                }
                                if let Some(script_index) = remove_script_index {
                                    endpoint.scripts.remove(script_index);
                                    changed = true;
                                }
                                ui.add_space(4.0);
                                if ui.button("+ Add Rule").cursor_hand().clicked() {
                                    endpoint.scripts.push(ResponseScript::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Body => {
                                ui.label(RichText::new("Mode").color(theme::MUTED).size(11.0));
                                ui.add_space(2.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                    for mode in BODY_MODE_OPTIONS {
                                        let label = match mode {
                                            "urlencoded" => "x-www-form-urlencoded",
                                            other => other,
                                        };
                                        if ui
                                            .selectable_value(
                                                &mut endpoint.body_mode,
                                                mode.to_owned(),
                                                label,
                                            )
                                            .cursor_hand()
                                            .clicked()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                                ui.add_space(4.0);
                                let body_mode = normalize_body_mode(&endpoint.body_mode);
                                match body_mode {
                                    "urlencoded" => {
                                        changed |= render_body_fields_table(
                                            ui,
                                            &mut endpoint.body,
                                            "&",
                                            "Use the empty row to add URL-encoded fields.",
                                            "value",
                                        );
                                    }
                                    "form-data" => {
                                        changed |= render_body_fields_table(
                                            ui,
                                            &mut endpoint.body,
                                            "\n",
                                            "Use @/path/to/file in the value column to upload a file.",
                                            "value or @/path/to/file",
                                        );
                                    }
                                    "binary" => {
                                        let r = ui.add(
                                            TextEdit::multiline(&mut endpoint.body)
                                                .desired_width(f32::INFINITY)
                                                .desired_rows(12)
                                                .code_editor()
                                                .hint_text("@/tmp/payload.bin"),
                                        );
                                        attach_text_context_menu(&r, &endpoint.body, true);
                                        if r.changed() {
                                            changed = true;
                                        }
                                    }
                                    "none" | "raw" => {
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
                                    _ => {}
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.separator();
                        let resolved = resolve_endpoint_url(endpoint, &env_vars);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Resolved URL:")
                                    .color(theme::MUTED)
                                    .size(11.0),
                            );
                            ui.label(RichText::new(&resolved).monospace().size(11.0));
                        });
                        ui.label(
                            RichText::new("Use ${variable_name} in URL, params, headers, and body.")
                                .color(theme::MUTED)
                                .size(11.0),
                        );

                        tab.editor_tab = request_editor_tab;
                    });

                if changed {
                    self.mark_active_request_dirty();
                } else if let Some(tab) = self.active_request_tab_mut() {
                    tab.editor_tab = request_editor_tab;
                }

                if do_send {
                    self.send_selected_request();
                }
                if do_copy_curl {
                    self.copy_curl_for_selected_request(ctx);
                }
            });

        self.render_request_tab_dialogs(ctx);
    }

    fn render_request_tab_strip(&mut self, ui: &mut egui::Ui) {
        let tabs = self
            .open_request_tabs
            .iter()
            .map(|tab| {
                (
                    tab.id.clone(),
                    tab.draft.method.clone(),
                    tab.draft.name.clone(),
                    tab.is_dirty,
                    self.active_request_tab_id.as_deref() == Some(tab.id.as_str()),
                    self.is_request_in_flight(&tab.id),
                )
            })
            .collect::<Vec<_>>();
        let mut pending_move: Option<(String, usize)> = None;

        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(
                0,
                REQUEST_HEADER_CONTENT_PAD_Y as i8,
            ))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(
                        ui.available_width(),
                        REQUEST_HEADER_BAR_HEIGHT - REQUEST_HEADER_CONTENT_PAD_Y * 2.0,
                    ),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("request-tabs-strip")
                            .max_height(
                                REQUEST_HEADER_BAR_HEIGHT - REQUEST_HEADER_CONTENT_PAD_Y * 2.0,
                            )
                            .show(ui, |ui| {
                                ui.set_min_height(
                                    REQUEST_HEADER_BAR_HEIGHT - REQUEST_HEADER_CONTENT_PAD_Y * 2.0,
                                );
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            for (
                                                index,
                                                (
                                                    tab_id,
                                                    method,
                                                    name,
                                                    is_dirty,
                                                    is_active,
                                                    is_in_flight,
                                                ),
                                            ) in tabs.iter().enumerate()
                                            {
                                                ui.group(|ui| {
                                                    ui.spacing_mut().item_spacing.x = 4.0;
                                                    let label = if *is_dirty {
                                                        format!(
                                                            "* {}",
                                                            if name.is_empty() {
                                                                "Untitled"
                                                            } else {
                                                                name
                                                            }
                                                        )
                                                    } else if name.is_empty() {
                                                        "Untitled".to_owned()
                                                    } else {
                                                        name.clone()
                                                    };
                                                    let response = ui
                                                        .selectable_label(
                                                            *is_active,
                                                            RichText::new(format!(
                                                                "{method} {label}"
                                                            ))
                                                            .color(method_color(method))
                                                            .size(11.5),
                                                        )
                                                        .cursor_hand();
                                                    if response.clicked() {
                                                        self.activate_request_tab(Some(
                                                            tab_id.clone(),
                                                        ));
                                                    }
                                                    if response.drag_started() {
                                                        self.dragging_tab_id = Some(tab_id.clone());
                                                    }
                                                    if let Some(dragging_tab_id) =
                                                        self.dragging_tab_id.clone()
                                                        && dragging_tab_id != *tab_id
                                                        && response.hovered()
                                                        && ui.input(|input| {
                                                            input.pointer.primary_down()
                                                        })
                                                    {
                                                        pending_move =
                                                            Some((dragging_tab_id, index));
                                                    }
                                                    response.context_menu(|ui| {
                                                        if ui.button("Rename").clicked() {
                                                            self.rename_tab_id =
                                                                Some(tab_id.clone());
                                                            self.rename_buffer = name.clone();
                                                            ui.close();
                                                        }
                                                        if ui.button("Close All").clicked() {
                                                            self.pending_request_action = Some(
                                                                PendingRequestAction::CloseAll,
                                                            );
                                                            ui.close();
                                                        }
                                                        if ui.button("Close All Saved").clicked() {
                                                            self.close_all_saved_tabs();
                                                            ui.close();
                                                        }
                                                    });

                                                    if ui
                                                        .add(
                                                            egui::Button::new(
                                                                RichText::new("x")
                                                                    .size(11.0)
                                                                    .color(if *is_in_flight {
                                                                        theme::MUTED
                                                                    } else {
                                                                        ui.visuals().text_color()
                                                                    }),
                                                            )
                                                            .frame(false),
                                                        )
                                                        .cursor_hand()
                                                        .clicked()
                                                    {
                                                        self.pending_request_action =
                                                            Some(PendingRequestAction::CloseTab {
                                                                tab_id: tab_id.clone(),
                                                            });
                                                    }
                                                });
                                            }
                                        });
                                    },
                                );
                            });
                    },
                );
            });

        if let Some((dragging_tab_id, target_index)) = pending_move {
            self.move_request_tab(&dragging_tab_id, target_index);
        }
        if !ui.input(|input| input.pointer.primary_down()) {
            self.dragging_tab_id = None;
        }

        ui.separator();
    }

    fn render_request_tab_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(rename_tab_id) = self.rename_tab_id.clone() {
            let mut open = true;
            let mut should_close = false;
            egui::Window::new("Rename Request")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    let response = ui.add(
                        TextEdit::singleline(&mut self.rename_buffer)
                            .desired_width(320.0)
                            .hint_text("Request name"),
                    );
                    attach_text_context_menu(&response, &self.rename_buffer, true);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").cursor_hand().clicked() {
                            if let Some(tab) = self
                                .open_request_tabs
                                .iter_mut()
                                .find(|tab| tab.id == rename_tab_id)
                            {
                                tab.draft.name = self.rename_buffer.trim().to_owned();
                                tab.is_dirty = true;
                                self.mark_workspace_ui_dirty();
                            }
                            should_close = true;
                        }
                        if ui.button("Cancel").cursor_hand().clicked() {
                            should_close = true;
                        }
                    });
                });
            if !open || should_close {
                self.rename_tab_id = None;
                self.rename_buffer.clear();
            }
        }

        if let Some(action) = self.pending_request_action.clone() {
            let dirty_count = self.pending_request_action_dirty_tab_ids().len();
            let mut open = true;
            let mut should_close = false;
            let title = match action {
                PendingRequestAction::CloseTab { .. } => "Close Request Tab",
                PendingRequestAction::CloseAll => "Close All Tabs",
                PendingRequestAction::DeleteActive => "Delete Request",
                PendingRequestAction::DeleteAll => "Delete All Requests",
            };
            egui::Window::new(title)
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    let message = if dirty_count == 0 {
                        "This action will apply immediately."
                    } else if dirty_count == 1 {
                        "There is 1 dirty request tab. Save it before continuing?"
                    } else {
                        "There are dirty request tabs. Save them before continuing?"
                    };
                    ui.label(message);
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if dirty_count > 0
                            && ui
                                .button(
                                    if matches!(action, PendingRequestAction::CloseTab { .. }) {
                                        "Save"
                                    } else {
                                        "Save All"
                                    },
                                )
                                .cursor_hand()
                                .clicked()
                        {
                            if let Err(err) = self.resolve_pending_request_action(true) {
                                self.status_line = err;
                            }
                            should_close = true;
                        }

                        if ui
                            .button(if dirty_count > 0 {
                                "Discard"
                            } else {
                                "Continue"
                            })
                            .cursor_hand()
                            .clicked()
                        {
                            if let Err(err) = self.resolve_pending_request_action(false) {
                                self.status_line = err;
                            }
                            should_close = true;
                        }

                        if ui.button("Cancel").cursor_hand().clicked() {
                            should_close = true;
                        }
                    });
                });
            if !open || should_close {
                if should_close {
                    self.pending_request_action = None;
                }
                if !open {
                    self.pending_request_action = None;
                }
            }
        }
    }
}

fn kv_row_widths(row_width: f32, spacing: f32) -> (f32, f32, f32) {
    let remove_width = 28.0_f32;
    let key_width = (row_width * 0.35).clamp(100.0, 240.0);
    let value_width = (row_width - key_width - remove_width - spacing * 2.0).max(100.0);
    (key_width, value_width, remove_width)
}

fn render_body_fields_table(
    ui: &mut egui::Ui,
    body: &mut String,
    separator: &str,
    helper_text: &str,
    value_hint: &str,
) -> bool {
    let (key_w, val_w, rm_w) = kv_row_widths(ui.available_width(), ui.spacing().item_spacing.x);
    ui.label(RichText::new(helper_text).color(theme::MUTED).size(11.0));
    ui.add_space(4.0);

    let mut fields = parse_body_fields_lossless(body)
        .into_iter()
        .map(|(key, value)| KeyValue { key, value })
        .collect::<Vec<_>>();
    fields.push(KeyValue::default());

    let mut changed = false;
    let mut remove_index = None;
    let last_index = fields.len().saturating_sub(1);

    for (field_index, field) in fields.iter_mut().enumerate() {
        let is_trailing_blank = field_index == last_index
            && field.key.trim().is_empty()
            && field.value.trim().is_empty();

        ui.horizontal(|ui| {
            let r = ui.add(
                TextEdit::singleline(&mut field.key)
                    .desired_width(key_w)
                    .hint_text("key"),
            );
            attach_text_context_menu(&r, &field.key, true);
            if r.changed() {
                changed = true;
            }

            let r = ui.add(
                TextEdit::singleline(&mut field.value)
                    .desired_width(val_w)
                    .hint_text(value_hint),
            );
            attach_text_context_menu(&r, &field.value, true);
            if r.changed() {
                changed = true;
            }

            if is_trailing_blank {
                ui.add_space(rm_w);
            } else if ui
                .add(egui::Button::new("x").min_size(egui::vec2(rm_w, 0.0)))
                .cursor_hand()
                .clicked()
            {
                remove_index = Some(field_index);
            }
        });
    }

    if let Some(remove_index) = remove_index {
        fields.remove(remove_index);
        changed = true;
    }

    if changed {
        *body = serialize_body_fields_lossless(&fields, separator);
    }

    changed
}
