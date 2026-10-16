//! Custom-painted widgets: full-width list rows, icon buttons, framed fields,
//! cards and key caps. Everything clickable shows a hand cursor and eases its
//! hover state instead of snapping.

use crate::icons;
use crate::theme;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Response, Sense, Stroke, TextFormat, Ui, Vec2};
use peek_core::Protocol;
use std::sync::Arc;

const HOVER_TIME: f32 = 0.12;

pub fn protocol_color(p: Protocol) -> Color32 {
    match p {
        Protocol::Rdp => Color32::from_rgb(106, 164, 255),
        Protocol::Vnc => Color32::from_rgb(76, 195, 138),
        Protocol::Ssh => Color32::from_rgb(240, 176, 74),
    }
}

pub fn protocol_icon(p: Protocol) -> &'static str {
    match p {
        Protocol::Rdp => icons::MONITOR,
        Protocol::Vnc => icons::DESKTOP,
        Protocol::Ssh => icons::TERMINAL_WINDOW,
    }
}

/// Single-line text cut with an ellipsis to fit `max_width`.
pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping { max_width: max_width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// 0..1 hover amount for `id`, eased.
fn hover_t(ui: &Ui, id: Id, hovered: bool) -> f32 {
    ui.ctx().animate_bool_with_time(id.with("hover"), hovered, HOVER_TIME)
}

fn text_color(ui: &Ui) -> Color32 {
    ui.visuals().text_color()
}

fn weak_color(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

fn hover_fill(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_white_alpha(14) } else { Color32::from_black_alpha(12) }
}

/// Rounded square with the protocol glyph, tinted in the protocol color.
/// `status` adds a ringed dot in the corner (live sessions).
pub fn protocol_tile(ui: &Ui, rect: Rect, protocol: Protocol, status: Option<Color32>, ring: Color32) {
    let c = protocol_color(protocol);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), c.gamma_multiply(if ui.visuals().dark_mode { 0.16 } else { 0.14 }));
    p.text(rect.center(), Align2::CENTER_CENTER, protocol_icon(protocol), FontId::proportional(rect.height() * 0.55), c);
    if let Some(s) = status {
        let at = rect.right_bottom() - Vec2::splat(2.0);
        p.circle_filled(at, 5.5, ring);
        p.circle_filled(at, 3.8, s);
    }
}

/// Frameless icon button with an eased round hover background.
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    icon_button_sized(ui, icon, tooltip, 28.0, None)
}

