use eframe::egui::{self, RichText};

use crate::app::MailmanApp;
use crate::models::ResponseViewTab;

use super::shared::{HandCursor, render_json_tree};
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

                let Some(index) = self.active_request_tab_index() else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Response").strong().size(14.0));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("No active request tab.")
                                .color(theme::MUTED)
                                .italics()
                                .size(13.0),
                        );
                    });
                    return;
                };

                let response_snapshot = self.open_request_tabs[index].response.clone();
                let scripts_ran = self.open_request_tabs[index].scripts_ran;
                let response_raw_chunks = self.open_request_tabs[index].response_raw_chunks.clone();
                let parsed_response_json =
                    self.open_request_tabs[index].parsed_response_json.clone();
                let parsed_response_json_error = self.open_request_tabs[index]
                    .parsed_response_json_error
                    .clone();
                let mut response_view_tab = self.open_request_tabs[index].response_view_tab;

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Response").strong().size(14.0));
                    if let Some(status_code) = response_snapshot.status_code {
                        let color = theme::status_code_color(status_code);
                        ui.add_space(6.0);
                        egui::Frame::default()
                            .corner_radius(egui::CornerRadius::same(4))
                            .fill(color.gamma_multiply(0.18))
                            .inner_margin(egui::Margin {
                                left: 6,
                                right: 6,
                                top: 1,
                                bottom: 1,
                            })
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {}",
                                        status_code, response_snapshot.status_text
                                    ))
                                    .strong()
                                    .size(12.0)
                                    .color(color),
                                );
                            });
                        if let Some(duration) = response_snapshot.duration_ms {
                            ui.label(
                                RichText::new(format!("{duration} ms"))
                                    .color(theme::MUTED)
                                    .size(12.0),
                            );
                        }
                        if scripts_ran > 0 {
                            ui.add_space(4.0);
                            egui::Frame::default()
                                .corner_radius(egui::CornerRadius::same(4))
                                .fill(theme::ACCENT.gamma_multiply(0.15))
                                .inner_margin(egui::Margin {
                                    left: 6,
                                    right: 6,
                                    top: 1,
                                    bottom: 1,
                                })
                                .show(ui, |ui| {
                                    let label = if scripts_ran == 1 {
                                        "1 script ran".to_owned()
                                    } else {
                                        format!("{scripts_ran} scripts ran")
                                    };
                                    ui.label(RichText::new(label).size(12.0).color(theme::ACCENT));
                                });
                        }
                    } else if !response_snapshot.status_text.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&response_snapshot.status_text)
                                .color(theme::MUTED)
                                .italics()
                                .size(13.0),
                        );
                    }
                });

                ui.horizontal(|ui| {
                    ui.selectable_value(&mut response_view_tab, ResponseViewTab::Raw, "Raw")
                        .cursor_hand();
                    ui.selectable_value(&mut response_view_tab, ResponseViewTab::Pretty, "Pretty")
                        .cursor_hand();
                });
                ui.separator();

                if let Some(error) = &response_snapshot.error {
                    ui.colored_label(theme::ERROR, error);
                }

                match response_view_tab {
                    ResponseViewTab::Raw => {
                        if response_raw_chunks.is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("No response body.")
                                    .color(theme::MUTED)
                                    .italics(),
                            );
                        } else {
                            let chunk_count = response_raw_chunks.len();
                            let line_h = ui.text_style_height(&egui::TextStyle::Monospace);
                            let sb = ui.spacing().scroll.bar_outer_margin
                                + ui.spacing().scroll.bar_width;

                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show_rows(ui, line_h, chunk_count, |ui, row_range| {
                                    ui.set_max_width((ui.available_width() - sb).max(100.0));
                                    for i in row_range {
                                        if let Some(chunk) = response_raw_chunks.get(i) {
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
                                if !response_snapshot.headers.is_empty() {
                                    ui.collapsing(
                                        RichText::new(format!(
                                            "Headers ({})",
                                            response_snapshot.headers.len()
                                        ))
                                        .size(12.0)
                                        .color(theme::MUTED),
                                        |ui| {
                                            for header in &response_snapshot.headers {
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

                                if let Some(value) = &parsed_response_json {
                                    render_json_tree(ui, None, value, "$");
                                } else if let Some(err) = &parsed_response_json_error {
                                    ui.colored_label(
                                        theme::ERROR,
                                        format!("Not valid JSON: {err}"),
                                    );
                                } else if response_snapshot.body.trim().is_empty() {
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

                if let Some(tab) = self.active_request_tab_mut() {
                    tab.response_view_tab = response_view_tab;
                }
            });
    }
}
