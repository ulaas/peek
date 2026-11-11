//! Update check and one-click self-update.
//!
//! Peek asks GitHub for its latest release: once a day at startup unless that
//! is turned off in Settings, and whenever the user asks. The request carries
//! nothing but a `peek/<version>` User-Agent. Nothing is downloaded until the
//! user clicks; then the file for this kind of install is fetched from
//! github.com/ulaas/peek over HTTPS and installed only if its SHA-256 matches
//! the release's `SHA256SUMS.txt` and GitHub's own digest of the file.
//!
//! How the update is put in place depends on how Peek was installed (see
//! [`InstallKind`]). Copies that belong to a package manager are never
//! touched: they only say how to update.

use crate::{logo, util};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Where the latest release is shown, and the fallback for every link.
pub const RELEASES_URL: &str = "https://github.com/ulaas/peek/releases/latest";
/// Seconds between two automatic checks.
const CHECK_EVERY: u64 = 86_400;
const SUMS: &str = "SHA256SUMS.txt";
/// No release file is anywhere near this size; a larger one is refused.
const MAX_ASSET: u64 = 256 << 20;
/// Cap for the release description and the checksum list.
const MAX_TEXT: u64 = 1 << 20;
const CANCELLED: &str = "cancelled";
/// First argument of the helper copy that runs the installer once Peek has closed.
pub const FINISH_ARG: &str = "--finish-update";
/// How the restarted Peek learns what became of the update: `ok:<previous version>` or `failed:<why>`.
const RESULT_ENV: &str = "PEEK_UPDATE_RESULT";

// ---------------------------------------------------------------------------
// Versions

/// Numeric parts of `1.2.3`, and whether a pre-release suffix follows (`1.2.3-beta.1`).
fn parse_version(text: &str) -> Option<(Vec<u64>, bool)> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let text = text.split('+').next()?;
    let (core, pre) = match text.split_once('-') {
        Some((core, _)) => (core, true),
        None => (text, false),
    };
    let nums: Vec<u64> = core.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    (!nums.is_empty() && nums.len() <= 4).then_some((nums, pre))
}

/// Whether `candidate` is a later version than `current`, compared number by
/// number (0.10.0 is newer than 0.9.3). A release is newer than its own pre-releases.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some((mut a, a_pre)), Some((mut b, b_pre))) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => b_pre && !a_pre,
        order => order == std::cmp::Ordering::Greater,
    }
}

/// Whether the automatic check should run now: at most once a day, and again
/// if the clock was set back past the last check.
pub fn check_due(enabled: bool, last: Option<u64>, now: u64) -> bool {
    enabled && last.is_none_or(|t| t > now || now - t >= CHECK_EVERY)
}

// ---------------------------------------------------------------------------
// Releases

/// Where releases come from. Outside tests this is always GitHub over HTTPS.
#[derive(Clone, Debug)]
pub struct Source {
    /// Address that answers with the latest release as JSON.
    api: String,
    /// Every download address starts with this.
    downloads: String,
    /// Every release page starts with this.
    pages: String,
    /// Refuse anything but HTTPS, redirects included.
    https_only: bool,
}

impl Source {
    pub fn github() -> Self {
        Self {
            api: "https://api.github.com/repos/ulaas/peek/releases/latest".into(),
            downloads: "https://github.com/ulaas/peek/releases/download/".into(),
            pages: "https://github.com/ulaas/peek/".into(),
            https_only: true,
        }
    }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    name: String,
    url: String,
    /// Size GitHub reports, 0 if it reports none.
    size: u64,
    /// GitHub's own SHA-256 of the file, as lowercase hex, if it reports one.
    sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release page, always under the project's address.
    pub page: String,
    assets: Vec<Asset>,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn sha256_hex(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())).then(|| text.to_ascii_lowercase())
}

/// Read GitHub's description of a release. `Ok(None)` for a draft or a
/// pre-release, which are never offered. Files whose address is not the
/// release's own download address are left out, so they can never be fetched.
pub fn parse_release(json: &str, source: &Source) -> Result<Option<Release>, String> {
    let raw: RawRelease = serde_json::from_str(json).map_err(|e| format!("GitHub's answer is not a release ({e})"))?;
    if raw.draft || raw.prerelease || raw.tag_name.contains('-') {
        return Ok(None);
    }
    let tag = raw.tag_name;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() || !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') {
        return Err(format!("the latest release has an unexpected tag ({tag:?})"));
    }
    let page = if raw.html_url == format!("{}releases/tag/{tag}", source.pages) {
        raw.html_url
    } else {
        format!("{}releases/latest", source.pages)
    };
    let mut assets = Vec::new();
    for a in raw.assets {
        let plain_name = !a.name.is_empty() && a.name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if plain_name && a.browser_download_url == format!("{}{tag}/{}", source.downloads, a.name) {
            let sha256 = a.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).and_then(sha256_hex);
            assets.push(Asset { name: a.name, url: a.browser_download_url, size: a.size, sha256 });
        } else {
            tracing::warn!("update: ignoring release file {:?} at {:?}", a.name, a.browser_download_url);
        }
    }
    Ok(Some(Release { version, page, assets }))
}

/// The checksum `SHA256SUMS.txt` lists for `name` (`<hex>  <name>`, as sha256sum writes it).
fn sums_entry(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then(|| sha256_hex(hash)).flatten()
    })
}

/// The SHA-256 a download must have. `sums` is the release's SHA256SUMS.txt
/// when it has one. Every source that exists must agree, and one must exist.
fn expected_sha256(asset: &Asset, sums: Option<&str>) -> Result<String, String> {
    let listed = match sums {
        Some(text) => {
            Some(sums_entry(text, &asset.name).ok_or_else(|| format!("{SUMS} does not list {}", asset.name))?)
        }
        None => None,
    };
    match (listed, &asset.sha256) {
        (Some(a), Some(b)) if a != *b => {
            Err(format!("{SUMS} and GitHub disagree about the checksum of {}; nothing was downloaded", asset.name))
        }
        (Some(a), _) => Ok(a),
        (None, Some(b)) => Ok(b.clone()),
        (None, None) => Err(format!("the release has no checksum for {}; nothing was downloaded", asset.name)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// HTTPS

fn agent(source: &Source, total: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    let mut config = ureq::Agent::config_builder()
        .https_only(source.https_only)
        .user_agent(format!("peek/{}", logo::VERSION))
        .max_redirects(5)
        .timeout_global(Some(total))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .tls_config(tls);
    if !source.https_only {
        // A local test server is reached directly, whatever proxy the environment names.
        config = config.proxy(None);
    }
    config.build().into()
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub is limiting requests from this address; try again later".into(),
        ureq::Error::StatusCode(404) => "not found (404)".into(),
        ureq::Error::StatusCode(code) => format!("the server answered {code}"),
        e => e.to_string(),
    }
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut resp = agent.get(url).call().map_err(describe)?;
    resp.body_mut().with_config().limit(MAX_TEXT).read_to_string().map_err(describe)
}

/// Ask for the latest published release; `Ok(None)` if there is none to offer.
pub fn latest(source: &Source) -> Result<Option<Release>, String> {
    let text = get_text(&agent(source, Duration::from_secs(30)), &source.api)?;
    parse_release(&text, source)
}

/// Where a running update is, shared between its thread and the UI.
#[derive(Default)]
pub struct Progress {
    stage: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Checking,
    Downloading,
    Verifying,
    Installing,
}

impl Progress {
    fn set(&self, stage: Stage) {
        self.stage.store(stage as u8, Ordering::Relaxed);
    }

    pub fn stage(&self) -> Stage {
        match self.stage.load(Ordering::Relaxed) {
            0 => Stage::Checking,
            1 => Stage::Downloading,
            2 => Stage::Verifying,
            _ => Stage::Installing,
        }
    }

    /// Bytes downloaded and bytes expected.
    pub fn bytes(&self) -> (u64, u64) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn check_cancel(&self) -> Result<(), String> {
        if self.cancelled() { Err(CANCELLED.into()) } else { Ok(()) }
    }
}

/// Download one release file to `dest`. Only the release's own download
/// address is fetched, and never more than the size GitHub announced.
fn download(
    agent: &ureq::Agent,
    source: &Source,
    asset: &Asset,
    dest: &Path,
    progress: &Progress,
    notify: &dyn Fn(),
) -> Result<(), String> {
    if !asset.url.starts_with(&source.downloads) {
        return Err(format!("{} is not a Peek download address", asset.url));
    }
    let limit = if asset.size == 0 { MAX_ASSET } else { asset.size };
    if limit > MAX_ASSET {
        return Err(format!("{} is {}, more than an update can be", asset.name, util::human_bytes(limit)));
    }
    progress.total.store(asset.size, Ordering::Relaxed);
    let resp = agent.get(&asset.url).call().map_err(describe)?;
    let mut reader = resp.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        progress.check_cancel()?;
        let n = reader.read(&mut buf).map_err(|e| format!("the download stopped: {e}"))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > limit {
            return Err(format!("{} is larger than the release says; the download was stopped", asset.name));
        }
        file.write_all(&buf[..n]).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        // Repaint about once per percent.
        let step = (limit / 100).max(1);
        if progress.done.swap(done, Ordering::Relaxed) / step != done / step {
            notify();
        }
    }
    file.sync_all().map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    if asset.size != 0 && done != asset.size {
        return Err(format!("the download of {} is incomplete ({done} of {} bytes)", asset.name, asset.size));
    }
    Ok(())
}

/// The release's checksum list, itself checked against GitHub's digest of it.
fn fetch_sums(agent: &ureq::Agent, source: &Source, asset: &Asset) -> Result<String, String> {
    if !asset.url.starts_with(&source.downloads) {
        return Err(format!("{} is not a Peek download address", asset.url));
    }
    let text = get_text(agent, &asset.url).map_err(|e| format!("cannot read {SUMS}: {e}"))?;
    match &asset.sha256 {
        Some(want) if *want != hex(&Sha256::digest(text.as_bytes())) => {
            Err(format!("{SUMS} does not match GitHub's checksum of it; nothing was downloaded"))
        }
        _ => Ok(text),
    }
}

// ---------------------------------------------------------------------------
// Archives. Peek's portable builds are one folder in a zip (Windows) or a
// tar.gz (Linux); only the executable is taken out of them.

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// One file out of a zip archive (stored or deflated, no zip64), CRC checked.
fn unzip_one(zip: &[u8], want: &str) -> Result<Vec<u8>, String> {
    let bad = || "the archive is not a zip file Peek can read".to_string();
    // The end record sits in the last 22 bytes, or before a comment of up to 64 kB.
    let floor = zip.len().saturating_sub(22 + 0xFFFF);
    let end =
        (floor..=zip.len().saturating_sub(22)).rev().find(|&i| zip[i..].starts_with(b"PK\x05\x06")).ok_or_else(bad)?;
    let count = le16(zip, end + 10).ok_or_else(bad)?;
    let mut at = le32(zip, end + 16).ok_or_else(bad)?;
    for _ in 0..count {
        if zip.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(bad());
        }
        let field = |offset: usize| le32(zip, at + offset).ok_or_else(bad);
        let (method, crc, packed, size, local) =
            (le16(zip, at + 10).ok_or_else(bad)?, field(16)?, field(20)?, field(24)?, field(42)?);
        let (name_len, extra_len, comment_len) = (
            le16(zip, at + 28).ok_or_else(bad)?,
            le16(zip, at + 30).ok_or_else(bad)?,
            le16(zip, at + 32).ok_or_else(bad)?,
        );
        let name = zip.get(at + 46..at + 46 + name_len).ok_or_else(bad)?;
        at += 46 + name_len + extra_len + comment_len;
        if String::from_utf8_lossy(name).replace('\\', "/") != want {
            continue;
        }
        if size as u64 > MAX_ASSET || [packed, size, local].contains(&(u32::MAX as usize)) {
            return Err(bad());
        }
        if zip.get(local..local + 4) != Some(b"PK\x03\x04") {
            return Err(bad());
        }
        let start = local + 30 + le16(zip, local + 26).ok_or_else(bad)? + le16(zip, local + 28).ok_or_else(bad)?;
        let data = zip.get(start..start + packed).ok_or_else(bad)?;
        let mut out = Vec::with_capacity(size);
        match method {
            0 => out.extend_from_slice(data),
            8 => {
                let read = flate2::read::DeflateDecoder::new(data).take(size as u64 + 1).read_to_end(&mut out);
                read.map_err(|e| format!("{want} in the archive is damaged: {e}"))?;
            }
            _ => return Err(bad()),
        }
        let mut sum = flate2::Crc::new();
        sum.update(&out);
        if out.len() != size || sum.sum() as usize != crc {
            return Err(format!("{want} in the archive is damaged"));
        }
        return Ok(out);
    }
    Err(format!("the archive has no {want}"))
}

