//! Text clipboard over the CLIPRDR channel.
//!
//! IronRDP calls the backend from inside its channel processing, where it
//! cannot send anything itself, so the backend only queues work. The
//! session loop drains the queue after every step and performs it on the
//! channel. Clipboard text from the server is handed to the UI, which owns
//! the OS clipboard; the same path serves VNC, so behaviour is identical.

use ironrdp::cliprdr::CliprdrClient;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::core::{IntoOwned as _, impl_as_any};
use ironrdp::session::{ActiveStage, SessionResult};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

pub enum ClipWork {
    Channel(ClipboardMessage),
    RemoteText(String),
}

#[derive(Debug)]
pub struct TextClipboard {
    queue: mpsc::Sender<ClipWork>,
    local: Arc<Mutex<Option<String>>>,
    requested: Option<ClipboardFormatId>,
}

impl std::fmt::Debug for ClipWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipWork::Channel(m) => write!(f, "Channel({m:?})"),
            ClipWork::RemoteText(t) => write!(f, "RemoteText({} chars)", t.len()),
        }
    }
}

/// Handle kept by the session loop.
pub struct ClipboardLink {
    pub work: mpsc::Receiver<ClipWork>,
    local: Arc<Mutex<Option<String>>>,
}

pub fn new() -> (TextClipboard, ClipboardLink) {
    let (tx, rx) = mpsc::channel();
    let local = Arc::new(Mutex::new(None));
    (
        TextClipboard { queue: tx, local: local.clone(), requested: None },
        ClipboardLink { work: rx, local },
    )
}

fn text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
}

impl_as_any!(TextClipboard);

impl TextClipboard {
    fn push(&self, m: ClipboardMessage) {
        let _ = self.queue.send(ClipWork::Channel(m));
    }
}

impl CliprdrBackend for TextClipboard {
    fn temporary_directory(&self) -> &str {
        "."
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        let has_text = self.local.lock().map(|l| l.is_some()).unwrap_or(false);
        self.push(ClipboardMessage::SendInitiateCopy(if has_text { text_formats() } else { Vec::new() }));
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        let pick = [ClipboardFormatId::CF_UNICODETEXT, ClipboardFormatId::CF_TEXT]
            .into_iter()
            .find(|want| formats.iter().any(|f| f.id() == *want));
        if let Some(id) = pick {
            self.requested = Some(id);
            self.push(ClipboardMessage::SendInitiatePaste(id));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let text = self.local.lock().ok().and_then(|l| l.clone());
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
            _ => response.to_unicode_string(),
        };
        if let Ok(t) = text {
            let _ = self.queue.send(ClipWork::RemoteText(t.trim_end_matches('\0').replace("\r\n", "\n")));
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

impl ClipboardLink {
    /// Offer local text to the server.
    pub fn offer(&self, stage: &mut ActiveStage, text: String) -> SessionResult<Option<Vec<u8>>> {
        if let Ok(mut l) = self.local.lock() {
            *l = Some(text.replace('\n', "\r\n"));
        }
        let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(None) };
        let msgs = c.initiate_copy(&text_formats()).map_err(|e| ironrdp::session::custom_err!("CLIPRDR", e))?;
        stage.process_svc_processor_messages(msgs).map(Some)
    }

    /// Perform queued channel work. Returns frames to send and remote text.
    pub fn drain(&self, stage: &mut ActiveStage) -> SessionResult<(Vec<Vec<u8>>, Vec<String>)> {
        let mut frames = Vec::new();
        let mut texts = Vec::new();
        while let Ok(w) = self.work.try_recv() {
            let m = match w {
                ClipWork::RemoteText(t) => {
                    texts.push(t);
                    continue;
                }
                ClipWork::Channel(m) => m,
            };
            let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() else { continue };
            let err = |e| ironrdp::session::custom_err!("CLIPRDR", e);
            let msgs = match m {
                ClipboardMessage::SendInitiateCopy(f) => c.initiate_copy(&f).map_err(err)?,
                ClipboardMessage::SendFormatData(r) => c.submit_format_data(r).map_err(err)?,
                ClipboardMessage::SendInitiatePaste(id) => c.initiate_paste(id).map_err(err)?,
                _ => continue,
            };
            let frame = stage.process_svc_processor_messages(msgs)?;
            if !frame.is_empty() {
                frames.push(frame);
            }
        }
        Ok((frames, texts))
    }
}
