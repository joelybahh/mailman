use eframe::egui::{self, Color32, CursorIcon, RichText};

use super::theme;

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
    current_text: &str,
    editable: bool,
) {
    let text_edit_id = response.id;
    let text_char_count = current_text.chars().count();
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
            let mut state =
                egui::TextEdit::load_state(ui.ctx(), text_edit_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(text_char_count),
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
            ui.ctx().copy_text(rendered_text.clone());
        }
        response.context_menu(|ui| {
            if ui.button("Copy Value").clicked() {
                ui.ctx().copy_text(rendered_text.clone());
                ui.close();
            }
            if ui.button("Copy JSON Path").clicked() {
                ui.ctx().copy_text(path.to_owned());
                ui.close();
            }
        });
    });
}

pub(in crate::app) fn render_json_tree(
    ui: &mut egui::Ui,
    key: Option<&str>,
    value: &serde_json::Value,
    path: &str,
) {
    match value {
        serde_json::Value::Object(map) => {
            let label = match key {
                Some(key) => format!("{key}: {{}} {}", map.len()),
                None => format!("{{}} {}", map.len()),
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
            let label = match key {
                Some(key) => format!("{key}: [] {}", items.len()),
                None => format!("[] {}", items.len()),
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
