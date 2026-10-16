//! One remote session inside a tab: streams damaged regions to a GPU
//! texture, translates local input, draws the remote cursor locally and
//! hosts the floating session toolbar.

use crate::grab::{GrabMode, GrabbedKey};
use crate::icons;
use crate::keymap;
use crate::theme;
use crate::util;
use egui::{Align2, Color32, ColorImage, CursorIcon, Event, Pos2, Rect as ERect, Sense, TextureHandle, TextureOptions, Vec2};
use peek_core::{
    AuthError, ConnectRequest, Credentials, CursorUpdate, Endpoint, InputEvent, KeyCode, MouseButton, Protocol, Rect,
    SessionCommand, SessionEvent, SessionHandle, SessionOptions, new_session,
};
use peek_record::Recorder;
use peek_store::{ProfileOptions, Resolution, ScaleMode};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub enum Phase {
    /// Waiting for the user to enter credentials before (re)connecting.
    NeedCredentials { message: Option<String> },
    Connecting(String),
    Connected,
    Disconnected { reason: Option<String>, error: bool },
}

/// Things the view asks the app to do.
pub enum ViewAction {
    ToggleFullscreen,
    SubmitCredentials { user: String, password: String, remember: bool },
    Close,
    Toast(String, ToastKind),
    StartRecording,
    StopRecording,
    ServerIdentity(String),
    Connected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
}

#[derive(Default)]
struct Rates {
    at: Option<Instant>,
    prev: (u64, u64, u64),
    down: f64,
    up: f64,
    ups: f64,
}

struct RemoteCursor {
    tex: TextureHandle,
    size: Vec2,
    hot: Vec2,
}

pub struct SessionView {
    pub id: u64,
    pub profile_id: Option<Uuid>,
    pub endpoint: Endpoint,
    pub title: String,
    pub options: ProfileOptions,
    pub phase: Phase,
    pub detail: String,
    pub fingerprint: Option<String>,
    pub vault_key: String,
    handle: Option<SessionHandle>,
    texture: Option<TextureHandle>,
    remote: (u32, u32),
    cursor_mode: CursorMode,
    cursor: Option<RemoteCursor>,
    focused: bool,
    sent_mods: egui::Modifiers,
    grab_ctrl: bool,
    grab_alt: bool,
    wheel: Vec2,
    pointer: Option<(u32, u32)>,
    resize_pending: Option<((u32, u32), Instant)>,
    resize_sent: (u32, u32),
    rates: Rates,
    pub show_stats: bool,
    pub recorder: Option<Recorder>,
    toolbar_pinned: bool,
    last_clipboard: Option<String>,
    // credential form
    form_user: String,
    form_password: String,
    form_remember: bool,
    /// Credentials of the current attempt, kept in memory for auto-reconnect.
    last_creds: Option<Credentials>,
    ever_connected: bool,
    /// Next automatic reconnect: when, and which attempt it is.
    reconnect: Option<(Instant, u32)>,
    attempts: u32,
}

const RECONNECT_DELAYS: [u64; 5] = [1, 2, 4, 8, 16];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorMode {
    Local,
    Remote,
    Hidden,
}

impl SessionView {
    pub fn new(id: u64, endpoint: Endpoint, title: String, options: ProfileOptions, vault_key: String) -> Self {
        Self {
            id,
            profile_id: None,
            form_user: endpoint.username.clone().unwrap_or_default(),
            endpoint,
            title,
            options,
            phase: Phase::NeedCredentials { message: None },
            detail: String::new(),
            fingerprint: None,
            vault_key,
            handle: None,
            texture: None,
            remote: (0, 0),
            cursor_mode: CursorMode::Local,
            cursor: None,
            focused: false,
            sent_mods: egui::Modifiers::NONE,
            grab_ctrl: false,
            grab_alt: false,
            wheel: Vec2::ZERO,
            pointer: None,
            resize_pending: None,
            resize_sent: (0, 0),
            rates: Rates::default(),
            show_stats: false,
            recorder: None,
            toolbar_pinned: false,
            last_clipboard: None,
            form_password: String::new(),
            form_remember: true,
            last_creds: None,
            ever_connected: false,
            reconnect: None,
            attempts: 0,
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self.phase, Phase::Connecting(_) | Phase::Connected)
    }

    pub fn has_keyboard_focus(&self) -> bool {
        self.focused
    }

