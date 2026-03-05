mod app;
mod domain;
mod models;
mod request_body;
mod storage;

use app::MailmanApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Mail Man",
        native_options,
        Box::new(|_cc| Ok(Box::new(MailmanApp::new()))),
    )
}

#[cfg(test)]
mod tests;
