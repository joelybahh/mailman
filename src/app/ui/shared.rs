use eframe::egui::{self, Color32, CursorIcon, RichText};

use super::theme;

pub(in crate::app) const REQUEST_HEADER_BAR_HEIGHT: f32 = 44.0;
pub(in crate::app) const REQUEST_HEADER_CONTENT_PAD_Y: f32 = 6.0;

/// Convenience extension — chain `.cursor_hand()` on any [`egui::Response`] to show a
/// pointer cursor when hovered. This is the only reliable per-widget mechanism
/// in this version of egui (`style.visuals.interact_cursor` is defined but
/// never read by widget rendering).
pub(in crate::app) trait HandCursor: Sized {
    fn cursor_hand(self) -> Self;
}

impl HandCursor for egui::Response {
    #[inline]
    fn cursor_hand(self) -> Self {
        self.on_hover_cursor(CursorIcon::PointingHand)
    }
}

pub(in crate::app) fn attach_text_context_menu(
    response: &egui::Response,
    _current_text: &str,
    editable: bool,
) {
    let text_edit_id = response.id;
    // Do NOT compute chars().count() here — this function is called every frame
    // and O(n) char iteration on a large response body freezes the UI at 60fps.
    // "Select All" uses usize::MAX instead; egui clamps it to the actual end.
    let selection_backup_id = text_edit_id.with("selection_backup");

    if let Some(state) = egui::TextEdit::load_state(&response.ctx, text_edit_id)
        && let Some(range) = state.cursor.char_range()
        && !range.is_empty()
        && !response.secondary_clicked()
    {
        response
            .ctx
            .data_mut(|data| data.insert_temp(selection_backup_id, range));
    }

    if response.secondary_clicked()
        && let Some(saved_range) = response
            .ctx
            .data(|data| data.get_temp::<egui::text::CCursorRange>(selection_backup_id))
        && let Some(mut state) = egui::TextEdit::load_state(&response.ctx, text_edit_id)
    {
        state.cursor.set_char_range(Some(saved_range));
        egui::TextEdit::store_state(&response.ctx, text_edit_id, state);
    }

    response.context_menu(move |ui| {
        if editable && ui.button("Cut").clicked() {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(text_edit_id));
            ui.ctx()
                .input_mut(|input| input.events.push(egui::Event::Cut));
            ui.close();
        }

        if ui.button("Copy").clicked() {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(text_edit_id));
            ui.ctx()
                .input_mut(|input| input.events.push(egui::Event::Copy));
            ui.close();
        }

        if editable && ui.button("Paste").clicked() {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(text_edit_id));
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }

        if ui.button("Select All").clicked() {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(text_edit_id));
            let mut state = egui::TextEdit::load_state(ui.ctx(), text_edit_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(usize::MAX), // egui clamps to actual end
                )));
            egui::TextEdit::store_state(ui.ctx(), text_edit_id, state);
            ui.close();
        }
    });
}

pub(in crate::app) fn render_json_leaf(
    ui: &mut egui::Ui,
    key: Option<&str>,
    path: &str,
    value_text: impl Into<String>,
    value_color: Color32,
) {
    let rendered_text = value_text.into();
    // Strip surrounding double-quotes for clipboard so copying a JSON string
    // value like `"Gold"` puts `Gold` on the clipboard, not `"Gold"`.
    let copy_text = if rendered_text.starts_with('"')
        && rendered_text.ends_with('"')
        && rendered_text.len() >= 2
    {
        rendered_text[1..rendered_text.len() - 1].to_owned()
    } else {
        rendered_text.clone()
    };

    ui.horizontal_wrapped(|ui| {
        if let Some(key) = key {
            ui.label(RichText::new(key).strong().color(theme::MUTED));
            ui.label(RichText::new(":").color(theme::MUTED));
        }
        let response = ui.add(
            egui::Label::new(
                RichText::new(rendered_text.clone())
                    .color(value_color)
                    .monospace(),
            )
            .sense(egui::Sense::click()),
        );

        if response.double_clicked() {
            ui.ctx().copy_text(copy_text.clone());
        }
        response.context_menu(|ui| {
            if ui.button("Copy Value").clicked() {
                ui.ctx().copy_text(copy_text.clone());
                ui.close();
            }
            if ui.button("Copy JSON Path").clicked() {
                ui.ctx().copy_text(path.to_owned());
                ui.close();
            }
        });
    });
}

