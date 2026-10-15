use crate::grab::{Grab, GrabMode};
use crate::icons;
use crate::keymap;
use crate::player::PlayerView;
use crate::theme;
use crate::util;
use crate::view::{Phase, SessionView, ToastKind, ViewAction};
use egui::{Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText, Vec2};
use peek_core::{Credentials, Endpoint, Protocol, SessionCommand};
use peek_store::{Profile, ScaleMode, Store, Theme, Trust, Vault};
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
    OpenRecordingsFolder,
    ToggleFullscreen,
    ToggleSidebar,
    Settings,
    CloseTab,
    SetTheme(Theme),
    CtrlAltDel,
    ToggleRecording,
    Home,
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

fn protocol_color(p: Protocol) -> Color32 {
    match p {
        Protocol::Rdp => Color32::from_rgb(106, 164, 255),
        Protocol::Vnc => Color32::from_rgb(76, 195, 138),
        Protocol::Ssh => Color32::from_rgb(240, 176, 74),
    }
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
        };
        match arg {
            Some(a) if a.to_lowercase().ends_with(".peekrec") => app.open_recording(PathBuf::from(a)),
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
            let creds = Credentials { username: view.endpoint.username.clone(), password, domain: None };
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
            profile,
        });
    }

    // ------------------------------------------------------------------ ui parts

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut close = None;
        let mut cmd = None;
        egui::Panel::top("topbar")
            .exact_size(40.0)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if ui.button(icons::LIST).on_hover_text("Connections").clicked() {
                        cmd = Some(Command::ToggleSidebar);
                    }
                    let home = ui.selectable_label(self.active.is_none(), "Home");
                    if home.clicked() {
                        self.active = None;
                    }
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let selected = self.active == Some(i);
                        let frame = egui::Frame::new()
                            .fill(if selected { ui.visuals().faint_bg_color } else { Color32::TRANSPARENT })
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(8, 3));
                        frame.show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let dot = match tab {
                                Tab::Session(s) => s.status_color(),
                                Tab::Player(_) => theme::accent(ui),
                            };
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 3.5, dot);
                            let mut title = tab.title().to_string();
                            if title.chars().count() > 22 {
                                title = title.chars().take(21).collect::<String>() + "…";
                            }
                            let label = ui.add(egui::Label::new(if selected { RichText::new(title).strong() } else { RichText::new(title) }).sense(egui::Sense::click()));
                            if label.clicked() {
                                self.active = Some(i);
                            }
                            if label.middle_clicked() {
                                close = Some(i);
                            }
                            if ui.add(egui::Button::new(RichText::new(icons::X).small()).frame(false)).on_hover_text("Close").clicked() {
                                close = Some(i);
                            }
                        });
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(icons::GEAR).on_hover_text("Settings").clicked() {
                            cmd = Some(Command::Settings);
                        }
                        let edit = egui::TextEdit::singleline(&mut self.quick)
                            .hint_text("Connect to…  host, rdp://, vnc://   Ctrl+L")
                            .desired_width(300.0);
                        let resp = ui.add(edit);
                        if self.focus_quick && self.active.is_some() {
                            resp.request_focus();
                            self.focus_quick = false;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.quick.trim().is_empty() {
                            cmd = Some(Command::QuickConnect(self.quick.clone()));
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

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut open = self.store.settings.sidebar_open;
        let mut cmd = None;
        let mut edit = None;
        let mut toggle_fav = None;
        let mut wake = None;
        egui::Panel::left("library")
            .default_size(260.0)
            .min_size(200.0)
            .frame(egui::Frame::new().fill(ui.visuals().window_fill).inner_margin(egui::Margin::symmetric(10, 10)))
            .show_collapsible(ui, &mut open, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Connections").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(icons::PLUS).on_hover_text("New connection (Ctrl+N)").clicked() {
                            cmd = Some(Command::NewConnection);
                        }
                    });
                });
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search").desired_width(f32::INFINITY));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let results: Vec<Profile> = self.store.library.search(&self.search).into_iter().cloned().collect();
                    if results.is_empty() {
                        ui.add_space(12.0);
                        ui.label(RichText::new(if self.search.is_empty() {
                            "No saved connections yet. Connect to a host and it appears here."
                        } else {
                            "Nothing matches."
                        }).weak());
                    }
                    let favorites: Vec<&Profile> = results.iter().filter(|p| p.favorite).collect();
                    let mut sections: Vec<(String, Vec<&Profile>)> = Vec::new();
                    if !favorites.is_empty() {
                        sections.push((format!("{} Favorites", icons::STAR), favorites));
                    }
                    for p in &results {
                        let g = if p.group.is_empty() { "Ungrouped".to_string() } else { p.group.clone() };
                        match sections.iter_mut().find(|(name, _)| *name == g) {
                            Some((_, v)) => v.push(p),
                            None => sections.push((g, vec![p])),
                        }
                    }
                    for (name, items) in sections {
                        egui::CollapsingHeader::new(RichText::new(name).small().weak()).default_open(true).show(ui, |ui| {
                            for p in items {
                                let live = self.tabs.iter().any(|t| matches!(t, Tab::Session(s) if s.profile_id == Some(p.id) && s.is_live()));
                                let row = ui.horizontal(|ui| {
                                    ui.label(RichText::new(p.endpoint.protocol.label()).small().monospace().color(protocol_color(p.endpoint.protocol)));
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        ui.label(if live { RichText::new(&p.name).strong() } else { RichText::new(&p.name) });
                                        let host = if p.endpoint.port == p.endpoint.protocol.default_port() {
                                            p.endpoint.host.clone()
                                        } else {
                                            p.endpoint.socket_addr()
                                        };
                                        let sub = match &p.endpoint.username {
                                            Some(u) => format!("{u}@{host}"),
                                            None => host,
                                        };
                                        ui.label(RichText::new(sub).small().weak());
                                    });
                                });
                                let resp = ui.interact(row.response.rect, ui.scope_id().with(p.id), egui::Sense::click());
                                if resp.hovered() {
                                    ui.painter().rect_filled(row.response.rect.expand(2.0), 4.0, ui.visuals().faint_bg_color.gamma_multiply(0.6));
                                }
                                if resp.double_clicked() {
                                    cmd = Some(Command::Connect(p.id));
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("Connect").clicked() {
                                        cmd = Some(Command::Connect(p.id));
                                        ui.close();
                                    }
                                    if ui.button("Edit…").clicked() {
                                        edit = Some(p.id);
                                        ui.close();
                                    }
                                    if ui.button(if p.favorite { "Remove from favorites" } else { "Add to favorites" }).clicked() {
                                        toggle_fav = Some(p.id);
                                        ui.close();
                                    }
                                    if let Some(mac) = &p.mac {
                                        if ui.button("Wake up (Wake-on-LAN)").clicked() {
                                            wake = Some(mac.clone());
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    if ui.button(RichText::new("Delete…").color(theme::DANGER)).clicked() {
                                        self.confirm_delete = Some(p.id);
                                        ui.close();
                                    }
                                });
                                resp.on_hover_text("Double-click to connect, right-click for more");
                            }
                        });
                    }
                });
            });
        if open != self.store.settings.sidebar_open {
            self.store.settings.sidebar_open = open;
            self.save_settings();
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
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            let margin = (ui.available_width() - width) / 2.0;
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(ui.ctx().content_rect().height() * 0.12);
                    ui.label(RichText::new("Peek").size(34.0).strong());
                    ui.label(RichText::new("Connect to a computer by name, address or URL.").weak());
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(&mut self.quick)
                            .hint_text("server01   ·   alice@10.0.0.5   ·   vnc://pi.local:1")
                            .font(egui::FontId::proportional(17.0))
                            .margin(egui::vec2(12.0, 9.0))
                            .desired_width(width - 110.0);
                        let resp = ui.add(edit);
                        if self.focus_quick {
                            resp.request_focus();
                            self.focus_quick = false;
                        }
                        let go = ui.add_sized([100.0, 38.0], egui::Button::new(RichText::new("Connect").strong()));
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if (go.clicked() || enter) && !self.quick.trim().is_empty() {
                            cmd = Some(Command::QuickConnect(self.quick.clone()));
                        }
                    });
                    match &self.quick_error {
                        Some(e) => ui.colored_label(theme::DANGER, e),
                        None => match Endpoint::parse(&self.quick) {
                            Ok(e) if !self.quick.trim().is_empty() => ui.label(RichText::new(format!("{} · port {}", e.protocol.label(), e.port)).small().weak()),
                            _ => ui.label(""),
                        },
                    };

                    // Live sessions overview.
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
                        ui.add_space(22.0);
                        ui.label(RichText::new("OPEN SESSIONS").small().weak());
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            for (i, title, tex, color) in live {
                                let size = Vec2::new(240.0, 150.0);
                                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                                let p = ui.painter();
                                p.rect_filled(rect, 8.0, theme::canvas(ui.visuals().dark_mode));
                                if let Some((id, remote)) = tex {
                                    let inner = rect.shrink(8.0);
                                    let k = (inner.width() / remote.x).min((inner.height() - 10.0) / remote.y);
                                    let r = egui::Rect::from_center_size(inner.center() - Vec2::new(0.0, 6.0), remote * k);
                                    p.image(id, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                p.circle_filled(egui::pos2(rect.left() + 14.0, rect.bottom() - 14.0), 3.5, color);
                                p.text(egui::pos2(rect.left() + 24.0, rect.bottom() - 14.0), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(13.0), Color32::from_gray(225));
                                if resp.hovered() {
                                    p.rect_stroke(rect, 8.0, egui::Stroke::new(1.5, theme::accent(ui)), egui::StrokeKind::Inside);
                                }
                                if resp.clicked() {
                                    self.active = Some(i);
                                }
                            }
                        });
                    }

                    let recent: Vec<Profile> = self.store.library.recent(8).into_iter().cloned().collect();
                    if !recent.is_empty() {
                        ui.add_space(22.0);
                        ui.label(RichText::new("RECENT").small().weak());
                        ui.add_space(4.0);
                        egui::Grid::new("recent").num_columns(4).spacing([10.0, 6.0]).striped(false).show(ui, |ui| {
                            for p in recent {
                                ui.label(RichText::new(p.endpoint.protocol.label()).small().monospace().color(protocol_color(p.endpoint.protocol)));
                                if ui.link(RichText::new(&p.name).strong()).clicked() {
                                    cmd = Some(Command::Connect(p.id));
                                }
                                ui.label(RichText::new(p.endpoint.to_string()).weak().small());
                                ui.label(RichText::new(p.last_used.map(util::ago).unwrap_or_default()).weak().small());
                                ui.end_row();
                            }
                        });
                    }

                    ui.add_space(26.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        if ui.button(format!("{} New connection", icons::PLUS)).clicked() {
                            cmd = Some(Command::NewConnection);
                        }
                        if ui.button(format!("{} Open recording…", icons::FILM_STRIP)).clicked() {
                            cmd = Some(Command::OpenRecording);
                        }
                        if ui.button(format!("{} Recordings folder", icons::FOLDER_OPEN)).clicked() {
                            cmd = Some(Command::OpenRecordingsFolder);
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Ctrl+K commands  ·  Ctrl+L connect  ·  Ctrl+Alt+Enter full screen  ·  drop a .peekrec file to play it")
                            .small()
                            .weak(),
                    );
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
                    ui.add(egui::TextEdit::singleline(&mut ed.user).hint_text("DOMAIN\\user").desired_width(260.0));
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
                });
                ui.add_space(4.0);
                ui.checkbox(&mut ed.profile.options.resize_remote, "Resize the remote desktop to fit the window");
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
                ui.label(RichText::new("About").strong());
                ui.label(format!("Peek {} · keyboard layout {:08X}", env!("CARGO_PKG_VERSION"), self.keyboard_layout));
                ui.label(RichText::new(format!("Settings in {}", self.store.paths.config.display())).small().weak());
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
            ("Open recordings folder".into(), Command::OpenRecordingsFolder),
            ("Toggle full screen".into(), Command::ToggleFullscreen),
            ("Toggle connections sidebar".into(), Command::ToggleSidebar),
            ("Settings".into(), Command::Settings),
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
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("peekrec")) {
                self.open_recording(path);
            } else {
                self.toast("Only .peekrec recordings can be dropped here", ToastKind::Info);
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
                    s.show(ui, &grabbed, grab_mode, &mut view_actions);
                }
                Some(Tab::Player(p)) => p.show(ui),
                None => self.home(ui),
            }
        });
        for a in view_actions {
            self.handle_action(&ctx, view_id, a);
        }

        self.editor_window(&ctx);
        self.settings_window(&ctx);
        self.palette_window(&ctx);
        self.dialogs(&ctx);
        self.toasts(&ctx);
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
