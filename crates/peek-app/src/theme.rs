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

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
}

pub fn install(ctx: &egui::Context, theme: peek_store::Theme) {
    let mut fonts = egui::FontDefinitions::default();
    crate::icons::install(&mut fonts);
    ctx.set_fonts(fonts);
    ctx.style_mut_of(Theme::Dark, |s| tune(s, true));
    ctx.style_mut_of(Theme::Light, |s| tune(s, false));
    apply_preference(ctx, theme);
}

pub fn apply_preference(ctx: &egui::Context, theme: peek_store::Theme) {
    ctx.set_theme(match theme {
        peek_store::Theme::System => egui::ThemePreference::System,
        peek_store::Theme::Dark => egui::ThemePreference::Dark,
        peek_store::Theme::Light => egui::ThemePreference::Light,
    });
}