/// Build a compact single-line preview of an object or array value, truncated
/// at `max_chars` with a trailing `…` when needed. Used as the suffix in
/// collapsing header labels so the user gets an at-a-glance sense of the
/// contents without expanding.
fn json_preview(value: &serde_json::Value, max_chars: usize) -> String {
    let preview = match value {
        serde_json::Value::Object(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let v_str = match v {
                        serde_json::Value::String(s) => format!("\"{s}\""),
                        serde_json::Value::Null => "null".to_owned(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Object(m) => format!("{{…{}}}", m.len()),
                        serde_json::Value::Array(a) => format!("[…{}]", a.len()),
                    };
                    format!("\"{k}\": {v_str}")
                })
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => format!("\"{s}\""),
                    serde_json::Value::Null => "null".to_owned(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Object(m) => format!("{{…{}}}", m.len()),
                    serde_json::Value::Array(a) => format!("[…{}]", a.len()),
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
        _ => return String::new(),
    };

    if preview.chars().count() <= max_chars {
        preview
    } else {
        let truncated: String = preview.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

pub(in crate::app) fn render_json_tree(
    ui: &mut egui::Ui,
    key: Option<&str>,
    value: &serde_json::Value,
    path: &str,
) {
    match value {
        serde_json::Value::Object(map) => {
            // At the root level show just the key count; for nested objects
            // (e.g. array items) show a truncated inline preview instead.
            let label = if path == "$" {
                match key {
                    Some(k) => format!("{k}: {{}} {}", map.len()),
                    None => format!("{{}} {}", map.len()),
                }
            } else {
                let preview = json_preview(value, 80);
                match key {
                    Some(k) => format!("{k}: {preview}"),
                    None => preview,
                }
            };
            egui::CollapsingHeader::new(label)
                .id_salt(path)
                .default_open(path == "$")
                .show(ui, |ui| {
                    for (child_key, child_value) in map {
                        let child_path = format!("{path}.{child_key}");
                        render_json_tree(ui, Some(child_key), child_value, &child_path);
                    }
                });
        }
        serde_json::Value::Array(items) => {
            let label = if path == "$" {
                match key {
                    Some(k) => format!("{k}: [] {}", items.len()),
                    None => format!("[] {}", items.len()),
                }
            } else {
                let preview = json_preview(value, 80);
                match key {
                    Some(k) => format!("{k}: {preview}"),
                    None => preview,
                }
            };
            egui::CollapsingHeader::new(label)
                .id_salt(path)
                .default_open(path == "$")
                .show(ui, |ui| {
                    for (index, item) in items.iter().enumerate() {
                        let child_key = format!("[{index}]");
                        let child_path = format!("{path}[{index}]");
                        render_json_tree(ui, Some(&child_key), item, &child_path);
                    }
                });
        }
        serde_json::Value::String(text) => {
            render_json_leaf(ui, key, path, format!("\"{text}\""), theme::JSON_STRING);
        }
        serde_json::Value::Number(number) => {
            render_json_leaf(ui, key, path, number.to_string(), theme::JSON_NUMBER);
        }
        serde_json::Value::Bool(val) => {
            render_json_leaf(ui, key, path, val.to_string(), theme::JSON_BOOL);
        }
        serde_json::Value::Null => {
            render_json_leaf(ui, key, path, "null", theme::MUTED);
        }
    }
}
