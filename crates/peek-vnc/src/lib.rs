//! VNC (RFB 3.3–3.8) client backend.
//!
//! One task reads and decodes server messages straight into the shared
//! framebuffer; the session task turns UI commands into client messages.
//! Both write through a single writer task so messages never interleave.

mod decode;
mod handshake;
mod io;

use crate::decode::*;
use crate::io::{Msg, Reader};
use anyhow::{Context, bail};
use peek_core::{
    ConnectRequest, CursorImage, CursorUpdate, EventSink, InputEvent, KeyCode, MouseButton, Rect, SessionCommand,
    SessionContext, SessionEvent, SessionStats, SharedFrame, char_to_keysym,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub use handshake::vnc_auth_response;


const MAX_DIM: u32 = 16384;
const NO_SCREEN: u32 = u32::MAX;

type Tx = mpsc::UnboundedSender<Vec<u8>>;

fn encodings() -> Vec<i32> {
    vec![
        ENC_TIGHT,
        ENC_ZRLE,
        ENC_HEXTILE,
        ENC_COPYRECT,
        ENC_RRE,
        ENC_RAW,
        ENC_PSEUDO_JPEG_QUALITY_0 + 8,
        ENC_PSEUDO_COMPRESS_0 + 2,
        ENC_PSEUDO_DESKTOP_SIZE,
        ENC_PSEUDO_EXT_DESKTOP_SIZE,
        ENC_PSEUDO_CURSOR,
        ENC_PSEUDO_LAST_RECT,
    ]
}

fn msg_set_pixel_format() -> Vec<u8> {
    Msg::default()
        .u8(0)
        .pad(3)
        .u8(32) // bits per pixel
        .u8(24) // depth
        .u8(0) // little endian
        .u8(1) // true colour
        .u16(255)
        .u16(255)
        .u16(255)
        .u8(0) // red shift
        .u8(8) // green shift
        .u8(16) // blue shift
        .pad(3)
        .0
}

fn msg_set_encodings(enc: &[i32]) -> Vec<u8> {
    enc.iter().fold(Msg::default().u8(2).pad(1).u16(enc.len() as u16), |m, &e| m.i32(e)).0
}

fn msg_update_request(incremental: bool, w: u32, h: u32) -> Vec<u8> {
    Msg::default().u8(3).u8(incremental as u8).u16(0).u16(0).u16(w as u16).u16(h as u16).0
}

fn msg_key(down: bool, keysym: u32) -> Vec<u8> {
    Msg::default().u8(4).u8(down as u8).pad(2).u32(keysym).0
}

fn msg_pointer(mask: u8, x: u32, y: u32) -> Vec<u8> {
    Msg::default().u8(5).u8(mask).u16(x.min(u16::MAX as u32) as u16).u16(y.min(u16::MAX as u32) as u16).0
}

fn msg_cut_text(text: &str) -> Vec<u8> {
    // RFB cut text is Latin-1.
    let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    Msg::default().u8(6).pad(3).u32(bytes.len() as u32).bytes(&bytes).0
}

fn msg_set_desktop_size(w: u32, h: u32, screen_id: u32) -> Vec<u8> {
    Msg::default()
        .u8(251)
        .pad(1)
        .u16(w as u16)
        .u16(h as u16)
        .u8(1)
        .pad(1)
        .u32(screen_id)
        .u16(0)
        .u16(0)
        .u16(w as u16)
        .u16(h as u16)
        .u32(0)
        .0
}

/// Run a VNC session until it ends. Errors are returned to the caller, which
/// reports them to the UI.
pub async fn run(req: ConnectRequest, mut ctx: SessionContext) -> anyhow::Result<Option<String>> {
    let addr = req.endpoint.socket_addr();
    ctx.events.status(format!("Connecting to {addr}"));
    let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addr))
        .await
        .context("connection timed out")?
        .with_context(|| format!("could not reach {addr}"))?;
    stream.set_nodelay(true)?;
    let (rh, mut wh) = stream.into_split();
    let mut r = Reader::new(rh, Some(ctx.stats.clone()));

    ctx.events.status("Negotiating");
    let init = handshake::handshake(&mut r, &mut wh, req.credentials.password.as_deref(), true).await?;
    let (w, h) = (init.width as u32, init.height as u32);
    ctx.frame.write().resize(w, h);

    let mut hello = msg_set_pixel_format();
    hello.extend(msg_set_encodings(&encodings()));
    hello.extend(msg_update_request(false, w, h));
    wh.write_all(&hello).await?;

    ctx.events.send(SessionEvent::Connected { width: w, height: h, detail: format!("VNC · {}", init.name) });

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let stats = ctx.stats.clone();
    let writer = tokio::spawn(async move {
        while let Some(mut buf) = rx.recv().await {
            // Batch whatever else is queued into one write.
            while let Ok(more) = rx.try_recv() {
                buf.extend(more);
            }
            if wh.write_all(&buf).await.is_err() {
                break;
            }
            stats.add_out(buf.len());
        }
    });

    let screen_id = Arc::new(AtomicU32::new(NO_SCREEN));
    let mut reader = tokio::spawn(read_loop(
        r,
        ctx.frame.clone(),
        ctx.events.clone(),
        ctx.stats.clone(),
        tx.clone(),
        screen_id.clone(),
    ));

    let mut input = InputState::default();
    let view_only = req.options.view_only;
    let result = loop {
        tokio::select! {
            cmd = ctx.commands.recv() => match cmd {
                None | Some(SessionCommand::Disconnect) => break Ok(()),
                Some(SessionCommand::Input(ev)) if !view_only => input.handle(ev, &tx),
                Some(SessionCommand::Input(_)) => {}
                Some(SessionCommand::CtrlAltDel) if !view_only => {
                    for (k, d) in [(0xffe3, true), (0xffe9, true), (0xffff, true), (0xffff, false), (0xffe9, false), (0xffe3, false)] {
                        let _ = tx.send(msg_key(d, k));
                    }
                }
                Some(SessionCommand::CtrlAltDel) => {}
                Some(SessionCommand::ClipboardText(t)) => { let _ = tx.send(msg_cut_text(&t)); }
                Some(SessionCommand::Resize { width, height }) => {
                    let id = screen_id.load(Ordering::Relaxed);
                    if id != NO_SCREEN && width > 0 && height > 0 {
                        let _ = tx.send(msg_set_desktop_size(width.min(MAX_DIM), height.min(MAX_DIM), id));
                    }
                }
                Some(SessionCommand::Refresh) => {
                    let (w, h) = ctx.frame.size();
                    let _ = tx.send(msg_update_request(false, w, h));
                }
            },
            res = &mut reader => break match res {
                Ok(r) => r,
                Err(e) => Err(anyhow::anyhow!("reader task failed: {e}")),
            },
        }
    };
    reader.abort();
    drop(tx);
    let _ = writer.await;
    result.map(|()| None)
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut r: Reader<R>,
    frame: Arc<SharedFrame>,
    events: EventSink,
    stats: Arc<SessionStats>,
    tx: Tx,
    screen_id: Arc<AtomicU32>,
) -> anyhow::Result<()> {
    let mut pixels: Vec<u32> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut inflated: Vec<u8> = Vec::new();
    let mut zrle_z = flate2::Decompress::new(true);
    let mut tight = Tight::default();

    loop {
        let ty = match r.u8().await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match ty {
            0 => {
                r.u8().await?;
                let n = r.u16().await?;
                for _ in 0..n {
                    let x = r.u16().await? as u32;
                    let y = r.u16().await? as u32;
                    let w = r.u16().await? as u32;
                    let h = r.u16().await? as u32;
                    let enc = r.i32().await?;
                    let rect = Rect::new(x, y, w, h);
                    // Pixel-carrying rectangles must lie on screen; this also bounds
                    // every allocation a hostile server can cause.
                    if enc >= 0 {
                        let (fw, fh) = frame.size();
                        if x + w > fw || y + h > fh {
                            bail!("server sent a {w}x{h} rectangle at {x},{y} outside the {fw}x{fh} screen");
                        }
                    }
                    match enc {
                        ENC_RAW => {
                            r.read_into(w as usize * h as usize * 4, &mut buf).await?;
                            wire_to_pixels(&buf, &mut pixels);
                            frame.write().put_rect(rect, &pixels);
                        }
                        ENC_COPYRECT => {
                            let sx = r.u16().await? as u32;
                            let sy = r.u16().await? as u32;
                            frame.write().copy_rect(sx, sy, rect);
                        }
                        ENC_RRE => {
                            rre(&mut r, w, h, &mut pixels).await?;
                            frame.write().put_rect(rect, &pixels);
                        }
                        ENC_HEXTILE => {
                            hextile(&mut r, w, h, &mut pixels).await?;
                            frame.write().put_rect(rect, &pixels);
                        }
                        ENC_ZRLE => {
                            let len = r.u32().await? as usize;
                            // zlib never expands beyond ~0.1%; anything far above raw size is bogus.
                            if len > (w as usize * h as usize * 4 + 1024) * 2 {
                                bail!("ZRLE payload of {len} bytes for a {w}x{h} rectangle");
                            }
                            r.read_into(len, &mut buf).await?;
                            // Upper bound: every tile raw (1 + 3 bytes/pixel) plus a full palette header.
                            let tiles = (w as usize).div_ceil(64) * (h as usize).div_ceil(64);
                            inflate(&mut zrle_z, &buf, None, w as usize * h as usize * 4 + tiles * 400, &mut inflated)?;
                            zrle(&inflated, w, h, &mut pixels)?;
                            frame.write().put_rect(rect, &pixels);
                        }
                        ENC_TIGHT => {
                            tight.decode(&mut r, w, h, &mut pixels).await?;
                            frame.write().put_rect(rect, &pixels);
                        }
                        ENC_PSEUDO_CURSOR => {
                            if w > 512 || h > 512 {
                                bail!("server sent an oversized {w}x{h} cursor");
                            }
                            let (wu, hu) = (w as usize, h as usize);
                            let px = r.vec(wu * hu * 4).await?;
                            let mask = r.vec(wu.div_ceil(8) * hu).await?;
                            let update = if w == 0 || h == 0 {
                                CursorUpdate::Hidden
                            } else {
                                CursorUpdate::Image(Arc::new(CursorImage {
                                    width: w,
                                    height: h,
                                    hot_x: x,
                                    hot_y: y,
                                    rgba: cursor_rgba(&px, &mask, w, h),
                                }))
                            };
                            events.send(SessionEvent::Cursor(update));
                        }
                        ENC_PSEUDO_DESKTOP_SIZE => resize(&frame, &events, &tx, w, h)?,
                        ENC_PSEUDO_EXT_DESKTOP_SIZE => {
                            let count = r.u8().await?;
                            r.skip(3).await?;
                            let mut first_id = NO_SCREEN;
                            for i in 0..count {
                                let id = r.u32().await?;
                                r.skip(12).await?;
                                if i == 0 {
                                    first_id = id;
                                }
                            }
                            screen_id.store(if first_id == NO_SCREEN { 0 } else { first_id }, Ordering::Relaxed);
                            // y carries the status of a client-requested change; 0 = success.
                            if y == 0 {
                                resize(&frame, &events, &tx, w, h)?;
                            }
                        }
                        ENC_PSEUDO_LAST_RECT => break,
                        other => bail!("server sent unsupported encoding {other}"),
                    }
                }
                stats.add_update();
                let (fw, fh) = frame.size();
                let _ = tx.send(msg_update_request(true, fw, fh));
            }
            1 => {
                r.skip(3).await?;
                let n = r.u16().await? as usize;
                r.skip(n * 6).await?;
            }
            2 => events.send(SessionEvent::Bell),
            3 => {
                r.skip(3).await?;
                let len = r.i32().await?;
                if len < 0 {
                    // Extended clipboard capability message; not negotiated yet.
                    r.skip(len.unsigned_abs() as usize).await?;
                } else {
                    let bytes = r.vec((len as usize).min(16 << 20)).await?;
                    events.send(SessionEvent::ClipboardText(bytes.iter().map(|&b| b as char).collect()));
                }
            }
            150 => {} // EndOfContinuousUpdates
            248 => {
                r.skip(3).await?;
                r.u32().await?;
                let n = r.u8().await? as usize;
                r.skip(n).await?;
            }
            other => bail!("unknown server message type {other}"),
        }
    }
}

