mod app;
mod domain;
mod models;
mod request_body;
mod storage;

use app::MailmanApp;
use eframe::egui::ViewportBuilder;
use storage::AppStorage;

fn main() -> eframe::Result<()> {
    let mut native_options = eframe::NativeOptions::default();
    let mut viewport = ViewportBuilder::default();

    #[cfg(target_os = "macos")]
    {
        // Use the bundled app icon on macOS to match Dock sizing behavior.
        viewport = viewport.with_icon(eframe::egui::IconData::default());
        // Make the title bar transparent and extend content behind it, so the
        // toolbar sits next to the traffic-light buttons (Xcode / Arc style).
        // with_titlebar_shown(false) = transparent titlebar (content shows through)
        // with_fullsize_content_view(true) = content rect fills the whole window
        // with_title_shown(false) = no "Mailman" text in the title bar
        viewport = viewport
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true)
            .with_title_shown(false);
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(icon) =
            eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/128x128.png"))
        {
            viewport = viewport.with_icon(icon);
        }
    }

    if let Ok(config) = AppStorage::new().load_config() {
        if let (Some(width), Some(height)) = (config.window_width, config.window_height) {
            if width > 0 && height > 0 {
                viewport = viewport.with_inner_size([width as f32, height as f32]);
            }
        }
    }
    native_options.viewport = viewport;

    eframe::run_native(
        "Mailman",
        native_options,
        Box::new(|_cc| Ok(Box::new(MailmanApp::new()))),
    )
}

#[cfg(test)]
mod tests;