/// One regular file out of a gzip-compressed tar archive.
fn untar_one(tar_gz: &[u8], want: &str) -> Result<Vec<u8>, String> {
    let bad = |e: std::io::Error| format!("the archive is not a tar.gz file Peek can read: {e}");
    let text = |field: &[u8]| String::from_utf8_lossy(field.split(|&b| b == 0).next().unwrap_or_default()).into_owned();
    let mut tar = flate2::read::GzDecoder::new(tar_gz);
    let mut header = [0u8; 512];
    loop {
        match tar.read_exact(&mut header) {
            Ok(()) if header.iter().any(|&b| b != 0) => {}
            // Zero blocks, or the end of the data, close the archive.
            Ok(()) | Err(_) => return Err(format!("the archive has no {want}")),
        }
        let size = u64::from_str_radix(text(&header[124..136]).trim(), 8)
            .map_err(|_| "the archive is not a tar.gz file Peek can read".to_string())?;
        let mut name = text(&header[..100]);
        if header[257..262] == *b"ustar" && header[345] != 0 {
            name = format!("{}/{name}", text(&header[345..500]));
        }
        let regular = matches!(header[156], b'0' | 0);
        if regular && name.trim_start_matches("./") == want {
            if size > MAX_ASSET {
                return Err(format!("{want} in the archive is too large"));
            }
            let mut out = Vec::with_capacity(size as usize);
            tar.by_ref().take(size).read_to_end(&mut out).map_err(bad)?;
            return if out.len() as u64 == size { Ok(out) } else { Err(format!("{want} in the archive is cut short")) };
        }
        // Skip the entry's data, which is padded to whole blocks.
        std::io::copy(&mut tar.by_ref().take(size.div_ceil(512) * 512), &mut std::io::sink()).map_err(bad)?;
    }
}

// ---------------------------------------------------------------------------
// How Peek was installed

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    Mac,
}

/// Why a copy of Peek does not update itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Managed {
    Scoop,
    Homebrew,
    /// A deb, rpm or AUR package under /usr.
    SystemPackage,
    /// Peek cannot write where it runs from.
    ReadOnly,
    /// A debug build from a source tree.
    DevBuild,
    /// No release file fits this system or this layout.
    Unsupported,
}

/// How this copy of Peek got onto the computer, which decides how it is updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// peek.exe from the portable zip, in a folder the user can write to: swapped in place.
    WindowsPortable,
    /// Installed for all users by the MSI: the new MSI is run, Windows asks for permission.
    WindowsMsi,
    /// ~/.local/bin/peek or an unpacked tarball in a writable folder: replaced in place.
    LinuxBinary,
    /// Peek.app: the new disk image is downloaded, checked and opened.
    MacApp,
    /// Updated by something else; Peek only says how.
    Managed(Managed),
}

/// What classification looks at, gathered once from the running process.
#[derive(Clone, Debug)]
pub struct Facts {
    pub os: Os,
    pub exe: PathBuf,
    /// Peek can create files next to its executable.
    pub dir_writable: bool,
    /// The MSI's registry key (HKLM\Software\Peek) exists.
    pub msi_marker: bool,
    /// The Program Files folders of this Windows.
    pub program_files: Vec<PathBuf>,
    /// Homebrew has a cask named peek.
    pub brew_cask: bool,
    pub release_build: bool,
    /// Releases carry a build for this processor.
    pub arch_ok: bool,
}

#[cfg(windows)]
fn msi_marker() -> bool {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_64KEY, RegCloseKey, RegOpenKeyExW,
    };
    let key: Vec<u16> = "Software\\Peek\0".encode_utf16().collect();
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: `key` is NUL-terminated and outlives the call; the handle is closed before returning.
    unsafe {
        let found = RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_QUERY_VALUE | KEY_WOW64_64KEY, &mut handle)
            == ERROR_SUCCESS;
        if found {
            RegCloseKey(handle);
        }
        found
    }
}

#[cfg(not(windows))]
fn msi_marker() -> bool {
    false
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".peek-update-{}.tmp", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

impl Facts {
    pub fn detect(exe: &Path) -> Self {
        let os = if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        };
        let arch_ok = match os {
            Os::Windows | Os::Linux => cfg!(target_arch = "x86_64"),
            Os::Mac => cfg!(target_arch = "aarch64"),
        };
        Self {
            os,
            exe: exe.to_path_buf(),
            dir_writable: exe.parent().is_some_and(dir_writable),
            msi_marker: msi_marker(),
            program_files: ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
                .collect(),
            brew_cask: ["/opt/homebrew/Caskroom/peek", "/usr/local/Caskroom/peek"]
                .iter()
                .any(|p| Path::new(p).exists()),
            release_build: !cfg!(debug_assertions),
            arch_ok,
        }
    }
}

/// Path components, lowercased where the file system ignores case.
fn segments(path: &Path, os: Os) -> Vec<String> {
    path.to_string_lossy()
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "?")
        .map(|s| if os == Os::Linux { s.to_string() } else { s.to_lowercase() })
        .collect()
}

pub fn classify(f: &Facts) -> InstallKind {
    use InstallKind::Managed as M;
    let seg = segments(&f.exe, f.os);
    let has = |name: &str| seg.iter().any(|s| s == name);
    let own_folder = if f.dir_writable { None } else { Some(M(Managed::ReadOnly)) };
    if !f.release_build {
        return M(Managed::DevBuild);
    }
    if !f.arch_ok {
        return M(Managed::Unsupported);
    }
    match f.os {
        Os::Windows => {
            if seg.windows(2).any(|w| w[0] == "scoop" && w[1] == "apps") {
                return M(Managed::Scoop);
            }
            if f.program_files.iter().any(|p| seg.starts_with(&segments(p, f.os))) {
                // Program Files is the MSI's; a copy put there by hand needs an administrator.
                return if f.msi_marker { InstallKind::WindowsMsi } else { M(Managed::ReadOnly) };
            }
            own_folder.unwrap_or(InstallKind::WindowsPortable)
        }
        Os::Linux => {
            if has(".linuxbrew") || has("linuxbrew") || has("Cellar") || has("Caskroom") {
                return M(Managed::Homebrew);
            }
            let first = seg.first().map(String::as_str).unwrap_or_default();
            let local = seg.get(1).is_some_and(|s| s == "local");
            if ["bin", "sbin", "lib", "nix", "gnu", "snap", "app"].contains(&first) || (first == "usr" && !local) {
                return M(Managed::SystemPackage);
            }
            own_folder.unwrap_or(InstallKind::LinuxBinary)
        }
        Os::Mac => {
            let bundle = matches!(seg.as_slice(), [.., app, contents, macos, _]
                if app.ends_with(".app") && contents == "contents" && macos == "macos");
            if has("caskroom") || has("cellar") || (bundle && f.brew_cask) {
                M(Managed::Homebrew)
            } else if bundle {
                InstallKind::MacApp
            } else {
                M(Managed::Unsupported)
            }
        }
    }
}

