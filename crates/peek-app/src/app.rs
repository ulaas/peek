use crate::grab::{Grab, GrabMode};
use crate::icons;
use crate::keymap;
use crate::logo;
use crate::player::PlayerView;
use crate::theme;
use crate::util;
use crate::view::{Phase, SessionView, ToastKind, ViewAction};
use crate::widgets;
use egui::{Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText, Vec2};
use peek_core::{Credentials, Endpoint, Protocol, SessionCommand};
use peek_store::{Profile, Resolution, ScaleMode, Store, Theme, Trust, Vault};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

enum Tab {
    Session(SessionView),
    Player(PlayerView),
}

impl Tab {
    fn title(&self) -> &str {
        match self {
            Tab::Session(s) => &s.title,
            Tab::Player(p) => &p.title,
        }
    }
}

struct Toast {
    text: String,
    kind: ToastKind,
    until: Instant,
}

struct Editor {
    profile: Profile,
    address: String,
    user: String,
    password: String,
    has_saved_password: bool,
    is_new: bool,
    error: Option<String>,
    /// Custom width and height fields are showing.
    custom_size: bool,
}

struct Palette {
    query: String,
    selected: usize,
}

#[derive(Clone)]
enum Command {
    Connect(Uuid),
    QuickConnect(String),
    NewConnection,
    OpenRecording,
    ImportRdp,
    OpenRecordingsFolder,
    ToggleFullscreen,
    ToggleSidebar,
    Settings,
    CloseTab,
    SetTheme(Theme),
    CtrlAltDel,
    ToggleRecording,
    Home,
    About,
}

struct CertWarning {
    session: u64,
    endpoint: Endpoint,
    previous: String,
    current: String,
}

pub struct PeekApp {
    rt: tokio::runtime::Handle,
    store: Store,
    tabs: Vec<Tab>,
    active: Option<usize>,
    next_id: u64,
    quick: String,
    quick_error: Option<String>,
    focus_quick: bool,
    search: String,
    editor: Option<Editor>,
    settings_open: bool,
    palette: Option<Palette>,
    toasts: Vec<Toast>,
    fullscreen: bool,
    keyboard_layout: u32,
    desired_px: (u32, u32),
    confirm_delete: Option<Uuid>,
    cert_warning: Option<CertWarning>,
    grab: Grab,
    window_focused: bool,
    /// Highlighted row in the connection list.
    selected: Option<Uuid>,
    about_open: bool,
    logo: egui::TextureHandle,
    /// Session shown last frame, to hand it keyboard focus on tab switches.
    last_shown: Option<u64>,
}

/// Credentials are keyed by endpoint so quick connect and saved
/// connections to the same account share one keychain entry.
fn vault_key(e: &Endpoint) -> String {
    format!(
        "{}://{}@{}:{}",
        e.protocol.scheme(),
        e.username.as_deref().unwrap_or(""),
        e.host.to_lowercase(),
        e.port
    )
}

fn open_in_file_manager(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(any(windows, target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

/// Screen size choice: match the window, a common preset, or a custom size.
fn resolution_picker(ui: &mut egui::Ui, res: &mut Resolution, custom: &mut bool, protocol: Protocol) {
    ui.vertical(|ui| {
        let selected = if *custom { "Custom".to_string() } else { res.label() };
        egui::ComboBox::from_id_salt("ed-resolution").selected_text(selected).width(200.0).show_ui(ui, |ui| {
            if ui.selectable_label(*res == Resolution::Window && !*custom, "Match window").on_hover_text("Follows the window when it is resized").clicked() {
                *res = Resolution::Window;
                *custom = false;
            }
            ui.separator();
            for (w, h) in Resolution::PRESETS {
                let r = Resolution::Fixed { width: w, height: h };
                let text = if r == Resolution::default() { format!("{w} × {h}   (default)") } else { format!("{w} × {h}") };
                if ui.selectable_label(*res == r && !*custom, text).clicked() {
                    *res = r;
                    *custom = false;
                }
            }
            ui.separator();
            if ui.selectable_label(*custom, "Custom…").clicked() {
                *custom = true;
                if *res == Resolution::Window {
                    *res = Resolution::default();
                }
            }
        });
        if *custom {
            if let Resolution::Fixed { width, height } = res {
                let (mut w, mut h) = (*width, *height);
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut w).range(200..=8192).speed(8).suffix(" px"));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut h).range(200..=8192).speed(8).suffix(" px"));
                });
                *res = Resolution::fixed(w, h);
            }
        }
        if protocol == Protocol::Vnc && *res != Resolution::Window {
            ui.label(RichText::new("VNC servers pick their own size; this only applies with Match window").small().weak());
        }
    });
}

/// Hairline around the undecorated window so it stays distinct from what is behind it.
fn window_border(ctx: &egui::Context) {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("window-border")));
    let color = ctx.global_style().visuals.widgets.noninteractive.bg_stroke.color;
    painter.rect_stroke(ctx.content_rect(), 0.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
}

/// Send a Wake-on-LAN magic packet to the broadcast address.
fn wake_on_lan(mac: &str) -> anyhow::Result<()> {
    let bytes: Vec<u8> = mac
        .split([':', '-'])
        .map(|h| u8::from_str_radix(h.trim(), 16))
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow::anyhow!("\"{mac}\" is not a MAC address"))?;
    anyhow::ensure!(bytes.len() == 6, "\"{mac}\" is not a MAC address");
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.send_to(&packet, "255.255.255.255:9")?;
    Ok(())
}

