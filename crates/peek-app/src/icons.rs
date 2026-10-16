//! Phosphor icon font (MIT, https://phosphoricons.com), regular and fill weights.
//! Only the glyphs Peek uses are named here; both weights share code points.

pub const ARROW_CLOCKWISE: &str = "\u{E036}";
pub const ARROW_RIGHT: &str = "\u{E06C}";
pub const CAMERA: &str = "\u{E10E}";
pub const CHART_BAR: &str = "\u{E150}";
pub const COPY: &str = "\u{E1CA}";
pub const CORNERS_OUT: &str = "\u{E1D0}";
pub const DESKTOP: &str = "\u{E560}";
pub const DOTS_THREE: &str = "\u{E1FE}";
pub const FILM_STRIP: &str = "\u{E792}";
pub const FOLDER_OPEN: &str = "\u{E256}";
pub const GEAR: &str = "\u{E270}";
pub const GIT_COMMIT: &str = "\u{E27A}";
pub const HOUSE: &str = "\u{E2C2}";
pub const INFO: &str = "\u{E2CE}";
pub const KEYBOARD: &str = "\u{E2D8}";
pub const LIGHTNING: &str = "\u{E2DE}";
pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
pub const MONITOR: &str = "\u{E32E}";
pub const PAUSE: &str = "\u{E39E}";
pub const PENCIL_SIMPLE: &str = "\u{E3B4}";
pub const PLAY: &str = "\u{E3D0}";
pub const PLUGS_CONNECTED: &str = "\u{EB5A}";
pub const PLUS: &str = "\u{E3D4}";
pub const POWER: &str = "\u{E3DA}";
pub const PUSH_PIN: &str = "\u{E3E2}";
pub const RECORD: &str = "\u{E3EE}";
pub const SIDEBAR_SIMPLE: &str = "\u{EC24}";
pub const SIGN_OUT: &str = "\u{E42A}";
pub const STAR: &str = "\u{E46A}";
pub const TERMINAL_WINDOW: &str = "\u{EAE8}";
pub const TRASH: &str = "\u{E4A6}";
pub const WARNING: &str = "\u{E4E0}";
pub const X: &str = "\u{E4F6}";

const FILL: &str = "phosphor-fill";

/// Add the icon font as a fallback after the text font, so icons can be
/// mixed into any label. The fill weight gets its own family.
pub fn install(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "phosphor".into(),
        egui::FontData::from_static(include_bytes!("../assets/Phosphor.ttf")).into(),
    );
    fonts.font_data.insert(
        FILL.into(),
        egui::FontData::from_static(include_bytes!("../assets/Phosphor-Fill.ttf")).into(),
    );
    fonts.families.insert(egui::FontFamily::Name(FILL.into()), vec![FILL.into()]);
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(keys) = fonts.families.get_mut(&family) {
            let at = keys.len().min(1);
            keys.insert(at, "phosphor".into());
        }
    }
}

/// Font for the filled icon variant.
pub fn fill_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FILL.into()))
}
