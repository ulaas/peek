//! `.peekrec` session recordings.
//!
//! A recording stores exactly what changed on screen: damaged rectangles with
//! their pixels, desktop resizes and pointer positions, each with a
//! millisecond timestamp. Idle sessions therefore cost almost nothing and
//! playback is pixel-exact.
//!
//! File layout:
//! ```text
//! header   "PEEKREC\x01" | u32 header_len | header_len bytes of UTF-8 "key=value\n" metadata
//! segment* u32 compressed_len | u32 first_ts_ms | u8 flags (bit0 = starts with keyframe)
//!          | zstd frame of records
//! record   u32 ts_ms | u8 kind | payload
//!   kind 1 region: u16 x, y, w, h | w*h*4 RGBA bytes
//!   kind 2 resize: u16 w, h
//!   kind 3 pointer: u16 x, y
//!   kind 4 audio:   u16 len | one Opus packet, 48 kHz stereo, 20 ms
//! ```
//!
//! Audio is stored as Opus (about 96 kbit/s) rather than PCM, which would
//! outweigh the picture of a typical session many times over.
//! Each segment is an independent zstd frame, so a recording that was cut
//! off (crash, power loss) is readable up to its last complete segment, and
//! the player can seek by jumping to the nearest keyframe segment.

#[cfg(feature = "mp4")]
pub mod export;

use anyhow::{Context, bail, ensure};
use peek_core::{AudioChunk, Framebuffer, Rect};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"PEEKREC\x01";
const KIND_REGION: u8 = 1;
const KIND_RESIZE: u8 = 2;
const KIND_POINTER: u8 = 3;
const KIND_AUDIO: u8 = 4;

/// Recorded audio format: Opus at 48 kHz stereo in 20 ms packets.
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 2;
const AUDIO_FRAME: usize = 960;
const FLAG_KEYFRAME: u8 = 1;

/// Flush a segment after this much time or this many raw bytes.
const SEGMENT_TIME: Duration = Duration::from_secs(2);
const SEGMENT_BYTES: usize = 48 << 20;
/// Start a segment with a full frame at least this often (seek granularity).
const KEYFRAME_EVERY: Duration = Duration::from_secs(10);

enum Msg {
    Region { at: Instant, rect: Rect, rgba: Vec<u8> },
    Resize { at: Instant, w: u32, h: u32 },
    Pointer { at: Instant, x: u32, y: u32 },
    Audio { ts: u32, packet: Vec<u8> },
}

/// Handle for an in-progress recording. Cheap to feed: data is copied into a
/// channel and compressed on a background thread.
pub struct Recorder {
    tx: Option<mpsc::Sender<Msg>>,
    thread: Option<JoinHandle<anyhow::Result<RecordingInfo>>>,
    path: PathBuf,
    started: Instant,
    /// Tells the audio encoder thread (if any) to finish.
    audio_stop: Arc<AtomicBool>,
    audio_thread: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub struct RecordingInfo {
    pub path: PathBuf,
    pub duration: Duration,
    pub bytes: u64,
}

impl Recorder {
    /// Start recording. `initial` is the current screen; it becomes the first keyframe.
    pub fn start(path: impl Into<PathBuf>, metadata: &[(&str, &str)], initial: &Framebuffer) -> anyhow::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file =
            BufWriter::new(File::create(&path).with_context(|| format!("cannot create {}", path.display()))?);
        let meta: String = metadata
            .iter()
            .map(|(k, v)| format!("{}={}\n", k.replace(['=', '\n'], "_"), v.replace('\n', " ")))
            .collect();
        file.write_all(MAGIC)?;
        file.write_all(&(meta.len() as u32).to_le_bytes())?;
        file.write_all(meta.as_bytes())?;