    /// Start (or restart) the connection.
    pub fn connect(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        credentials: Credentials,
        desired: (u32, u32),
        keyboard_layout: u32,
    ) {
        self.disconnect();
        self.last_creds = Some(credentials.clone());
        let (w, h) = match self.options.resolution {
            Resolution::Window => (desired.0.max(640), desired.1.max(480)),
            Resolution::Fixed { width, height } => (width, height),
        };
        let (handle, sctx) = new_session(w, h);
        let repaint = ctx.clone();
        handle.frame.set_waker(move || repaint.request_repaint());
        let req = ConnectRequest {
            endpoint: self.endpoint.clone(),
            credentials,
            options: SessionOptions {
                width: w,
                height: h,
                scale_percent: ((ctx.pixels_per_point() * 100.0).round() as u32).clamp(100, 500),
                view_only: self.options.view_only,
                clipboard: self.options.clipboard,
                audio: self.options.audio,
                keyboard_layout,
            },
        };
        let events = sctx.events.clone();
        let finish = move |result: anyhow::Result<Option<String>>| match result {
            Ok(reason) => events.send(SessionEvent::Disconnected { reason, error: false }),
            Err(e) => match e.downcast_ref::<AuthError>() {
                Some(a) => events.send(SessionEvent::AuthRequired { message: a.to_string() }),
                None => events.send(SessionEvent::Disconnected { reason: Some(format!("{e:#}")), error: true }),
            },
        };
        match req.endpoint.protocol {
            // IronRDP futures are not Send, and RemoteFX decoding is heavy:
            // give every RDP session its own thread and runtime.
            Protocol::Rdp => {
                let spawned = std::thread::Builder::new().name(format!("peek-rdp-{}", self.id)).spawn(move || {
                    let result = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(anyhow::Error::from)
                        .and_then(|rt| rt.block_on(peek_rdp::run(req, sctx)));
                    finish(result);
                });
                if let Err(e) = spawned {
                    tracing::error!("cannot start RDP thread: {e}");
                }
            }
            Protocol::Vnc => {
                rt.spawn(async move { finish(peek_vnc::run(req, sctx).await) });
            }
            Protocol::Ssh => finish(Err(anyhow::anyhow!("SSH sessions are not available yet"))),
        }
        self.texture = None;
        self.cursor = None;
        self.cursor_mode = CursorMode::Local;
        self.rates = Rates::default();
        self.resize_sent = (w, h);
        self.phase = Phase::Connecting("Starting".into());
        self.handle = Some(handle);
    }

    pub fn disconnect(&mut self) {
        self.reconnect = None;
        if let Some(h) = self.handle.take() {
            h.send(SessionCommand::Disconnect);
        }
        if let Some(r) = self.recorder.take() {
            let _ = r.stop();
        }
    }

    pub fn send(&self, cmd: SessionCommand) {
        if let Some(h) = &self.handle {
            h.send(cmd);
        }
    }

    /// Drain backend events. Call once per frame for every tab.
    pub fn poll(&mut self, ctx: &egui::Context, actions: &mut Vec<ViewAction>) {
        let Some(h) = &self.handle else { return };
        let events: Vec<SessionEvent> = h.poll_events().collect();
        for ev in events {
            match ev {
                SessionEvent::Status(s) => self.phase = Phase::Connecting(s),
                SessionEvent::Connected { width, height, detail } => {
                    self.remote = (width, height);
                    self.detail = detail;
                    tracing::info!(session = self.id, "connected");
                    self.phase = Phase::Connected;
                    self.ever_connected = true;
                    self.attempts = 0;
                    actions.push(ViewAction::Connected);
                    if self.options.auto_record {
                        actions.push(ViewAction::StartRecording);
                    }
                }
                SessionEvent::Resized { width, height } => {
                    self.remote = (width, height);
                    if let Some(r) = &self.recorder {
                        r.resize(width, height);
                    }
                }
                SessionEvent::Cursor(c) => self.set_cursor(ctx, c),
                SessionEvent::ClipboardText(t) => {
                    if self.options.clipboard && self.last_clipboard.as_deref() != Some(t.as_str()) {
                        ctx.copy_text(t.clone());
                        self.last_clipboard = Some(t);
                    }
                }
                SessionEvent::Bell => {}
                SessionEvent::ServerIdentity { fingerprint } => {
                    self.fingerprint = Some(fingerprint.clone());
                    actions.push(ViewAction::ServerIdentity(fingerprint));
                }
                SessionEvent::AuthRequired { message } => {
                    self.handle = None;
                    self.phase = Phase::NeedCredentials { message: Some(message) };
                }
                SessionEvent::Disconnected { reason, error } => {
                    tracing::info!(session = self.id, ?reason, error, "disconnected");
                    self.handle = None;
                    if let Some(r) = self.recorder.take() {
                        if let Ok(info) = r.stop() {
                            actions.push(ViewAction::Toast(
                                format!("Recording saved ({})", util::human_bytes(info.bytes)),
                                ToastKind::Success,
                            ));
                        }
                    }
                    // Dropped connections come back on their own; logoffs do not.
                    if error && self.ever_connected && (self.attempts as usize) < RECONNECT_DELAYS.len() {
                        let delay = Duration::from_secs(RECONNECT_DELAYS[self.attempts as usize]);
                        self.attempts += 1;
                        self.reconnect = Some((Instant::now() + delay, self.attempts));
                        ctx.request_repaint_after(delay);
                    }
                    self.phase = Phase::Disconnected { reason, error };
                }
            }
        }
    }

