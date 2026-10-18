//! Clipboard over the CLIPRDR channel: text both ways, and files both ways.
//!
//! IronRDP calls the backend from inside its channel processing, where it
//! cannot send anything itself, so the backend only queues work. The
//! session loop drains the queue after every step and performs it on the
//! channel. Clipboard text from the server is handed to the UI, which owns
//! the OS clipboard; the same path serves VNC, so behaviour is identical.
//!
//! Files copied locally are announced to the server, which then reads them
//! piece by piece. Files copied on the server are downloaded right away into
//! a temporary folder and handed to the UI as a file list for the local
//! clipboard, so a later paste in Explorer works like any local copy.

use ironrdp::cliprdr::CliprdrClient;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp::cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName, ClipboardGeneralCapabilityFlags,
    FileContentsFlags, FileContentsRequest, FileContentsResponse, FileDescriptor, FormatDataRequest,
    FormatDataResponse, LockDataId,
};
use ironrdp::core::{IntoOwned as _, impl_as_any};
use ironrdp::session::{ActiveStage, SessionResult};
use peek_core::SessionEvent;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Bytes asked for per file contents request when downloading.
const CHUNK: u32 = 1 << 20;
/// Remote copies above this size are not downloaded automatically.
const MAX_DOWNLOAD: u64 = 2 << 30;
/// Most entries offered from one local copy (folders are walked).
const MAX_FILES: usize = 10_000;
/// Longest name the file descriptor can carry (260 UTF-16 units with the terminator).
const MAX_NAME: usize = 259;

pub enum ClipWork {
    Channel(ClipboardMessage),
    Event(SessionEvent),
}

/// What Peek last offered to the server, read when the server pastes.
#[derive(Debug, Default)]
struct Local {
    text: Option<String>,
    /// Paths in the order of the announced file list; the index is the `lindex`.
    files: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct RdpClipboard {
    queue: mpsc::Sender<ClipWork>,
    local: Arc<Mutex<Local>>,
    requested: Option<ClipboardFormatId>,
    files_enabled: bool,
    download: Option<Download>,
    /// Folder of the last finished download, removed when a new one starts.
    last_folder: Option<PathBuf>,
    next_stream: u32,
}

/// A remote file list being fetched, one file and one request at a time.
#[derive(Debug)]
struct Download {
    folder: PathBuf,
    items: Vec<Item>,
    /// Top-level entries, which go on the local clipboard.
    roots: Vec<PathBuf>,
    index: usize,
    file: Option<File>,
    offset: u64,
    stream: u32,
}

#[derive(Debug)]
struct Item {
    lindex: i32,
    path: PathBuf,
    dir: bool,
    size: Option<u64>,
}

impl std::fmt::Debug for ClipWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipWork::Channel(m) => write!(f, "Channel({m:?})"),
            ClipWork::Event(e) => write!(f, "Event({e:?})"),
        }
    }
}

/// Handle kept by the session loop.
pub struct ClipboardLink {
    pub work: mpsc::Receiver<ClipWork>,
    local: Arc<Mutex<Local>>,
}

pub fn new() -> (RdpClipboard, ClipboardLink) {
    let (tx, rx) = mpsc::channel();
    let local = Arc::new(Mutex::new(Local::default()));
    (
        RdpClipboard {
            queue: tx,
            local: local.clone(),
            requested: None,
            files_enabled: false,
            download: None,
            last_folder: None,
            next_stream: 1,
        },
        ClipboardLink { work: rx, local },
    )
}

fn text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
}

impl_as_any!(RdpClipboard);

impl RdpClipboard {
    fn push(&self, m: ClipboardMessage) {
        let _ = self.queue.send(ClipWork::Channel(m));
    }

    fn notice(&self, text: impl Into<String>) {
        let _ = self.queue.send(ClipWork::Event(SessionEvent::Notice(text.into())));
    }

    fn stream_id(&mut self) -> u32 {
        self.next_stream = self.next_stream.wrapping_add(1).max(1);
        self.next_stream
    }