        let started = Instant::now();
        let shadow = initial.clone();
        let (tx, rx) = mpsc::channel();
        let thread_path = path.clone();
        let thread = std::thread::Builder::new()
            .name("peek-recorder".into())
            .spawn(move || writer_thread(file, rx, shadow, started, thread_path))?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            path,
            started,
            audio_stop: Arc::new(AtomicBool::new(false)),
            audio_thread: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Record a changed region; `rgba` holds `rect.w * rect.h` pixels.
    pub fn region(&self, rect: Rect, rgba: Vec<u8>) {
        debug_assert_eq!(rgba.len(), rect.area() as usize * 4);
        self.send(Msg::Region { at: Instant::now(), rect, rgba });
    }

    pub fn resize(&self, w: u32, h: u32) {
        self.send(Msg::Resize { at: Instant::now(), w, h });
    }

    pub fn pointer(&self, x: u32, y: u32) {
        self.send(Msg::Pointer { at: Instant::now(), x, y });
    }

    /// A sender for the session's decoded audio; the recording encodes it
    /// to Opus on its own thread. Call once per recording.
    pub fn audio_tap(&mut self) -> anyhow::Result<mpsc::Sender<AudioChunk>> {
        let Some(tx) = self.tx.clone() else { bail!("recording stopped") };
        let (atx, arx) = mpsc::channel::<AudioChunk>();
        let mut enc = opus2::Encoder::new(AUDIO_RATE, opus2::Channels::Stereo, opus2::Application::Audio)?;
        enc.set_bitrate(opus2::Bitrate::Bits(96_000))?;
        let (started, stop) = (self.started, self.audio_stop.clone());
        let t = std::thread::Builder::new()
            .name("peek-rec-audio".into())
            .spawn(move || audio_thread(arx, tx, enc, started, stop))?;
        self.audio_thread = Some(t);
        Ok(atx)
    }

    fn send(&self, m: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(m);
        }
    }

    /// Finish the file and wait for the writer thread.
    pub fn stop(mut self) -> anyhow::Result<RecordingInfo> {
        self.finish()
    }

    fn finish(&mut self) -> anyhow::Result<RecordingInfo> {
        // The audio thread holds a sender too: stop it first, keeping what it encoded.
        self.audio_stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.audio_thread.take() {
            let _ = t.join();
        }
        drop(self.tx.take());
        match self.thread.take() {
            Some(t) => t.join().map_err(|_| anyhow::anyhow!("recorder thread panicked"))?,
            None => bail!("recording already stopped"),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.finish();
        }
    }
}

/// Resample incoming audio to 48 kHz stereo, cut it into 20 ms Opus packets
/// and timestamp them. Continuous audio gets back-to-back timestamps;
/// after a pause (the server sends nothing during silence) the clock
/// re-syncs to when the sound arrived.
fn audio_thread(
    rx: mpsc::Receiver<AudioChunk>,
    tx: mpsc::Sender<Msg>,
    mut enc: opus2::Encoder,
    started: Instant,
    stop: Arc<AtomicBool>,
) {
    let mut resampler = peek_audio::Resampler::default();
    let mut pcm: Vec<i16> = Vec::new();
    let mut pcm_start_ms: f64 = 0.0;
    // Set by the first chunk. An empty buffer alone is no reason to re-sync:
    // 10 ms chunks fill 20 ms packets exactly, so it empties after every packet.
    let mut synced = false;
    let ch = AUDIO_CHANNELS as usize;
    let frame_ms = AUDIO_FRAME as f64 * 1000.0 / AUDIO_RATE as f64;
    let mut packet = vec![0u8; 4000];
    let mut emit = |pcm: &mut Vec<i16>, start: &mut f64, flush: bool| {
        if flush && !pcm.is_empty() {
            let pad = AUDIO_FRAME * ch - pcm.len() % (AUDIO_FRAME * ch);
            if pad < AUDIO_FRAME * ch {
                pcm.extend(std::iter::repeat_n(0, pad));
            }
        }
        while pcm.len() >= AUDIO_FRAME * ch {
            if let Ok(n) = enc.encode(&pcm[..AUDIO_FRAME * ch], &mut packet) {
                let _ = tx.send(Msg::Audio { ts: *start as u32, packet: packet[..n].to_vec() });
            }
            pcm.drain(..AUDIO_FRAME * ch);
            *start += frame_ms;
        }
    };
    loop {
        let chunk = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => continue,
            Err(_) => break,
        };
        let samples = resampler.process_i16(&chunk.samples, chunk.rate, chunk.channels, AUDIO_RATE, AUDIO_CHANNELS);
        let dur_ms = (samples.len() / ch) as f64 * 1000.0 / AUDIO_RATE as f64;
        let arrived_ms = started.elapsed().as_secs_f64() * 1000.0;
        let starts_at = (arrived_ms - dur_ms).max(0.0);
        let buffered_end = pcm_start_ms + (pcm.len() / ch) as f64 * 1000.0 / AUDIO_RATE as f64;
        if !synced || (starts_at - buffered_end).abs() > 200.0 {
            emit(&mut pcm, &mut pcm_start_ms, true);
            pcm_start_ms = starts_at;
            synced = true;
        }
        pcm.extend_from_slice(&samples);
        emit(&mut pcm, &mut pcm_start_ms, false);
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    emit(&mut pcm, &mut pcm_start_ms, true);
}

