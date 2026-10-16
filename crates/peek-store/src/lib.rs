//! Persistent state: the connection library, app settings, trusted server
//! certificates and the credential vault.
//!
//! Plain TOML files hold everything except secrets, which go to the OS
//! credential store (Windows Credential Manager, macOS Keychain, Secret
//! Service on Linux) and never touch disk in plain text.

use anyhow::Context;
use peek_core::Endpoint;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub recordings: PathBuf,
}

impl Paths {
    pub fn detect() -> anyhow::Result<Self> {
        let dirs = directories::ProjectDirs::from("dev", "Peek", "Peek").context("no home directory")?;
        let videos = directories::UserDirs::new()
            .and_then(|u| u.video_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());
        Ok(Self { config: dirs.config_dir().to_path_buf(), recordings: videos.join("Peek") })
    }
}

fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            // Keep the original: the next save would otherwise replace it with defaults.
            let backup = path.with_extension("toml.unreadable");
            let _ = std::fs::copy(path, &backup);
            tracing::warn!("ignoring unreadable {} (kept a copy at {}): {e}", path.display(), backup.display());
            T::default()
        }),
        Err(_) => T::default(),
    }
}

/// Write via a temp file and rename, so a crash never leaves half a file.
fn save<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, toml::to_string_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScaleMode {
    /// Keep aspect ratio, fill the window.
    #[default]
    Fit,
    /// One remote pixel per screen pixel, scroll if larger.
    Native,
    /// Fill the window, ignore aspect ratio.
    Stretch,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 3] = [ScaleMode::Fit, ScaleMode::Native, ScaleMode::Stretch];

    pub fn label(self) -> &'static str {
        match self {
            ScaleMode::Fit => "Fit to window",
            ScaleMode::Native => "Actual size",
            ScaleMode::Stretch => "Stretch",
        }
    }
}

/// Remote desktop size for a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "mode")]
pub enum Resolution {
    /// Match the window and follow it when it is resized (RDP display
    /// control, VNC SetDesktopSize).
    Window,
    /// Ask for this size and keep it.
    Fixed { width: u32, height: u32 },
}

impl Default for Resolution {
    fn default() -> Self {
        Resolution::Fixed { width: 1920, height: 1080 }
    }
}

impl Resolution {
    /// Common display sizes offered in the editor.
    pub const PRESETS: [(u32, u32); 12] = [
        (1280, 720),
        (1280, 800),
        (1366, 768),
        (1440, 900),
        (1600, 900),
        (1680, 1050),
        (1920, 1080),
        (1920, 1200),
        (2560, 1080),
        (2560, 1440),
        (3440, 1440),
        (3840, 2160),
    ];

    /// RDP accepts 200..=8192 pixels per side; even widths avoid server quirks.
    pub fn fixed(width: u32, height: u32) -> Self {
        Resolution::Fixed { width: width.clamp(200, 8192) & !1, height: height.clamp(200, 8192) }
    }

    pub fn label(self) -> String {
        match self {
            Resolution::Window => "Match window".into(),
            Resolution::Fixed { width, height } => format!("{width} × {height}"),
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileOptions {
    #[serde(default)]
    pub scale: ScaleMode,
    #[serde(default)]
    pub resolution: Resolution,
    #[serde(default)]
    pub view_only: bool,
    #[serde(default = "yes")]
    pub audio: bool,
    #[serde(default = "yes")]
    pub clipboard: bool,
    /// Start recording as soon as the session connects.
    #[serde(default)]
    pub auto_record: bool,
}

impl Default for ProfileOptions {
    fn default() -> Self {
        Self { scale: ScaleMode::Fit, resolution: Resolution::default(), view_only: false, audio: true, clipboard: true, auto_record: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub endpoint: Endpoint,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
    /// MAC address for Wake-on-LAN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default)]
    pub options: ProfileOptions,
}

impl Profile {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: endpoint.host.clone(),
            endpoint,
            group: String::new(),
            domain: None,
            favorite: false,
            last_used: None,
            mac: None,
            notes: String::new(),
            options: ProfileOptions::default(),
        }
    }

    pub fn vault_key(&self) -> String {
        self.id.to_string()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default, rename = "profile")]
    pub profiles: Vec<Profile>,
}

impl Library {
    pub fn get(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    pub fn find_endpoint(&self, e: &Endpoint) -> Option<&Profile> {
        self.profiles.iter().find(|p| {
            p.endpoint.protocol == e.protocol && p.endpoint.host.eq_ignore_ascii_case(&e.host) && p.endpoint.port == e.port
        })
    }

    pub fn upsert(&mut self, p: Profile) {
        match self.get_mut(p.id) {
            Some(existing) => *existing = p,
            None => self.profiles.push(p),
        }
    }

    pub fn remove(&mut self, id: Uuid) -> Option<Profile> {
        let i = self.profiles.iter().position(|p| p.id == id)?;
        Some(self.profiles.remove(i))
    }

    pub fn touch(&mut self, id: Uuid) {
        if let Some(p) = self.get_mut(id) {
            p.last_used = Some(now_unix());
        }
    }

    /// Most recently used first.
    pub fn recent(&self, n: usize) -> Vec<&Profile> {
        let mut v: Vec<&Profile> = self.profiles.iter().filter(|p| p.last_used.is_some()).collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.last_used));
        v.truncate(n);
        v
    }

    /// Case-insensitive match on name, host, group and user.
    pub fn search<'a>(&'a self, query: &str) -> Vec<&'a Profile> {
        let q = query.trim().to_lowercase();
        let mut v: Vec<&Profile> = self
            .profiles
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.endpoint.host.to_lowercase().contains(&q)
                    || p.group.to_lowercase().contains(&q)
                    || p.endpoint.username.as_deref().is_some_and(|u| u.to_lowercase().contains(&q))
            })
            .collect();
        v.sort_by(|a, b| (a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(b.group.to_lowercase(), b.name.to_lowercase())));
        v
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: Theme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recordings_dir: Option<PathBuf>,
    #[serde(default)]
    pub show_stats: bool,
    #[serde(default = "yes")]
    pub sidebar_open: bool,
    /// Grab Alt+Tab, the Windows key etc. while a session has focus.
    #[serde(default = "yes")]
    pub grab_keyboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: Theme::System, recordings_dir: None, show_stats: false, sidebar_open: true, grab_keyboard: true }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    /// First time we see this server; its fingerprint is now remembered.
    New,
    Known,
    /// The server presents a different certificate than last time.
    Changed { previous: String },
}

