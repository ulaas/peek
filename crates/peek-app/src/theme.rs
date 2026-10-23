//! Visual style: quiet neutrals with a single blue accent, tuned so the
//! remote desktop stays the brightest thing on screen.

use egui::{Color32, CornerRadius, Stroke, Style, Theme, Visuals};

pub const ACCENT_DARK: Color32 = Color32::from_rgb(106, 164, 255);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(42, 111, 219);
pub const DANGER: Color32 = Color32::from_rgb(232, 84, 84);
pub const OK: Color32 = Color32::from_rgb(76, 195, 138);
pub const WARN: Color32 = Color32::from_rgb(240, 176, 74);

/// Background behind the remote picture (letterboxing).
pub fn canvas(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(10, 12, 15) } else { Color32::from_rgb(38, 42, 48) }
}

pub fn accent(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode { ACCENT_DARK } else { ACCENT_LIGHT }
}

fn tune(style: &mut Style, dark: bool) {
    let v: &mut Visuals = &mut style.visuals;
    let (bg, panel, faint, text, line, accent) = if dark {
        (
            Color32::from_rgb(17, 20, 24),
            Color32::from_rgb(22, 26, 32),
            Color32::from_rgb(30, 35, 43),
            Color32::from_rgb(220, 226, 233),
            Color32::from_rgb(38, 45, 55),
            ACCENT_DARK,
        )
    } else {
        (
            Color32::from_rgb(243, 245, 247),
            Color32::from_rgb(250, 251, 252),
            Color32::from_rgb(232, 236, 241),
            Color32::from_rgb(23, 32, 43),
            Color32::from_rgb(217, 223, 230),
            ACCENT_LIGHT,
        )
    };
    v.window_fill = panel;
    v.panel_fill = bg;
    v.faint_bg_color = faint;
    v.extreme_bg_color = if dark { Color32::from_rgb(12, 14, 18) } else { Color32::WHITE };
    v.override_text_color = Some(text);
    v.window_stroke = Stroke::new(1.0, line);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.selection.bg_fill = accent.gamma_multiply(if dark { 0.35 } else { 0.25 });
    v.selection.stroke = Stroke::new(1.0, accent);
    v.hyperlink_color = accent;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, line);
    v.widgets.inactive.weak_bg_fill = faint;
    v.widgets.inactive.bg_fill = faint;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, line);

    v.widgets.hovered.weak_bg_fill = lerp(faint, text, 0.06);
    v.widgets.hovered.bg_fill = lerp(faint, text, 0.06);
    v.widgets.active.weak_bg_fill = lerp(faint, text, 0.12);
    v.widgets.active.bg_fill = lerp(faint, text, 0.12);
    for w in [&mut v.widgets.hovered, &mut v.widgets.active] {
        w.expansion = 0.0;
    }
    // Hand over anything clickable; labels are not selectable text (no I-beam
    // over list rows and captions).
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 38 }),
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
    };
    v.striped = false;
    // Grid-fitted glyphs; no sub-pixel binning, which trades sharpness for spacing.
    v.text_options.font_hinting = true;
    v.text_options.subpixel_binning = false;
    // Unchecked checkboxes and radio buttons need a visible box.
    v.widgets.inactive.bg_fill = lerp(faint, text, 0.10);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, lerp(faint, text, 0.18));
    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.35;
    style.animation_time = 0.14;

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.spacing.scroll.floating_width = 6.0;
    style.spacing.combo_height = 320.0;

    use egui::{FontFamily::Proportional, FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Heading, FontId::new(20.0, Proportional)),
        (TextStyle::Body, FontId::new(13.5, Proportional)),
        (TextStyle::Button, FontId::new(13.5, Proportional)),
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, egui::FontFamily::Monospace)),
    ]
    .into();
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    crate::widgets::lerp_color(a, b, t)
}

pub fn install(ctx: &egui::Context, theme: peek_store::Theme) {
    let mut fonts = egui::FontDefinitions::default();
    crate::icons::install(&mut fonts);
    install_system_fonts(&mut fonts);
    ctx.set_fonts(fonts);
    ctx.style_mut_of(Theme::Dark, |s| tune(s, true));
    ctx.style_mut_of(Theme::Light, |s| tune(s, false));
    apply_preference(ctx, theme);
}

const SEMIBOLD: &str = "ui-semibold";

/// Heavier face for headings (Segoe UI Semibold on Windows; regular elsewhere).
pub fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(SEMIBOLD.into()))
}

/// The platform UI fonts, read at startup rather than bundled: Segoe UI and
/// Cascadia Mono / Consolas on Windows. They carry hinting made for screens,
/// unlike egui's built-in Ubuntu Light. Missing files keep egui's fonts.
fn install_system_fonts(fonts: &mut egui::FontDefinitions) {
    let dir = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"));
    let load =
        |names: &[&str]| -> Option<Vec<u8>> { names.iter().find_map(|n| std::fs::read(dir.as_ref()?.join(n)).ok()) };
    let mut put = |key: &str, bytes: Vec<u8>, family: egui::FontFamily| {
        fonts.font_data.insert(key.into(), egui::FontData::from_owned(bytes).into());
        fonts.families.entry(family).or_default().insert(0, key.into());
    };
    let regular = load(&["segoeui.ttf"]);
    if let Some(b) = regular.clone() {
        put("ui", b, egui::FontFamily::Proportional);
    }
    if let Some(b) = load(&["CascadiaMono.ttf", "consola.ttf"]) {
        put("ui-mono", b, egui::FontFamily::Monospace);
    }
    // Semibold family: the heavy face first, then everything Proportional has
    // (icons, symbols) as fallbacks.
    let mut chain: Vec<String> = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(b) = load(&["seguisb.ttf"]) {
        fonts.font_data.insert("ui-sb".into(), egui::FontData::from_owned(b).into());
        chain.insert(0, "ui-sb".into());
    }
    fonts.families.insert(egui::FontFamily::Name(SEMIBOLD.into()), chain);
}

pub fn apply_preference(ctx: &egui::Context, theme: peek_store::Theme) {
    ctx.set_theme(match theme {
        peek_store::Theme::System => egui::ThemePreference::System,
        peek_store::Theme::Dark => egui::ThemePreference::Dark,
        peek_store::Theme::Light => egui::ThemePreference::Light,
    });
}