struct SegmentBuf {
    raw: Vec<u8>,
    first_ts: Option<u32>,
    keyframe: bool,
    opened: Instant,
}

impl SegmentBuf {
    fn new() -> Self {
        Self { raw: Vec::with_capacity(1 << 20), first_ts: None, keyframe: false, opened: Instant::now() }
    }

    fn header(&mut self, ts: u32, kind: u8) {
        self.first_ts.get_or_insert(ts);
        self.raw.extend_from_slice(&ts.to_le_bytes());
        self.raw.push(kind);
    }

    fn region(&mut self, ts: u32, r: Rect, rgba: &[u8]) {
        self.header(ts, KIND_REGION);
        for v in [r.x, r.y, r.w, r.h] {
            self.raw.extend_from_slice(&(v as u16).to_le_bytes());
        }
        self.raw.extend_from_slice(rgba);
    }

    fn keyframe(&mut self, ts: u32, fb: &Framebuffer) {
        self.keyframe = true;
        self.header(ts, KIND_RESIZE);
        self.raw.extend_from_slice(&(fb.width() as u16).to_le_bytes());
        self.raw.extend_from_slice(&(fb.height() as u16).to_le_bytes());
        self.region(ts, Rect::new(0, 0, fb.width(), fb.height()), fb.as_bytes());
    }
}