    fn abort_download(&mut self, why: Option<&str>) {
        if let Some(d) = self.download.take() {
            drop(d.file);
            let _ = std::fs::remove_dir_all(&d.folder);
            if let Some(why) = why {
                tracing::warn!("clipboard: file download stopped: {why}");
                self.notice(format!("Could not copy files from the remote clipboard: {why}"));
            }
        }
    }

    /// Request the next piece of the download, moving past folders and
    /// finished files. Hands the files to the UI once everything arrived.
    fn advance(&mut self) {
        let stream = self.stream_id();
        let Some(d) = self.download.as_mut() else { return };
        let result = loop {
            let Some(item) = d.items.get(d.index) else { break Ok(None) };
            if item.dir {
                if let Err(e) = std::fs::create_dir_all(&item.path) {
                    break Err(e);
                }
                d.index += 1;
                continue;
            }
            if d.file.is_none() {
                let created = item.path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| File::create(&item.path));
                match created {
                    Ok(f) => d.file = Some(f),
                    Err(e) => break Err(e),
                }
                d.offset = 0;
            }
            let request = match item.size {
                Some(size) if d.offset >= size => {
                    d.file = None;
                    d.index += 1;
                    continue;
                }
                Some(size) => FileContentsRequest {
                    stream_id: stream,
                    index: item.lindex,
                    flags: FileContentsFlags::RANGE,
                    position: d.offset,
                    requested_size: (size - d.offset).min(u64::from(CHUNK)) as u32,
                    data_id: None,
                },
                None => FileContentsRequest {
                    stream_id: stream,
                    index: item.lindex,
                    flags: FileContentsFlags::SIZE,
                    position: 0,
                    requested_size: 8,
                    data_id: None,
                },
            };
            d.stream = stream;
            break Ok(Some(request));
        };
        match result {
            Ok(Some(request)) => self.push(ClipboardMessage::SendFileContentsRequest(request)),
            Ok(None) => {
                let d = self.download.take().expect("download in progress");
                tracing::info!("clipboard: {} remote item(s) copied to {}", d.items.len(), d.folder.display());
                self.last_folder = Some(d.folder);
                let _ = self.queue.send(ClipWork::Event(SessionEvent::ClipboardFiles(d.roots)));
            }
            Err(e) => self.abort_download(Some(&e.to_string())),
        }
    }
}

impl CliprdrBackend for RdpClipboard {
    fn temporary_directory(&self) -> &str {
        "."
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        // Files can only be announced once the channel is ready; they are
        // offered again when Peek regains focus or before a paste.
        let has_text = self.local.lock().map(|l| l.text.is_some()).unwrap_or(false);
        self.push(ClipboardMessage::SendInitiateCopy(if has_text { text_formats() } else { Vec::new() }));
    }

