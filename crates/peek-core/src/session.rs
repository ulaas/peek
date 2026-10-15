//! The contract between the UI and a protocol backend.
//!
//! The UI creates a session with [`new_session`], hands the
//! [`SessionContext`] to a backend task, and keeps the [`SessionHandle`].
//! Pixels travel through the shared framebuffer; everything else through
//! two channels: commands (UI → backend) and events (backend → UI).

use crate::endpoint::Endpoint;
use crate::frame::SharedFrame;
use crate::input::InputEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

#[derive(Clone, Default)]
pub struct Credentials {
    pub username: Option<String>,
    pub password: Option<String>,
    pub domain: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("domain", &self.domain)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Desired desktop size (RDP) or a hint (VNC, which uses the server's).
    pub width: u32,
    pub height: u32,
    /// Desktop scale factor in percent for RDP (100, 125, 150 ...).
    pub scale_percent: u32,
    pub view_only: bool,
    pub clipboard: bool,
    pub audio: bool,
    /// Windows keyboard layout id (e.g. 0x0409 US) sent to RDP servers.
    pub keyboard_layout: u32,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale_percent: 100,
            view_only: false,
            clipboard: true,
            audio: true,
            keyboard_layout: 0x0409,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub endpoint: Endpoint,
    pub credentials: Credentials,
    pub options: SessionOptions,
}

#[derive(Clone, Debug)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    /// Straight (non-premultiplied) RGBA8.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum CursorUpdate {
    Image(Arc<CursorImage>),
    Hidden,
    Default,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// Human-readable progress ("Negotiating TLS…").
    Status(String),
    Connected { width: u32, height: u32, detail: String },
    Resized { width: u32, height: u32 },
    Cursor(CursorUpdate),
    ClipboardText(String),
    Bell,
    /// Server certificate / host key fingerprint seen during connect.
    ServerIdentity { fingerprint: String },
    Disconnected { reason: Option<String>, error: bool },
}

#[derive(Clone, Debug)]
pub enum SessionCommand {
    Input(InputEvent),
    ClipboardText(String),
    /// Ask the server for a new desktop size (RDP display control, VNC
    /// SetDesktopSize). Servers may ignore it.
    Resize { width: u32, height: u32 },
    CtrlAltDel,
    /// Request a full framebuffer refresh.
    Refresh,
    Disconnect,
}

/// Counters a backend updates and the UI reads for its overlay.
#[derive(Default)]
pub struct SessionStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    /// Framebuffer update messages processed.
    pub updates: AtomicU64,
    /// Most recent round-trip estimate in microseconds (0 = unknown).
    pub rtt_us: AtomicU64,
}

impl SessionStats {
    #[inline]
    pub fn add_in(&self, n: usize) {
        self.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_out(&self, n: usize) {
        self.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_update(&self) {
        self.updates.fetch_add(1, Ordering::Relaxed);
    }
}

/// Sends events to the UI and wakes it up.
#[derive(Clone)]
pub struct EventSink {
    tx: std::sync::mpsc::Sender<SessionEvent>,
    frame: Arc<SharedFrame>,
}

impl EventSink {
    pub fn send(&self, ev: SessionEvent) {
        let _ = self.tx.send(ev);
        self.frame.wake();
    }

    pub fn status(&self, s: impl Into<String>) {
        self.send(SessionEvent::Status(s.into()));
    }
}

/// Backend side of a session.
pub struct SessionContext {
    pub frame: Arc<SharedFrame>,
    pub stats: Arc<SessionStats>,
    pub events: EventSink,
    pub commands: mpsc::UnboundedReceiver<SessionCommand>,
}

/// UI side of a session.
pub struct SessionHandle {
    pub frame: Arc<SharedFrame>,
    pub stats: Arc<SessionStats>,
    commands: mpsc::UnboundedSender<SessionCommand>,
    events: std::sync::mpsc::Receiver<SessionEvent>,
}

impl SessionHandle {
    pub fn send(&self, cmd: SessionCommand) {
        let _ = self.commands.send(cmd);
    }

    pub fn input(&self, ev: InputEvent) {
        self.send(SessionCommand::Input(ev));
    }

    /// Drain pending events without blocking.
    pub fn poll_events(&self) -> impl Iterator<Item = SessionEvent> + '_ {
        self.events.try_iter()
    }
}

pub fn new_session(width: u32, height: u32) -> (SessionHandle, SessionContext) {
    let frame = SharedFrame::new(width, height);
    let stats = Arc::new(SessionStats::default());
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    (
        SessionHandle { frame: frame.clone(), stats: stats.clone(), commands: cmd_tx, events: ev_rx },
        SessionContext { frame: frame.clone(), stats, events: EventSink { tx: ev_tx, frame }, commands: cmd_rx },
    )
}