fn writer_thread(
    mut file: BufWriter<File>,
    rx: mpsc::Receiver<Msg>,
    mut shadow: Framebuffer,
    started: Instant,
    path: PathBuf,
) -> anyhow::Result<RecordingInfo> {
    let ts = |at: Instant| at.saturating_duration_since(started).as_millis().min(u32::MAX as u128) as u32;
    let mut seg = SegmentBuf::new();
    seg.keyframe(0, &shadow);
    let mut last_key = Instant::now();
    let mut last_ts = 0u32;
    let mut enc = zstd::bulk::Compressor::new(3)?;

    let mut flush = |seg: &mut SegmentBuf, file: &mut BufWriter<File>| -> anyhow::Result<()> {
        if seg.raw.is_empty() {
            return Ok(());
        }
        let z = enc.compress(&seg.raw)?;
        file.write_all(&(z.len() as u32).to_le_bytes())?;
        file.write_all(&seg.first_ts.unwrap_or(0).to_le_bytes())?;
        file.write_all(&[if seg.keyframe { FLAG_KEYFRAME } else { 0 }])?;
        file.write_all(&z)?;
        file.flush()?;
        *seg = SegmentBuf::new();
        Ok(())
    };

    loop {
        let msg = match rx.recv_timeout(SEGMENT_TIME) {
            Ok(m) => Some(m),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(m) = msg {
            match m {
                Msg::Region { at, rect, rgba } => {
                    last_ts = ts(at);
                    let px: &[u32] = bytemuck::cast_slice(&rgba);
                    shadow.put_rect(rect, px);
                    seg.region(last_ts, rect, &rgba);
                }
                Msg::Resize { at, w, h } => {
                    last_ts = ts(at);
                    shadow.resize(w, h);
                    seg.header(last_ts, KIND_RESIZE);
                    seg.raw.extend_from_slice(&(w as u16).to_le_bytes());
                    seg.raw.extend_from_slice(&(h as u16).to_le_bytes());
                }
                Msg::Pointer { at, x, y } => {
                    last_ts = ts(at);
                    seg.header(last_ts, KIND_POINTER);
                    seg.raw.extend_from_slice(&(x as u16).to_le_bytes());
                    seg.raw.extend_from_slice(&(y as u16).to_le_bytes());
                }
                Msg::Audio { ts: t, packet } => {
                    last_ts = last_ts.max(t);
                    seg.header(t, KIND_AUDIO);
                    seg.raw.extend_from_slice(&(packet.len() as u16).to_le_bytes());
                    seg.raw.extend_from_slice(&packet);
                }
            }
        }
        if seg.opened.elapsed() >= SEGMENT_TIME || seg.raw.len() >= SEGMENT_BYTES {
            flush(&mut seg, &mut file)?;
            if last_key.elapsed() >= KEYFRAME_EVERY {
                seg.keyframe(last_ts, &shadow);
                last_key = Instant::now();
            }
        }
    }
    flush(&mut seg, &mut file)?;
    let bytes = file.get_ref().metadata().map(|m| m.len()).unwrap_or(0);
    Ok(RecordingInfo { path, duration: Duration::from_millis(last_ts as u64), bytes })
}

/// One decoded event from a recording.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Region {
        rect: Rect,
        rgba: Vec<u8>,
    },
    Resize {
        w: u32,
        h: u32,
    },
    Pointer {
        x: u32,
        y: u32,
    },
    /// One Opus packet (48 kHz stereo, 20 ms).
    Audio {
        packet: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug)]
struct SegmentIndex {
    offset: u64,
    len: u32,
    first_ts: u32,
    keyframe: bool,
}

/// Random-access reader for `.peekrec` files.
pub struct Player {
    file: BufReader<File>,
    pub metadata: Vec<(String, String)>,
    segments: Vec<SegmentIndex>,
    pub frame: Framebuffer,
    pub pointer: Option<(u32, u32)>,
    /// Timestamp the frame currently shows.
    pub position_ms: u32,
    pending: std::vec::IntoIter<(u32, Event)>,
    next_segment: usize,
    pub duration_ms: u32,
    /// Audio packets passed by the last `advance_to`, with their timestamps.
    /// Seeking clears them; the caller drains them to play.
    pub audio: Vec<(u32, Vec<u8>)>,
}

impl Player {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut file = BufReader::new(File::open(path.as_ref())?);
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        ensure!(&magic == MAGIC, "not a Peek recording");
        let meta_len = read_u32(&mut file)? as usize;
        ensure!(meta_len < 1 << 20, "corrupt header");
        let mut meta = vec![0u8; meta_len];
        file.read_exact(&mut meta)?;
        let metadata = String::from_utf8_lossy(&meta)
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();

        // Index segments; stop quietly at a truncated tail.
        let mut segments = Vec::new();
        let mut offset = 12 + meta_len as u64;
        let total = file.get_ref().metadata()?.len();
        while offset + 9 <= total {
            file.seek(SeekFrom::Start(offset))?;
            let len = read_u32(&mut file)?;
            let first_ts = read_u32(&mut file)?;
            let mut flags = [0u8];
            file.read_exact(&mut flags)?;
            if offset + 9 + len as u64 > total {
                break;
            }
            segments.push(SegmentIndex { offset: offset + 9, len, first_ts, keyframe: flags[0] & FLAG_KEYFRAME != 0 });
            offset += 9 + len as u64;
        }
        ensure!(!segments.is_empty(), "recording contains no data");