    fn on_process_negotiated_capabilities(&mut self, flags: ClipboardGeneralCapabilityFlags) {
        self.files_enabled = flags.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
        tracing::debug!("clipboard: negotiated {flags:?}");
    }

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        // New remote content replaces whatever was being fetched.
        self.abort_download(None);
        self.requested = None;
        let files = formats
            .iter()
            .find(|f| f.name().is_some_and(|n| n.value() == ClipboardFormatName::FILE_LIST.value()));
        if let Some(f) = files.filter(|_| self.files_enabled) {
            // The file list arrives in on_remote_file_list.
            self.push(ClipboardMessage::SendInitiatePaste(f.id()));
            return;
        }
        let pick = [ClipboardFormatId::CF_UNICODETEXT, ClipboardFormatId::CF_TEXT]
            .into_iter()
            .find(|want| formats.iter().any(|f| f.id() == *want));
        if let Some(id) = pick {
            self.requested = Some(id);
            self.push(ClipboardMessage::SendInitiatePaste(id));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let text = self.local.lock().ok().and_then(|l| l.text.clone());
        let response = match (request.format, text) {
            (ClipboardFormatId::CF_UNICODETEXT, Some(t)) => FormatDataResponse::new_unicode_string(&t),
            (ClipboardFormatId::CF_TEXT, Some(t)) => FormatDataResponse::new_string(&t),
            _ => FormatDataResponse::new_error(),
        };
        self.push(ClipboardMessage::SendFormatData(response.into_owned()));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        let text = match self.requested.take() {
            Some(ClipboardFormatId::CF_TEXT) => response.to_string(),
            Some(ClipboardFormatId::CF_UNICODETEXT) => response.to_unicode_string(),
            _ => return,
        };
        if let Ok(t) = text {
            let t = t.trim_end_matches('\0').replace("\r\n", "\n");
            let _ = self.queue.send(ClipWork::Event(SessionEvent::ClipboardText(t)));
        }
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], _: Option<u32>) {
        self.abort_download(None);
        let total: u64 = files.iter().filter_map(|f| f.file_size).sum();
        if total > MAX_DOWNLOAD {
            self.notice(format!(
                "The remote clipboard holds {} of files, more than Peek copies automatically ({}).",
                human_size(total),
                human_size(MAX_DOWNLOAD)
            ));
            return;
        }
        if let Some(old) = self.last_folder.take() {
            let _ = std::fs::remove_dir_all(old);
        }
        let folder = download_folder(self.next_stream);
        let mut items = Vec::new();
        let mut roots = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let Some(rel) = safe_relative(f) else {
                tracing::warn!("clipboard: skipping remote file with unsafe name {:?}", f.name);
                continue;
            };
            let path = folder.join(&rel);
            if rel.components().count() == 1 {
                roots.push(path.clone());
            }
            let dir = f.attributes.is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY));
            items.push(Item { lindex: i as i32, path, dir, size: f.file_size });
        }
        if roots.is_empty() {
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.notice(format!("Could not copy files from the remote clipboard: {e}"));
            return;
        }
        tracing::info!("clipboard: copying {} remote item(s), {}", items.len(), human_size(total));
        self.download = Some(Download { folder, items, roots, index: 0, file: None, offset: 0, stream: 0 });
        self.advance();
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let path = self
            .local
            .lock()
            .ok()
            .and_then(|l| usize::try_from(request.index).ok().and_then(|i| l.files.get(i).cloned()));
        let response = match path.map(|p| read_contents(&p, &request)) {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                tracing::warn!("clipboard: serving file {} failed: {e}", request.index);
                FileContentsResponse::new_error(request.stream_id)
            }
            None => FileContentsResponse::new_error(request.stream_id),
        };
        self.push(ClipboardMessage::SendFileContentsResponse(response));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        let Some(d) = self.download.as_mut() else { return };
        if response.stream_id() != d.stream {
            return;
        }
        if response.is_error() {
            return self.abort_download(Some("the remote side refused to send a file"));
        }
        let item = &mut d.items[d.index];
        if item.size.is_none() {
            match response.data_as_size() {
                Ok(size) => item.size = Some(size),
                Err(_) => return self.abort_download(Some("bad file size from the remote side")),
            }
            return self.advance();
        }
        let size = item.size.unwrap_or(0);
        let data = response.data();
        // Never write past the size the remote announced.
        let take = data.len().min(usize::try_from(size - d.offset).unwrap_or(usize::MAX));
        if let Some(file) = d.file.as_mut()
            && let Err(e) = file.write_all(&data[..take])
        {
            return self.abort_download(Some(&e.to_string()));
        }
        d.offset += take as u64;
        if take == 0 {
            // The remote has nothing more for this file: keep what arrived.
            item.size = Some(d.offset);
        }
        self.advance();
    }

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

/// Answer a file contents request for a locally offered file.
fn read_contents(path: &Path, request: &FileContentsRequest) -> std::io::Result<FileContentsResponse<'static>> {
    if request.flags.contains(FileContentsFlags::SIZE) {
        return Ok(FileContentsResponse::new_size_response(request.stream_id, std::fs::metadata(path)?.len()));
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(request.position))?;
    let mut buf = Vec::with_capacity(request.requested_size.min(CHUNK * 8) as usize);
    file.take(u64::from(request.requested_size)).read_to_end(&mut buf)?;
    Ok(FileContentsResponse::new_data_response(request.stream_id, buf))
}

