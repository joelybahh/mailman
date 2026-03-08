use eframe::egui::{self, RichText};

use crate::app::{MailmanApp, ResponseViewTab};

use super::shared::{render_json_tree, HandCursor};
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_response_panel(&mut self, ctx: &egui::Context) {
        let max_response_height = (ctx.content_rect().height() * 0.60).max(180.0);
        egui::TopBottomPanel::bottom("response")
            .resizable(true)
            .default_height(260.0)
            .min_height(140.0)
            .max_height(max_response_height)
            .show(ctx, |ui| {
                ui.add_space(4.0);

                // ── Status header ─────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Response").strong().size(14.0));
                    if let Some(status_code) = self.response.status_code {
                        let color = theme::status_code_color(status_code);
                        ui.add_space(6.0);
                        egui::Frame::default()
                            .corner_radius(egui::CornerRadius::same(4))
                            .fill(color.gamma_multiply(0.18))
                            .inner_margin(egui::Margin { left: 6, right: 6, top: 1, bottom: 1 })
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {}",
                                        status_code, self.response.status_text
                                    ))
                                    .strong()
                                    .size(12.0)
                                    .color(color),
                                );
                            });
                        if let Some(duration) = self.response.duration_ms {
                            ui.label(
                                RichText::new(format!("{duration} ms"))
                                    .color(theme::MUTED)
                                    .size(12.0),
                            );
                        }
                        if self.scripts_ran > 0 {
                            ui.add_space(4.0);
                            egui::Frame::default()
                                .corner_radius(egui::CornerRadius::same(4))
                                .fill(theme::ACCENT.gamma_multiply(0.15))
                                .inner_margin(egui::Margin { left: 6, right: 6, top: 1, bottom: 1 })
                                .show(ui, |ui| {
                                    let label = if self.scripts_ran == 1 {
                                        "1 script ran".to_owned()
                                    } else {
                                        format!("{} scripts ran", self.scripts_ran)
                                    };
                                    ui.label(
                                        RichText::new(label)
                                            .size(12.0)
                                            .color(theme::ACCENT),
                                    );
                                });
                        }
                    } else if !self.response.status_text.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&self.response.status_text)
                                .color(theme::MUTED)
                                .italics()
                                .size(13.0),
                        );
                    }
                });

                // ── Tab bar ───────────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.response_view_tab, ResponseViewTab::Raw, "Raw")
                        .cursor_hand();
                    ui.selectable_value(
                        &mut self.response_view_tab,
                        ResponseViewTab::Pretty,
                        "Pretty",
                    )
                    .cursor_hand();
                });
                ui.separator();

                // ── Error banner (both tabs) ──────────────────────────────────
                if let Some(error) = &self.response.error.clone() {
                    ui.colored_label(theme::ERROR, error);
                }

                // ── Tab bodies ────────────────────────────────────────────────
                match self.response_view_tab {
                    ResponseViewTab::Raw => {
                        // Virtual line-by-line scroll. egui's TextEdit computes
                        // a full galley layout for every character on every frame,
                        // which hangs on large payloads. show_rows only renders
                        // the visible lines → O(visible_lines) not O(total_chars).
                        if self.response_raw_chunks.is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("No response body.")
                                    .color(theme::MUTED)
                                    .italics(),
                            );
                        } else {
                            // response_raw_chunks is pre-computed once on
                            // arrival. Long lines are split at token boundaries
                            // (~400 chars each) so double-click word-boundary
                            // search is O(chunk) not O(line_length).
                            let chunk_count = self.response_raw_chunks.len();
                            let line_h =
                                ui.text_style_height(&egui::TextStyle::Monospace);
                            let sb = ui.spacing().scroll.bar_outer_margin
                                + ui.spacing().scroll.bar_width;

                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show_rows(ui, line_h, chunk_count, |ui, row_range| {
                                    ui.set_max_width(
                                        (ui.available_width() - sb).max(100.0),
                                    );
                                    for i in row_range {
                                        if let Some(chunk) = self.response_raw_chunks.get(i) {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(chunk.as_str())
                                                        .monospace()
                                                        .size(12.0),
                                                )
                                                .selectable(true),
                                            );
                                        }
                                    }
                                });
                        }
                    }

                    ResponseViewTab::Pretty => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if !self.response.headers.is_empty() {
                                    ui.collapsing(
                                        RichText::new(format!(
                                            "Headers ({})",
                                            self.response.headers.len()
                                        ))
                                        .size(12.0)
                                        .color(theme::MUTED),
                                        |ui| {
                                            for header in &self.response.headers {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        RichText::new(&header.key)
                                                            .strong()
                                                            .size(11.0),
                                                    );
                                                    ui.label(
                                                        RichText::new(&header.value)
                                                            .monospace()
                                                            .size(11.0),
                                                    );
                                                });
                                            }
                                        },
                                    );
                                    ui.separator();
                                }

                                if let Some(value) = &self.parsed_response_json {
                                    render_json_tree(ui, None, value, "$");
                                } else if let Some(err) = &self.parsed_response_json_error {
                                    ui.colored_label(
                                        theme::ERROR,
                                        format!("Not valid JSON: {err}"),
                                    );
                                } else if self.response.body.trim().is_empty() {
                                    ui.label(
                                        RichText::new("No response body.")
                                            .color(theme::MUTED)
                                            .italics(),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new(
                                            "Pretty view is only available for JSON responses.",
                                        )
                                        .color(theme::MUTED),
                                    );
                                }
                            });
                    }
                }
            });
    }
}
