//! MP4 (H.264) export of `.peekrec` recordings.
//!
//! The recording is replayed and only frames where something changed are
//! encoded; each one lasts until the next change (variable frame rate), with
//! a repeat at least every [`MAX_FRAME_MS`] so players can seek. Idle
//! stretches cost nothing. The pointer, stored as positions in the
//! recording, is drawn into the video.

use crate::Player;
use anyhow::{Context, bail, ensure};
use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Longest a single video frame may last before it is repeated.
pub const MAX_FRAME_MS: u32 = 2000;

#[derive(Clone, Copy, Debug)]
pub struct Mp4Options {
    /// Highest frame rate; changes faster than this are sampled.
    pub fps: u32,
    /// Target bit rate; `None` picks one from the frame size.
    pub bitrate_bps: Option<u32>,
    /// Draw the pointer into the video.
    pub draw_pointer: bool,
}

impl Default for Mp4Options {
    fn default() -> Self {
        Self { fps: 30, bitrate_bps: None, draw_pointer: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp4Summary {
    pub width: u32,
    pub height: u32,
    /// Encoded video frames (changes plus repeats).
    pub frames: u32,
    pub duration_ms: u32,
    pub bytes: u64,
}

/// Export `input` to `output`. `progress` gets 0.0..=1.0 and returns false
/// to cancel; a cancelled or failed export removes the partial file.
pub fn export_mp4(input: &Path, output: &Path, opts: Mp4Options, progress: impl FnMut(f32) -> bool) -> anyhow::Result<Mp4Summary> {
    let result = export_inner(input, output, opts, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

fn export_inner(input: &Path, output: &Path, opts: Mp4Options, mut progress: impl FnMut(f32) -> bool) -> anyhow::Result<Mp4Summary> {
    let mut player = Player::open(input).context("cannot open the recording")?;
    let duration = player.duration_ms.max(1);
    // H.264 4:2:0 needs even dimensions; the first frame decides the size.
    let (w, h) = ((player.frame.width() & !1).max(16), (player.frame.height() & !1).max(16));
    let fps = opts.fps.clamp(1, 60);
    let step = 1000 / fps;
    let bitrate = opts.bitrate_bps.unwrap_or_else(|| default_bitrate(w, h));

    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Quality)
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .skip_frames(false)
        // Not available for screen content; set so OpenH264 does not warn.
        .adaptive_quantization(false)
        .background_detection(false)
        // A keyframe at least every 10 s of video keeps seeking quick.
        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 10));
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config).context("cannot start the H.264 encoder")?;

    let file = BufWriter::new(File::create(output).with_context(|| format!("cannot create {}", output.display()))?);
    let mp4_config = mp4::Mp4Config {
        major_brand: "isom".parse().expect("fourcc"),
        minor_version: 512,
        compatible_brands: ["isom", "iso2", "avc1", "mp41"].iter().map(|b| b.parse().expect("fourcc")).collect(),
        timescale: 1000,
    };
    let mut writer = mp4::Mp4Writer::write_start(file, &mp4_config)?;
    let mut track_added = false;

    let mut canvas = vec![0u8; (w * h * 4) as usize];
    let mut yuv = YUVBuffer::new(w as usize, h as usize);
    // The sample waiting for its duration (known once the next one starts).
    let mut pending: Option<(u32, Vec<u8>, bool)> = None;
    let mut frames = 0u32;
    let mut last_pointer = None;
    let mut last_encoded = 0u32;
    let mut t = 0u32;
    let mut first = true;
    player.seek(0)?;

    while t <= duration {
        let damage = player.advance_to(t)?;
        let pointer = if opts.draw_pointer { player.pointer } else { None };
        let changed = first || !damage.is_empty() || pointer != last_pointer || t - last_encoded >= MAX_FRAME_MS;
        if changed {
            compose(&player, w, h, pointer, &mut canvas);
            yuv.read_rgba8(RgbaSliceU8::new(&canvas, (w as usize, h as usize)));
            let bits = encoder.encode_at(&yuv, openh264::Timestamp::from_millis(t as u64))?;
            let (sample, sync, sps, pps) = to_avcc(&bits);
            if !track_added {
                let (Some(sps), Some(pps)) = (sps, pps) else { bail!("the encoder produced no stream headers") };
                writer.add_track(&mp4::TrackConfig {
                    track_type: mp4::TrackType::Video,
                    timescale: 1000,
                    language: "und".into(),
                    media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                        width: w as u16,
                        height: h as u16,
                        seq_param_set: sps,
                        pic_param_set: pps,
                    }),
                })?;
                track_added = true;
            }
            if !sample.is_empty() {
                if let Some((start, bytes, is_sync)) = pending.take() {
                    write_sample(&mut writer, start, t - start, bytes, is_sync)?;
                }
                pending = Some((t, sample, sync));
                frames += 1;
            }
            last_encoded = t;
            last_pointer = pointer;
            first = false;
            if !progress(t as f32 / duration as f32) {
                bail!("export cancelled");
            }
        }
        if t == duration {
            break;
        }
        t = (t + step).min(duration);
    }
    if let Some((start, bytes, is_sync)) = pending.take() {
        write_sample(&mut writer, start, (duration - start).max(step), bytes, is_sync)?;
    }
    ensure!(frames > 0, "nothing to export");
    writer.write_end()?;
    let mut file = writer.into_writer();
    std::io::Write::flush(&mut file)?;
    drop(file);
    progress(1.0);
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    Ok(Mp4Summary { width: w, height: h, frames, duration_ms: duration, bytes })
}

/// About 0.08 bits per pixel at 30 fps: 1080p lands near 5 Mbit/s. Screen
/// content needs far less than camera video for the same quality.
fn default_bitrate(w: u32, h: u32) -> u32 {
    ((w as u64 * h as u64 * 30 * 8 / 100) as u32).clamp(1_000_000, 20_000_000)
}

