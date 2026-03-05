mod app;
mod domain;
mod models;
mod request_body;
mod storage;

use app::MailmanApp;
use storage::AppStorage;

fn main() -> eframe::Result<()> {
    let mut native_options = eframe::NativeOptions::default();
    if let Ok(config) = AppStorage::new().load_config() {
        if let (Some(width), Some(height)) = (config.window_width, config.window_height) {
            if width > 0 && height > 0 {
                native_options.viewport = eframe::egui::ViewportBuilder::default()
                    .with_inner_size([width as f32, height as f32]);
            }
        }
    }

    eframe::run_native(
        "Mail Man",
        native_options,
        Box::new(|_cc| Ok(Box::new(MailmanApp::new()))),
    )
}

#[cfg(test)]
mod tests;