    fn set_cursor(&mut self, ctx: &egui::Context, c: CursorUpdate) {
        match c {
            CursorUpdate::Default => self.cursor_mode = CursorMode::Local,
            CursorUpdate::Hidden => self.cursor_mode = CursorMode::Hidden,
            CursorUpdate::Image(img) => {
                let size = [img.width as usize, img.height as usize];
                if img.rgba.len() != size[0] * size[1] * 4 || size[0] == 0 {
                    return;
                }
                let image = ColorImage::from_rgba_unmultiplied(size, &img.rgba);
                let tex = ctx.load_texture(format!("cursor-{}", self.id), image, TextureOptions::LINEAR);
                self.cursor = Some(RemoteCursor {
                    tex,
                    size: Vec2::new(img.width as f32, img.height as f32),
                    hot: Vec2::new(img.hot_x as f32, img.hot_y as f32),
                });
                self.cursor_mode = CursorMode::Remote;
            }
        }
    }

    /// Upload damaged regions to the texture (and the recorder).
    fn upload(&mut self, ctx: &egui::Context) {
        let Some(h) = &self.handle else { return };
        let (fw, fh) = h.frame.size();
        let size_ok = self.texture.as_ref().is_some_and(|t| t.size() == [fw as usize, fh as usize]);
        if !size_ok {
            h.frame.invalidate();
        }
        let texture = &mut self.texture;
        let recorder = &self.recorder;
        let id = self.id;
        h.frame.read_dirty(|fb, rects| {
            let full = texture.as_ref().is_none_or(|t| t.size() != [fb.width() as usize, fb.height() as usize]);
            if full {
                let px: &[Color32] = bytemuck::cast_slice(fb.pixels());
                let img = ColorImage::new([fb.width() as usize, fb.height() as usize], px.to_vec());
                *texture = Some(ctx.load_texture(format!("remote-{id}"), img, TextureOptions::LINEAR));
                if let Some(r) = recorder {
                    r.resize(fb.width(), fb.height());
                    r.region(Rect::new(0, 0, fb.width(), fb.height()), fb.as_bytes().to_vec());
                }
                return;
            }
            let tex = texture.as_mut().expect("texture exists");
            for r in rects {
                let mut px: Vec<Color32> = Vec::with_capacity(r.area() as usize);
                for y in r.y..r.bottom() {
                    px.extend_from_slice(bytemuck::cast_slice(fb.row(y, r.x, r.w)));
                }
                if let Some(rec) = recorder {
                    rec.region(*r, bytemuck::cast_slice(&px).to_vec());
                }
                tex.set_partial([r.x as usize, r.y as usize], ColorImage::new([r.w as usize, r.h as usize], px), TextureOptions::LINEAR);
            }
        });
    }

    fn update_rates(&mut self) {
        let Some(h) = &self.handle else { return };
        let now = Instant::now();
        let cur = (
            h.stats.bytes_in.load(std::sync::atomic::Ordering::Relaxed),
            h.stats.bytes_out.load(std::sync::atomic::Ordering::Relaxed),
            h.stats.updates.load(std::sync::atomic::Ordering::Relaxed),
        );
        match self.rates.at {
            Some(t) if now.duration_since(t) >= Duration::from_millis(500) => {
                let dt = now.duration_since(t).as_secs_f64();
                let blend = |old: f64, new: f64| old * 0.4 + new * 0.6;
                self.rates.down = blend(self.rates.down, cur.0.saturating_sub(self.rates.prev.0) as f64 / dt);
                self.rates.up = blend(self.rates.up, cur.1.saturating_sub(self.rates.prev.1) as f64 / dt);
                self.rates.ups = blend(self.rates.ups, cur.2.saturating_sub(self.rates.prev.2) as f64 / dt);
                self.rates.at = Some(now);
                self.rates.prev = cur;
            }
            Some(_) => {}
            None => {
                self.rates.at = Some(now);
                self.rates.prev = cur;
            }
        }
    }