        let mut p = Self {
            file,
            metadata,
            segments,
            frame: Framebuffer::new(1, 1),
            pointer: None,
            position_ms: 0,
            pending: Vec::new().into_iter(),
            next_segment: 0,
            duration_ms: 0,
            audio: Vec::new(),
        };
        // Duration: last timestamp of the last segment.
        let last = p.segments.len() - 1;
        p.duration_ms = p.decode_segment(last)?.last().map(|(t, _)| *t).unwrap_or(0);
        p.seek(0)?;
        Ok(p)
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn decode_segment(&mut self, i: usize) -> anyhow::Result<Vec<(u32, Event)>> {
        let s = self.segments[i];
        self.file.seek(SeekFrom::Start(s.offset))?;
        let mut z = vec![0u8; s.len as usize];
        self.file.read_exact(&mut z)?;
        let raw = zstd::stream::decode_all(&z[..])?;
        parse_records(&raw)
    }

    /// Jump to `ms`: restart from the nearest keyframe and replay up to it.
    pub fn seek(&mut self, ms: u32) -> anyhow::Result<()> {
        let start = self.segments.iter().rposition(|s| s.keyframe && s.first_ts <= ms).unwrap_or(0);
        self.next_segment = start;
        self.pending = Vec::new().into_iter();
        self.position_ms = 0;
        self.advance_to(ms)?;
        self.audio.clear();
        Ok(())
    }

    /// Apply every event with a timestamp up to `ms`. Returns the damage.
    pub fn advance_to(&mut self, ms: u32) -> anyhow::Result<Vec<Rect>> {
        let mut damage = Vec::new();
        loop {
            let next = match self.pending.as_slice().first() {
                Some((t, _)) => *t,
                None => {
                    if self.next_segment >= self.segments.len() {
                        break;
                    }
                    let events = self.decode_segment(self.next_segment)?;
                    self.next_segment += 1;
                    self.pending = events.into_iter();
                    continue;
                }
            };
            if next > ms {
                break;
            }
            let (_, ev) = self.pending.next().expect("peeked");
            match ev {
                Event::Region { rect, rgba } => {
                    self.frame.put_rect(rect, bytemuck::cast_slice(&rgba));
                    damage.push(rect);
                }
                Event::Resize { w, h } => {
                    self.frame.resize(w, h);
                    damage.push(Rect::new(0, 0, w, h));
                }
                Event::Pointer { x, y } => self.pointer = Some((x, y)),
                Event::Audio { packet } => self.audio.push((next, packet)),
            }
        }
        self.position_ms = ms.min(self.duration_ms);
        Ok(damage)
    }

    pub fn at_end(&self) -> bool {
        self.next_segment >= self.segments.len() && self.pending.as_slice().is_empty()
    }
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn parse_records(raw: &[u8]) -> anyhow::Result<Vec<(u32, Event)>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> anyhow::Result<&[u8]> {
        let s = raw.get(*p..*p + n).context("truncated record")?;
        *p += n;
        Ok(s)
    };
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u32;
    while p < raw.len() {
        let h = take(&mut p, 5)?;
        let ts = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let ev = match h[4] {
            KIND_REGION => {
                let g = take(&mut p, 8)?;
                let rect = Rect::new(u16_at(g, 0), u16_at(g, 2), u16_at(g, 4), u16_at(g, 6));
                Event::Region { rect, rgba: take(&mut p, rect.area() as usize * 4)?.to_vec() }
            }
            KIND_RESIZE => {
                let g = take(&mut p, 4)?;
                Event::Resize { w: u16_at(g, 0), h: u16_at(g, 2) }
            }
            KIND_POINTER => {
                let g = take(&mut p, 4)?;
                Event::Pointer { x: u16_at(g, 0), y: u16_at(g, 2) }
            }
            KIND_AUDIO => {
                let n = u16_at(take(&mut p, 2)?, 0) as usize;
                Event::Audio { packet: take(&mut p, n)?.to_vec() }
            }
            k => bail!("unknown record kind {k}"),
        };
        out.push((ts, ev));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peek_core::rgb;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("peekrec-test-{}", std::process::id()));
        let path = dir.join("t.peekrec");
        let mut fb = Framebuffer::new(8, 8);
        fb.fill_rect(Rect::new(0, 0, 8, 8), rgb(10, 20, 30));