/// Where a remote file list is downloaded to; unique per process and download.
fn download_folder(n: u32) -> PathBuf {
    std::env::temp_dir().join("peek-clipboard").join(format!("{}-{n}", std::process::id()))
}

/// Relative path of a remote entry, or None if any part of it could escape
/// the download folder or name a device.
fn safe_relative(f: &FileDescriptor) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    let parts = f.relative_path.iter().flat_map(|p| p.split('\\')).chain(std::iter::once(f.name.as_str()));
    for part in parts {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(['/', ':', '\0'])
            || ironrdp::cliprdr::is_windows_device_name(part);
        if bad {
            return None;
        }
        rel.push(part);
    }
    Some(rel)
}

/// File descriptors for local paths, with folders walked so their contents
/// follow them. Returns the descriptors and the matching paths.
fn describe(paths: &[PathBuf]) -> (Vec<FileDescriptor>, Vec<PathBuf>) {
    fn add(path: &Path, parent: Option<&str>, descs: &mut Vec<FileDescriptor>, files: &mut Vec<PathBuf>) {
        if descs.len() >= MAX_FILES {
            return;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return };
        // Linked folders are skipped so a link cycle cannot loop forever.
        let Ok(link) = std::fs::symlink_metadata(path) else { return };
        let Ok(meta) = std::fs::metadata(path) else { return };
        if link.file_type().is_symlink() && meta.is_dir() {
            return;
        }
        let wire = parent.map_or(0, |p| p.chars().count() + 1) + name.chars().count();
        if wire > MAX_NAME {
            tracing::warn!("clipboard: {} has too long a path to send", path.display());
            return;
        }
        let mut desc = FileDescriptor::new(name);
        if let Some(p) = parent {
            desc = desc.with_relative_path(p);
        }
        if let Some(t) = meta.modified().ok().and_then(filetime) {
            desc = desc.with_last_write_time(t);
        }
        if meta.is_dir() {
            descs.push(desc.with_attributes(ClipboardFileAttributes::DIRECTORY));
            files.push(path.to_path_buf());
            let child_parent = match parent {
                Some(p) => format!("{p}\\{name}"),
                None => name.to_string(),
            };
            let Ok(entries) = std::fs::read_dir(path) else { return };
            let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            children.sort();
            for child in children {
                add(&child, Some(&child_parent), descs, files);
            }
        } else {
            descs.push(desc.with_attributes(ClipboardFileAttributes::NORMAL).with_file_size(meta.len()));
            files.push(path.to_path_buf());
        }
    }
    let (mut descs, mut files) = (Vec::new(), Vec::new());
    for p in paths {
        add(p, None, &mut descs, &mut files);
    }
    (descs, files)
}

/// Windows FILETIME: 100 ns intervals since 1601-01-01.
fn filetime(t: std::time::SystemTime) -> Option<u64> {
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100 + 116_444_736_000_000_000)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / f64::from(1u32 << 30)),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1u32 << 20)),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