fn write_sample(w: &mut mp4::Mp4Writer<BufWriter<File>>, start: u32, duration: u32, bytes: Vec<u8>, is_sync: bool) -> anyhow::Result<()> {
    w.write_sample(
        1,
        &mp4::Mp4Sample { start_time: start as u64, duration: duration.max(1), rendering_offset: 0, is_sync, bytes: bytes.into() },
    )?;
    Ok(())
}

/// Copy the current frame into the even-sized canvas (padding or cropping
/// if the remote size changed during the recording) and draw the pointer.
fn compose(player: &Player, w: u32, h: u32, pointer: Option<(u32, u32)>, canvas: &mut [u8]) {
    let src = player.frame.as_bytes();
    let (sw, sh) = (player.frame.width(), player.frame.height());
    if sw == w && sh == h {
        canvas.copy_from_slice(src);
    } else {
        canvas.fill(0);
        let cw = sw.min(w) as usize * 4;
        for y in 0..sh.min(h) as usize {
            let s = y * sw as usize * 4;
            let d = y * w as usize * 4;
            canvas[d..d + cw].copy_from_slice(&src[s..s + cw]);
        }
    }
    if let Some((px, py)) = pointer {
        draw_arrow(canvas, w, h, px as i32, py as i32);
    }
}

/// A classic arrow pointer, white with a black outline, tip at (x, y).
fn draw_arrow(canvas: &mut [u8], w: u32, h: u32, x: i32, y: i32) {
    const ARROW: [&str; 17] = [
        "X",
        "XX",
        "X.X",
        "X..X",
        "X...X",
        "X....X",
        "X.....X",
        "X......X",
        "X.......X",
        "X........X",
        "X.....XXXXX",
        "X..X..X",
        "X.X X..X",
        "XX  X..X",
        "X    X..X",
        "     X..X",
        "      XX",
    ];
    for (dy, row) in ARROW.iter().enumerate() {
        for (dx, c) in row.bytes().enumerate() {
            let color = match c {
                b'X' => [0, 0, 0, 255],
                b'.' => [255, 255, 255, 255],
                _ => continue,
            };
            let (px, py) = (x + dx as i32, y + dy as i32);
            if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                let i = ((py as u32 * w + px as u32) * 4) as usize;
                canvas[i..i + 4].copy_from_slice(&color);
            }
        }
    }
}

/// Annex B NAL units (start codes) → one AVCC sample (4-byte lengths), with
/// SPS and PPS split out for the track header.
fn to_avcc(bits: &openh264::encoder::EncodedBitStream<'_>) -> (Vec<u8>, bool, Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sample = Vec::new();
    let (mut sync, mut sps, mut pps) = (false, None, None);
    for l in 0..bits.num_layers() {
        let Some(layer) = bits.layer(l) else { continue };
        for n in 0..layer.nal_count() {
            let Some(nal) = layer.nal_unit(n) else { continue };
            let body = strip_start_code(nal);
            let Some(&head) = body.first() else { continue };
            match head & 0x1F {
                7 => sps = Some(body.to_vec()),
                8 => pps = Some(body.to_vec()),
                t => {
                    sync |= t == 5;
                    sample.extend_from_slice(&(body.len() as u32).to_be_bytes());
                    sample.extend_from_slice(body);
                }
            }
        }
    }
    (sample, sync, sps, pps)
}

fn strip_start_code(nal: &[u8]) -> &[u8] {
    if nal.starts_with(&[0, 0, 0, 1]) {
        &nal[4..]
    } else if nal.starts_with(&[0, 0, 1]) {
        &nal[3..]
    } else {
        nal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Recorder;
    use peek_core::{Framebuffer, Rect};

    #[test]
    fn start_codes() {
        assert_eq!(strip_start_code(&[0, 0, 0, 1, 0x65, 9]), &[0x65, 9]);
        assert_eq!(strip_start_code(&[0, 0, 1, 0x67]), &[0x67]);
    }

    /// Record a few seconds of changes, export, and read the MP4 back.
    #[test]
    fn export_round_trip() {
        let dir = std::env::temp_dir().join(format!("peek-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rec = dir.join("t.peekrec");
        let out = dir.join("t.mp4");
        let mut fb = Framebuffer::new(321, 201); // odd size: must be evened
        fb.fill_rect(Rect::new(0, 0, 321, 201), 0xFF20_4060);
        let r = Recorder::start(&rec, &[("host", "test")], &fb).unwrap();
        for i in 0..12u32 {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let rect = Rect::new(i * 20, 40, 20, 20);
            r.region(rect, vec![0xFF; (20 * 20 * 4) as usize]);
            r.pointer(i * 20, 60);
        }
        r.stop().unwrap();

        let mut seen = Vec::new();
        let s = export_mp4(&rec, &out, Mp4Options::default(), |p| {
            seen.push(p);
            true
        })
        .unwrap();
        assert_eq!((s.width, s.height), (320, 200));
        assert!(s.frames >= 12, "{s:?}");
        assert_eq!(seen.last(), Some(&1.0));

        let f = File::open(&out).unwrap();
        let size = f.metadata().unwrap().len();
        let mp4 = mp4::Mp4Reader::read_header(std::io::BufReader::new(f), size).unwrap();
        let track = mp4.tracks().values().next().unwrap();
        assert_eq!((track.width(), track.height()), (320, 200));
        assert_eq!(track.sample_count(), s.frames);
        assert!(mp4.duration().as_millis() >= 1000, "{:?}", mp4.duration());

        // Cancelling removes the partial file.
        assert!(export_mp4(&rec, &out, Mp4Options::default(), |_| false).is_err());
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