impl PeekApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle, arg: Option<String>) -> Self {
        let store = Store::load().unwrap_or_else(|e| {
            tracing::error!("cannot load settings: {e:#}");
            Store::in_memory(std::env::temp_dir().join("peek"))
        });
        theme::install(&cc.egui_ctx, store.settings.theme);
        let mut app = Self {
            rt,
            store,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            quick: String::new(),
            quick_error: None,
            focus_quick: true,
            search: String::new(),
            editor: None,
            settings_open: false,
            palette: None,
            toasts: Vec::new(),
            fullscreen: false,
            keyboard_layout: keymap::local_keyboard_layout(),
            desired_px: (1920, 1080),
            confirm_delete: None,
            cert_warning: None,
            grab: Grab::new(&cc.egui_ctx),
            window_focused: true,
            selected: None,
            about_open: false,
            last_shown: None,
            logo: logo::texture(&cc.egui_ctx),
        };
        match arg {
            Some(a) if a.to_lowercase().ends_with(".peekrec") => app.open_recording(PathBuf::from(a)),
            Some(a) if a.to_lowercase().ends_with(".rdp") => app.import_rdp(std::path::Path::new(&a)),
            Some(a) => app.quick_connect(&cc.egui_ctx, &a),
            None => {}
        }
        app
    }

    fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(Toast { text: text.into(), kind, until: Instant::now() + Duration::from_secs(4) });
    }

    fn save_library(&mut self) {
        if let Err(e) = self.store.save_library() {
            self.toast(format!("Could not save connections: {e}"), ToastKind::Warning);
        }
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.store.save_settings() {
            self.toast(format!("Could not save settings: {e}"), ToastKind::Warning);
        }
    }

    fn session_mut(&mut self, id: u64) -> Option<&mut SessionView> {
        self.tabs.iter_mut().find_map(|t| match t {
            Tab::Session(s) if s.id == id => Some(s),
            _ => None,
        })
    }

    fn active_session_mut(&mut self) -> Option<&mut SessionView> {
        match self.active.and_then(|i| self.tabs.get_mut(i)) {
            Some(Tab::Session(s)) => Some(s),
            _ => None,
        }
    }

    fn any_session_focused(&self) -> bool {
        self.tabs.iter().any(|t| matches!(t, Tab::Session(s) if s.has_keyboard_focus()))
    }

    // ------------------------------------------------------------------ connect

    fn open_endpoint(&mut self, ctx: &egui::Context, endpoint: Endpoint, profile_id: Option<Uuid>) {
        // Re-use a live tab for the same target instead of opening a second session.
        if let Some(i) = self.tabs.iter().position(|t| matches!(t, Tab::Session(s) if s.endpoint == endpoint && s.is_live())) {
            self.active = Some(i);
            return;
        }
        let profile = profile_id.and_then(|id| self.store.library.get(id)).or_else(|| self.store.library.find_endpoint(&endpoint)).cloned();
        let (title, options, pid, domain) = match &profile {
            Some(p) => (p.name.clone(), p.options.clone(), Some(p.id), p.domain.clone()),
            None => (endpoint.host.clone(), Default::default(), None, None),
        };
        let id = self.next_id;
        self.next_id += 1;
        let key = vault_key(&endpoint);
        let mut view = SessionView::new(id, endpoint.clone(), title, options, key.clone());
        view.profile_id = pid;
        view.show_stats = self.store.settings.show_stats;

        let password = Vault::get(&key);
        let ready = match endpoint.protocol {
            Protocol::Rdp => password.is_some() && endpoint.username.is_some(),
            Protocol::Vnc => true,
            Protocol::Ssh => false,
        };
        if ready {
            let creds = Credentials { username: endpoint.username.clone(), password, domain };
            view.connect(&self.rt, ctx, creds, self.desired_px, self.keyboard_layout);
        }
        if let Some(pid) = pid {
            self.store.library.touch(pid);
            self.save_library();
        }
        self.tabs.push(Tab::Session(view));
        self.active = Some(self.tabs.len() - 1);
    }

    fn quick_connect(&mut self, ctx: &egui::Context, text: &str) {
        match Endpoint::parse(text) {
            Ok(e) => {
                self.quick_error = None;
                self.quick.clear();
                self.focus_quick = false;
                self.open_endpoint(ctx, e, None);
            }
            Err(e) => self.quick_error = Some(e.to_string()),
        }
    }

    fn submit_credentials(&mut self, ctx: &egui::Context, id: u64, user: String, password: String, remember: bool) {
        let (rt, layout, desired) = (self.rt.clone(), self.keyboard_layout, self.desired_px);
        let mut toast = None;
        let mut profile_update = None;
        let pid = self.session_mut(id).and_then(|v| v.profile_id);
        let domain = pid.and_then(|pid| self.store.library.get(pid)).and_then(|p| p.domain.clone());
        let Some(view) = self.session_mut(id) else { return };
        if !user.trim().is_empty() {
            view.endpoint.username = Some(user.trim().to_string());
        }
        view.vault_key = vault_key(&view.endpoint);
        let password = if password.is_empty() { Vault::get(&view.vault_key) } else { Some(password) };
        if remember {
            if let Some(pw) = &password {
                if let Err(e) = Vault::set(&view.vault_key, pw) {
                    toast = Some(format!("Could not save the password: {e}"));
                }
            }
        }
        if let Some(pid) = view.profile_id {
            profile_update = Some((pid, view.endpoint.username.clone()));
        }
        let ready = match view.endpoint.protocol {
            Protocol::Rdp => password.is_some() && view.endpoint.username.is_some(),
            _ => true,
        };
        if ready {
            let creds = Credentials { username: view.endpoint.username.clone(), password, domain };
            view.connect(&rt, ctx, creds, desired, layout);
        } else {
            view.phase = Phase::NeedCredentials { message: None };
        }
        if let Some((pid, user)) = profile_update {
            if let Some(p) = self.store.library.get_mut(pid) {
                if p.endpoint.username != user {
                    p.endpoint.username = user;
                    self.save_library();
                }
            }
        }
        if let Some(t) = toast {
            self.toast(t, ToastKind::Warning);
        }
    }

    fn open_recording(&mut self, path: PathBuf) {
        match PlayerView::open(path) {
            Ok(p) => {
                self.tabs.push(Tab::Player(p));
                self.active = Some(self.tabs.len() - 1);
            }
            Err(e) => self.toast(format!("Cannot open recording: {e:#}"), ToastKind::Warning),
        }
    }

    fn import_rdp(&mut self, path: &std::path::Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match std::fs::read(path).map_err(anyhow::Error::from).and_then(|b| peek_store::import_rdp_file(&b, &name)) {
            Ok(p) => {
                let label = p.name.clone();
                if self.store.library.find_endpoint(&p.endpoint).is_some() {
                    self.toast(format!("{label} is already in your connections"), ToastKind::Info);
                    return;
                }
                self.store.library.upsert(p);
                self.save_library();
                self.toast(format!("Imported {label}"), ToastKind::Success);
            }
            Err(e) => self.toast(format!("Cannot import {name}: {e:#}"), ToastKind::Warning),
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            self.active = match self.active {
                _ if self.tabs.is_empty() => None,
                Some(a) if a > i => Some(a - 1),
                Some(a) if a == i => Some(i.min(self.tabs.len() - 1)),
                other => other,
            };
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn handle_action(&mut self, ctx: &egui::Context, id: u64, action: ViewAction) {
        match action {
            ViewAction::ToggleFullscreen => self.set_fullscreen(ctx, !self.fullscreen),
            ViewAction::SubmitCredentials { user, password, remember } => self.submit_credentials(ctx, id, user, password, remember),
            ViewAction::Close => {
                if let Some(i) = self.tabs.iter().position(|t| matches!(t, Tab::Session(s) if s.id == id)) {
                    self.close_tab(i);
                }
            }
            ViewAction::Toast(t, k) => self.toast(t, k),
            ViewAction::StartRecording => {
                let dir = self.store.recordings_dir();
                if let Some(v) = self.session_mut(id) {
                    match v.start_recording(dir) {
                        Ok(p) => {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            self.toast(format!("Recording to {name}"), ToastKind::Info);
                        }
                        Err(e) => self.toast(format!("Cannot start recording: {e:#}"), ToastKind::Warning),
                    }
                }
            }
            ViewAction::StopRecording => {
                if let Some(info) = self.session_mut(id).and_then(|v| v.stop_recording()) {
                    self.toast(
                        format!("Recording saved: {} in {}", util::human_bytes(info.bytes), util::clock(info.duration)),
                        ToastKind::Success,
                    );
                }
            }
            ViewAction::ServerIdentity(fp) => {
                let Some(endpoint) = self.session_mut(id).map(|v| v.endpoint.clone()) else { return };
                match self.store.known_hosts.check(&endpoint, &fp) {
                    Trust::Known => {}
                    Trust::New => {
                        let _ = self.store.save_known_hosts();
                        self.toast(format!("Saved the certificate of {}", endpoint.host), ToastKind::Info);
                    }
                    Trust::Changed { previous } => {
                        self.cert_warning = Some(CertWarning { session: id, endpoint, previous, current: fp });
                    }
                }
            }
            ViewAction::Connected => {
                // Successful quick connects join the library so they show up in Recent.
                let Some(v) = self.session_mut(id) else { return };
                if v.profile_id.is_none() {
                    let mut p = Profile::new(v.endpoint.clone());
                    p.last_used = Some(peek_store::now_unix());
                    v.profile_id = Some(p.id);
                    self.store.library.upsert(p);
                    self.save_library();
                }
            }
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::Connect(id) => {
                if let Some(p) = self.store.library.get(id) {
                    let e = p.endpoint.clone();
                    self.open_endpoint(ctx, e, Some(id));
                }
            }
            Command::QuickConnect(s) => self.quick_connect(ctx, &s),
            Command::NewConnection => self.open_editor(None),
            Command::OpenRecording => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Peek recording", &["peekrec"])
                    .set_directory(self.store.recordings_dir())
                    .pick_file()
                {
                    self.open_recording(path);
                }
            }
            Command::ImportRdp => {
                if let Some(paths) = rfd::FileDialog::new().add_filter("Remote Desktop file", &["rdp"]).pick_files() {
                    for p in paths {
                        self.import_rdp(&p);
                    }
                }
            }
            Command::OpenRecordingsFolder => open_in_file_manager(&self.store.recordings_dir()),
            Command::ToggleFullscreen => self.set_fullscreen(ctx, !self.fullscreen),
            Command::ToggleSidebar => {
                self.store.settings.sidebar_open = !self.store.settings.sidebar_open;
                self.save_settings();
            }
            Command::Settings => self.settings_open = true,
            Command::CloseTab => {
                if let Some(i) = self.active {
                    self.close_tab(i);
                }
            }
            Command::SetTheme(t) => {
                self.store.settings.theme = t;
                theme::apply_preference(ctx, t);
                self.save_settings();
            }
            Command::CtrlAltDel => {
                if let Some(s) = self.active_session_mut() {
                    s.send(SessionCommand::CtrlAltDel);
                }
            }
            Command::ToggleRecording => {
                if let Some(s) = self.active_session_mut() {
                    let id = s.id;
                    let action = if s.recorder.is_some() { ViewAction::StopRecording } else { ViewAction::StartRecording };
                    self.handle_action(ctx, id, action);
                }
            }
            Command::Home => self.active = None,
            Command::About => self.about_open = true,
        }
    }

    fn open_editor(&mut self, id: Option<Uuid>) {
        let (profile, is_new) = match id.and_then(|id| self.store.library.get(id)) {
            Some(p) => (p.clone(), false),
            None => (Profile::new(Endpoint::new(Protocol::Rdp, "")), true),
        };
        let address = if is_new { String::new() } else { format!("{}:{}", profile.endpoint.host, profile.endpoint.port) };
        let has_saved_password = !is_new && Vault::get(&vault_key(&profile.endpoint)).is_some();
        self.editor = Some(Editor {
            user: profile.endpoint.username.clone().unwrap_or_default(),
            address,
            password: String::new(),
            has_saved_password,
            is_new,
            error: None,
            custom_size: matches!(profile.options.resolution, Resolution::Fixed { width, height } if !Resolution::PRESETS.contains(&(width, height))),
            profile,
        });
    }

    // ------------------------------------------------------------------ ui parts

    /// Custom title bar: logo, tabs, a drag area, quick connect, settings and
    /// the window buttons. Dragging the empty space moves the window (with
    /// Windows snapping); double-click maximizes.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut close = None;
        let mut cmd = None;
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let height = 44.0;
        egui::Panel::top("topbar")
            .exact_size(height)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin { left: 10, right: 0, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (rect, logo) = ui.allocate_exact_size(Vec2::splat(24.0), egui::Sense::click());
                    let hover = ui.ctx().animate_bool_with_time(logo.id, logo.hovered(), 0.12);
                    let r = rect.expand(hover);
                    ui.painter().image(self.logo.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    let tip = format!("Peek {}  ·  {}", logo::VERSION, logo::short_commit());
                    if logo.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                        cmd = Some(Command::About);
                    }
                    ui.add_space(8.0);
                    let tip = if self.store.settings.sidebar_open { "Hide connections" } else { "Show connections" };
                    if widgets::icon_button(ui, icons::SIDEBAR_SIMPLE, tip).clicked() {
                        cmd = Some(Command::ToggleSidebar);
                    }
                    ui.add_space(4.0);
                    if widgets::nav_chip(ui, egui::Id::unique("tab-home"), icons::HOUSE, "Home", self.active.is_none()).clicked() {
                        self.active = None;
                    }
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let (dot, key) = match tab {
                            Tab::Session(s) => (s.status_color(), s.id),
                            Tab::Player(_) => (theme::accent(ui), u64::MAX - i as u64),
                        };
                        let out = widgets::tab_chip(ui, egui::Id::unique(("tab", key)), tab.title(), dot, self.active == Some(i));
                        if out.clicked {
                            self.active = Some(i);
                        }
                        if out.close {
                            close = Some(i);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let size = Vec2::new(46.0, height);
                        if widgets::window_button(ui, widgets::WindowButton::Close, size).on_hover_text("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let (kind, tip) = if maximized { (widgets::WindowButton::Restore, "Restore") } else { (widgets::WindowButton::Maximize, "Maximize") };
                        if widgets::window_button(ui, kind, size).on_hover_text(tip).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if widgets::window_button(ui, widgets::WindowButton::Minimize, size).on_hover_text("Minimize").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if widgets::nav_chip(ui, egui::Id::unique("settings-chip"), icons::GEAR, "Settings", self.settings_open)
                            .on_hover_text("Settings (Ctrl+,)")
                            .clicked()
                        {
                            cmd = Some(Command::Settings);
                        }
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(260.0, 30.0), egui::Sense::hover());
                        let resp = ui
                            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                                widgets::search_field(ui, &mut self.quick, icons::LIGHTNING, "Connect to…   Ctrl+L", egui::Id::unique("quick-top"), 13.0, 30.0)
                            })
                            .inner;
                        if self.focus_quick && self.active.is_some() {
                            resp.request_focus();
                            self.focus_quick = false;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.quick.trim().is_empty() {
                            cmd = Some(Command::QuickConnect(self.quick.clone()));
                        }
                        // Whatever is left between the tabs and the controls moves the window.
                        let rest = ui.available_rect_before_wrap();
                        if rest.width() > 4.0 {
                            let drag = ui.interact(rest, egui::Id::unique("titlebar-drag"), egui::Sense::click_and_drag());
                            if drag.double_clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                            } else if drag.drag_started_by(egui::PointerButton::Primary) {
                                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                            }
                        }
                    });
                });
            });
        if let Some(i) = close {
            self.close_tab(i);
        }
        if let Some(c) = cmd {
            self.run_command(&ctx, c);
        }
    }

    /// Without native decorations the edges resize the window: hover shows the
    /// resize cursor, a press hands the drag to the OS.
    fn edge_resize(&self, ctx: &egui::Context) {
        if self.fullscreen || ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
        let r = ctx.content_rect();
        let m = 5.0;
        let (l, rt, t, b) = (pos.x <= r.left() + m, pos.x >= r.right() - m, pos.y <= r.top() + m, pos.y >= r.bottom() - m);
        use egui::{CursorIcon as C, ResizeDirection as D};
        let (dir, icon) = match (l, rt, t, b) {
            (true, _, true, _) => (D::NorthWest, C::ResizeNwSe),
            (_, true, true, _) => (D::NorthEast, C::ResizeNeSw),
            (true, _, _, true) => (D::SouthWest, C::ResizeNeSw),
            (_, true, _, true) => (D::SouthEast, C::ResizeNwSe),
            (true, ..) => (D::West, C::ResizeHorizontal),
            (_, true, ..) => (D::East, C::ResizeHorizontal),
            (_, _, true, _) => (D::North, C::ResizeVertical),
            (_, _, _, true) => (D::South, C::ResizeVertical),
            _ => return,
        };
        ctx.set_cursor_icon(icon);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut copied = None;
        egui::Window::new("About Peek")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .fixed_size([360.0, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::new((self.logo.id(), Vec2::splat(72.0))));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Peek").size(24.0).strong());
                    ui.label(RichText::new("Fast, quiet remote desktop").weak());
                    ui.add_space(12.0);
                });
                egui::Grid::new("about-grid").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Version").weak());
                    ui.label(logo::VERSION);
                    ui.end_row();
                    ui.label(RichText::new("Commit").weak());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}  {}", icons::GIT_COMMIT, logo::short_commit())).monospace());
                        if widgets::icon_button_sized(ui, icons::COPY, "Copy the full version line", 22.0, None).clicked() {
                            ui.ctx().copy_text(logo::version_line());
                            copied = Some(());
                        }
                    });
                    ui.end_row();
                    if logo::COMMIT.ends_with("-dirty") {
                        ui.label("");
                        ui.label(RichText::new("built with uncommitted changes").small().color(theme::WARN));
                        ui.end_row();
                    }
                    ui.label(RichText::new("Built").weak());
                    ui.label(format!("{} · {}", logo::BUILD_DATE, logo::PROFILE));
                    ui.end_row();
                    ui.label(RichText::new("Settings").weak());
                    ui.label(RichText::new(self.store.paths.config.display().to_string()).small());
                    ui.end_row();
                });
                ui.add_space(12.0);
                ui.label(
                    RichText::new("RDP by IronRDP · UI by egui · icons by Phosphor (MIT). Peek's VNC client, recorder and widgets are its own.")
                        .small()
                        .weak(),
                );
                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    if widgets::primary_button(ui, "Close", Vec2::new(110.0, 32.0)).clicked() {
                        self.about_open = false;
                    }
                });
                ui.add_space(4.0);
            });
        if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.about_open = false;
        }
        if copied.is_some() {
            self.toast("Version copied", ToastKind::Success);
        }
    }

    /// Status dot for a saved connection with an open session.
    fn profile_status(&self, id: Uuid) -> Option<Color32> {
        self.tabs.iter().find_map(|t| match t {
            Tab::Session(s) if s.profile_id == Some(id) && s.is_live() => Some(s.status_color()),
            _ => None,
        })
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut open = self.store.settings.sidebar_open;
        let mut cmd = None;
        let mut edit = None;
        let mut toggle_fav = None;
        let mut wake = None;
        let mut delete = None;
        egui::Panel::left("library")
            .default_size(272.0)
            .min_size(220.0)
            .frame(egui::Frame::new().fill(ui.visuals().window_fill).inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 8 }))
            .show_collapsible(ui, &mut open, |ui| {
                let total = self.store.library.profiles.len();
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Connections").size(15.0).strong());
                    if total > 0 {
                        ui.label(RichText::new(total.to_string()).size(11.0).weak());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, icons::PLUS, "New connection (Ctrl+N)").clicked() {
                            cmd = Some(Command::NewConnection);
                        }
                    });
                });
                ui.add_space(6.0);
                let search = widgets::search_field(ui, &mut self.search, icons::MAGNIFYING_GLASS, "Search", egui::Id::unique("library-search"), 13.0, 32.0);
                ui.add_space(8.0);

                let results: Vec<Profile> = self.store.library.search(&self.search).into_iter().cloned().collect();
                let favorites: Vec<&Profile> = results.iter().filter(|p| p.favorite).collect();
                let mut sections: Vec<(String, Vec<&Profile>)> = Vec::new();
                if !favorites.is_empty() && self.search.is_empty() {
                    sections.push(("Favorites".to_string(), favorites));
                }
                for p in &results {
                    let g = if p.group.is_empty() { "Connections".to_string() } else { p.group.clone() };
                    match sections.iter_mut().find(|(name, _)| *name == g) {
                        Some((_, v)) => v.push(p),
                        None => sections.push((g, vec![p])),
                    }
                }

                // Keyboard: arrows move through the list while searching, Enter connects.
                let order: Vec<Uuid> = sections.iter().flat_map(|(_, v)| v.iter().map(|p| p.id)).collect();
                if search.has_focus() && !order.is_empty() {
                    let pos = self.selected.and_then(|s| order.iter().position(|&x| x == s));
                    let (down, up) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
                    if down {
                        self.selected = Some(order[pos.map_or(0, |p| (p + 1).min(order.len() - 1))]);
                    } else if up {
                        self.selected = Some(order[pos.map_or(0, |p| p.saturating_sub(1))]);
                    }
                }
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let target = self.selected.filter(|s| order.contains(s)).or(order.first().copied());
                    if let Some(id) = target {
                        cmd = Some(Command::Connect(id));
                    }
                }

                let footer_h = 40.0;
                egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - footer_h).max(60.0)).show(ui, |ui| {
                    if results.is_empty() {
                        ui.add_space(28.0);
                        ui.vertical_centered(|ui| {
                            let (icon, line1, line2) = if self.search.is_empty() {
                                (icons::PLUGS_CONNECTED, "No saved connections", "Connect to a computer and it is saved here.")
                            } else {
                                (icons::MAGNIFYING_GLASS, "Nothing matches", "Try a host name, user or group.")
                            };
                            ui.label(RichText::new(icon).size(30.0).weak());
                            ui.add_space(4.0);
                            ui.label(RichText::new(line1).strong());
                            ui.label(RichText::new(line2).small().weak());
                            if self.search.is_empty() {
                                ui.add_space(10.0);
                                if widgets::ghost_button(ui, icons::PLUS, "New connection").clicked() {
                                    cmd = Some(Command::NewConnection);
                                }
                            }
                        });
                    }
                    let multi = sections.len() > 1;
                    for (si, (name, items)) in sections.iter().enumerate() {
                        let id = ui.make_persistent_id(("section", name));
                        let mut body = |ui: &mut egui::Ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for p in items {
                                let host = if p.endpoint.port == p.endpoint.protocol.default_port() {
                                    p.endpoint.host.clone()
                                } else {
                                    p.endpoint.socket_addr()
                                };
                                let sub = match &p.endpoint.username {
                                    Some(u) => format!("{u}  ·  {host}"),
                                    None => host,
                                };
                                let row = widgets::connection_row(
                                    ui,
                                    widgets::RowContent {
                                        id: egui::Id::unique(("row", si, p.id)),
                                        protocol: p.endpoint.protocol,
                                        title: &p.name,
                                        subtitle: &sub,
                                        favorite: p.favorite,
                                        status: self.profile_status(p.id),
                                        selected: self.selected == Some(p.id),
                                    },
                                );
                                if row.response.clicked() {
                                    self.selected = Some(p.id);
                                }
                                if row.response.double_clicked() || row.connect {
                                    cmd = Some(Command::Connect(p.id));
                                }
                                if row.toggle_favorite {
                                    toggle_fav = Some(p.id);
                                }
                                let mut popup = egui::Popup::context_menu(&row.response);
                                if row.menu {
                                    popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
                                }
                                popup.show(|ui| {
                                    ui.set_min_width(190.0);
                                    if ui.button(format!("{}  Connect", icons::PLAY)).clicked() {
                                        cmd = Some(Command::Connect(p.id));
                                    }
                                    if ui.button(format!("{}  Edit…", icons::PENCIL_SIMPLE)).clicked() {
                                        edit = Some(p.id);
                                    }
                                    let fav = if p.favorite { "Remove from favorites" } else { "Add to favorites" };
                                    if ui.button(format!("{}  {fav}", icons::STAR)).clicked() {
                                        toggle_fav = Some(p.id);
                                    }
                                    if let Some(mac) = &p.mac {
                                        if ui.button(format!("{}  Wake up (Wake-on-LAN)", icons::POWER)).clicked() {
                                            wake = Some(mac.clone());
                                        }
                                    }
                                    ui.separator();
                                    if ui.button(RichText::new(format!("{}  Delete…", icons::TRASH)).color(theme::DANGER)).clicked() {
                                        delete = Some(p.id);
                                    }
                                });
                            }
                        };
                        if multi {
                            let mut state = widgets::section_header(ui, id, name, items.len());
                            state.show_body_unindented(ui, |ui| body(ui));
                            ui.add_space(6.0);
                        } else {
                            body(ui);
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if widgets::nav_chip(ui, egui::Id::unique("side-settings"), icons::GEAR, "Settings", self.settings_open).on_hover_text("Ctrl+,").clicked() {
                        cmd = Some(Command::Settings);
                    }
                    if widgets::nav_chip(ui, egui::Id::unique("side-about"), icons::INFO, "About", self.about_open).clicked() {
                        cmd = Some(Command::About);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(logo::short_commit()).size(10.5).monospace().weak()).on_hover_text(logo::version_line());
                    });
                });
            });
        if open != self.store.settings.sidebar_open {
            self.store.settings.sidebar_open = open;
            self.save_settings();
        }
        if delete.is_some() {
            self.confirm_delete = delete;
        }
        if let Some(id) = toggle_fav {
            if let Some(p) = self.store.library.get_mut(id) {
                p.favorite = !p.favorite;
            }
            self.save_library();
        }
        if let Some(mac) = wake {
            match wake_on_lan(&mac) {
                Ok(()) => self.toast("Wake-on-LAN packet sent", ToastKind::Success),
                Err(e) => self.toast(format!("Wake-on-LAN failed: {e}"), ToastKind::Warning),
            }
        }
        if let Some(id) = edit {
            self.open_editor(Some(id));
        }
        if let Some(c) = cmd {
            self.run_command(&ctx, c);
        }
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut cmd = None;
        let full = ui.max_rect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(780.0) - 32.0;
            let margin = (ui.available_width() - width) / 2.0;
            let top = ui.ctx().content_rect().height() * 0.11;
            widgets::glow(ui, egui::pos2(full.center().x, full.top() + top + 60.0), width * 0.55);
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(top);
                    ui.label(RichText::new("Peek").size(36.0).strong());
                    ui.label(RichText::new("Connect to a computer by name, address or URL.").size(14.5).weak());
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(width - 130.0, 46.0), egui::Sense::hover());
                        let resp = ui
                            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                                widgets::search_field(
                                    ui,
                                    &mut self.quick,
                                    icons::PLUGS_CONNECTED,
                                    "server01   ·   alice@10.0.0.5   ·   vnc://pi.local:1",
                                    egui::Id::unique("quick-home"),
                                    16.0,
                                    46.0,
                                )
                            })
                            .inner;
                        if self.focus_quick {
                            resp.request_focus();
                            self.focus_quick = false;
                        }
                        let go = widgets::primary_button(ui, "Connect", Vec2::new(120.0, 46.0));
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if (go.clicked() || enter) && !self.quick.trim().is_empty() {
                            cmd = Some(Command::QuickConnect(self.quick.clone()));
                        }
                    });
                    ui.add_space(4.0);
                    match &self.quick_error {
                        Some(e) => {
                            ui.colored_label(theme::DANGER, format!("{}  {e}", icons::WARNING));
                        }
                        None => match Endpoint::parse(&self.quick) {
                            Ok(e) if !self.quick.trim().is_empty() => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(e.protocol.label()).small().strong().color(widgets::protocol_color(e.protocol)));
                                    let user = e.username.as_deref().map(|u| format!(" as {u}")).unwrap_or_default();
                                    ui.label(RichText::new(format!("{}:{}{user}", e.host, e.port)).small().weak());
                                });
                            }
                            _ => {
                                ui.label(" ");
                            }
                        },
                    }

                    // Open sessions with live thumbnails.
                    let live: Vec<(usize, String, Option<(egui::TextureId, Vec2)>, Color32)> = self
                        .tabs
                        .iter()
                        .enumerate()
                        .filter_map(|(i, t)| match t {
                            Tab::Session(s) => Some((i, s.title.clone(), s.texture_id(), s.status_color())),
                            _ => None,
                        })
                        .collect();
                    if !live.is_empty() {
                        ui.add_space(26.0);
                        widgets::caption(ui, "Open sessions");
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(14.0, 14.0);
                            for (i, title, tex, color) in live {
                                let size = Vec2::new(244.0, 160.0);
                                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                                let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);
                                let card = rect.translate(Vec2::new(0.0, -2.0 * t));
                                let p = ui.painter();
                                let shadow = egui::Shadow { offset: [0, (3.0 + 4.0 * t) as i8], blur: (8.0 + 12.0 * t) as u8, spread: 0, color: Color32::from_black_alpha((50.0 + 50.0 * t) as u8) };
                                p.add(shadow.as_shape(card, egui::CornerRadius::same(10)));
                                p.rect_filled(card, 10.0, theme::canvas(ui.visuals().dark_mode));
                                let screen = egui::Rect::from_min_max(card.min + Vec2::splat(8.0), card.max - Vec2::new(8.0, 30.0));
                                if let Some((id, remote)) = tex {
                                    let k = (screen.width() / remote.x).min(screen.height() / remote.y);
                                    let r = egui::Rect::from_center_size(screen.center(), remote * k);
                                    p.image(id, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                } else {
                                    p.text(screen.center(), egui::Align2::CENTER_CENTER, icons::MONITOR, egui::FontId::proportional(28.0), Color32::from_gray(80));
                                }
                                p.circle_filled(egui::pos2(card.left() + 16.0, card.bottom() - 15.0), 3.5, color);
                                let g = widgets::elided(ui, &title, egui::FontId::proportional(13.0), Color32::from_gray(228), card.width() - 40.0);
                                p.galley(egui::pos2(card.left() + 26.0, card.bottom() - 15.0 - g.size().y / 2.0), g, Color32::from_gray(228));
                                p.rect_stroke(card, 10.0, egui::Stroke::new(1.5, theme::accent(ui).gamma_multiply(t)), egui::StrokeKind::Inside);
                                if resp.clicked() {
                                    self.active = Some(i);
                                }
                            }
                        });
                    }

                    let recent: Vec<Profile> = self.store.library.recent(6).into_iter().cloned().collect();
                    if !recent.is_empty() {
                        ui.add_space(26.0);
                        widgets::caption(ui, "Recent");
                        ui.add_space(6.0);
                        let cols = if width > 640.0 { 3 } else if width > 420.0 { 2 } else { 1 };
                        let gap = 12.0;
                        let card_w = (width - gap * (cols as f32 - 1.0)) / cols as f32;
                        for chunk in recent.chunks(cols) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for p in chunk {
                                    let sub = p.endpoint.username.as_deref().map(|u| format!("{u} · {}", p.endpoint.host)).unwrap_or_else(|| p.endpoint.host.clone());
                                    let meta = p.last_used.map(util::ago).unwrap_or_default();
                                    let card = widgets::connection_card(
                                        ui,
                                        egui::Id::unique(("recent", p.id)),
                                        Vec2::new(card_w, 74.0),
                                        p.endpoint.protocol,
                                        &p.name,
                                        &sub,
                                        &meta,
                                        self.profile_status(p.id),
                                    );
                                    if card.clicked() {
                                        cmd = Some(Command::Connect(p.id));
                                    }
                                }
                            });
                            ui.add_space(gap - ui.spacing().item_spacing.y);
                        }
                    }

                    ui.add_space(24.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        if widgets::ghost_button(ui, icons::PLUS, "New connection").clicked() {
                            cmd = Some(Command::NewConnection);
                        }
                        if widgets::ghost_button(ui, icons::FILM_STRIP, "Open recording…").clicked() {
                            cmd = Some(Command::OpenRecording);
                        }
                        if widgets::ghost_button(ui, icons::FOLDER_OPEN, "Recordings folder").clicked() {
                            cmd = Some(Command::OpenRecordingsFolder);
                        }
                    });
                    ui.add_space(18.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 18.0;
                        widgets::shortcut_hint(ui, &["Ctrl", "K"], "Commands");
                        widgets::shortcut_hint(ui, &["Ctrl", "L"], "Connect");
                        widgets::shortcut_hint(ui, &["Ctrl", "Alt", "Enter"], "Full screen");
                        ui.label(RichText::new("Drop .rdp files to import").size(11.5).weak());
                    });
                    ui.add_space(24.0);
                });
            });
        });
        if let Some(c) = cmd {
            self.run_command(&ctx, c);
        }
    }

    fn editor_window(&mut self, ctx: &egui::Context) {
        let Some(ed) = &mut self.editor else { return };
        let mut open = true;
        let mut save = false;
        let mut connect = false;
        let mut cancel = false;
        let title = if ed.is_new { "New connection" } else { "Edit connection" };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("editor").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Address");
                    ui.add(egui::TextEdit::singleline(&mut ed.address).hint_text("host, host:port or rdp://…").desired_width(260.0));
                    ui.end_row();
                    ui.label("Protocol");
                    let parsed = Endpoint::parse(&ed.address).ok();
                    ui.horizontal(|ui| {
                        for p in [Protocol::Rdp, Protocol::Vnc] {
                            ui.selectable_value(&mut ed.profile.endpoint.protocol, p, p.label());
                        }
                        if let Some(e) = &parsed {
                            if ed.address.contains("://") || ed.address.contains(':') {
                                ed.profile.endpoint.protocol = e.protocol;
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Name");
                    let hint = parsed.as_ref().map(|e| e.host.clone()).unwrap_or_default();
                    ui.add(egui::TextEdit::singleline(&mut ed.profile.name).hint_text(hint).desired_width(260.0));
                    ui.end_row();
                    ui.label("User name");
                    ui.add(egui::TextEdit::singleline(&mut ed.user).hint_text("DOMAIN\\user or user@domain").desired_width(260.0));
                    ui.end_row();
                    ui.label("Password");
                    let hint = if ed.has_saved_password { "saved in keychain (unchanged)" } else { "optional" };
                    ui.add(egui::TextEdit::singleline(&mut ed.password).password(true).hint_text(hint).desired_width(260.0));
                    ui.end_row();
                    ui.label("Group");
                    ui.add(egui::TextEdit::singleline(&mut ed.profile.group).hint_text("e.g. Office, Lab").desired_width(260.0));
                    ui.end_row();
                    ui.label("MAC address");
                    let mut mac = ed.profile.mac.clone().unwrap_or_default();
                    ui.add(egui::TextEdit::singleline(&mut mac).hint_text("for Wake-on-LAN (optional)").desired_width(260.0));
                    ed.profile.mac = (!mac.trim().is_empty()).then(|| mac.trim().to_string());
                    ui.end_row();
                    ui.label("Display");
                    egui::ComboBox::from_id_salt("ed-scale").selected_text(ed.profile.options.scale.label()).show_ui(ui, |ui| {
                        for m in ScaleMode::ALL {
                            ui.selectable_value(&mut ed.profile.options.scale, m, m.label());
                        }
                    });
                    ui.end_row();
                    ui.label("Screen size");
                    resolution_picker(ui, &mut ed.profile.options.resolution, &mut ed.custom_size, ed.profile.endpoint.protocol);
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.checkbox(&mut ed.profile.options.clipboard, "Share the clipboard");
                ui.checkbox(&mut ed.profile.options.audio, "Play remote audio");
                ui.checkbox(&mut ed.profile.options.view_only, "View only (send no input)");
                ui.checkbox(&mut ed.profile.options.auto_record, "Record every session automatically");
                ui.add_space(4.0);
                ui.add(egui::TextEdit::multiline(&mut ed.profile.notes).hint_text("Notes").desired_rows(2).desired_width(f32::INFINITY));
                if let Some(e) = &ed.error {
                    ui.colored_label(theme::DANGER, e);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    connect = ui.button(RichText::new("Save and connect").strong()).clicked();
                    save = ui.button("Save").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if !open || cancel {
            self.editor = None;
            return;
        }
        if save || connect {
            let ed = self.editor.as_mut().expect("editor");
            let mut e = match Endpoint::parse(&ed.address) {
                Ok(e) => e,
                Err(err) => {
                    ed.error = Some(err.to_string());
                    return;
                }
            };
            if !ed.address.contains("://") {
                e.protocol = ed.profile.endpoint.protocol;
                if !ed.address.contains(':') || ed.address.starts_with('[') && !ed.address.contains("]:") {
                    e.port = e.protocol.default_port();
                }
            }
            e.username = (!ed.user.trim().is_empty()).then(|| ed.user.trim().to_string());
            ed.profile.endpoint = e;
            if ed.profile.name.trim().is_empty() {
                ed.profile.name = ed.profile.endpoint.host.clone();
            }
            if !ed.password.is_empty() {
                if let Err(err) = Vault::set(&vault_key(&ed.profile.endpoint), &ed.password) {
                    ed.error = Some(format!("Could not save the password: {err}"));
                    return;
                }
            }
            let profile = ed.profile.clone();
            self.editor = None;
            self.store.library.upsert(profile.clone());
            self.save_library();
            if connect {
                self.open_endpoint(ctx, profile.endpoint.clone(), Some(profile.id));
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let mut cmd = None;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let s = &mut self.store.settings;
                ui.label(RichText::new("Appearance").strong());
                ui.horizontal(|ui| {
                    for (t, label) in [(Theme::System, "System"), (Theme::Dark, "Dark"), (Theme::Light, "Light")] {
                        if ui.selectable_value(&mut s.theme, t, label).changed() {
                            theme::apply_preference(ctx, t);
                            changed = true;
                        }
                    }
                });
                changed |= ui.checkbox(&mut s.show_stats, "Show connection statistics in new sessions").changed();
                changed |= ui
                    .checkbox(&mut s.grab_keyboard, "Send system keys (Windows key, Alt+Tab, Print Screen) to the remote computer")
                    .changed();
                ui.add_space(8.0);
                ui.label(RichText::new("Recordings").strong());
                let dir = self.store.settings.recordings_dir.clone().unwrap_or_else(|| self.store.paths.recordings.clone());
                ui.label(RichText::new(dir.display().to_string()).monospace().small());
                ui.horizontal(|ui| {
                    if ui.button("Change…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                            self.store.settings.recordings_dir = Some(d);
                            changed = true;
                        }
                    }
                    if ui.button("Open folder").clicked() {
                        cmd = Some(Command::OpenRecordingsFolder);
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(format!("{}  About Peek", icons::INFO)).clicked() {
                        cmd = Some(Command::About);
                    }
                    ui.label(RichText::new(format!("{} · keyboard layout {:08X}", logo::short_commit(), self.keyboard_layout)).small().weak());
                });
            });
        if changed {
            self.save_settings();
        }
        if !open {
            self.settings_open = false;
        }
        if let Some(c) = cmd {
            self.run_command(ctx, c);
        }
    }

    fn palette_commands(&self, query: &str) -> Vec<(String, Command)> {
        let mut v: Vec<(String, Command)> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            if let Ok(e) = Endpoint::parse(q) {
                v.push((format!("Connect to {e}"), Command::QuickConnect(q.to_string())));
            }
        }
        for p in self.store.library.search("") {
            v.push((format!("Connect: {} ({})", p.name, p.endpoint.host), Command::Connect(p.id)));
        }
        let mut statics = vec![
            ("New connection…".to_string(), Command::NewConnection),
            ("Home".into(), Command::Home),
            ("Open recording…".into(), Command::OpenRecording),
            ("Import .rdp files…".into(), Command::ImportRdp),
            ("Open recordings folder".into(), Command::OpenRecordingsFolder),
            ("Toggle full screen".into(), Command::ToggleFullscreen),
            ("Toggle connections sidebar".into(), Command::ToggleSidebar),
            ("Settings".into(), Command::Settings),
            ("About Peek".into(), Command::About),
            ("Theme: dark".into(), Command::SetTheme(Theme::Dark)),
            ("Theme: light".into(), Command::SetTheme(Theme::Light)),
            ("Theme: follow system".into(), Command::SetTheme(Theme::System)),
        ];
        if self.active.is_some() {
            statics.push(("Close tab".into(), Command::CloseTab));
        }
        if matches!(self.active.and_then(|i| self.tabs.get(i)), Some(Tab::Session(_))) {
            statics.push(("Send Ctrl+Alt+Del".into(), Command::CtrlAltDel));
            statics.push(("Start or stop recording".into(), Command::ToggleRecording));
        }
        v.extend(statics);
        let ql = q.to_lowercase();
        v.retain(|(label, c)| matches!(c, Command::QuickConnect(_)) || ql.split_whitespace().all(|w| label.to_lowercase().contains(w)));
        v.truncate(12);
        v
    }

    fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(mut pal) = self.palette.take() else { return };
        let items = self.palette_commands(&pal.query);
        let mut run = None;
        let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
        if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
            pal.selected = (pal.selected + 1).min(items.len().saturating_sub(1));
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
            pal.selected = pal.selected.saturating_sub(1);
        }
        let modal = egui::Modal::new(egui::Id::unique("palette")).show(ctx, |ui| {
            ui.set_width(520.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut pal.query)
                    .hint_text("Type a command, connection or address")
                    .font(egui::FontId::proportional(16.0))
                    .desired_width(f32::INFINITY),
            );
            resp.request_focus();
            if resp.changed() {
                pal.selected = 0;
            }
            ui.add_space(4.0);
            for (i, (label, cmd)) in items.iter().enumerate() {
                let sel = i == pal.selected;
                if ui.add(egui::Button::selectable(sel, label.as_str()).min_size(Vec2::new(ui.available_width(), 26.0))).clicked() {
                    run = Some(cmd.clone());
                }
            }
            if items.is_empty() {
                ui.label(RichText::new("No matches").weak());
            }
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                run = items.get(pal.selected).map(|(_, c)| c.clone());
            }
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(c) = run {
            self.run_command(ctx, c);
        } else if !close {
            self.palette = Some(pal);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.confirm_delete {
            let name = self.store.library.get(id).map(|p| p.name.clone()).unwrap_or_default();
            let mut decided = None;
            egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.label(RichText::new(format!("Delete \"{name}\"?")).strong());
                ui.label(RichText::new("The saved password is removed from the keychain too.").weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Delete").color(theme::DANGER)).clicked() {
                        decided = Some(true);
                    }
                    if ui.button("Keep").clicked() {
                        decided = Some(false);
                    }
                });
            });
            match decided {
                Some(true) => {
                    if let Some(p) = self.store.library.remove(id) {
                        Vault::delete(&vault_key(&p.endpoint));
                    }
                    self.confirm_delete = None;
                    self.save_library();
                }
                Some(false) => self.confirm_delete = None,
                None => {}
            }
        }

        if let Some(w) = &self.cert_warning {
            let mut decided = None;
            egui::Modal::new(egui::Id::unique("cert-warning")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.label(RichText::new(format!("{} The certificate of {} changed", icons::WARNING, w.endpoint.host)).strong().color(theme::WARN));
                ui.label("This happens when the server was reinstalled or its certificate renewed. It can also mean someone is intercepting the connection.");
                ui.add_space(6.0);
                ui.label(RichText::new("Previous").small().weak());
                ui.label(RichText::new(&w.previous).monospace().small());
                ui.label(RichText::new("Now").small().weak());
                ui.label(RichText::new(&w.current).monospace().small());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Trust the new certificate").clicked() {
                        decided = Some(true);
                    }
                    if ui.button(RichText::new("Disconnect").color(theme::DANGER)).clicked() {
                        decided = Some(false);
                    }
                });
            });
            if let Some(trust) = decided {
                let w = self.cert_warning.take().expect("warning");
                if trust {
                    self.store.known_hosts.trust(&w.endpoint, &w.current);
                    let _ = self.store.save_known_hosts();
                } else if let Some(v) = self.session_mut(w.session) {
                    v.disconnect();
                    v.phase = Phase::Disconnected { reason: Some("Certificate not trusted".into()), error: true };
                }
            }
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::unique("toasts"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
            .show(ctx, |ui| {
                for t in &self.toasts {
                    let color = match t.kind {
                        ToastKind::Info => theme::accent(ui),
                        ToastKind::Success => theme::OK,
                        ToastKind::Warning => theme::WARN,
                    };
                    egui::Frame::popup(ui.style()).inner_margin(egui::vec2(12.0, 8.0)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::new(3.0, 16.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 1.0, color);
                            ui.label(&t.text);
                        });
                    });
                }
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let focused_session = self.any_session_focused();
        let text_focus = ctx.memory(|m| m.focused().is_some()) && !focused_session;
        if self.palette.is_some() || self.editor.is_some() {
            return;
        }
        if focused_session {
            return; // keys belong to the remote computer
        }
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let mut cmd = None;
        ctx.input_mut(|i| {
            if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::K)) {
                cmd = Some(0);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::L)) {
                cmd = Some(1);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::N)) {
                cmd = Some(2);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::W)) {
                cmd = Some(3);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::T)) {
                cmd = Some(4);
            } else if i.consume_shortcut(&sc(Modifiers::NONE, Key::F11)) {
                cmd = Some(5);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Tab)) {
                cmd = Some(6);
            } else if i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Comma)) {
                cmd = Some(7);
            } else if !text_focus && self.fullscreen && i.consume_key(Modifiers::NONE, Key::Escape) {
                cmd = Some(5);
            }
        });
        match cmd {
            Some(0) => self.palette = Some(Palette { query: String::new(), selected: 0 }),
            Some(1) => self.focus_quick = true,
            Some(2) => self.open_editor(None),
            Some(3) => self.run_command(ctx, Command::CloseTab),
            Some(4) => {
                self.active = None;
                self.focus_quick = true;
            }
            Some(5) => self.set_fullscreen(ctx, !self.fullscreen),
            Some(7) => self.settings_open = true,
            Some(6) if !self.tabs.is_empty() => {
                self.active = Some(self.active.map_or(0, |a| (a + 1) % self.tabs.len()));
            }
            _ => {}
        }
    }
}

