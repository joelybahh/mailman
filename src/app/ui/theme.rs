use eframe::egui::Color32;

// Logo-inspired palette: warm orange gradient background + postal blue uniform
pub(in crate::app) const ACCENT: Color32 = Color32::from_rgb(222, 108, 24); // logo's warm orange

pub(in crate::app) const ERROR: Color32 = Color32::from_rgb(230, 80, 80);
pub(in crate::app) const SUCCESS: Color32 = Color32::from_rgb(80, 195, 130);
pub(in crate::app) const WARNING: Color32 = Color32::from_rgb(225, 155, 55);
pub(in crate::app) const REDIRECT: Color32 = Color32::from_rgb(80, 175, 220);
pub(in crate::app) const MUTED: Color32 = Color32::from_rgb(140, 140, 155);

pub(in crate::app) const JSON_STRING: Color32 = Color32::from_rgb(120, 210, 170);
pub(in crate::app) const JSON_NUMBER: Color32 = Color32::from_rgb(240, 200, 120);
pub(in crate::app) const JSON_BOOL: Color32 = Color32::from_rgb(130, 180, 255);

pub(in crate::app) fn status_code_color(code: u16) -> Color32 {
    match code {
        200..=299 => SUCCESS,
        300..=399 => REDIRECT,
        400..=499 => WARNING,
        500..=599 => ERROR,
        _ => MUTED,
    }
}