    /// Where the remote picture goes inside `area`, in points, aligned to
    /// physical pixels so 1:1 output stays sharp.
    fn image_rect(&self, ctx: &egui::Context, area: ERect, pointer: Option<Pos2>) -> ERect {
        let ppp = ctx.pixels_per_point();
        let native = Vec2::new(self.remote.0 as f32, self.remote.1 as f32) / ppp;
        let size = match self.options.scale {
            ScaleMode::Stretch => area.size(),
            ScaleMode::Native => native,
            ScaleMode::Fit => native * (area.width() / native.x).min(area.height() / native.y),
        };
        let mut min = area.center() - size / 2.0;
        // Larger than the window in 1:1 mode: pan with the pointer.
        for axis in 0..2 {
            if size[axis] > area.size()[axis] {
                let t = pointer.map_or(0.5, |p| ((p[axis] - area.min[axis]) / area.size()[axis]).clamp(0.0, 1.0));
                min[axis] = area.min[axis] - (size[axis] - area.size()[axis]) * t;
            }
        }
        let min = Pos2::new((min.x * ppp).round() / ppp, (min.y * ppp).round() / ppp);
        ERect::from_min_size(min, size)
    }

    fn to_remote(&self, img: ERect, p: Pos2, clamp: bool) -> Option<(u32, u32)> {
        if !clamp && !img.contains(p) {
            return None;
        }
        let rel = (p - img.min) / img.size();
        let x = (rel.x.clamp(0.0, 0.9999) * self.remote.0 as f32) as u32;
        let y = (rel.y.clamp(0.0, 0.9999) * self.remote.1 as f32) as u32;
        Some((x, y))
    }

    fn input(&self, ev: InputEvent) {
        self.send(SessionCommand::Input(ev));
    }

    fn sync_modifiers(&mut self, m: egui::Modifiers) {
        let pairs = [
            (self.sent_mods.shift, m.shift, KeyCode::ShiftLeft),
            (self.sent_mods.ctrl, m.ctrl, KeyCode::ControlLeft),
            (self.sent_mods.alt, m.alt, KeyCode::AltLeft),
            (self.sent_mods.mac_cmd, m.mac_cmd, KeyCode::MetaLeft),
        ];
        for (was, is, code) in pairs {
            if was != is {
                self.input(InputEvent::Key { code, down: is });
            }
        }
        self.sent_mods = m;
    }