pub fn icon_button_sized(ui: &mut Ui, icon: &str, tooltip: &str, size: f32, color: Option<Color32>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let t = hover_t(ui, resp.id, resp.hovered());
    let bg = hover_fill(ui).gamma_multiply(t * if resp.is_pointer_button_down_on() { 1.8 } else { 1.0 });
    ui.painter().rect_filled(rect, CornerRadius::same((size / 3.5) as u8), bg);
    let fg = color.unwrap_or_else(|| lerp_color(weak_color(ui), text_color(ui), t));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(size * 0.55), fg);
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) }
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Filled accent button for the main action of a surface.
pub fn primary_button(ui: &mut Ui, text: &str, size: Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = hover_t(ui, resp.id, resp.hovered());
    // The deeper accent in both themes keeps white text readable.
    let accent = theme::ACCENT_LIGHT;
    let fill = if resp.is_pointer_button_down_on() {
        lerp_color(accent, Color32::BLACK, 0.15)
    } else {
        lerp_color(accent, Color32::WHITE, 0.12 * t)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.5), Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Quiet button with a leading icon; fills in on hover.
pub fn ghost_button(ui: &mut Ui, icon: &str, text: &str) -> Response {
    let font = FontId::proportional(13.5);
    let label = ui.painter().layout_no_wrap(format!("{icon}  {text}"), font, text_color(ui));
    let size = label.size() + Vec2::new(24.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = hover_t(ui, resp.id, resp.hovered());
    let v = ui.visuals();
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        lerp_color(v.faint_bg_color.gamma_multiply(0.5), v.faint_bg_color, t),
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6 + 0.4 * t)),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(rect.center() - label.size() / 2.0, label, text_color(ui));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Text field inside a rounded frame with a leading icon and a clear button.
/// Returns the text edit's response.
pub fn search_field(ui: &mut Ui, text: &mut String, icon: &str, hint: &str, id: Id, font_size: f32, height: f32) -> Response {
    let width = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let edit_id = id.with("edit");
    let focused = ui.memory(|m| m.has_focus(edit_id));
    let t_focus = ui.ctx().animate_bool_with_time(id.with("focus"), focused, HOVER_TIME);
    let hovered = ui.rect_contains_pointer(outer);
    let t_hover = hover_t(ui, id, hovered);
    let v = ui.visuals().clone();
    let base = v.widgets.noninteractive.bg_stroke.color;
    let stroke = lerp_color(lerp_color(base, weak_color(ui), t_hover * 0.4), theme::accent(ui), t_focus);
    ui.painter().rect(outer, CornerRadius::same(8), v.extreme_bg_color, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    if t_focus > 0.0 {
        // Soft focus ring.
        ui.painter().rect_stroke(outer.expand(2.0), CornerRadius::same(10), Stroke::new(3.0, theme::accent(ui).gamma_multiply(0.18 * t_focus)), egui::StrokeKind::Outside);
    }
    let pad = 10.0;
    let icon_w = font_size + 4.0;
    ui.painter().text(
        Pos2::new(outer.left() + pad + icon_w / 2.0, outer.center().y),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(font_size + 1.0),
        lerp_color(weak_color(ui), theme::accent(ui), t_focus),
    );
    let clear_w = if text.is_empty() { 0.0 } else { height - 8.0 };
    let edit_rect = Rect::from_min_max(
        Pos2::new(outer.left() + pad + icon_w + 6.0, outer.top()),
        Pos2::new(outer.right() - pad * 0.5 - clear_w, outer.bottom()),
    );
    // Place the edit at exactly one text row, centered, so text and icon share a midline.
    let font = FontId::proportional(font_size);
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let text_rect = Rect::from_center_size(edit_rect.center(), Vec2::new(edit_rect.width(), row_h));
    let resp = ui.put(
        text_rect,
        egui::TextEdit::singleline(text)
            .id(edit_id)
            .hint_text(hint)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .font(font)
            .desired_width(text_rect.width()),
    );
    if !text.is_empty() {
        let r = Rect::from_center_size(Pos2::new(outer.right() - 4.0 - clear_w / 2.0, outer.center().y), Vec2::splat(clear_w - 4.0));
        let c = ui.interact(r, id.with("clear"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let t = hover_t(ui, c.id, c.hovered());
        ui.painter().circle_filled(r.center(), r.width() / 2.0, hover_fill(ui).gamma_multiply(t));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::X, FontId::proportional(font_size * 0.85), lerp_color(weak_color(ui), text_color(ui), t));
        if c.clicked() {
            text.clear();
            resp.request_focus();
        }
    }
    resp
}

/// Collapsible section header: small caps title, count, rotating chevron.
/// Returns the collapsing state so the caller can show the body.
pub fn section_header(ui: &mut Ui, id: Id, title: &str, count: usize) -> egui::collapsing_header::CollapsingState {
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        state.toggle(ui);
    }
    let t = hover_t(ui, id, resp.hovered());
    let color = lerp_color(weak_color(ui), text_color(ui), t * 0.7);
    let openness = state.openness(ui.ctx());
    // Chevron drawn as two strokes so it can rotate smoothly.
    let c = Pos2::new(rect.left() + 8.0, rect.center().y);
    let angle = (openness - 1.0) * std::f32::consts::FRAC_PI_2;
    let rot = egui::emath::Rot2::from_angle(angle);
    let pts = [Vec2::new(-3.5, -1.8), Vec2::new(0.0, 1.8), Vec2::new(3.5, -1.8)].map(|v| c + rot * v);
    ui.painter().line(pts.to_vec(), Stroke::new(1.4, color));
    let label = ui.painter().layout_no_wrap(title.to_uppercase(), crate::theme::semibold(11.5), color);
    let lx = rect.left() + 20.0;
    let lw = label.size().x;
    ui.painter().galley(Pos2::new(lx, rect.center().y - label.size().y / 2.0), label, color);
    let n = ui.painter().layout_no_wrap(count.to_string(), FontId::proportional(11.0), weak_color(ui));
    let pill = Rect::from_center_size(Pos2::new(lx + lw + 10.0 + n.size().x / 2.0 + 5.0, rect.center().y), n.size() + Vec2::new(10.0, 3.0));
    ui.painter().rect_filled(pill, CornerRadius::same(8), ui.visuals().faint_bg_color);
    ui.painter().galley(pill.center() - n.size() / 2.0, n, weak_color(ui));
    state
}

pub struct RowOutput {
    pub response: Response,
    pub connect: bool,
    pub toggle_favorite: bool,
    pub menu: bool,
}

pub struct RowContent<'a> {
    pub id: Id,
    pub protocol: Protocol,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub favorite: bool,
    pub status: Option<Color32>,
    pub selected: bool,
}

/// Full-width connection row: protocol tile, name and address, and on hover a
/// favorite star, a menu and a connect button. Hand cursor across the row.
pub fn connection_row(ui: &mut Ui, c: RowContent<'_>) -> RowOutput {
    let height = 48.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let hovered = ui.rect_contains_pointer(rect) && !ui.ctx().is_being_dragged(resp.id);
    let t = hover_t(ui, c.id, hovered || resp.has_focus());
    let accent = theme::accent(ui);
    let v = ui.visuals().clone();
    let p = ui.painter().clone();

    if ui.is_rect_visible(rect) {
        let mut fill = hover_fill(ui).gamma_multiply(t);
        if c.selected {
            fill = accent.gamma_multiply(if v.dark_mode { 0.16 } else { 0.12 });
        }
        if resp.is_pointer_button_down_on() {
            fill = hover_fill(ui).gamma_multiply(2.0);
        }
        p.rect_filled(rect, CornerRadius::same(8), fill);
        if c.selected {
            let bar = Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 10.0), Vec2::new(3.0, height - 20.0));
            p.rect_filled(bar, CornerRadius::same(2), accent);
        }

        let tile = Rect::from_min_size(rect.left_top() + Vec2::new(8.0, 8.0), Vec2::splat(30.0));
        let ring = if c.selected || t > 0.5 { lerp_color(v.window_fill, fill.to_opaque(), 0.0) } else { v.window_fill };
        protocol_tile(ui, tile, c.protocol, c.status, ring);

        // Hover actions take room on the right; the text yields to them.
        let actions_w = if hovered { 3.0 * 26.0 + 6.0 } else if c.favorite { 24.0 } else { 0.0 };
        let text_x = tile.right() + 10.0;
        let max_w = rect.right() - 8.0 - actions_w - text_x;
        let title_color = if c.status.is_some() || c.selected { v.strong_text_color() } else { v.text_color() };
        let g1 = elided(ui, c.title, FontId::proportional(13.5), title_color, max_w);
        let g2 = elided(ui, c.subtitle, FontId::proportional(12.0), v.weak_text_color(), max_w);
        let total = g1.size().y + 1.0 + g2.size().y;
        let y = rect.center().y - total / 2.0;
        p.galley(Pos2::new(text_x, y), g1.clone(), title_color);
        p.galley(Pos2::new(text_x, y + g1.size().y + 1.0), g2, v.weak_text_color());
    }

    let mut out = RowOutput { response: resp.clone(), connect: false, toggle_favorite: false, menu: false };
    let slot = |i: f32| Rect::from_center_size(Pos2::new(rect.right() - 8.0 - 13.0 - i * 26.0, rect.center().y), Vec2::splat(24.0));
    if hovered {
        let play = ui.interact(slot(0.0), c.id.with("go"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Connect");
        let pt = hover_t(ui, play.id, play.hovered());
        p.circle_filled(play.rect.center(), 12.0, accent.gamma_multiply(0.18 + 0.82 * pt));
        p.text(play.rect.center() + Vec2::new(1.0, 0.0), Align2::CENTER_CENTER, icons::PLAY, FontId::proportional(12.0), lerp_color(accent, Color32::WHITE, pt));
        out.connect = play.clicked();

        let menu = ui.interact(slot(1.0), c.id.with("menu"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text("More");
        let mt = hover_t(ui, menu.id, menu.hovered());
        p.rect_filled(menu.rect, CornerRadius::same(6), hover_fill(ui).gamma_multiply(mt * 1.5));
        p.text(menu.rect.center(), Align2::CENTER_CENTER, icons::DOTS_THREE, FontId::proportional(15.0), lerp_color(v.weak_text_color(), v.text_color(), mt));
        out.menu = menu.clicked();
    }
    if hovered || c.favorite {
        let star = ui.interact(slot(if hovered { 2.0 } else { 0.0 }), c.id.with("fav"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let star = star.on_hover_text(if c.favorite { "Remove from favorites" } else { "Add to favorites" });
        let st = hover_t(ui, star.id, star.hovered());
        let gold = Color32::from_rgb(245, 190, 70);
        let (glyph, font, color) = if c.favorite {
            (icons::STAR, icons::fill_font(14.0), gold)
        } else {
            (icons::STAR, FontId::proportional(14.0), lerp_color(v.weak_text_color(), gold, st))
        };
        p.text(star.rect.center(), Align2::CENTER_CENTER, glyph, font, color);
        out.toggle_favorite = star.clicked();
    }
    out.response = resp.on_hover_cursor(CursorIcon::PointingHand);
    out
}

/// Clickable card for the home screen.
pub fn connection_card(ui: &mut Ui, id: Id, size: Vec2, protocol: Protocol, title: &str, subtitle: &str, meta: &str, status: Option<Color32>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = hover_t(ui, id, resp.hovered());
    let v = ui.visuals().clone();
    let lift = 2.0 * t;
    let card = rect.translate(Vec2::new(0.0, -lift));
    let p = ui.painter();
    // Shadow grows as the card lifts.
    let shadow = egui::Shadow {
        offset: [0, (2.0 + 4.0 * t) as i8],
        blur: (6.0 + 12.0 * t) as u8,
        spread: 0,
        color: Color32::from_black_alpha(if v.dark_mode { (40.0 + 60.0 * t) as u8 } else { (14.0 + 24.0 * t) as u8 }),
    };
    p.add(shadow.as_shape(card, CornerRadius::same(10)));
    let border = lerp_color(v.widgets.noninteractive.bg_stroke.color, theme::accent(ui).gamma_multiply(0.8), t);
    p.rect(card, CornerRadius::same(10), v.window_fill, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let tile = Rect::from_min_size(card.left_top() + Vec2::new(12.0, (size.y - 34.0) / 2.0), Vec2::splat(34.0));
    protocol_tile(ui, tile, protocol, status, v.window_fill);
    let x = tile.right() + 12.0;
    let max_w = card.right() - 12.0 - x;
    let g1 = elided(ui, title, FontId::proportional(14.0), v.strong_text_color(), max_w);
    let g2 = elided(ui, subtitle, FontId::proportional(12.0), v.weak_text_color(), max_w);
    let g3 = elided(ui, meta, FontId::proportional(11.5), v.weak_text_color().gamma_multiply(0.85), max_w);
    let total = g1.size().y + g2.size().y + g3.size().y + 2.0;
    let mut y = card.center().y - total / 2.0;
    for g in [g1, g2, g3] {
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x, y), g, v.text_color());
        y += h + 1.0;
    }
    if t > 0.01 {
        let arrow = Pos2::new(card.right() - 16.0, card.top() + 16.0);
        ui.painter().text(arrow, Align2::CENTER_CENTER, icons::ARROW_RIGHT, FontId::proportional(13.0), theme::accent(ui).gamma_multiply(t));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A keyboard key cap, e.g. `Ctrl`, followed by nothing.
pub fn key_cap(ui: &mut Ui, key: &str) {
    let g = ui.painter().layout_no_wrap(key.to_owned(), FontId::proportional(11.5), weak_color(ui));
    let size = g.size() + Vec2::new(10.0, 5.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let v = ui.visuals();
    ui.painter().rect(rect, CornerRadius::same(4), v.faint_bg_color, Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
    ui.painter().line_segment(
        [rect.left_bottom() + Vec2::new(2.0, -0.5), rect.right_bottom() + Vec2::new(-2.0, -0.5)],
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color.gamma_multiply(1.6)),
    );
    ui.painter().galley(rect.center() - g.size() / 2.0, g, weak_color(ui));
}

/// `Ctrl` `K`  label
pub fn shortcut_hint(ui: &mut Ui, keys: &[&str], label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for k in keys {
            key_cap(ui, k);
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(label).size(12.0).color(weak_color(ui)));
    });
}

/// Small uppercase section label.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text.to_uppercase()).font(crate::theme::semibold(11.5)).color(weak_color(ui)).extra_letter_spacing(0.6));
}

/// Soft accent glow behind the home hero: a mesh fading from tinted to clear.
pub fn glow(ui: &Ui, center: Pos2, radius: f32) {
    let accent = theme::accent(ui);
    let inner = accent.gamma_multiply(if ui.visuals().dark_mode { 0.10 } else { 0.07 });
    let mut mesh = egui::Mesh::default();
    let n = 48;
    mesh.colored_vertex(center, inner);
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + Vec2::new(a.cos() * radius * 1.6, a.sin() * radius), Color32::TRANSPARENT);
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    ui.painter().add(mesh);
}

pub struct TabOutput {
    pub clicked: bool,
    pub close: bool,
}

/// Tab in the top bar: status dot, title, close button on hover, accent
/// underline when selected. Middle click closes.
pub fn tab_chip(ui: &mut Ui, id: Id, title: &str, dot: Color32, selected: bool) -> TabOutput {
    let v = ui.visuals().clone();
    let color = if selected { v.strong_text_color() } else { v.text_color() };
    let g = elided(ui, title, FontId::proportional(13.0), color, 170.0);
    let size = Vec2::new(12.0 + 8.0 + 8.0 + g.size().x + 6.0 + 20.0 + 4.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let t = hover_t(ui, id, hovered);
    let sel = ui.ctx().animate_bool_with_time(id.with("sel"), selected, HOVER_TIME);
    let p = ui.painter().clone();
    let fill = lerp_color(hover_fill(ui).gamma_multiply(t), v.faint_bg_color, sel);
    p.rect_filled(rect, CornerRadius::same(7), fill);
    if sel > 0.0 {
        let w = (rect.width() - 20.0) * sel;
        let bar = Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - 1.0), Vec2::new(w, 2.0));
        p.rect_filled(bar, CornerRadius::same(1), theme::accent(ui));
    }
    p.circle_filled(Pos2::new(rect.left() + 16.0, rect.center().y), 3.5, dot);
    p.galley(Pos2::new(rect.left() + 28.0, rect.center().y - g.size().y / 2.0), g, color);
    let xr = Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(18.0));
    let mut close = false;
    if hovered || selected {
        let x = ui.interact(xr, id.with("x"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Close (Ctrl+W)");
        let xt = hover_t(ui, x.id, x.hovered());
        p.rect_filled(xr, CornerRadius::same(5), hover_fill(ui).gamma_multiply(1.6 * xt));
        let c = lerp_color(v.weak_text_color(), v.text_color(), xt).gamma_multiply(0.4 + 0.6 * t.max(sel));
        p.text(xr.center(), Align2::CENTER_CENTER, icons::X, FontId::proportional(11.0), c);
        close = x.clicked();
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    TabOutput { clicked: resp.clicked() && !close, close: close || resp.middle_clicked() }
}

/// Icon-and-label navigation chip (Home).
pub fn nav_chip(ui: &mut Ui, id: Id, icon: &str, label: &str, selected: bool) -> Response {
    let v = ui.visuals().clone();
    let color = if selected { v.strong_text_color() } else { v.text_color() };
    let g = ui.painter().layout_no_wrap(format!("{icon}  {label}"), FontId::proportional(13.0), color);
    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(22.0, 30.0 - g.size().y), Sense::click());
    let t = hover_t(ui, id, resp.hovered());
    let sel = ui.ctx().animate_bool_with_time(id.with("sel"), selected, HOVER_TIME);
    ui.painter().rect_filled(rect, CornerRadius::same(7), lerp_color(hover_fill(ui).gamma_multiply(t), v.faint_bg_color, sel));
    if sel > 0.0 {
        let bar = Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - 1.0), Vec2::new((rect.width() - 20.0) * sel, 2.0));
        ui.painter().rect_filled(bar, CornerRadius::same(1), theme::accent(ui));
    }
    ui.painter().galley(rect.center() - g.size() / 2.0, g, color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

#[derive(Clone, Copy, PartialEq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Caption button in the Windows style: full title bar height, glyphs drawn
/// with hairlines, close turns red on hover.
pub fn window_button(ui: &mut Ui, kind: WindowButton, size: Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = hover_t(ui, resp.id, resp.hovered());
    let v = ui.visuals();
    let (fill, fg) = if kind == WindowButton::Close {
        let red = Color32::from_rgb(196, 43, 28);
        (red.gamma_multiply(t * if resp.is_pointer_button_down_on() { 0.8 } else { 1.0 }), lerp_color(v.text_color(), Color32::WHITE, t))
    } else {
        (hover_fill(ui).gamma_multiply(t * if resp.is_pointer_button_down_on() { 2.0 } else { 1.2 }), v.text_color())
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, fill);
    let c = rect.center();
    let stroke = Stroke::new(1.0, fg);
    let h = 5.0;
    match kind {
        WindowButton::Minimize => {
            p.line_segment([c + Vec2::new(-h, 0.0), c + Vec2::new(h, 0.0)], stroke);
        }
        WindowButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, Vec2::splat(2.0 * h)), CornerRadius::same(1), stroke, egui::StrokeKind::Inside);
        }
        WindowButton::Restore => {
            let front = Rect::from_center_size(c + Vec2::new(-1.0, 1.0), Vec2::splat(2.0 * h - 2.0));
            p.rect_stroke(front, CornerRadius::same(1), stroke, egui::StrokeKind::Inside);
            p.line(
                vec![front.left_top() + Vec2::new(2.0, -2.0), front.right_top() + Vec2::new(2.0, -2.0), front.right_bottom() + Vec2::new(2.0, -2.0)],
                stroke,
            );
        }
        WindowButton::Close => {
            p.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], stroke);
            p.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], stroke);
        }
    }
    resp
}