impl eframe::App for PeekApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Backend events for every session, visible or not.
        let mut actions = Vec::new();
        for tab in &mut self.tabs {
            if let Tab::Session(s) = tab {
                let mut a = Vec::new();
                s.poll(&ctx, &mut a);
                actions.extend(a.into_iter().map(|x| (s.id, x)));
            }
        }
        for (id, a) in actions {
            self.handle_action(&ctx, id, a);
        }
        for tab in &mut self.tabs {
            if let Tab::Session(s) = tab {
                if let Some(creds) = s.take_due_reconnect() {
                    s.connect(&self.rt, &ctx, creds, self.desired_px, self.keyboard_layout);
                }
            }
        }

        // Dropped recordings open in the player.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for path in dropped {
            match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
                Some("peekrec") => self.open_recording(path),
                Some("rdp") => self.import_rdp(&path),
                _ => self.toast("Drop .rdp files to import them, or .peekrec recordings to play them", ToastKind::Info),
            }
        }

        self.shortcuts(&ctx);

        // Keyboard grab follows focus: only while Peek is in front and a
        // connected session has keyboard focus.
        let window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let grab_mode = match self.active.and_then(|i| self.tabs.get(i)) {
            Some(Tab::Session(s))
                if self.store.settings.grab_keyboard
                    && window_focused
                    && s.has_keyboard_focus()
                    && s.is_connected()
                    && !s.options.view_only =>
            {
                if s.endpoint.protocol == Protocol::Rdp { GrabMode::All } else { GrabMode::System }
            }
            _ => GrabMode::Off,
        };
        self.grab.set_mode(grab_mode);
        let grabbed = self.grab.drain();

        // Coming back to Peek: hand the current clipboard to the active session.
        if window_focused && !self.window_focused {
            if let Some(text) = util::read_clipboard_text() {
                if let Some(s) = self.active_session_mut() {
                    s.offer_clipboard(text);
                }
            }
        }
        self.window_focused = window_focused;

        if !self.fullscreen {
            self.top_bar(ui);
            self.sidebar(ui);
        }

        let mut view_actions = Vec::new();
        let mut view_id = 0;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(ui.visuals().panel_fill)).show(ui, |ui| {
            let rect = ui.available_rect_before_wrap();
            let ppp = ui.ctx().pixels_per_point();
            self.desired_px = (((rect.width() * ppp) as u32) & !1, ((rect.height() * ppp) as u32) & !1);
            match self.active.and_then(|i| self.tabs.get_mut(i)) {
                Some(Tab::Session(s)) => {
                    view_id = s.id;
                    if self.last_shown != Some(s.id) && s.is_connected() {
                        s.take_focus();
                    }
                    s.show(ui, &grabbed, &mut view_actions);
                }
                Some(Tab::Player(p)) => p.show(ui),
                None => self.home(ui),
            }
        });
        self.last_shown = (view_id != 0).then_some(view_id);
        for a in view_actions {
            self.handle_action(&ctx, view_id, a);
        }

        self.editor_window(&ctx);
        self.settings_window(&ctx);
        self.palette_window(&ctx);
        self.about_window(&ctx);
        self.dialogs(&ctx);
        self.toasts(&ctx);
        if !self.fullscreen {
            self.edge_resize(&ctx);
            window_border(&ctx);
        }
    }

    fn on_exit(&mut self) {
        for t in &mut self.tabs {
            if let Tab::Session(s) = t {
                s.disconnect();
            }
        }
        let _ = self.store.save_settings();
    }
}
