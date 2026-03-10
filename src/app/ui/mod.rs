pub(in crate::app) mod auth;
pub(in crate::app) mod dialogs;
pub(in crate::app) mod env_panel;
pub(in crate::app) mod request;
pub(in crate::app) mod response;
pub(in crate::app) mod shared;
pub(in crate::app) mod sidebar;
pub(in crate::app) mod status;
pub(in crate::app) mod theme;
pub(in crate::app) mod toolbar;

use std::time::Duration;

use crate::app::{AppPhase, MailmanApp};

impl eframe::App for MailmanApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_window_resolution(ctx);
        ctx.set_visuals(egui::Visuals::dark());

        if self.phase != AppPhase::Ready {
            self.poll_auth_channel();
            self.render_auth_screen(ctx);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }

        self.poll_response_channel();

        self.render_toolbar(ctx);
        self.render_endpoints_panel(ctx);
        if self.show_environment_panel {
            self.render_environment_panel(ctx);
        }
        self.render_request_editor(ctx);
        self.render_response_panel(ctx);
        self.render_postman_import_dialog(ctx);
        self.render_share_bundle_dialogs(ctx);
        self.render_status_line(ctx);

        self.try_auto_save();
        self.try_auto_save_workspace_ui();
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(err) = self.storage.save_config(&self.config) {
            eprintln!("Failed to persist app config on exit: {err}");
        }
        if let Err(err) = self
            .storage
            .save_workspace_ui(&self.current_workspace_ui_state())
        {
            eprintln!("Failed to persist tab workspace on exit: {err}");
        }
    }
}

use eframe::egui;