impl InstallKind {
    /// The release file that updates this kind of install; none for copies something else updates.
    pub fn asset(self) -> Option<&'static str> {
        match self {
            InstallKind::WindowsPortable => Some("peek-windows-x64.zip"),
            InstallKind::WindowsMsi => Some("peek-windows-x64.msi"),
            InstallKind::LinuxBinary => Some("peek-linux-x64.tar.gz"),
            InstallKind::MacApp => Some("peek-macos-arm64.dmg"),
            InstallKind::Managed(_) => None,
        }
    }

    /// Label of the one-click action, where there is one.
    pub fn action(self) -> Option<&'static str> {
        match self {
            InstallKind::WindowsPortable | InstallKind::WindowsMsi | InstallKind::LinuxBinary => {
                Some("Update and restart")
            }
            InstallKind::MacApp => Some("Download update"),
            InstallKind::Managed(_) => None,
        }
    }

    /// What the click will do, or how to update a copy Peek does not update itself;
    /// the second part is a command to run, if there is one.
    pub fn advice(self) -> (&'static str, Option<&'static str>) {
        match self {
            InstallKind::WindowsPortable | InstallKind::LinuxBinary => {
                ("Downloads the new version, checks it and restarts Peek.", None)
            }
            InstallKind::WindowsMsi => {
                ("Downloads the new installer, checks it and runs it. Windows asks for permission.", None)
            }
            InstallKind::MacApp => {
                ("Downloads the new disk image, checks it and opens it; then drag Peek to Applications.", None)
            }
            InstallKind::Managed(Managed::Scoop) => {
                ("Peek was installed with Scoop. Update it with:", Some("scoop update peek"))
            }
            InstallKind::Managed(Managed::Homebrew) => {
                ("Peek was installed with Homebrew. Update it with:", Some("brew upgrade --cask peek"))
            }
            InstallKind::Managed(Managed::SystemPackage) => (
                "Peek was installed as a system package. Update it with your package manager, or get the new package from the release page.",
                None,
            ),
            InstallKind::Managed(Managed::ReadOnly) => {
                ("Peek cannot write to the folder it runs from. Get the new version from the release page.", None)
            }
            InstallKind::Managed(Managed::DevBuild) => {
                ("This is a development build; it does not update itself.", None)
            }
            InstallKind::Managed(Managed::Unsupported) => {
                ("This copy of Peek cannot update itself. Get the new version from the release page.", None)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Swapping the executable

/// A replaced executable and the previous one set aside, so the swap can be undone.
#[derive(Debug)]
pub struct Swap {
    target: PathBuf,
    old: PathBuf,
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Where the previous executable waits until the next start removes it: `peek.exe.old`.
fn old_path(target: &Path) -> PathBuf {
    with_suffix(target, ".old")
}

/// Where the new executable is written before the swap: `peek.new.exe`, so it can be run for a check.
fn new_path(target: &Path) -> PathBuf {
    let mut name = target.file_stem().unwrap_or_default().to_os_string();
    name.push(".new");
    if let Some(ext) = target.extension() {
        name.push(".");
        name.push(ext);
    }
    target.with_file_name(name)
}

/// Put `new` in place of `target`, all or nothing: on any failure `target` is
/// what it was and `new` is gone.
///
/// A running .exe cannot be overwritten on Windows but can be renamed, so with
/// `aside` the old file is moved to `<name>.old` first. Without it (Linux) the
/// old file is only linked there and the new one renamed over it, so `target`
/// never stops existing.
fn swap_in(target: &Path, new: &Path, aside: bool) -> Result<Swap, String> {
    let old = old_path(target);
    let fail = |what: &str, e: std::io::Error| {
        let _ = std::fs::remove_file(new);
        Err(format!("{what} {}: {e}", target.display()))
    };
    match std::fs::remove_file(&old) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return fail("an earlier update is still in the way of", e);
        }
        _ => {}
    }
    let set_aside = if aside {
        std::fs::rename(target, &old)
    } else {
        std::fs::hard_link(target, &old).or_else(|_| std::fs::copy(target, &old).map(drop))
    };
    if let Err(e) = set_aside {
        return fail("cannot set aside", e);
    }
    if let Err(e) = std::fs::rename(new, target) {
        if aside {
            let _ = std::fs::rename(&old, target);
        } else {
            let _ = std::fs::remove_file(&old);
        }
        return fail("cannot replace", e);
    }
    Ok(Swap { target: target.to_path_buf(), old })
}

impl Swap {
    /// Put the previous executable back, for when the new one does not start.
    fn rollback(&self) -> Result<(), String> {
        std::fs::rename(&self.old, &self.target).map_err(|e| e.to_string())
    }
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()
    }
    write(path, bytes).map_err(|e| {
        let _ = std::fs::remove_file(path);
        format!("cannot write {}: {e}", path.display())
    })
}

/// Run `<exe> --version` and expect it to name `version`: a new binary that
/// does not start here (an older glibc, a damaged file) must not replace a working one.
fn probe(exe: &Path, version: &str) -> Result<(), String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("the new Peek does not start on this computer: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the new Peek did not answer; nothing was replaced".into());
            }
        }
    }
    let mut out = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut out);
    }
    if out.starts_with(&format!("Peek {version} ")) {
        Ok(())
    } else {
        Err(format!("the download does not run as Peek {version} on this computer; nothing was replaced"))
    }
}

// ---------------------------------------------------------------------------
// The update itself

/// What to update and where.
pub struct Plan {
    pub kind: InstallKind,
    /// The running executable.
    pub exe: PathBuf,
    /// A folder of this update's own for the download.
    pub work: PathBuf,
    /// The running version.
    pub current: String,
    /// Run the new executable once before it replaces the old one.
    pub probe: bool,
}

/// A verified update, one step from done.
#[derive(Debug)]
pub enum Staged {
    /// The new executable is in place; starting it finishes the update.
    Swapped(Swap),
    /// A verified installer and its SHA-256, to run once Peek has closed.
    Installer(PathBuf, String),
    /// A verified disk image for the user to open.
    DiskImage(PathBuf),
}

/// Download, verify and stage the update for `plan`. `known` is the release a
/// check already found; without it the latest one is looked up first. On any
/// error nothing is left behind and the installed Peek is untouched.
pub fn install(
    source: &Source,
    plan: &Plan,
    known: Option<Release>,
    progress: &Progress,
    notify: &dyn Fn(),
) -> Result<(Release, Staged), String> {
    let Some(name) = plan.kind.asset() else { return Err(plan.kind.advice().0.into()) };
    progress.set(Stage::Checking);
    let release = match known {
        Some(r) => r,
        None => latest(source)?
            .filter(|r| is_newer(&r.version, &plan.current))
            .ok_or_else(|| format!("Peek {} is the latest version", plan.current))?,
    };
    let asset = release
        .asset(name)
        .ok_or_else(|| {
            format!("Peek {} has no {name} yet. Try again later, or use the release page.", release.version)
        })?
        .clone();
    let agent = agent(source, Duration::from_secs(30 * 60));
    let sums = release.asset(SUMS).map(|a| fetch_sums(&agent, source, a)).transpose()?;
    let expected = expected_sha256(&asset, sums.as_deref())?;
    progress.check_cancel()?;

    std::fs::create_dir_all(&plan.work).map_err(|e| format!("cannot create {}: {e}", plan.work.display()))?;
    let file = plan.work.join(name);
    let fetch = || -> Result<Vec<u8>, String> {
        progress.set(Stage::Downloading);
        notify();
        download(&agent, source, &asset, &file, progress, notify)?;
        progress.set(Stage::Verifying);
        notify();
        // What is installed is what was hashed: the bytes read back here.
        let bytes = std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        if hex(&Sha256::digest(&bytes)) != expected {
            return Err(format!(
                "{name} does not match the checksum published for Peek {}; nothing was installed",
                release.version
            ));
        }
        progress.check_cancel()?;
        progress.set(Stage::Installing);
        notify();
        Ok(bytes)
    };
    let staged = fetch().and_then(|bytes| stage(plan, &file, bytes, &release.version, progress));
    // The installer and the disk image are still needed; everything else goes.
    if !matches!(staged, Ok(Staged::Installer(..) | Staged::DiskImage(_))) {
        let _ = std::fs::remove_dir_all(&plan.work);
    }
    staged.map(|s| (release, s))
}

/// Put verified `bytes` (also at `file`) where a restart picks them up.
fn stage(plan: &Plan, file: &Path, bytes: Vec<u8>, version: &str, progress: &Progress) -> Result<Staged, String> {
    let name = plan.kind.asset().unwrap_or_default();
    let windows = match plan.kind {
        InstallKind::WindowsPortable => true,
        InstallKind::LinuxBinary => false,
        InstallKind::WindowsMsi => return Ok(Staged::Installer(file.to_path_buf(), hex(&Sha256::digest(&bytes)))),
        InstallKind::MacApp => return Ok(Staged::DiskImage(file.to_path_buf())),
        InstallKind::Managed(_) => return Err(plan.kind.advice().0.into()),
    };
    // Both archives hold one folder named like the archive, with the executable in it.
    let binary = if windows {
        unzip_one(&bytes, &format!("{}/peek.exe", name.trim_end_matches(".zip")))?
    } else {
        untar_one(&bytes, &format!("{}/peek", name.trim_end_matches(".tar.gz")))?
    };
    drop(bytes);
    let new = new_path(&plan.exe);
    write_executable(&new, &binary)?;
    let ready = if plan.probe { probe(&new, version) } else { Ok(()) }.and_then(|()| progress.check_cancel());
    if let Err(e) = ready {
        let _ = std::fs::remove_file(&new);
        return Err(e);
    }
    swap_in(&plan.exe, &new, windows).map(Staged::Swapped)
}

