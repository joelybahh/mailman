use eframe::egui::{self, RichText, TextEdit};

use crate::app::{MailmanApp, ResponseViewTab};

use super::shared::{attach_text_context_menu, render_json_tree};
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
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Response").strong().size(14.0));
                    if let Some(status_code) = self.response.status_code {
                        let color = theme::status_code_color(status_code);
                        ui.add_space(6.0);
                        // Pill-style status badge
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

                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.response_view_tab, ResponseViewTab::Raw, "Raw");
                    ui.selectable_value(
                        &mut self.response_view_tab,
                        ResponseViewTab::Pretty,
                        "Pretty",
                    );
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(error) = &self.response.error {
                            ui.colored_label(theme::ERROR, error);
                        }

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
                        }

                        ui.separator();
                        match self.response_view_tab {
                            ResponseViewTab::Raw => {
                                let body_height = ui.available_height().max(120.0);
                                let response = ui.add_sized(
                                    [ui.available_width(), body_height],
                                    TextEdit::multiline(&mut self.response_body_view)
                                        .desired_width(f32::INFINITY)
                                        .code_editor(),
                                );
                                attach_text_context_menu(
                                    &response,
                                    &self.response_body_view,
                                    true,
                                );
                            }
                            ResponseViewTab::Pretty => {
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
                            }
                        }
                    });
            });
    }
}
