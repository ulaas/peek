//! Built-in player for `.peekrec` recordings.

use crate::icons;
use crate::theme;
use crate::util;
use egui::{Color32, ColorImage, Pos2, Rect as ERect, TextureHandle, TextureOptions, Vec2};
use peek_record::Player;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct PlayerView {
    pub title: String,
    player: Player,
    texture: Option<TextureHandle>,
    needs_full: bool,
    playing: bool,
    speed: f32,
    clock_ms: f64,
    last_tick: Option<Instant>,
    error: Option<String>,
}

impl PlayerView {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let player = Player::open(&path)?;
        let title = player
            .meta("title")
            .map(str::to_string)
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        Ok(Self {
            title: format!("{} {title}", icons::FILM_STRIP),
            player,
            texture: None,
            needs_full: true,
            playing: true,
            speed: 1.0,
            clock_ms: 0.0,
            last_tick: None,
            error: None,
        })
    }

    fn upload(&mut self, ctx: &egui::Context, damage: &[peek_core::Rect]) {
        let fb = &self.player.frame;
        let size = [fb.width() as usize, fb.height() as usize];
        if self.needs_full || self.texture.as_ref().is_none_or(|t| t.size() != size) {
            let px: &[Color32] = bytemuck::cast_slice(fb.pixels());
            self.texture = Some(ctx.load_texture("player", ColorImage::new(size, px.to_vec()), TextureOptions::LINEAR));
            self.needs_full = false;
            return;
        }
        let tex = self.texture.as_mut().expect("texture");
        for r in damage.iter().map(|r| r.clamp_to(fb.width(), fb.height())).filter(|r| !r.is_empty()) {
            let mut px: Vec<Color32> = Vec::with_capacity(r.area() as usize);
            for y in r.y..r.bottom() {
                px.extend_from_slice(bytemuck::cast_slice(fb.row(y, r.x, r.w)));
            }
            tex.set_partial([r.x as usize, r.y as usize], ColorImage::new([r.w as usize, r.h as usize], px), TextureOptions::LINEAR);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let full = ui.available_rect_before_wrap();
        let bar_h = 44.0;
        let area = ERect::from_min_max(full.min, Pos2::new(full.max.x, full.max.y - bar_h));
        ui.painter().rect_filled(area, 0.0, theme::canvas(ui.visuals().dark_mode));

        // Advance the playback clock.
        let now = Instant::now();
        if self.playing {
            if let Some(t) = self.last_tick {
                self.clock_ms += now.duration_since(t).as_secs_f64() * 1000.0 * self.speed as f64;
            }
            if self.clock_ms >= self.player.duration_ms as f64 {
                self.clock_ms = self.player.duration_ms as f64;
                self.playing = false;
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        self.last_tick = Some(now);
        match self.player.advance_to(self.clock_ms as u32) {
            Ok(damage) => self.upload(&ctx, &damage),
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                self.playing = false;
            }
        }

        // Picture, fitted.
        if let Some(tex) = &self.texture {
            let s = Vec2::new(self.player.frame.width() as f32, self.player.frame.height() as f32);
            let k = (area.width() / s.x).min(area.height() / s.y);
            let r = ERect::from_center_size(area.center(), s * k);
            ui.painter().image(tex.id(), r, ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            if let Some((x, y)) = self.player.pointer {
                let p = r.min + Vec2::new(x as f32, y as f32) * k;
                ui.painter().circle(p, 5.0, theme::ACCENT_DARK.gamma_multiply(0.7), egui::Stroke::new(1.5, Color32::WHITE));
            }
        }
        if let Some(e) = &self.error {
            ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, e, egui::FontId::proportional(14.0), theme::DANGER);
        }

        // Transport bar.
        let bar = ERect::from_min_max(Pos2::new(full.min.x, area.max.y), full.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(12.0, 8.0))), |ui| {
            ui.horizontal_centered(|ui| {
                let label = if self.playing { icons::PAUSE } else { icons::PLAY };
                if ui.button(label).clicked() || ui.input(|i| i.key_pressed(egui::Key::Space)) {
                    if !self.playing && self.clock_ms >= self.player.duration_ms as f64 {
                        self.seek(0.0);
                    }
                    self.playing = !self.playing;
                }
                let dur = self.player.duration_ms.max(1) as f64;
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {}",
                        util::clock(Duration::from_millis(self.clock_ms as u64)),
                        util::clock(Duration::from_millis(dur as u64))
                    ))
                    .monospace(),
                );
                egui::ComboBox::from_id_salt("speed").selected_text(format!("{}×", self.speed)).width(60.0).show_ui(ui, |ui| {
                    for s in [0.5f32, 1.0, 2.0, 4.0, 8.0] {
                        ui.selectable_value(&mut self.speed, s, format!("{s}×"));
                    }
                });
                let mut pos = self.clock_ms;
                ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(80.0);
                let slider = ui.add(egui::Slider::new(&mut pos, 0.0..=dur).show_value(false));
                if slider.changed() {
                    self.seek(pos);
                }
            });
        });
    }

    fn seek(&mut self, ms: f64) {
        self.clock_ms = ms;
        if let Err(e) = self.player.seek(ms as u32) {
            self.error = Some(format!("{e:#}"));
        }
        self.needs_full = true;
    }
}