    fn tap(&self, code: KeyCode) {
        self.input(InputEvent::Key { code, down: true });
        self.input(InputEvent::Key { code, down: false });
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        img: ERect,
        hovered: bool,
        grabbed: &[GrabbedKey],
        grab: GrabMode,
        actions: &mut Vec<ViewAction>,
    ) {
        // Keys captured by the OS-level grab. In full-grab mode these are the
        // only keyboard input; egui sees nothing.
        if self.focused {
            for &(code, down) in grabbed {
                match code {
                    KeyCode::ControlLeft | KeyCode::ControlRight => self.grab_ctrl = down,
                    KeyCode::AltLeft | KeyCode::AltRight => self.grab_alt = down,
                    _ => {}
                }
                if down && code == KeyCode::Enter && self.grab_ctrl && self.grab_alt {
                    actions.push(ViewAction::ToggleFullscreen);
                    continue;
                }
                if down && code == KeyCode::KeyV && self.grab_ctrl && !self.grab_alt {
                    // Offer the local clipboard first so the paste gets current text.
                    if let Some(t) = util::read_clipboard_text() {
                        self.offer_clipboard(t);
                    }
                }
                self.input(InputEvent::Key { code, down });
            }
        }
        let keyboard_via_egui = self.focused && grab != GrabMode::All;
        let events = ui.input(|i| i.events.clone());
        let (wheel_threshold, rdp) = match self.endpoint.protocol {
            Protocol::Rdp => (15.0, true),
            _ => (120.0, false),
        };
        for ev in events {
            match ev {
                Event::PointerMoved(p) if hovered => {
                    if let Some(pos) = self.to_remote(img, p, false) {
                        if self.pointer != Some(pos) {
                            self.pointer = Some(pos);
                            self.input(InputEvent::PointerMove { x: pos.0, y: pos.1 });
                            if let Some(r) = &self.recorder {
                                r.pointer(pos.0, pos.1);
                            }
                        }
                    }
                }
                Event::PointerButton { pos, button, pressed, modifiers } if hovered || !pressed => {
                    if keyboard_via_egui {
                        self.sync_modifiers(modifiers);
                    }
                    let b = match button {
                        egui::PointerButton::Primary => MouseButton::Left,
                        egui::PointerButton::Secondary => MouseButton::Right,
                        egui::PointerButton::Middle => MouseButton::Middle,
                        egui::PointerButton::Extra1 => MouseButton::X1,
                        egui::PointerButton::Extra2 => MouseButton::X2,
                    };
                    if let Some((x, y)) = self.to_remote(img, pos, true) {
                        self.input(InputEvent::Button { button: b, down: pressed, x, y });
                    }
                }
                Event::MouseWheel { unit, delta, .. } if hovered => {
                    let per = match unit {
                        egui::MouseWheelUnit::Point => 3.0,
                        egui::MouseWheelUnit::Line => 120.0,
                        egui::MouseWheelUnit::Page => 360.0,
                    };
                    // egui: +y means content moves down (wheel away from user) = Windows +delta.
                    self.wheel += Vec2::new(-delta.x, delta.y) * per;
                    let take = |v: &mut f32| -> i32 {
                        if v.abs() < wheel_threshold {
                            return 0;
                        }
                        let n = if rdp { *v as i32 } else { (*v / 120.0) as i32 * 120 };
                        *v -= n as f32;
                        n
                    };
                    let (dx, dy) = (take(&mut self.wheel.x), take(&mut self.wheel.y));
                    if dx != 0 || dy != 0 {
                        let (x, y) = self.pointer.unwrap_or((0, 0));
                        self.input(InputEvent::Wheel { dx, dy, x, y });
                    }
                }
                Event::Key { key, physical_key, pressed, modifiers, .. } if keyboard_via_egui => {
                    if pressed && modifiers.ctrl && modifiers.alt && key == egui::Key::Enter {
                        actions.push(ViewAction::ToggleFullscreen);
                        continue;
                    }
                    self.sync_modifiers(modifiers);
                    if let Some(code) = keymap::map(physical_key.unwrap_or(key)) {
                        self.input(InputEvent::Key { code, down: pressed });
                    }
                }
                Event::ModifiersChanged(m) if keyboard_via_egui => self.sync_modifiers(m),
                Event::Text(t) if keyboard_via_egui => self.input(InputEvent::Text(t)),
                // egui turns these shortcuts into clipboard events and drops the key, so replay
                // the key. Shift+Delete / Shift+Insert are the Windows alternates for cut / paste.
                Event::Copy if keyboard_via_egui => self.tap(KeyCode::KeyC),
                Event::Cut if keyboard_via_egui => {
                    let m = ui.input(|i| i.modifiers);
                    self.tap(if m.shift && !m.ctrl { KeyCode::Delete } else { KeyCode::KeyX });
                }
                paste @ (Event::Paste(_) | Event::PasteImage(_)) if keyboard_via_egui => {
                    if let Event::Paste(text) = paste {
                        if self.options.clipboard {
                            self.last_clipboard = Some(text.clone());
                            self.send(SessionCommand::ClipboardText(text));
                        }
                    }
                    let m = ui.input(|i| i.modifiers);
                    self.tap(if m.shift && !m.ctrl { KeyCode::Insert } else { KeyCode::KeyV });
                }
                _ => {}
            }
        }
        if keyboard_via_egui {
            let m = ui.input(|i| i.modifiers);
            self.sync_modifiers(m);
        }
    }

    /// Offer local clipboard text to the server if it changed.
    pub fn offer_clipboard(&mut self, text: String) {
        if self.options.clipboard && self.is_live() && self.last_clipboard.as_deref() != Some(text.as_str()) {
            self.last_clipboard = Some(text.clone());
            self.send(SessionCommand::ClipboardText(text));
        }
    }

