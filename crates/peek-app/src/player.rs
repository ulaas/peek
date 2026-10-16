//! Built-in player for `.peekrec` recordings.

use crate::icons;
use crate::theme;
use crate::util;
use crate::widgets;
use egui::{Color32, ColorImage, Pos2, Rect as ERect, TextureHandle, TextureOptions, Vec2};
use peek_record::Player;
use peek_record::export::{Mp4Options, Mp4Summary, export_mp4};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// An MP4 export running on its own thread.
struct ExportJob {
    output: PathBuf,
    /// Progress in thousandths.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: mpsc::Receiver<Result<Mp4Summary, String>>,
    started: Instant,
}

enum ExportState {
    Running(ExportJob),
    Finished { output: PathBuf, summary: Mp4Summary, seconds: f32 },
    Failed(String),
}

/// Recording sound: decoder plus output, opened on the first packet.
struct PlayerAudio {
    out: Option<peek_audio::AudioOut>,
    open_failed: bool,
    decoder: Option<opus2::Decoder>,
    volume: u32,
    muted: bool,
    /// The recording contains sound (shows the volume controls).
    present: bool,
}

pub struct PlayerView {
    pub title: String,
    path: PathBuf,
    player: Player,
    export: Option<ExportState>,
    audio: PlayerAudio,
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
            path,
            player,
            export: None,
            audio: PlayerAudio { out: None, open_failed: false, decoder: None, volume: 100, muted: false, present: false },
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
        self.play_audio();

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
        self.export_card(ui, area);

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
                    if !self.playing {
                        self.silence();
                    }
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
                if self.audio.present {
                    let a = &mut self.audio;
                    let icon = if a.muted || a.volume == 0 {
                        icons::SPEAKER_X
                    } else if a.volume < 50 {
                        icons::SPEAKER_LOW
                    } else {
                        icons::SPEAKER_HIGH
                    };
                    let tip = if (self.speed - 1.0).abs() > f32::EPSILON { "Sound plays at 1x speed only" } else if a.muted { "Unmute" } else { "Mute" };
                    let mut cut = false;
                    if ui.button(icon).on_hover_text(tip).clicked() {
                        a.muted = !a.muted;
                        cut = a.muted;
                    }
                    let mut v = if a.muted { 0 } else { a.volume };
                    ui.spacing_mut().slider_width = 70.0;
                    if ui.add(egui::Slider::new(&mut v, 0..=100).show_value(false)).on_hover_text(format!("Volume {v}%")).changed() {
                        a.volume = v;
                        a.muted = v == 0;
                        cut = a.muted;
                    }
                    if cut {
                        self.silence();
                    }
                }
                let busy = matches!(self.export, Some(ExportState::Running(_)));
                let export = ui
                    .add_enabled(!busy, egui::Button::new(format!("{}  Export MP4", icons::EXPORT)))
                    .on_hover_text("Save this recording as an H.264 MP4 video");
                if export.clicked() {
                    self.export_dialog(&ctx);
                }
                let mut pos = self.clock_ms;
                ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(80.0);
                let slider = ui.add(egui::Slider::new(&mut pos, 0.0..=dur).show_value(false));
                if slider.changed() {
                    self.seek(pos);
                }
            });
        });
    }

    /// Ask where to save, then export on a background thread.
    pub fn export_dialog(&mut self, ctx: &egui::Context) {
        if matches!(self.export, Some(ExportState::Running(_))) {
            return;
        }
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "recording".into());
        let mut dialog = rfd::FileDialog::new().add_filter("MP4 video", &["mp4"]).set_file_name(format!("{stem}.mp4"));
        if let Some(dir) = self.path.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(output) = dialog.save_file() else { return };
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, done) = mpsc::channel();
        let (input, out, p, c, repaint) = (self.path.clone(), output.clone(), progress.clone(), cancel.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("peek-export".into()).spawn(move || {
            let result = export_mp4(&input, &out, Mp4Options::default(), |f| {
                let permille = (f * 1000.0) as u32;
                if p.swap(permille, Ordering::Relaxed) / 10 != permille / 10 {
                    repaint.request_repaint();
                }
                !c.load(Ordering::Relaxed)
            });
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            repaint.request_repaint();
        });
        self.export = Some(match spawned {
            Ok(_) => ExportState::Running(ExportJob { output, progress, cancel, done, started: Instant::now() }),
            Err(e) => ExportState::Failed(e.to_string()),
        });
    }

    /// Progress or result card over the bottom right of the picture.
    fn export_card(&mut self, ui: &mut egui::Ui, area: ERect) {
        if let Some(ExportState::Running(job)) = &self.export {
            if let Ok(result) = job.done.try_recv() {
                let seconds = job.started.elapsed().as_secs_f32();
                let output = job.output.clone();
                self.export = match result {
                    Ok(summary) => Some(ExportState::Finished { output, summary, seconds }),
                    Err(e) if e.contains("cancelled") => None,
                    Err(e) => Some(ExportState::Failed(e)),
                };
            }
        }
        let Some(state) = &self.export else { return };
        let mut close = false;
        let width = 330.0;
        let pos = area.right_bottom() + Vec2::new(-width - 16.0, -16.0);
        egui::Area::new(egui::Id::unique(("export-card", self.path.clone())))
            .fixed_pos(pos)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).corner_radius(10).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                    ui.set_width(width - 28.0);
                    match state {
                        ExportState::Running(job) => {
                            let f = job.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{}  Exporting MP4", icons::EXPORT)).font(theme::semibold(13.5)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(egui::RichText::new(format!("{:.0}%", f * 100.0)).monospace().weak());
                                });
                            });
                            ui.add_space(4.0);
                            ui.add(egui::ProgressBar::new(f).desired_height(6.0).fill(theme::accent(ui)));
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let name = job.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                ui.add(egui::Label::new(egui::RichText::new(name).small().weak()).truncate());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if widgets::secondary_button(ui, "Cancel", Vec2::new(72.0, 26.0)).clicked() {
                                        job.cancel.store(true, Ordering::Relaxed);
                                    }
                                });
                            });
                        }
                        ExportState::Finished { output, summary, seconds } => {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(icons::CHECK_CIRCLE).size(16.0).color(theme::OK));
                                ui.label(egui::RichText::new("MP4 saved").font(theme::semibold(13.5)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    close = widgets::icon_button_sized(ui, icons::X, "Dismiss", 22.0, None).clicked();
                                });
                            });
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} × {}{} · {} · {} · exported in {:.1} s",
                                    summary.width,
                                    summary.height,
                                    if summary.audio { " with sound" } else { "" },
                                    util::clock(Duration::from_millis(summary.duration_ms as u64)),
                                    util::human_bytes(summary.bytes),
                                    seconds
                                ))
                                .small()
                                .weak(),
                            );
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if widgets::primary_button(ui, "Open", Vec2::new(80.0, 28.0)).clicked() {
                                    util::open_file(output);
                                }
                                if widgets::secondary_button(ui, "Show in folder", Vec2::new(120.0, 28.0)).clicked() {
                                    util::reveal_file(output);
                                }
                            });
                        }
                        ExportState::Failed(e) => {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{}  Export failed", icons::WARNING)).font(theme::semibold(13.5)).color(theme::DANGER));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    close = widgets::icon_button_sized(ui, icons::X, "Dismiss", 22.0, None).clicked();
                                });
                            });
                            ui.label(egui::RichText::new(e).small());
                        }
                    }
                });
            });
        if close {
            self.export = None;
        }
    }

    /// Play the audio packets playback just passed; sound only at 1x.
    fn play_audio(&mut self) {
        let packets = std::mem::take(&mut self.player.audio);
        if packets.is_empty() {
            return;
        }
        let a = &mut self.audio;
        a.present = true;
        let audible = self.playing && (self.speed - 1.0).abs() < f32::EPSILON && !a.muted && a.volume > 0;
        if !audible {
            return;
        }
        if a.out.is_none() && !a.open_failed {
            match peek_audio::AudioOut::open() {
                Ok(o) => a.out = Some(o),
                Err(e) => {
                    tracing::warn!("no audio output: {e:#}");
                    a.open_failed = true;
                }
            }
        }
        let Some(out) = &a.out else { return };
        if a.decoder.is_none() {
            a.decoder = opus2::Decoder::new(peek_record::AUDIO_RATE, opus2::Channels::Stereo).ok();
        }
        let Some(dec) = a.decoder.as_mut() else { return };
        out.set_volume(a.volume as f32 / 100.0);
        let mut pcm = vec![0i16; 5760 * 2];
        for (_, packet) in packets {
            if let Ok(n) = dec.decode(&packet, &mut pcm, false) {
                out.push(&pcm[..n * 2], peek_record::AUDIO_RATE, peek_record::AUDIO_CHANNELS);
            }
        }
    }

    /// Cut queued sound (pause, seek, mute).
    fn silence(&mut self) {
        if let Some(out) = &self.audio.out {
            out.clear();
        }
        self.audio.decoder = None;
    }

    fn seek(&mut self, ms: f64) {
        self.silence();
        self.clock_ms = ms;
        if let Err(e) = self.player.seek(ms as u32) {
            self.error = Some(format!("{e:#}"));
        }
        self.needs_full = true;
    }
}