/// Start the new executable; if it does not start, put the previous one back.
fn start_new(swap: &Swap) -> Result<(), String> {
    Command::new(&swap.target).env(RESULT_ENV, format!("ok:{}", logo::VERSION)).spawn().map(drop).map_err(
        |e| match swap.rollback() {
            Ok(()) => format!("The new version did not start ({e}). The previous one is back in place."),
            Err(r) => format!(
                "The new version did not start ({e}) and the previous one could not be put back ({r}); it is at {}.",
                swap.old.display()
            ),
        },
    )
}

/// Hand the installer to a helper: a copy of this Peek outside the install
/// folder, which waits until Peek has closed, runs the installer and starts
/// whatever Peek is installed afterwards (see [`finish_update`]).
fn start_installer(msi: &Path, sha256: &str, exe: &Path) -> Result<(), String> {
    let dir = msi.parent().ok_or("the installer has no folder")?;
    let helper = dir.join("peek-update-helper.exe");
    std::fs::copy(exe, &helper).map_err(|e| format!("Cannot prepare the installer: {e}"))?;
    let child = Command::new(&helper)
        .arg(FINISH_ARG)
        .arg(msi)
        .arg(exe)
        .arg(sha256)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start the installer: {e}"))?;
    // The helper reads from this pipe until it closes, which is when Peek has exited.
    std::mem::forget(child);
    Ok(())
}

fn msiexec() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(root) => PathBuf::from(root).join("System32").join("msiexec.exe"),
        None => PathBuf::from("msiexec.exe"),
    }
}

/// Open the installer so that nothing can write, rename or delete it while
/// the handle lives, and check it is still the file that was verified. It sat
/// in a folder the user can write to, and Windows Installer runs it elevated.
fn open_verified(msi: &Path, sha256: &str) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: others may read, nothing else
    }
    let mut file = options.open(msi).map_err(|e| format!("The installer could not be opened ({e})"))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| format!("The installer could not be read ({e})"))?;
    if hex(&hash.finalize()) != sha256.to_ascii_lowercase() {
        return Err("The installer changed after it was checked, so it was not run".into());
    }
    Ok(file)
}

/// The helper's whole job (`peek --finish-update <msi> <installed exe> <sha256>`):
/// wait for the Peek that started it to exit, check the MSI is still the one
/// that was verified, run it with its progress window (Windows shows its own
/// permission prompt), then start the installed Peek again. If the prompt is
/// declined or the installer fails, Windows Installer leaves the old version
/// in place, and that is what starts.
pub fn finish_update(msi: &Path, exe: &Path, sha256: &str) {
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    std::thread::sleep(Duration::from_millis(500));
    // Held until the installer is done, so the file it reads is the file that was hashed.
    let held = open_verified(msi, sha256);
    let status = match &held {
        Ok(_) => Command::new(msiexec()).arg("/i").arg(msi).args(["/passive", "/norestart"]).status().map(|s| s.code()),
        Err(_) => Ok(None),
    };
    let result = match (&held, status) {
        (Err(why), _) => format!("failed:{why}; this is still the previous version."),
        (Ok(_), status) => installer_result(status),
    };
    drop(held);
    let _ = std::fs::remove_file(msi);
    let _ = Command::new(exe).env(RESULT_ENV, result).spawn();
}

fn installer_result(status: std::io::Result<Option<i32>>) -> String {
    match status {
        // 3010: installed, and Windows wants a restart at some point.
        Ok(Some(0 | 3010)) => format!("ok:{}", logo::VERSION),
        Ok(Some(1602)) => "failed:The update was cancelled; this is still the previous version.".into(),
        Ok(code) => format!(
            "failed:The installer stopped (code {}); this is still the previous version.",
            code.map_or("unknown".into(), |c| c.to_string())
        ),
        Err(e) => format!("failed:The installer did not start ({e}); this is still the previous version."),
    }
}

/// What a restart after an update has to say: `Ok` for a success message, `Err` for a failure.
fn restart_note(result: Option<&str>, version: &str) -> Option<Result<String, String>> {
    let result = result?;
    if let Some(why) = result.strip_prefix("failed:") {
        return Some(Err(why.to_string()));
    }
    let previous = result.strip_prefix("ok:")?;
    Some(if is_newer(version, previous) {
        Ok(format!("Peek was updated to {version}"))
    } else {
        Err(format!("The update was not installed; this is still Peek {version}."))
    })
}

/// Remove what an earlier update left: the previous executable next to this
/// one and old downloads. The old Peek may still be closing, so the first is retried.
fn clean_up(exe: Option<PathBuf>, work: Option<PathBuf>) {
    if let Some(dir) = work {
        let _ = std::fs::remove_dir_all(dir);
    }
    let Some(exe) = exe else { return };
    let _ = std::fs::remove_file(new_path(&exe));
    let old = old_path(&exe);
    for _ in 0..20 {
        match std::fs::remove_file(&old) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(500)),
            _ => break,
        }
    }
}

// ---------------------------------------------------------------------------
// State for the UI

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    CheckFailed(String),
    /// A newer version exists; nothing has been downloaded.
    Available,
    Installing,
    /// Verified and staged: a restart finishes the update.
    Ready,
    /// The disk image is downloaded and open; the user does the rest.
    Opened,
    Failed(String),
}

pub enum Event {
    /// A check got an answer: the newer version, if there is one.
    Checked {
        found: Option<String>,
        manual: bool,
    },
    CheckFailed {
        error: String,
        manual: bool,
    },
    /// The update is staged; [`Updater::relaunch`] finishes it.
    Ready,
    /// A verified disk image to open.
    Downloaded(PathBuf),
    InstallFailed(String),
}

type Notify = Arc<dyn Fn() + Send + Sync>;
type CheckResult = Result<Option<Release>, String>;
type InstallResult = Result<(Release, Staged), String>;

struct Job {
    progress: Arc<Progress>,
    done: mpsc::Receiver<InstallResult>,
}

/// A finished thread's result; a thread that died without one counts as a failure.
fn received<T>(rx: &mpsc::Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match rx.try_recv() {
        Ok(result) => Some(result),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err("the update stopped unexpectedly".into())),
    }
}

/// The update check and the update, as the UI sees them. Network and disk
/// work runs on its own threads; [`Updater::poll`] collects what they report.
pub struct Updater {
    source: Source,
    exe: Option<PathBuf>,
    work: Option<PathBuf>,
    kind: Option<InstallKind>,
    phase: Phase,
    /// The newer version, and its release once a check in this run has fetched it.
    found: Option<(String, Option<Release>)>,
    check: Option<(mpsc::Receiver<CheckResult>, bool)>,
    job: Option<Job>,
    staged: Option<Staged>,
    jobs: u32,
}

impl Updater {
    /// `seen` is the newer version an earlier run found, so it is still offered
    /// on a day without a check.
    pub fn new(seen: Option<&str>) -> Self {
        let found = seen.filter(|v| is_newer(v, logo::VERSION)).map(|v| (v.to_string(), None));
        Self {
            source: Source::github(),
            exe: std::env::current_exe().ok(),
            work: util::update_dir(),
            kind: None,
            phase: if found.is_some() { Phase::Available } else { Phase::Idle },
            found,
            check: None,
            job: None,
            staged: None,
            jobs: 0,
        }
    }

    /// What became of an update this Peek was restarted for, if it was.
    pub fn restart_note() -> Option<Result<String, String>> {
        restart_note(std::env::var(RESULT_ENV).ok().as_deref(), logo::VERSION)
    }