/// Horizontal tabs with a sliding accent underline. Returns true when the
/// selection changed.
pub fn tab_strip(ui: &mut Ui, id: Id, selected: &mut usize, tabs: &[(&str, &str)]) -> bool {
    let v = ui.visuals().clone();
    let start = ui.cursor().min;
    let mut changed = false;
    let mut spans = Vec::with_capacity(tabs.len());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, (icon, label)) in tabs.iter().enumerate() {
            let on = *selected == i;
            let color = if on { v.strong_text_color() } else { v.text_color() };
            let font = if on { crate::theme::semibold(13.5) } else { FontId::proportional(13.5) };
            let g = ui.painter().layout_no_wrap(format!("{icon}  {label}"), font, color);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 24.0, 34.0), Sense::click());
            let t = hover_t(ui, resp.id, resp.hovered() && !on);
            ui.painter().rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), CornerRadius::same(6), hover_fill(ui).gamma_multiply(t));
            let c = if on { color } else { lerp_color(v.weak_text_color(), color, 0.4 + 0.6 * t) };
            ui.painter().galley(rect.center() - g.size() / 2.0, g, c);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
                *selected = i;
                changed = true;
            }
            spans.push(rect.x_range());
        }
    });
    let bottom = ui.min_rect().bottom().max(start.y + 34.0);
    let full = Rect::from_min_max(Pos2::new(ui.max_rect().left(), bottom), Pos2::new(ui.max_rect().right(), bottom + 1.0));
    ui.painter().rect_filled(full, CornerRadius::ZERO, v.widgets.noninteractive.bg_stroke.color);
    if let Some(span) = spans.get(*selected) {
        let x = ui.ctx().animate_value_with_time(id.with("x"), span.min + 8.0, 0.16);
        let w = ui.ctx().animate_value_with_time(id.with("w"), span.span() - 16.0, 0.16);
        let bar = Rect::from_min_size(Pos2::new(x, bottom - 1.0), Vec2::new(w, 2.5));
        ui.painter().rect_filled(bar, CornerRadius::same(1), theme::accent(ui));
    }
    ui.add_space(4.0);
    changed
}