/// Server certificate fingerprints, trust-on-first-use like SSH known_hosts.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KnownHosts {
    #[serde(default)]
    hosts: BTreeMap<String, String>,
}

impl KnownHosts {
    fn key(e: &Endpoint) -> String {
        format!("{}://{}:{}", e.protocol.scheme(), e.host.to_lowercase(), e.port)
    }

    pub fn check(&mut self, e: &Endpoint, fingerprint: &str) -> Trust {
        match self.hosts.get(&Self::key(e)) {
            None => {
                self.hosts.insert(Self::key(e), fingerprint.to_string());
                Trust::New
            }
            Some(f) if f == fingerprint => Trust::Known,
            Some(f) => Trust::Changed { previous: f.clone() },
        }
    }

    pub fn trust(&mut self, e: &Endpoint, fingerprint: &str) {
        self.hosts.insert(Self::key(e), fingerprint.to_string());
    }
}

/// Secrets in the OS credential store.
pub struct Vault;

impl Vault {
    const SERVICE: &'static str = "dev.peek.remote-desktop";

    pub fn get(key: &str) -> Option<String> {
        keyring::Entry::new(Self::SERVICE, key).ok()?.get_password().ok()
    }

    pub fn set(key: &str, secret: &str) -> anyhow::Result<()> {
        keyring::Entry::new(Self::SERVICE, key)?.set_password(secret)?;
        Ok(())
    }

    pub fn delete(key: &str) {
        if let Ok(e) = keyring::Entry::new(Self::SERVICE, key) {
            let _ = e.delete_credential();
        }
    }
}

/// Everything persisted, loaded together.
pub struct Store {
    pub paths: Paths,
    pub library: Library,
    pub settings: Settings,
    pub known_hosts: KnownHosts,
}

impl Store {
    pub fn load() -> anyhow::Result<Self> {
        let paths = Paths::detect()?;
        Ok(Self {
            library: load(&paths.config.join("library.toml")),
            settings: load(&paths.config.join("settings.toml")),
            known_hosts: load(&paths.config.join("known_hosts.toml")),
            paths,
        })
    }

    /// A store that never touches disk, for tests and `--portable` runs.
    pub fn in_memory(dir: PathBuf) -> Self {
        Self {
            paths: Paths { config: dir.clone(), recordings: dir.join("recordings") },
            library: Library::default(),
            settings: Settings::default(),
            known_hosts: KnownHosts::default(),
        }
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.settings.recordings_dir.clone().unwrap_or_else(|| self.paths.recordings.clone())
    }

    pub fn save_library(&self) -> anyhow::Result<()> {
        save(&self.paths.config.join("library.toml"), &self.library)
    }

    pub fn save_settings(&self) -> anyhow::Result<()> {
        save(&self.paths.config.join("settings.toml"), &self.settings)
    }