impl ClipboardLink {
    /// Offer local text to the server.
    pub fn offer(&self, stage: &mut ActiveStage, text: String) -> SessionResult<Option<Vec<u8>>> {
        if let Ok(mut l) = self.local.lock() {
            l.text = Some(text.replace('\n', "\r\n"));
            l.files.clear();
        }
        let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(None) };
        let msgs = c.initiate_copy(&text_formats()).map_err(|e| ironrdp::session::custom_err!("CLIPRDR", e))?;
        stage.process_svc_processor_messages(msgs).map(Some)
    }

    /// Offer local files (and the contents of folders) to the server.
    pub fn offer_files(&self, stage: &mut ActiveStage, paths: &[PathBuf]) -> SessionResult<Option<Vec<u8>>> {
        let (descs, files) = describe(paths);
        if descs.is_empty() {
            return Ok(None);
        }
        let count = descs.len();
        if let Ok(mut l) = self.local.lock() {
            l.text = None;
            l.files = files;
        }
        let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(None) };
        match c.initiate_file_copy(descs) {
            Ok(msgs) => {
                tracing::debug!("clipboard: offered {count} local item(s)");
                stage.process_svc_processor_messages(msgs).map(Some)
            }
            // The server may not take files (or the channel is not up yet);
            // that is no reason to drop the session.
            Err(e) => {
                tracing::warn!("clipboard: could not offer files: {e}");
                Ok(None)
            }
        }
    }

    /// Perform queued channel work. Returns frames to send and events for the UI.
    pub fn drain(&self, stage: &mut ActiveStage) -> SessionResult<(Vec<Vec<u8>>, Vec<SessionEvent>)> {
        let mut frames = Vec::new();
        let mut events = Vec::new();
        while let Ok(w) = self.work.try_recv() {
            let m = match w {
                ClipWork::Event(e) => {
                    events.push(e);
                    continue;
                }
                ClipWork::Channel(m) => m,
            };
            let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() else { continue };
            let msgs = match m {
                ClipboardMessage::SendInitiateCopy(f) => c.initiate_copy(&f),
                ClipboardMessage::SendFormatData(r) => c.submit_format_data(r),
                ClipboardMessage::SendInitiatePaste(id) => c.initiate_paste(id),
                ClipboardMessage::SendFileContentsRequest(r) => c.request_file_contents(r),
                ClipboardMessage::SendFileContentsResponse(r) => c.submit_file_contents(r),
                _ => continue,
            };
            // A clipboard step the channel refuses is no reason to drop the session.
            let msgs = match msgs {
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::warn!("clipboard: {e}");
                    continue;
                }
            };
            let frame = stage.process_svc_processor_messages(msgs)?;
            if !frame.is_empty() {
                frames.push(frame);
            }
        }
        Ok((frames, events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_cannot_escape() {
        let ok = FileDescriptor::new("b.txt").with_relative_path("dir\\sub");
        assert_eq!(safe_relative(&ok), Some(PathBuf::from("dir").join("sub").join("b.txt")));
        for (path, name) in [("..", "x"), ("dir", ".."), ("C:", "x"), ("", "x"), ("a/b", "x"), ("dir", "CON.txt")] {
            let f = FileDescriptor::new(name).with_relative_path(path);
            assert_eq!(safe_relative(&f), None, "{path:?} {name:?}");
        }
    }

    #[test]
    fn folders_are_walked_in_order() {
        let root = std::env::temp_dir().join(format!("peek-describe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("top").join("inner")).unwrap();
        std::fs::write(root.join("top").join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("top").join("inner").join("b.txt"), b"").unwrap();
        let (descs, files) = describe(&[root.join("top")]);
        let names: Vec<_> = descs.iter().map(|d| (d.relative_path.clone(), d.name.clone(), d.file_size)).collect();
        assert_eq!(
            names,
            vec![
                (None, "top".to_string(), None),
                (Some("top".to_string()), "a.txt".to_string(), Some(5)),
                (Some("top".to_string()), "inner".to_string(), None),
                (Some("top\\inner".to_string()), "b.txt".to_string(), Some(0)),
            ]
        );
        assert_eq!(files.len(), 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_contents_are_served_in_ranges() {
        let path = std::env::temp_dir().join(format!("peek-range-{}.bin", std::process::id()));
        std::fs::write(&path, b"0123456789").unwrap();
        let req = |flags, position, requested_size| FileContentsRequest {
            stream_id: 7,
            index: 0,
            flags,
            position,
            requested_size,
            data_id: None,
        };
        let size = read_contents(&path, &req(FileContentsFlags::SIZE, 0, 8)).unwrap();
        assert_eq!(size.data_as_size().unwrap(), 10);
        let range = read_contents(&path, &req(FileContentsFlags::RANGE, 4, 100)).unwrap();
        assert_eq!(range.data(), b"456789");
        let _ = std::fs::remove_file(&path);
    }
}