/// Segmented control: one rounded track, the chosen option raised.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let v = ui.visuals().clone();
    let font = FontId::proportional(13.0);
    let widths: Vec<f32> = options.iter().map(|(_, l)| ui.painter().layout_no_wrap((*l).to_owned(), font.clone(), v.text_color()).size().x + 28.0).collect();
    let size = Vec2::new(widths.iter().sum::<f32>() + 4.0, 30.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(8), v.extreme_bg_color);
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    for ((opt, label), w) in options.iter().zip(widths) {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 2.0), Vec2::new(w, size.y - 4.0));
        let resp = ui.interact(r, ui.scope_id().with(("seg", label)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let on = *value == *opt;
        let t = hover_t(ui, resp.id, resp.hovered());
        if on {
            ui.painter().rect_filled(r, CornerRadius::same(6), lerp_color(v.faint_bg_color, v.text_color(), 0.12));
        } else {
            ui.painter().rect_filled(r, CornerRadius::same(6), hover_fill(ui).gamma_multiply(t));
        }
        let color = if on { v.strong_text_color() } else { v.weak_text_color() };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, if on { crate::theme::semibold(13.0) } else { font.clone() }, color);
        if resp.clicked() && !on {
            *value = *opt;
            changed = true;
        }
        x += w;
    }
    changed
}

/// Quiet text-only button for secondary actions (Cancel, Save).
pub fn secondary_button(ui: &mut Ui, text: &str, size: Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = hover_t(ui, resp.id, resp.hovered());
    let v = ui.visuals();
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        lerp_color(v.faint_bg_color, lerp_color(v.faint_bg_color, v.text_color(), 0.1), t),
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(13.5), v.text_color());
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