fn resize(frame: &SharedFrame, events: &EventSink, tx: &Tx, w: u32, h: u32) -> anyhow::Result<()> {
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        bail!("server announced an invalid desktop size {w}x{h}");
    }
    frame.write().resize(w, h);
    events.send(SessionEvent::Resized { width: w, height: h });
    let _ = tx.send(msg_update_request(false, w, h));
    Ok(())
}

/// Translates UI input into RFB key and pointer events.
#[derive(Default)]
struct InputState {
    buttons: u8,
    x: u32,
    y: u32,
    /// Keys we sent a press for, with the keysym used, so the release matches.
    down: HashMap<KeyCode, u32>,
}

impl InputState {
    fn chord_held(&self) -> bool {
        use KeyCode::*;
        let ctrl = self.down.contains_key(&ControlLeft) || self.down.contains_key(&ControlRight);
        let alt = self.down.contains_key(&AltLeft);
        let meta = self.down.contains_key(&MetaLeft) || self.down.contains_key(&MetaRight);
        // Ctrl+Alt together is AltGr on Windows: let the text path handle it.
        (ctrl ^ alt) || meta
    }

    fn handle(&mut self, ev: InputEvent, tx: &Tx) {
        match ev {
            InputEvent::Key { code, down: true } => {
                if code.is_printable() && !self.chord_held() {
                    return; // arrives as Text
                }
                let ks = code.keysym();
                self.down.insert(code, ks);
                let _ = tx.send(msg_key(true, ks));
            }
            InputEvent::Key { code, down: false } => {
                if let Some(ks) = self.down.remove(&code) {
                    let _ = tx.send(msg_key(false, ks));
                }
            }
            InputEvent::Text(s) => {
                for c in s.chars().filter(|c| !c.is_control()) {
                    let ks = char_to_keysym(c);
                    let _ = tx.send(msg_key(true, ks));
                    let _ = tx.send(msg_key(false, ks));
                }
            }
            InputEvent::PointerMove { x, y } => {
                (self.x, self.y) = (x, y);
                let _ = tx.send(msg_pointer(self.buttons, x, y));
            }
            InputEvent::Button { button, down, x, y } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 4,
                    MouseButton::X1 | MouseButton::X2 => return,
                };
                if down { self.buttons |= bit } else { self.buttons &= !bit }
                (self.x, self.y) = (x, y);
                let _ = tx.send(msg_pointer(self.buttons, x, y));
            }
            InputEvent::Wheel { dx, dy, x, y } => {
                (self.x, self.y) = (x, y);
                for (delta, pos, neg) in [(dy, 8u8, 16u8), (dx, 64, 32)] {
                    if delta == 0 {
                        continue;
                    }
                    let bit = if delta > 0 { pos } else { neg };
                    for _ in 0..(delta.unsigned_abs() / 120).max(1) {
                        let _ = tx.send(msg_pointer(self.buttons | bit, x, y));
                        let _ = tx.send(msg_pointer(self.buttons, x, y));
                    }
                }
            }
            InputEvent::ReleaseAll => {
                for (_, ks) in self.down.drain() {
                    let _ = tx.send(msg_key(false, ks));
                }
                if self.buttons != 0 {
                    self.buttons = 0;
                    let _ = tx.send(msg_pointer(0, self.x, self.y));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_layout() {
        let m = msg_set_pixel_format();
        assert_eq!(m.len(), 20);
        assert_eq!(&m[4..8], &[32, 24, 0, 1]);
    }

    #[test]
    fn printable_keys_go_through_text() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = InputState::default();
        s.handle(InputEvent::Key { code: KeyCode::KeyA, down: true }, &tx);
        assert!(rx.try_recv().is_err());
        s.handle(InputEvent::Text("a".into()), &tx);
        assert_eq!(rx.try_recv().unwrap(), msg_key(true, 0x61));
        assert_eq!(rx.try_recv().unwrap(), msg_key(false, 0x61));
        s.handle(InputEvent::Key { code: KeyCode::KeyA, down: false }, &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ctrl_chords_send_keysyms() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = InputState::default();
        s.handle(InputEvent::Key { code: KeyCode::ControlLeft, down: true }, &tx);
        s.handle(InputEvent::Key { code: KeyCode::KeyC, down: true }, &tx);
        s.handle(InputEvent::Key { code: KeyCode::KeyC, down: false }, &tx);
        let msgs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(msgs, vec![msg_key(true, 0xffe3), msg_key(true, 0x63), msg_key(false, 0x63)]);
    }
}
