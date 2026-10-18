//! The contract between the UI and a protocol backend.
//!
//! The UI creates a session with [`new_session`], hands the
//! [`SessionContext`] to a backend task, and keeps the [`SessionHandle`].
//! Pixels travel through the shared framebuffer; everything else through
//! two channels: commands (UI → backend) and events (backend → UI).

use crate::endpoint::Endpoint;
use crate::experience::Experience;
use crate::frame::SharedFrame;
use crate::input::InputEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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
    /// RDP visual effects, color depth and compression.
    pub experience: Experience,
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
            experience: Experience::default(),
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
    /// Files copied on the remote side, downloaded to local paths.
    ClipboardFiles(Vec<std::path::PathBuf>),
    /// Short message for the user (e.g. a clipboard transfer outcome).
    Notice(String),
    Bell,
    /// Server certificate / host key fingerprint seen during connect.
    ServerIdentity { fingerprint: String },
    /// The server rejected or asked for credentials; the UI should prompt.
    AuthRequired { message: String },
    Disconnected { reason: Option<String>, error: bool },
}

/// Returned by backends when credentials are missing or wrong, so the UI
/// can ask for them instead of showing a generic error.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("This server needs a password.")]
    PasswordRequired,
    #[error("Sign-in failed{}", if .0.is_empty() { ".".to_string() } else { format!(": {}.", .0) })]
    Rejected(String),
}

#[derive(Clone, Debug)]
pub enum SessionCommand {
    Input(InputEvent),
    ClipboardText(String),
    /// Local files offered for pasting on the remote side.
    ClipboardFiles(Vec<std::path::PathBuf>),
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
/// A block of decoded session audio: interleaved 16-bit PCM.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioChunk {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// Audio settings and state shared by the UI and the backend.
pub struct AudioControl {
    /// Local playback volume in percent.
    volume: AtomicU32,
    muted: std::sync::atomic::AtomicBool,
    /// When sound last came out (ms since the Unix epoch), for the speaker indicator.
    last_heard_ms: AtomicU64,
    /// Receives a copy of every chunk while set (recording).
    tap: std::sync::Mutex<Option<std::sync::mpsc::Sender<AudioChunk>>>,
}

impl Default for AudioControl {
    fn default() -> Self {
        Self { volume: AtomicU32::new(100), muted: Default::default(), last_heard_ms: AtomicU64::new(0), tap: Default::default() }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl AudioControl {
    pub fn volume(&self) -> u32 {
        self.volume.load(Ordering::Relaxed)
    }
    pub fn set_volume(&self, percent: u32) {
        self.volume.store(percent.min(100), Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }
    /// Gain to apply locally: volume, or zero when muted.
    pub fn gain(&self) -> f32 {
        if self.muted() { 0.0 } else { self.volume() as f32 / 100.0 }
    }
    /// Backend: sound is coming out now.
    pub fn heard(&self) {
        self.last_heard_ms.store(now_ms(), Ordering::Relaxed);
    }
    /// Sound came out within the last half second.
    pub fn recently_heard(&self) -> bool {
        now_ms().saturating_sub(self.last_heard_ms.load(Ordering::Relaxed)) < 500
    }
    /// Start or stop copying audio to `tx`.
    pub fn set_tap(&self, tx: Option<std::sync::mpsc::Sender<AudioChunk>>) {
        if let Ok(mut t) = self.tap.lock() {
            *t = tx;
        }
    }
    /// Backend: hand a chunk to the tap, if any. Drops the tap when its receiver is gone.
    pub fn feed_tap(&self, chunk: &AudioChunk) {
        if let Ok(mut t) = self.tap.lock() {
            if let Some(tx) = t.as_ref() {
                if tx.send(chunk.clone()).is_err() {
                    *t = None;
                }
            }
        }
    }
}

pub struct SessionContext {
    pub frame: Arc<SharedFrame>,
    pub stats: Arc<SessionStats>,
    pub audio: Arc<AudioControl>,
    pub events: EventSink,
    pub commands: mpsc::UnboundedReceiver<SessionCommand>,
}

/// UI side of a session.
pub struct SessionHandle {
    pub frame: Arc<SharedFrame>,
    pub stats: Arc<SessionStats>,
    pub audio: Arc<AudioControl>,
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
    let audio = Arc::new(AudioControl::default());
    (
        SessionHandle { frame: frame.clone(), stats: stats.clone(), audio: audio.clone(), commands: cmd_tx, events: ev_rx },
        SessionContext { frame: frame.clone(), stats, audio, events: EventSink { tx: ev_tx, frame }, commands: cmd_rx },
    )
}