    pub fn save_known_hosts(&self) -> anyhow::Result<()> {
        save(&self.paths.config.join("known_hosts.toml"), &self.known_hosts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_roundtrip() {
        let dir = std::env::temp_dir().join(format!("peek-store-{}", std::process::id()));
        let path = dir.join("library.toml");
        let mut lib = Library::default();
        let mut p = Profile::new(Endpoint::parse("vnc://pi.local:1").unwrap());
        p.group = "Lab".into();
        lib.upsert(p.clone());
        save(&path, &lib).unwrap();
        let back: Library = load(&path);
        assert_eq!(back.profiles, vec![p]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn search_and_recent() {
        let mut lib = Library::default();
        let a = Profile::new(Endpoint::parse("alpha").unwrap());
        let mut b = Profile::new(Endpoint::parse("bravo").unwrap());
        b.group = "Prod".into();
        let bid = b.id;
        lib.upsert(a);
        lib.upsert(b);
        assert_eq!(lib.search("prod").len(), 1);
        assert_eq!(lib.search("").len(), 2);
        lib.touch(bid);
        assert_eq!(lib.recent(5).len(), 1);
    }

    #[test]
    fn known_hosts_tofu() {
        let mut k = KnownHosts::default();
        let e = Endpoint::parse("srv").unwrap();
        assert_eq!(k.check(&e, "AA"), Trust::New);
        assert_eq!(k.check(&e, "AA"), Trust::Known);
        assert_eq!(k.check(&e, "BB"), Trust::Changed { previous: "AA".into() });
    }
}

/// Parse a Microsoft `.rdp` connection file into a profile.
///
/// Accepts the UTF-16LE (with BOM) files mstsc writes as well as UTF-8.
pub fn import_rdp_file(bytes: &[u8], file_name: &str) -> anyhow::Result<Profile> {
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).into_owned()
    };
    let mut fields = std::collections::HashMap::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        if let (Some(k), Some(_ty), Some(v)) = (parts.next(), parts.next(), parts.next()) {
            fields.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let address = fields.get("full address").filter(|a| !a.is_empty()).context("the file has no \"full address\"")?;
    let mut endpoint = Endpoint::parse(&format!("rdp://{address}"))?;
    let mut profile = Profile::new(endpoint.clone());
    if let Some(u) = fields.get("username").filter(|u| !u.is_empty()) {
        endpoint.username = Some(u.clone());
    }
    profile.domain = fields.get("domain").filter(|d| !d.is_empty()).cloned();
    profile.endpoint = endpoint;
    let stem = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    profile.name = stem.strip_suffix(".rdp").unwrap_or(stem).to_string();
    let flag = |k: &str| fields.get(k).map(|v| v.as_str());
    if flag("redirectclipboard") == Some("0") {
        profile.options.clipboard = false;
    }
    // audiomode 0 = play here, 1 = play on remote, 2 = never
    if matches!(flag("audiomode"), Some("1") | Some("2")) {
        profile.options.audio = false;
    }
    let size = |k: &str| fields.get(k).and_then(|v| v.parse::<u32>().ok()).filter(|&v| v >= 200);
    if let (Some(w), Some(h)) = (size("desktopwidth"), size("desktopheight")) {
        profile.options.resolution = Resolution::fixed(w, h);
    } else if flag("dynamic resolution") == Some("1") {
        profile.options.resolution = Resolution::Window;
    }
    if flag("smart sizing") == Some("0") && flag("dynamic resolution") == Some("0") {
        profile.options.scale = ScaleMode::Native;
    }
    Ok(profile)
}

#[cfg(test)]
mod rdp_tests {
    use super::*;

    #[test]
    fn utf16_rdp_file() {
        let src = "screen mode id:i:2\r\ndesktopwidth:i:2560\r\ndesktopheight:i:1440\r\nfull address:s:srv01.corp:3390\r\nusername:s:CORP\\alice\r\nredirectclipboard:i:0\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for u in src.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let p = import_rdp_file(&bytes, "C:\\Users\\x\\Work PC.rdp").unwrap();
        assert_eq!(p.name, "Work PC");
        assert_eq!((p.endpoint.host.as_str(), p.endpoint.port), ("srv01.corp", 3390));
        assert_eq!(p.endpoint.username.as_deref(), Some("CORP\\alice"));
        assert!(!p.options.clipboard);
        assert_eq!(p.options.resolution, Resolution::Fixed { width: 2560, height: 1440 });
    }

    #[test]
    fn resolution_default_and_legacy_profiles() {
        assert_eq!(ProfileOptions::default().resolution, Resolution::Fixed { width: 1920, height: 1080 });
        // Profiles saved before the setting existed still load, with the default size.
        let old: ProfileOptions = toml::from_str("scale = \"fit\"
resize_remote = true
").unwrap();
        assert_eq!(old.resolution, Resolution::default());
        for r in [Resolution::Window, Resolution::fixed(2560, 1440)] {
            let o = ProfileOptions { resolution: r, ..Default::default() };
            let text = toml::to_string_pretty(&o).unwrap();
            assert_eq!(toml::from_str::<ProfileOptions>(&text).unwrap(), o, "{text}");
        }
        assert_eq!(Resolution::fixed(1921, 50), Resolution::Fixed { width: 1920, height: 200 });
    }

    #[test]
    fn missing_address() {
        assert!(import_rdp_file(b"username:s:bob\n", "x.rdp").is_err());
    }
}