    /// Clear away what an earlier update left behind, off the UI thread.
    pub fn clean_up(&self) {
        let (exe, work) = (self.exe.clone(), self.work.clone());
        let _ = std::thread::Builder::new().name("peek-update".into()).spawn(move || clean_up(exe, work));
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The newer version on offer.
    pub fn version(&self) -> Option<&str> {
        self.found.as_ref().map(|(v, _)| v.as_str())
    }

    /// The page of the release on offer.
    pub fn page(&self) -> &str {
        self.found.as_ref().and_then(|(_, r)| r.as_ref()).map_or(RELEASES_URL, |r| r.page.as_str())
    }

    /// How this copy of Peek is installed, worked out on first use.
    pub fn kind(&mut self) -> InstallKind {
        *self.kind.get_or_insert_with(|| match &self.exe {
            Some(exe) => classify(&Facts::detect(exe)),
            None => InstallKind::Managed(Managed::Unsupported),
        })
    }

    pub fn busy(&self) -> bool {
        self.check.is_some() || self.job.is_some()
    }

    /// The running update's progress.
    pub fn progress(&self) -> Option<&Progress> {
        self.job.as_ref().map(|j| &*j.progress)
    }

    /// Ask GitHub for the latest release. `manual`: the user asked, so the answer is worth telling.
    pub fn check(&mut self, manual: bool, notify: Notify) {
        if self.busy() || matches!(self.phase, Phase::Ready) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let source = self.source.clone();
        let spawned = std::thread::Builder::new().name("peek-update".into()).spawn(move || {
            let _ = tx.send(latest(&source));
            notify();
        });
        match spawned {
            Ok(_) => {
                self.check = Some((rx, manual));
                self.phase = Phase::Checking;
            }
            Err(e) => self.phase = Phase::CheckFailed(e.to_string()),
        }
    }

    /// Download, verify and stage the update. Call only on the user's click.
    pub fn install(&mut self, notify: Notify) {
        if self.busy() || matches!(self.phase, Phase::Ready) {
            return;
        }
        let kind = self.kind();
        let (Some(exe), Some(work)) = (self.exe.clone(), self.work.clone()) else {
            self.phase = Phase::Failed("Peek cannot tell where it is installed.".into());
            return;
        };
        self.jobs += 1;
        let plan = Plan {
            kind,
            exe,
            work: work.join(format!("{}-{}", std::process::id(), self.jobs)),
            current: logo::VERSION.into(),
            probe: true,
        };
        let known = self.found.as_ref().and_then(|(_, r)| r.clone());
        let progress = Arc::new(Progress::default());
        let (tx, done) = mpsc::channel();
        let (source, p) = (self.source.clone(), progress.clone());
        let spawned = std::thread::Builder::new().name("peek-update".into()).spawn(move || {
            let _ = tx.send(install(&source, &plan, known, &p, &*notify));
            notify();
        });
        match spawned {
            Ok(_) => {
                self.job = Some(Job { progress, done });
                self.phase = Phase::Installing;
            }
            Err(e) => self.phase = Phase::Failed(e.to_string()),
        }
    }

    /// Stop a running update at its next step; nothing is installed.
    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Collect what the background threads have finished. Call every frame.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some((rx, manual)) = &self.check
            && let Some(result) = received(rx)
        {
            let manual = *manual;
            self.check = None;
            match result {
                Ok(release) => {
                    let newer = release.filter(|r| is_newer(&r.version, logo::VERSION));
                    let found = newer.as_ref().map(|r| r.version.clone());
                    self.phase = if newer.is_some() { Phase::Available } else { Phase::UpToDate };
                    self.found = newer.map(|r| (r.version.clone(), Some(r)));
                    events.push(Event::Checked { found, manual });
                }
                Err(error) => {
                    // An update found earlier stays on offer.
                    self.phase =
                        if self.found.is_some() { Phase::Available } else { Phase::CheckFailed(error.clone()) };
                    events.push(Event::CheckFailed { error, manual });
                }
            }
        }
        if let Some(job) = &self.job
            && let Some(result) = received(&job.done)
        {
            let cancelled = job.progress.cancelled();
            self.job = None;
            match result {
                Ok((release, staged)) => {
                    self.found = Some((release.version.clone(), Some(release)));
                    match staged {
                        Staged::DiskImage(path) => {
                            self.phase = Phase::Opened;
                            events.push(Event::Downloaded(path));
                        }
                        staged => {
                            self.staged = Some(staged);
                            self.phase = Phase::Ready;
                            events.push(Event::Ready);
                        }
                    }
                }
                Err(_) if cancelled => self.phase = Phase::Available,
                Err(error) => {
                    self.phase = Phase::Failed(error.clone());
                    events.push(Event::InstallFailed(error));
                }
            }
        }
        events
    }

    /// Start what replaces this Peek: the new executable, or the helper that
    /// runs the installer. On `Ok` the caller closes Peek; on `Err` the old
    /// version is still in place and stays open.
    pub fn relaunch(&mut self) -> Result<(), String> {
        let result = match (&self.staged, &self.exe) {
            (Some(Staged::Swapped(swap)), _) => start_new(swap),
            (Some(Staged::Installer(msi, sha256)), Some(exe)) => start_installer(msi, sha256, exe),
            _ => Err("There is no update to restart into.".into()),
        };
        if let Err(e) = &result {
            self.staged = None;
            self.phase = Phase::Failed(e.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::Mutex;

    // ------------------------------------------------------------- fixtures

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peek-update-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// A zip with one folder entry and the given files, deflated.
    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::{Compression, write::DeflateEncoder};
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in files {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).unwrap();
            let packed = enc.finish().unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let mut fields = Vec::new();
            for v in [crc.sum(), packed.len() as u32, data.len() as u32] {
                fields.extend_from_slice(&v.to_le_bytes());
            }
            fields.extend_from_slice(&(name.len() as u16).to_le_bytes());
            fields.extend_from_slice(&[0, 0]);
            let offset = out.len() as u32;
            // Local header: signature, version, flags, method 8, time, date, then the shared fields.
            out.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x00\x00\x00\x00");
            out.extend_from_slice(&fields);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            central.extend_from_slice(b"PK\x01\x02\x14\x00\x14\x00\x00\x00\x08\x00\x00\x00\x00\x00");
            central.extend_from_slice(&fields);
            central.extend_from_slice(&[0; 10]); // comment length, disk, attributes
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let (start, len, count) = (out.len() as u32, central.len() as u32, files.len() as u16);
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06\x00\x00\x00\x00");
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// A tar.gz with a folder entry first, as `tar -czf` writes it.
    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        let mut tar = Vec::new();
        let mut entry = |name: &str, kind: u8, data: &[u8]| {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..107].copy_from_slice(b"0000755");
            h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            h[156] = kind;
            h[257..263].copy_from_slice(b"ustar\0");
            tar.extend_from_slice(&h);
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        };
        entry("peek-linux-x64/", b'5', b"");
        for (name, data) in files {
            entry(name, b'0', data);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    /// A release as GitHub describes it: `(name, digest, size)` per file.
    fn release_json(base: &str, tag: &str, flags: &str, assets: &[(&str, Option<String>, usize)]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|(name, digest, size)| {
                let digest = digest.as_ref().map_or("null".to_string(), |d| format!("\"sha256:{d}\""));
                format!(
                    r#"{{"name":"{name}","size":{size},"digest":{digest},"browser_download_url":"{base}ulaas/peek/releases/download/{tag}/{name}"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}",{flags}"html_url":"{base}ulaas/peek/releases/tag/{tag}","assets":[{}]}}"#,
            assets.join(",")
        )
    }

    const GITHUB: &str = "https://github.com/";

    /// A local stand-in for GitHub. No test talks to the real network.
    struct Mock {
        source: Source,
        base: String,
        /// The head of every request received.
        requests: Arc<Mutex<Vec<String>>>,
        routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl Mock {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}/", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
            let (seen, served) = (requests.clone(), routes.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
                    seen.lock().unwrap().push(head);
                    let body = served.lock().unwrap().get(&path).cloned();
                    let (status, body) = match body {
                        Some(b) => ("200 OK", b),
                        None => ("404 Not Found", Vec::new()),
                    };
                    let head =
                        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                }
            });
            let source = Source {
                api: format!("{base}repos/ulaas/peek/releases/latest"),
                downloads: format!("{base}ulaas/peek/releases/download/"),
                pages: format!("{base}ulaas/peek/"),
                https_only: false,
            };
            Self { source, base, requests, routes }
        }

        fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
            self.routes.lock().unwrap().insert(path.to_string(), body.into());
        }

        /// Publish release v0.9.0 with one file, a checksum list that says
        /// `listed` about it and GitHub's digest `digest`.
        fn publish(&self, name: &str, body: &[u8], listed: Option<&str>, digest: Option<String>) {
            let mut assets = vec![(name, digest, body.len())];
            if let Some(hash) = listed {
                let sums = format!("{hash}  {name}\n");
                self.serve(&format!("/ulaas/peek/releases/download/v0.9.0/{SUMS}"), sums.clone());
                assets.push((SUMS, Some(sha(sums.as_bytes())), sums.len()));
            }
            self.serve(&format!("/ulaas/peek/releases/download/v0.9.0/{name}"), body);
            self.serve("/repos/ulaas/peek/releases/latest", release_json(&self.base, "v0.9.0", "", &assets));
        }

        fn hits(&self, needle: &str) -> usize {
            self.requests.lock().unwrap().iter().filter(|r| r.contains(needle)).count()
        }
    }

    /// A pretend install in a temp folder: `peek.exe` holding "old".
    fn plan(dir: &Path, kind: InstallKind) -> Plan {
        let exe = dir.join(if kind == InstallKind::LinuxBinary { "peek" } else { "peek.exe" });
        std::fs::write(&exe, b"old").unwrap();
        Plan { kind, exe, work: dir.join("work"), current: "0.5.0".into(), probe: false }
    }

    fn run(mock: &Mock, plan: &Plan) -> Result<(Release, Staged), String> {
        install(&mock.source, plan, None, &Progress::default(), &|| {})
    }