        let rec = Recorder::start(&path, &[("host", "box"), ("protocol", "VNC")], &fb).unwrap();
        let red: Vec<u32> = vec![rgb(255, 0, 0); 4];
        rec.region(Rect::new(2, 2, 2, 2), bytemuck::cast_slice(&red).to_vec());
        rec.pointer(3, 4);
        std::thread::sleep(Duration::from_millis(20));
        rec.resize(10, 6);
        let info = rec.stop().unwrap();
        assert!(info.bytes > 12);

        let mut p = Player::open(&path).unwrap();
        assert_eq!(p.meta("host"), Some("box"));
        // At t=0 we see the initial keyframe only.
        assert_eq!(p.frame.pixels()[0], rgb(10, 20, 30));
        p.advance_to(p.duration_ms).unwrap();
        assert_eq!((p.frame.width(), p.frame.height()), (10, 6));
        assert_eq!(p.frame.row(2, 2, 1)[0], rgb(255, 0, 0));
        assert_eq!(p.pointer, Some((3, 4)));
        assert!(p.at_end());

        p.seek(0).unwrap();
        assert_eq!((p.frame.width(), p.frame.height()), (8, 8));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn audio_is_recorded_and_replayed() {
        let dir = std::env::temp_dir().join(format!("peekrec-audio-{}", std::process::id()));
        let path = dir.join("t.peekrec");
        let fb = Framebuffer::new(4, 4);
        let mut rec = Recorder::start(&path, &[], &fb).unwrap();
        let tap = rec.audio_tap().unwrap();
        // 300 ms of a 44.1 kHz tone in 10 ms chunks, like an RDP server sends.
        // Paced against a fixed schedule so sleep overshoot on a busy machine
        // does not add up.
        let t0 = Instant::now();
        for i in 0..30 {
            let samples: Vec<i16> =
                (0..441 * 2).map(|j| (((i * 441 + j / 2) as f32 * 0.06).sin() * 9000.0) as i16).collect();
            tap.send(AudioChunk { rate: 44_100, channels: 2, samples }).unwrap();
            std::thread::sleep(
                (t0 + Duration::from_millis(10 * (i as u64 + 1))).saturating_duration_since(Instant::now()),
            );
        }
        std::thread::sleep(Duration::from_millis(30));
        rec.stop().unwrap(); // must not wait for `tap` to be dropped
        drop(tap);

        let mut p = Player::open(&path).unwrap();
        p.advance_to(p.duration_ms).unwrap();
        let n = p.audio.len();
        assert!((14..=16).contains(&n), "{n} packets for 300 ms");
        let ts: Vec<u32> = p.audio.iter().map(|(t, _)| *t).collect();
        assert!(ts.windows(2).all(|w| w[1] - w[0] == 20 || w[1] - w[0] == 19 || w[1] - w[0] == 21), "{ts:?}");
        let mut dec = opus2::Decoder::new(48_000, opus2::Channels::Stereo).unwrap();
        let mut out = vec![0i16; 960 * 2];
        assert_eq!(dec.decode(&p.audio[3].1, &mut out, false).unwrap(), 960);
        assert!(out.iter().any(|s| s.unsigned_abs() > 1000), "decoded audio is not silent");
        p.seek(0).unwrap();
        assert!(p.audio.is_empty(), "seeking must not replay audio");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn truncated_file_is_readable() {
        let dir = std::env::temp_dir().join(format!("peekrec-trunc-{}", std::process::id()));
        let path = dir.join("t.peekrec");
        let fb = Framebuffer::new(4, 4);
        Recorder::start(&path, &[], &fb).unwrap().stop().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0]); // half-written segment header
        std::fs::write(&path, &bytes).unwrap();
        assert!(Player::open(&path).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }
}
