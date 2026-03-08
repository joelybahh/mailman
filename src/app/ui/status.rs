use eframe::egui::{self, RichText};

use crate::app::MailmanApp;

use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_status_line(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status-line")
            .exact_height(22.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let status_text = if self.status_line.is_empty() {
                        RichText::new("Ready").color(theme::MUTED).size(11.0)
                    } else {
                        RichText::new(&self.status_line).size(11.0)
                    };
                    ui.label(status_text);
                    ui.separator();
                    ui.label(
                        RichText::new(format!("{}", self.storage.base_dir.display()))
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                });
            });
    }
}
