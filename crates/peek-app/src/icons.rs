//! Phosphor icon font (MIT, https://phosphoricons.com), regular weight.
//! Only the glyphs Peek uses are named here.

pub const ARROW_CLOCKWISE: &str = "\u{E036}";
pub const CAMERA: &str = "\u{E10E}";
pub const CHART_BAR: &str = "\u{E150}";
pub const CORNERS_OUT: &str = "\u{E1D0}";
pub const FILM_STRIP: &str = "\u{E792}";
pub const FOLDER_OPEN: &str = "\u{E256}";
pub const GEAR: &str = "\u{E270}";
pub const KEYBOARD: &str = "\u{E2D8}";
pub const LIST: &str = "\u{E2F0}";
pub const PAUSE: &str = "\u{E39E}";
pub const PLAY: &str = "\u{E3D0}";
pub const PLUS: &str = "\u{E3D4}";
pub const POWER: &str = "\u{E3DA}";
pub const PUSH_PIN: &str = "\u{E3E2}";
pub const RECORD: &str = "\u{E3EE}";
pub const SIGN_OUT: &str = "\u{E42A}";
pub const STAR: &str = "\u{E46A}";
pub const WARNING: &str = "\u{E4E0}";
pub const X: &str = "\u{E4F6}";

/// Add the icon font as a fallback after the text font, so icons can be
/// mixed into any label.
pub fn install(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "phosphor".into(),
        egui::FontData::from_static(include_bytes!("../assets/Phosphor.ttf")).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(keys) = fonts.families.get_mut(&family) {
            let at = keys.len().min(1);
            keys.insert(at, "phosphor".into());
        }
    }
}