    /// Credentials to reconnect with, once the backoff delay has passed.
    pub fn take_due_reconnect(&mut self) -> Option<Credentials> {
        match self.reconnect {
            Some((at, _)) if Instant::now() >= at => {
                self.reconnect = None;
                self.last_creds.clone()
            }
            _ => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.phase, Phase::Connected)
    }

    /// Resize the remote desktop to the window, debounced.
    fn follow_window_size(&mut self, ctx: &egui::Context, area: ERect) {
        if self.options.resolution != Resolution::Window || !matches!(self.phase, Phase::Connected) {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let want = (((area.width() * ppp) as u32) & !1, ((area.height() * ppp) as u32) & !1);
        if want.0 < 200 || want.1 < 200 || want == self.resize_sent {
            self.resize_pending = None;
            return;
        }
        match self.resize_pending {
            Some((w, since)) if w == want => {
                if since.elapsed() >= Duration::from_millis(350) {
                    self.send(SessionCommand::Resize { width: want.0, height: want.1 });
                    self.resize_sent = want;
                    self.resize_pending = None;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(360) - since.elapsed());
                }
            }
            _ => {
                self.resize_pending = Some((want, Instant::now()));
                ctx.request_repaint_after(Duration::from_millis(360));
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, grabbed: &[GrabbedKey], grab: GrabMode, actions: &mut Vec<ViewAction>) {
        let ctx = ui.ctx().clone();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme::canvas(ui.visuals().dark_mode));

        match self.phase.clone() {
            Phase::Connected => {}
            Phase::Connecting(status) => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(area.height() * 0.38);
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(&self.title).size(18.0).strong());
                        ui.label(egui::RichText::new(status).weak());
                        ui.add_space(12.0);
                        if ui.button("Cancel").clicked() {
                            self.disconnect();
                            self.phase = Phase::Disconnected { reason: Some("Cancelled".into()), error: false };
                        }
                    });
                });
                return;
            }
            Phase::NeedCredentials { message } => {
                self.credentials_card(ui, area, message, actions);
                return;
            }
            Phase::Disconnected { reason, error } => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(area.height() * 0.36);
                        let (icon, color) = if error { (icons::WARNING, theme::DANGER) } else { (icons::POWER, ui.visuals().weak_text_color()) };
                        ui.label(egui::RichText::new(icon).size(28.0).color(color));
                        ui.label(egui::RichText::new(if error { "Connection failed" } else { "Disconnected" }).size(18.0).strong());
                        if let Some(r) = reason {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(egui::RichText::new(r).weak()).wrap());
                        }
                        if let Some((at, attempt)) = self.reconnect {
                            let secs = at.saturating_duration_since(Instant::now()).as_secs() + 1;
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(format!("Reconnecting in {secs} s (attempt {attempt} of {})", RECONNECT_DELAYS.len())).color(theme::WARN));
                            ui.ctx().request_repaint_after(Duration::from_millis(250));
                            if ui.small_button("Stop trying").clicked() {
                                self.reconnect = None;
                            }
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            let w = 220.0;
                            ui.add_space((ui.available_width() - w) / 2.0);
                            if ui.add(egui::Button::new(format!("{} Reconnect", icons::ARROW_CLOCKWISE)).min_size(egui::vec2(100.0, 30.0))).clicked() {
                                self.phase = Phase::NeedCredentials { message: None };
                                actions.push(ViewAction::SubmitCredentials {
                                    user: self.form_user.clone(),
                                    password: String::new(),
                                    remember: false,
                                });
                            }
                            if ui.add(egui::Button::new("Close tab").min_size(egui::vec2(100.0, 30.0))).clicked() {
                                actions.push(ViewAction::Close);
                            }
                        });
                    });
                });
                return;
            }
        }

        self.upload(&ctx);
        self.update_rates();
        self.follow_window_size(&ctx, area);

        let id = ui.scope_id().with(("session-view", self.id));
        let resp = ui.interact(area, id, Sense::click_and_drag());
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            resp.request_focus();
        }
        let focused_now = resp.has_focus();
        if focused_now {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() },
                )
            });
        } else if self.focused {
            self.input(InputEvent::ReleaseAll);
            self.sent_mods = egui::Modifiers::NONE;
            self.grab_ctrl = false;
            self.grab_alt = false;
        }
        self.focused = focused_now;

        let hover_pos = ui.input(|i| i.pointer.hover_pos());
        let hovered = resp.contains_pointer();
        let img = self.image_rect(&ctx, area, if hovered { hover_pos } else { None });

        if let Some(tex) = &self.texture {
            let painter = ui.painter_at(area);
            painter.image(tex.id(), img, ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }

        self.handle_input(ui, img, hovered, grabbed, grab, actions);

        // Draw the remote cursor ourselves: no round trip, no lag.
        if hovered && img.contains(hover_pos.unwrap_or_default()) {
            match self.cursor_mode {
                CursorMode::Local => {}
                CursorMode::Hidden => ctx.set_cursor_icon(CursorIcon::None),
                CursorMode::Remote => {
                    if let (Some(c), Some(p)) = (&self.cursor, hover_pos) {
                        ctx.set_cursor_icon(CursorIcon::None);
                        let k = img.width() / self.remote.0.max(1) as f32;
                        let r = ERect::from_min_size(p - c.hot * k, c.size * k);
                        ui.painter_at(area).image(c.tex.id(), r, ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                }
            }
        }

        self.overlays(ui, area, hover_pos, actions);
    }

    fn credentials_card(&mut self, ui: &mut egui::Ui, area: ERect, message: Option<String>, actions: &mut Vec<ViewAction>) {
        let card = ERect::from_center_size(area.center(), egui::vec2(340.0, 250.0).min(area.size()));
        ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
            egui::Frame::window(ui.style()).inner_margin(20.0).show(ui, |ui| {
                ui.set_width(card.width() - 40.0);
                ui.label(egui::RichText::new(format!("Sign in to {}", self.title)).size(17.0).strong());
                ui.label(egui::RichText::new(self.endpoint.to_string()).weak().small());
                if let Some(m) = &message {
                    ui.add_space(4.0);
                    ui.colored_label(theme::WARN, m);
                }
                ui.add_space(10.0);
                let needs_user = self.endpoint.protocol != Protocol::Vnc;
                if needs_user {
                    ui.label("User name");
                    ui.add(egui::TextEdit::singleline(&mut self.form_user).hint_text("DOMAIN\\user, user@domain or MicrosoftAccount\\you@outlook.com").desired_width(f32::INFINITY));
                }
                ui.label("Password");
                let pw = ui.add(egui::TextEdit::singleline(&mut self.form_password).password(true).desired_width(f32::INFINITY));
                if self.form_password.is_empty() && !pw.has_focus() && ui.memory(|m| m.focused().is_none()) {
                    pw.request_focus();
                }
                ui.checkbox(&mut self.form_remember, "Remember in the system keychain");
                ui.add_space(6.0);
                let enter = pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let connect = ui.add(egui::Button::new(egui::RichText::new("Connect").strong()).fill(theme::accent(ui).gamma_multiply(0.8)));
                    if connect.clicked() || enter {
                        actions.push(ViewAction::SubmitCredentials {
                            user: self.form_user.clone(),
                            password: std::mem::take(&mut self.form_password),
                            remember: self.form_remember,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        actions.push(ViewAction::Close);
                    }
                });
            });
        });
    }

    fn overlays(&mut self, ui: &mut egui::Ui, area: ERect, hover: Option<Pos2>, actions: &mut Vec<ViewAction>) {
        let ctx = ui.ctx().clone();
        let near_top = hover.is_some_and(|p| area.contains(p) && p.y - area.top() < 48.0);
        let show_bar = near_top || self.toolbar_pinned || !self.focused;

        if show_bar {
            egui::Area::new(egui::Id::unique(("toolbar", self.id)))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(area.center().x, area.top() + 8.0))
                .pivot(Align2::CENTER_TOP)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(18).inner_margin(egui::vec2(10.0, 4.0)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(egui::RichText::new(&self.title).strong());
                            ui.label(egui::RichText::new(&self.detail).weak().small());
                            ui.separator();
                            let mut scale = self.options.scale;
                            egui::ComboBox::from_id_salt(("scale", self.id))
                                .selected_text(scale.label())
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    for m in ScaleMode::ALL {
                                        ui.selectable_value(&mut scale, m, m.label());
                                    }
                                });
                            self.options.scale = scale;
                            if ui.button(icons::CORNERS_OUT).on_hover_text("Full screen (Ctrl+Alt+Enter)").clicked() {
                                actions.push(ViewAction::ToggleFullscreen);
                            }
                            if ui.button(format!("{} Ctrl+Alt+Del", icons::KEYBOARD)).on_hover_text("Send Ctrl+Alt+Del").clicked() {
                                self.send(SessionCommand::CtrlAltDel);
                            }
                            let rec = self.recorder.is_some();
                            let rec_btn = egui::Button::new(egui::RichText::new(icons::RECORD).color(if rec { theme::DANGER } else { ui.visuals().text_color() }));
                            if ui.add(rec_btn).on_hover_text(if rec { "Stop recording" } else { "Record this session" }).clicked() {
                                actions.push(if rec { ViewAction::StopRecording } else { ViewAction::StartRecording });
                            }
                            if ui.button(icons::CAMERA).on_hover_text("Copy screenshot to clipboard").clicked() {
                                self.screenshot(&ctx);
                                actions.push(ViewAction::Toast("Screenshot copied".into(), ToastKind::Success));
                            }
                            ui.toggle_value(&mut self.show_stats, icons::CHART_BAR).on_hover_text("Connection statistics");
                            ui.toggle_value(&mut self.toolbar_pinned, icons::PUSH_PIN).on_hover_text("Keep this toolbar visible");
                            ui.separator();
                            if ui.button(egui::RichText::new(format!("{} Disconnect", icons::SIGN_OUT)).color(theme::DANGER)).clicked() {
                                self.disconnect();
                                self.phase = Phase::Disconnected { reason: None, error: false };
                            }
                        });
                    });
                });
        }

        if let Some(r) = &self.recorder {
            let text = format!("{} REC {}", icons::RECORD, util::clock(r.elapsed()));
            ui.painter().text(
                Pos2::new(area.right() - 12.0, area.bottom() - 10.0),
                Align2::RIGHT_BOTTOM,
                text,
                egui::FontId::monospace(12.0),
                theme::DANGER,
            );
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        if self.show_stats {
            let lines = format!(
                "{}×{}  ·  {:.0} updates/s\n↓ {}   ↑ {}",
                self.remote.0,
                self.remote.1,
                self.rates.ups,
                util::human_rate(self.rates.down),
                util::human_rate(self.rates.up)
            );
            let galley = ui.painter().layout_no_wrap(lines, egui::FontId::monospace(11.5), Color32::from_gray(230));
            let pos = Pos2::new(area.right() - galley.size().x - 20.0, area.top() + 12.0);
            ui.painter().rect_filled(ERect::from_min_size(pos - egui::vec2(8.0, 6.0), galley.size() + egui::vec2(16.0, 12.0)), 6.0, Color32::from_black_alpha(170));
            ui.painter().galley(pos, galley, Color32::WHITE);
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    pub fn screenshot(&self, ctx: &egui::Context) {
        if let Some(h) = &self.handle {
            let img = h.frame.read(|fb| {
                let px: &[Color32] = bytemuck::cast_slice(fb.pixels());
                ColorImage::new([fb.width() as usize, fb.height() as usize], px.to_vec())
            });
            ctx.copy_image(img);
        }
    }

    pub fn start_recording(&mut self, dir: PathBuf) -> anyhow::Result<PathBuf> {
        let h = self.handle.as_ref().ok_or_else(|| anyhow::anyhow!("not connected"))?;
        let name = format!("{}_{}.peekrec", util::safe_file_name(&self.title), util::file_stamp());
        let path = dir.join(name);
        let ep = self.endpoint.to_string();
        let initial = h.frame.read(|fb| fb.clone());
        let rec = Recorder::start(&path, &[("endpoint", ep.as_str()), ("title", self.title.as_str())], &initial)?;
        self.recorder = Some(rec);
        Ok(path)
    }

    pub fn stop_recording(&mut self) -> Option<peek_record::RecordingInfo> {
        self.recorder.take().and_then(|r| r.stop().ok())
    }

    pub fn status_color(&self) -> Color32 {
        match &self.phase {
            Phase::Connected => theme::OK,
            Phase::Connecting(_) | Phase::NeedCredentials { .. } => theme::WARN,
            Phase::Disconnected { error: true, .. } => theme::DANGER,
            Phase::Disconnected { .. } => Color32::GRAY,
        }
    }

    /// Small live preview for the overview grid.
    pub fn texture_id(&self) -> Option<(egui::TextureId, Vec2)> {
        self.texture.as_ref().map(|t| (t.id(), Vec2::new(self.remote.0 as f32, self.remote.1 as f32)))
    }
}

impl Drop for SessionView {
    fn drop(&mut self) {
        self.disconnect();
    }
}