    fn facts(os: Os, exe: &str) -> Facts {
        Facts {
            os,
            exe: PathBuf::from(exe),
            dir_writable: true,
            msi_marker: false,
            program_files: vec![PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")],
            brew_cask: false,
            release_build: true,
            arch_ok: true,
        }
    }

    // ---------------------------------------------------------------- tests

    #[test]
    fn versions_compare_by_number() {
        for (candidate, current) in [
            ("0.6.0", "0.5.0"),
            ("0.10.0", "0.9.3"),
            ("v1.0.0", "0.99.99"),
            ("0.5.1", "0.5"),
            ("0.5.0.1", "0.5.0"),
            ("0.6.0", "0.6.0-beta.2"),
            ("0.6.0", "0.5.0-dirty"),
        ] {
            assert!(is_newer(candidate, current), "{candidate} > {current}");
            assert!(!is_newer(current, candidate), "{current} < {candidate}");
        }
        for (a, b) in [("0.5.0", "0.5.0"), ("0.5", "0.5.0"), ("v0.5.0", "0.5.0"), ("0.6.0-beta.1", "0.6.0-beta.2")] {
            assert!(!is_newer(a, b) && !is_newer(b, a), "{a} = {b}");
        }
        for junk in ["", "latest", "1.x", "1..2", "-1.0", "1.2.3.4.5", "99999999999999999999.0"] {
            assert!(!is_newer(junk, "0.5.0"), "{junk:?}");
            assert!(!is_newer("0.5.0", junk), "{junk:?}");
        }
    }

    #[test]
    fn automatic_check_runs_once_a_day() {
        let now = 1_794_000_000;
        assert!(check_due(true, None, now));
        assert!(!check_due(true, Some(now - 3600), now));
        assert!(!check_due(true, Some(now - CHECK_EVERY + 1), now));
        assert!(check_due(true, Some(now - CHECK_EVERY), now));
        assert!(check_due(true, Some(now + 9_000_000), now), "a clock set back does not stop the check for good");
        assert!(!check_due(false, None, now) && !check_due(false, Some(0), now), "off means never");
    }

    #[test]
    fn release_parsing() {
        let source = Source::github();
        let digest = "AB".repeat(32);
        let zip = ("peek-windows-x64.zip", Some(digest.clone()), 1234);
        let json = release_json(GITHUB, "v0.6.0", "", &[zip.clone(), (SUMS, None, 700)]);
        let r = parse_release(&json, &source).unwrap().unwrap();
        assert_eq!(r.version, "0.6.0");
        assert_eq!(r.page, "https://github.com/ulaas/peek/releases/tag/v0.6.0");
        let a = r.asset("peek-windows-x64.zip").unwrap();
        assert_eq!(a.url, "https://github.com/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip");
        assert_eq!((a.size, a.sha256.clone()), (1234, Some(digest.to_lowercase())));
        assert_eq!(r.asset(SUMS).unwrap().sha256, None);

        // Drafts and pre-releases are never offered, however they are marked.
        for (tag, flags) in [
            ("v0.6.0", r#""draft":true,"#),
            ("v0.6.0", r#""prerelease":true,"#),
            ("v0.6.0-beta.1", ""),
            ("v0.6.0-rc1", r#""draft":false,"prerelease":false,"#),
        ] {
            let json = release_json(GITHUB, tag, flags, std::slice::from_ref(&zip));
            assert_eq!(parse_release(&json, &source), Ok(None), "{tag} {flags}");
        }

        // A file is only kept at the release's own download address.
        let good = "https://github.com/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip";
        for bad in [
            "http://github.com/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip",
            "https://github.com/someone/peek/releases/download/v0.6.0/peek-windows-x64.zip",
            "https://github.com.evil.example/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip",
            "https://evil.example/?https://github.com/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip",
            "https://github.com/ulaas/peek/releases/download/v0.5.0/peek-windows-x64.zip",
            "https://github.com/ulaas/peek/releases/download/v0.6.0/../../../evil/peek-windows-x64.zip",
            "http://127.0.0.1:8080/ulaas/peek/releases/download/v0.6.0/peek-windows-x64.zip",
        ] {
            let r = parse_release(&json.replace(good, bad), &source).unwrap().unwrap();
            assert!(r.asset("peek-windows-x64.zip").is_none(), "{bad}");
            assert!(r.asset(SUMS).is_some());
        }
        // A page elsewhere is replaced by the project's own.
        let elsewhere = json.replace("https://github.com/ulaas/peek/releases/tag/v0.6.0", "https://evil.example/x");
        assert_eq!(parse_release(&elsewhere, &source).unwrap().unwrap().page, RELEASES_URL);

        // Digests Peek cannot compare count as absent; nonsense is an error, not a release.
        let odd = json.replace(&format!("sha256:{digest}"), "sha512:abcdef");
        assert_eq!(parse_release(&odd, &source).unwrap().unwrap().asset("peek-windows-x64.zip").unwrap().sha256, None);
        assert!(parse_release("{\"message\":\"Not Found\"}", &source).is_err());
        assert!(parse_release(&release_json(GITHUB, "nightly", "", &[]), &source).is_err());
        assert!(parse_release("<html>", &source).is_err());
    }

    #[test]
    fn install_kinds() {
        use InstallKind::*;
        let kind = |os, exe: &str, change: &dyn Fn(&mut Facts)| {
            let mut f = facts(os, exe);
            change(&mut f);
            classify(&f)
        };
        let same = |_: &mut Facts| {};
        let read_only = |f: &mut Facts| f.dir_writable = false;
        let msi = |f: &mut Facts| (f.msi_marker, f.dir_writable) = (true, false);

        // Windows
        assert_eq!(kind(Os::Windows, r"D:\Tools\peek-windows-x64\peek.exe", &same), WindowsPortable);
        assert_eq!(kind(Os::Windows, r"D:\Tools\peek\peek.exe", &|f| f.msi_marker = true), WindowsPortable);
        assert_eq!(kind(Os::Windows, r"D:\Tools\peek\peek.exe", &read_only), Managed(super::Managed::ReadOnly));
        assert_eq!(kind(Os::Windows, r"C:\Program Files\Peek\peek.exe", &msi), WindowsMsi);
        assert_eq!(kind(Os::Windows, r"\\?\c:\PROGRAM FILES\Peek\peek.exe", &msi), WindowsMsi);
        assert_eq!(kind(Os::Windows, r"C:\Program Files\Peek\peek.exe", &same), Managed(super::Managed::ReadOnly));
        assert_eq!(
            kind(Os::Windows, r"C:\Program Files (x86)\Peek\peek.exe", &same),
            Managed(super::Managed::ReadOnly)
        );
        for scoop in
            [r"C:\Users\ada\scoop\apps\peek\current\peek.exe", r"C:\ProgramData\Scoop\Apps\peek\0.5.0\peek.exe"]
        {
            assert_eq!(kind(Os::Windows, scoop, &same), Managed(super::Managed::Scoop));
            assert_eq!(kind(Os::Windows, scoop, &|f| f.msi_marker = true), Managed(super::Managed::Scoop));
        }

        // Linux
        assert_eq!(kind(Os::Linux, "/home/ada/.local/bin/peek", &same), LinuxBinary);
        assert_eq!(kind(Os::Linux, "/home/ada/apps/peek-linux-x64/peek", &same), LinuxBinary);
        assert_eq!(kind(Os::Linux, "/opt/peek/peek", &read_only), Managed(super::Managed::ReadOnly));
        assert_eq!(kind(Os::Linux, "/usr/local/bin/peek", &read_only), Managed(super::Managed::ReadOnly));
        for packaged in ["/usr/bin/peek", "/bin/peek", "/usr/lib/peek/peek", "/nix/store/abc-peek/bin/peek"] {
            // Even root, who could write there, leaves a package's files to the package manager.
            assert_eq!(kind(Os::Linux, packaged, &same), Managed(super::Managed::SystemPackage), "{packaged}");
            assert_eq!(kind(Os::Linux, packaged, &read_only), Managed(super::Managed::SystemPackage));
        }
        assert_eq!(
            kind(Os::Linux, "/home/linuxbrew/.linuxbrew/Cellar/peek/0.5.0/bin/peek", &same),
            Managed(super::Managed::Homebrew)
        );

        // macOS
        let app = "/Applications/Peek.app/Contents/MacOS/peek";
        assert_eq!(kind(Os::Mac, app, &same), MacApp);
        assert_eq!(kind(Os::Mac, app, &read_only), MacApp, "the disk image is only opened, nothing is written");
        assert_eq!(kind(Os::Mac, app, &|f| f.brew_cask = true), Managed(super::Managed::Homebrew));
        assert_eq!(
            kind(Os::Mac, "/opt/homebrew/Caskroom/peek/0.5.0/Peek.app/Contents/MacOS/peek", &same),
            Managed(super::Managed::Homebrew)
        );
        assert_eq!(
            kind(Os::Mac, "/Users/ada/src/peek/target/release/peek", &same),
            Managed(super::Managed::Unsupported)
        );

        // Everywhere
        for (os, exe) in [(Os::Windows, r"D:\src\peek\target\debug\peek.exe"), (Os::Linux, "/home/ada/.local/bin/peek")]
        {
            assert_eq!(kind(os, exe, &|f| f.release_build = false), Managed(super::Managed::DevBuild));
            assert_eq!(kind(os, exe, &|f| f.arch_ok = false), Managed(super::Managed::Unsupported));
        }
    }

    #[test]
    fn asset_per_install_kind() {
        use InstallKind::*;
        assert_eq!(WindowsPortable.asset(), Some("peek-windows-x64.zip"));
        assert_eq!(WindowsMsi.asset(), Some("peek-windows-x64.msi"));
        assert_eq!(LinuxBinary.asset(), Some("peek-linux-x64.tar.gz"));
        assert_eq!(MacApp.asset(), Some("peek-macos-arm64.dmg"));
        for kind in [WindowsPortable, WindowsMsi, LinuxBinary] {
            assert_eq!(kind.action(), Some("Update and restart"));
        }
        assert_eq!(MacApp.action(), Some("Download update"));
        use super::Managed as M;
        for m in [M::Scoop, M::Homebrew, M::SystemPackage, M::ReadOnly, M::DevBuild, M::Unsupported] {
            assert_eq!((Managed(m).asset(), Managed(m).action()), (None, None), "{m:?}");
            assert!(!Managed(m).advice().0.is_empty());
        }
        assert_eq!(Managed(M::Scoop).advice().1, Some("scoop update peek"));
        assert_eq!(Managed(M::Homebrew).advice().1, Some("brew upgrade --cask peek"));
    }

    #[test]
    fn checksum_sources_must_agree() {
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let asset = |sha256: Option<&String>| Asset {
            name: "peek-linux-x64.tar.gz".into(),
            url: String::new(),
            size: 1,
            sha256: sha256.cloned(),
        };
        let sums = format!(
            "{b}  peek-windows-x64.zip\n{}  peek-linux-x64.tar.gz\n{b} *peek-macos-arm64.dmg\n",
            a.to_uppercase()
        );
        assert_eq!(sums_entry(&sums, "peek-linux-x64.tar.gz"), Some(a.clone()));
        assert_eq!(sums_entry(&sums, "peek-macos-arm64.dmg"), Some(b.clone()));
        assert_eq!(sums_entry(&sums, "x64.tar.gz"), None);
        assert_eq!(sums_entry("abc  peek-linux-x64.tar.gz", "peek-linux-x64.tar.gz"), None, "not a SHA-256");

        assert_eq!(expected_sha256(&asset(Some(&a)), Some(&sums)), Ok(a.clone()));
        assert_eq!(expected_sha256(&asset(None), Some(&sums)), Ok(a.clone()));
        assert_eq!(expected_sha256(&asset(Some(&a)), None), Ok(a.clone()));
        assert!(expected_sha256(&asset(Some(&b)), Some(&sums)).unwrap_err().contains("disagree"));
        assert!(expected_sha256(&asset(None), None).unwrap_err().contains("no checksum"));
        // A checksum list that leaves the file out is not made up for by the digest.
        assert!(expected_sha256(&asset(Some(&a)), Some("")).unwrap_err().contains("does not list"));
    }

    #[test]
    fn archives() {
        let exe: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let z = zip(&[("peek-windows-x64/README.md", b"read me"), ("peek-windows-x64/peek.exe", &exe)]);
        assert_eq!(unzip_one(&z, "peek-windows-x64/peek.exe").unwrap(), exe);
        assert_eq!(unzip_one(&z, "peek-windows-x64/README.md").unwrap(), b"read me");
        assert!(unzip_one(&z, "peek.exe").unwrap_err().contains("has no"));
        assert!(unzip_one(b"not a zip", "peek.exe").is_err());
        assert!(unzip_one(&z[..z.len() - 30], "peek-windows-x64/peek.exe").is_err());
        // A flipped byte in the data fails the CRC (or the inflate), never yields a file.
        let mut damaged = z.clone();
        let at = z.windows(4).rposition(|w| w == b"PK\x03\x04").unwrap() + 30 + "peek-windows-x64/peek.exe".len() + 3;
        damaged[at] ^= 0xFF;
        assert!(unzip_one(&damaged, "peek-windows-x64/peek.exe").is_err());

        let t = tar_gz(&[("peek-linux-x64/install.sh", b"#!/bin/sh\n"), ("peek-linux-x64/peek", &exe)]);
        assert_eq!(untar_one(&t, "peek-linux-x64/peek").unwrap(), exe);
        assert_eq!(untar_one(&t, "peek-linux-x64/install.sh").unwrap(), b"#!/bin/sh\n");
        assert!(untar_one(&t, "peek-linux-x64/").unwrap_err().contains("has no"), "a folder is not a file");
        assert!(untar_one(&t, "peek").unwrap_err().contains("has no"));
        assert!(untar_one(b"not gzip", "peek").is_err());
        assert!(untar_one(&t[..t.len() / 2], "peek-linux-x64/peek").is_err());
    }

    #[test]
    fn swap_and_rollback() {
        for aside in [true, false] {
            let dir = temp(if aside { "swap-aside" } else { "swap-over" });
            let target = dir.join("peek.exe");
            let (old, new) = (old_path(&target), new_path(&target));
            assert_eq!(old.file_name().unwrap(), "peek.exe.old");
            assert_eq!(new.file_name().unwrap(), "peek.new.exe");
            assert_eq!(new_path(&dir.join("peek")).file_name().unwrap(), "peek.new");

            // The swap: new in place, old set aside, nothing else left.
            std::fs::write(&target, b"old").unwrap();
            std::fs::write(&new, b"new").unwrap();
            std::fs::write(&old, b"left by an earlier update").unwrap();
            let swap = swap_in(&target, &new, aside).unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"new");
            assert_eq!(std::fs::read(&old).unwrap(), b"old");
            assert!(!new.exists());

            // Undone when the new version does not start.
            swap.rollback().unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert!(!old.exists());

            // A swap that cannot finish leaves the old executable where it was.
            let err = swap_in(&target, &new, aside).unwrap_err();
            assert!(err.contains("cannot replace"), "{err}");
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert!(!old.exists() && !new.exists());

            // Nothing to replace: the new file does not stay behind.
            std::fs::remove_file(&target).unwrap();
            std::fs::write(&new, b"new").unwrap();
            assert!(swap_in(&target, &new, aside).is_err());
            assert!(!target.exists() && !old.exists() && !new.exists());

            // What the next start clears away.
            std::fs::write(&target, b"new").unwrap();
            std::fs::write(&old, b"old").unwrap();
            std::fs::write(&new, b"stray").unwrap();
            std::fs::create_dir_all(dir.join("work/1-1")).unwrap();
            clean_up(Some(target.clone()), Some(dir.join("work")));
            assert!(target.exists() && !old.exists() && !new.exists() && !dir.join("work").exists());
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn check_finds_the_latest_release_and_sends_only_a_user_agent() {
        let mock = Mock::start();
        mock.publish("peek-windows-x64.zip", b"zip", None, Some(sha(b"zip")));
        let r = latest(&mock.source).unwrap().unwrap();
        assert_eq!(r.version, "0.9.0");
        assert!(is_newer(&r.version, "0.5.0"));
        assert_eq!(r.asset("peek-windows-x64.zip").unwrap().sha256, Some(sha(b"zip")));

        let requests = mock.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "a check is one request and downloads nothing");
        let head = requests[0].to_lowercase();
        assert!(head.starts_with("get /repos/ulaas/peek/releases/latest "), "{head}");
        assert!(head.contains(&format!("user-agent: peek/{}\r\n", logo::VERSION)), "{head}");
        let names: Vec<&str> = head.lines().skip(1).filter_map(|l| l.split_once(':')).map(|(name, _)| name).collect();
        for name in &names {
            assert!(["host", "user-agent", "accept"].contains(name), "unexpected header {name}: {head}");
        }

        // No release at all, or an answer that is not one, is an error to report, not an update.
        let empty = Mock::start();
        assert!(latest(&empty.source).unwrap_err().contains("404"));
        empty.serve("/repos/ulaas/peek/releases/latest", "busy");
        assert!(latest(&empty.source).is_err());
    }

    #[test]
    fn https_only_outside_tests() {
        let source = Source::github();
        assert!(source.https_only);
        for url in [&source.api, &source.downloads, &source.pages] {
            assert!(url.starts_with("https://"), "{url}");
        }
        // A plain-http or foreign address is refused before anything is sent.
        let dir = temp("https");
        for url in ["http://127.0.0.1:9/ulaas/peek/releases/download/v0.9.0/x.zip", "https://example.com/x.zip"] {
            let asset = Asset { name: "x.zip".into(), url: url.into(), size: 3, sha256: None };
            let agent = agent(&source, Duration::from_secs(5));
            let err = download(&agent, &source, &asset, &dir.join("x.zip"), &Progress::default(), &|| {}).unwrap_err();
            assert!(err.contains("not a Peek download address"), "{err}");
            assert!(fetch_sums(&agent, &source, &asset).unwrap_err().contains("not a Peek download address"));
        }
        // And the client itself will not speak plain http, whatever it is asked.
        let err = get_text(&agent(&source, Duration::from_secs(5)), "http://127.0.0.1:9/").unwrap_err();
        assert!(err.to_lowercase().contains("https"), "{err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn portable_update_end_to_end() {
        // Windows: peek.exe out of the zip, the old one set aside.
        let mock = Mock::start();
        let archive = zip(&[("peek-windows-x64/LICENSE-MIT", b"MIT"), ("peek-windows-x64/peek.exe", b"new peek")]);
        mock.publish("peek-windows-x64.zip", &archive, Some(&sha(&archive)), Some(sha(&archive)));
        let dir = temp("e2e-windows");
        let plan_win = plan(&dir, InstallKind::WindowsPortable);
        let progress = Progress::default();
        let (release, staged) = install(&mock.source, &plan_win, None, &progress, &|| {}).unwrap();
        assert_eq!(release.version, "0.9.0");
        assert!(matches!(staged, Staged::Swapped(_)), "{staged:?}");
        assert_eq!(std::fs::read(&plan_win.exe).unwrap(), b"new peek");
        assert_eq!(std::fs::read(old_path(&plan_win.exe)).unwrap(), b"old");
        assert!(!new_path(&plan_win.exe).exists() && !plan_win.work.exists(), "nothing else is left behind");
        assert_eq!(progress.stage(), Stage::Installing);
        assert_eq!(progress.bytes(), (archive.len() as u64, archive.len() as u64));
        assert_eq!(mock.hits("GET "), 3, "the release, the checksum list and the one file");
        std::fs::remove_dir_all(dir).ok();

        // Linux: peek out of the tarball, renamed over the old one. The
        // checksum list alone is enough when GitHub reports no digest.
        let mock = Mock::start();
        let archive = tar_gz(&[("peek-linux-x64/install.sh", b"#!/bin/sh\n"), ("peek-linux-x64/peek", b"new peek")]);
        mock.publish("peek-linux-x64.tar.gz", &archive, Some(&sha(&archive)), None);
        let dir = temp("e2e-linux");
        let plan_linux = plan(&dir, InstallKind::LinuxBinary);
        // A release the check already fetched is used as it is.
        let known = latest(&mock.source).unwrap();
        let (_, staged) = install(&mock.source, &plan_linux, known, &Progress::default(), &|| {}).unwrap();
        assert_eq!(std::fs::read(&plan_linux.exe).unwrap(), b"new peek");
        let Staged::Swapped(swap) = staged else { panic!("not swapped") };
        swap.rollback().unwrap();
        assert_eq!(std::fs::read(&plan_linux.exe).unwrap(), b"old");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn installer_and_disk_image_are_verified_and_kept() {
        for (kind, name) in
            [(InstallKind::WindowsMsi, "peek-windows-x64.msi"), (InstallKind::MacApp, "peek-macos-arm64.dmg")]
        {
            let mock = Mock::start();
            mock.publish(name, b"installer bytes", Some(&sha(b"installer bytes")), Some(sha(b"installer bytes")));
            let dir = temp(name);
            let plan = plan(&dir, kind);
            let (_, staged) = run(&mock, &plan).unwrap();
            let (Staged::Installer(path, _) | Staged::DiskImage(path)) = &staged else { panic!("{staged:?}") };
            assert_eq!(matches!(staged, Staged::Installer(..)), kind == InstallKind::WindowsMsi);
            if let Staged::Installer(_, sha256) = &staged {
                // The helper runs the installer only while it still has this hash.
                assert!(open_verified(path, sha256).is_ok());
                assert!(open_verified(path, &sha256.to_uppercase()).is_ok());
                std::fs::write(path, b"something else").unwrap();
                assert!(open_verified(path, sha256).unwrap_err().contains("changed"));
                std::fs::write(path, b"installer bytes").unwrap();
            }
            assert_eq!(*path, plan.work.join(name));
            assert_eq!(std::fs::read(path).unwrap(), b"installer bytes");
            assert_eq!(std::fs::read(&plan.exe).unwrap(), b"old", "the installer is not run here");

            // The same download with a wrong checksum is deleted, not kept.
            mock.publish(name, b"installer bytes", Some(&sha(b"something else")), None);
            let dir2 = temp(&format!("{name}-bad"));
            let plan = self::plan(&dir2, kind);
            assert!(run(&mock, &plan).unwrap_err().contains("does not match the checksum"));
            assert!(!plan.work.exists());
            std::fs::remove_dir_all(dir).ok();
            std::fs::remove_dir_all(dir2).ok();
        }
    }

    #[test]
    fn bad_downloads_are_refused() {
        let archive = zip(&[("peek-windows-x64/peek.exe", b"new peek")]);
        let good = sha(&archive);
        let other = sha(b"something else");
        let name = "peek-windows-x64.zip";
        let untouched = |plan: &Plan, why: &str| {
            assert_eq!(std::fs::read(&plan.exe).unwrap(), b"old", "{why}");
            assert!(!old_path(&plan.exe).exists() && !new_path(&plan.exe).exists(), "{why}");
            assert!(!plan.work.exists(), "{why}: the download is deleted");
        };
        let dir = temp("refused");
        let plan = plan(&dir, InstallKind::WindowsPortable);

        // The file is not what the checksum list says.
        let mock = Mock::start();
        mock.publish(name, &archive, Some(&other), None);
        let err = run(&mock, &plan).unwrap_err();
        assert!(err.contains("does not match the checksum"), "{err}");
        untouched(&plan, "checksum list mismatch");

        // The file is not what GitHub's digest says.
        let mock = Mock::start();
        mock.publish(name, &archive, None, Some(other.clone()));
        assert!(run(&mock, &plan).unwrap_err().contains("does not match the checksum"));
        untouched(&plan, "digest mismatch");

        // The two sources disagree: not even downloaded.
        let mock = Mock::start();
        mock.publish(name, &archive, Some(&good), Some(other.clone()));
        assert!(run(&mock, &plan).unwrap_err().contains("disagree"));
        assert_eq!(mock.hits(&format!("/{name} ")), 0);
        untouched(&plan, "sources disagree");

        // No checksum anywhere: not downloaded.
        let mock = Mock::start();
        mock.publish(name, &archive, None, None);
        assert!(run(&mock, &plan).unwrap_err().contains("no checksum"));
        assert_eq!(mock.hits(&format!("/{name} ")), 0);
        untouched(&plan, "no checksum");

        // The checksum list itself was tampered with.
        let mock = Mock::start();
        mock.publish(name, &archive, Some(&good), Some(good.clone()));
        mock.serve(&format!("/ulaas/peek/releases/download/v0.9.0/{SUMS}"), format!("{other}  {name}\n"));
        assert!(run(&mock, &plan).unwrap_err().contains(SUMS));
        assert_eq!(mock.hits(&format!("/{name} ")), 0);
        untouched(&plan, "tampered checksum list");

        // More bytes than the release announced, or fewer.
        for body in [[archive.clone(), b"extra".to_vec()].concat(), archive[..archive.len() - 1].to_vec()] {
            let mock = Mock::start();
            mock.publish(name, &archive, Some(&good), Some(good.clone()));
            mock.serve(&format!("/ulaas/peek/releases/download/v0.9.0/{name}"), body);
            let err = run(&mock, &plan).unwrap_err();
            assert!(err.contains("larger than the release says") || err.contains("incomplete"), "{err}");
            untouched(&plan, "wrong size");
        }

        // Larger than any update can be: refused on the announced size alone.
        let mock = Mock::start();
        let assets = [(name, Some(good.clone()), (MAX_ASSET + 1) as usize)];
        mock.serve("/repos/ulaas/peek/releases/latest", release_json(&mock.base, "v0.9.0", "", &assets));
        assert!(run(&mock, &plan).unwrap_err().contains("more than an update can be"));
        assert_eq!(mock.hits(&format!("/{name} ")), 0);
        untouched(&plan, "oversized");

        // A verified archive without the executable in it.
        let mock = Mock::start();
        let empty = zip(&[("peek-windows-x64/README.md", b"read me")]);
        mock.publish(name, &empty, Some(&sha(&empty)), None);
        assert!(run(&mock, &plan).unwrap_err().contains("has no peek-windows-x64/peek.exe"));
        untouched(&plan, "no executable");

        // The release has no file for this kind of install, or is not newer.
        let mock = Mock::start();
        mock.publish("peek-linux-x64.tar.gz", b"x", Some(&sha(b"x")), None);
        assert!(run(&mock, &plan).unwrap_err().contains("has no peek-windows-x64.zip"));
        let current = Plan { current: "0.9.0".into(), ..self::plan(&dir, InstallKind::WindowsPortable) };
        assert!(run(&mock, &current).unwrap_err().contains("latest version"));
        untouched(&plan, "nothing to install");

        // Cancelled: nothing is installed.
        let mock = Mock::start();
        mock.publish(name, &archive, Some(&good), Some(good.clone()));
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert_eq!(install(&mock.source, &plan, None, &progress, &|| {}).unwrap_err(), CANCELLED);
        untouched(&plan, "cancelled");

        // A new executable that does not run as the expected version is not swapped in.
        let mock = Mock::start();
        mock.publish(name, &archive, Some(&good), Some(good.clone()));
        let probing = Plan { probe: true, ..self::plan(&dir, InstallKind::WindowsPortable) };
        let err = run(&mock, &probing).unwrap_err();
        assert!(err.contains("does not start") || err.contains("does not run as Peek"), "{err}");
        untouched(&probing, "probe failed");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn package_managed_installs_refuse() {
        use super::Managed as M;
        let mock = Mock::start();
        let archive = zip(&[("peek-windows-x64/peek.exe", b"new peek")]);
        mock.publish("peek-windows-x64.zip", &archive, Some(&sha(&archive)), Some(sha(&archive)));
        let dir = temp("managed");
        for m in [M::Scoop, M::Homebrew, M::SystemPackage, M::ReadOnly, M::DevBuild, M::Unsupported] {
            let plan = plan(&dir, InstallKind::Managed(m));
            let err = run(&mock, &plan).unwrap_err();
            assert_eq!(err, InstallKind::Managed(m).advice().0);
            assert_eq!(std::fs::read(&plan.exe).unwrap(), b"old");
            assert!(!plan.work.exists());
        }
        assert_eq!(mock.hits("GET "), 0, "a copy something else updates asks for nothing");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn restart_notes() {
        assert_eq!(restart_note(None, "0.6.0"), None);
        assert_eq!(restart_note(Some("ok:0.5.0"), "0.6.0"), Some(Ok("Peek was updated to 0.6.0".into())));
        assert!(restart_note(Some("ok:0.5.0"), "0.5.0").unwrap().unwrap_err().contains("not installed"));
        assert_eq!(
            restart_note(Some("failed:The update was cancelled."), "0.5.0"),
            Some(Err("The update was cancelled.".into()))
        );
        assert_eq!(restart_note(Some("whatever"), "0.5.0"), None);
    }

    #[test]
    fn updater_offers_a_version_seen_earlier() {
        let fresh = Updater::new(None);
        assert_eq!((fresh.phase(), fresh.version(), fresh.page()), (&Phase::Idle, None, RELEASES_URL));
        let seen = Updater::new(Some("999.0.0"));
        assert_eq!((seen.phase(), seen.version()), (&Phase::Available, Some("999.0.0")));
        assert!(!seen.busy() && seen.progress().is_none());
        // Once installed, the remembered version is no longer news.
        assert_eq!(Updater::new(Some(logo::VERSION)).phase(), &Phase::Idle);
        assert_eq!(Updater::new(Some("0.0.1")).version(), None);
    }
}
