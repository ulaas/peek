//! RDP audio output (rdpsnd): decodes what the server sends, plays it
//! through [`peek_audio::AudioOut`] and copies it to the session's audio tap
//! (recording).
//!
//! Offered formats: Opus 48 kHz (about 64 kbit/s instead of 1.5 Mbit/s for
//! PCM), PCM 48 kHz and PCM 44.1 kHz, all stereo 16-bit. Opus comes first
//! unless the connection is a LAN, where PCM costs nothing and sounds best.

use ironrdp::rdpsnd::client::RdpsndClientHandler;
use ironrdp::rdpsnd::pdu::{AudioFormat, AudioFormatFlags, PitchPdu, VolumePdu, WaveFormat};
use peek_audio::AudioOut;
use peek_core::{AudioChunk, AudioControl};
use std::borrow::Cow;
use std::sync::Arc;

pub struct SessionSound {
    control: Arc<AudioControl>,
    formats: Vec<AudioFormat>,
    out: Option<AudioOut>,
    open_failed: bool,
    opus: Option<(usize, opus2::Decoder)>,
    /// Volume set on the remote computer, 0.0 ..= 1.0.
    server_gain: f32,
}

impl std::fmt::Debug for SessionSound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSound").field("formats", &self.formats.len()).field("open", &self.out.is_some()).finish()
    }
}

fn pcm(rate: u32) -> AudioFormat {
    AudioFormat {
        format: WaveFormat::PCM,
        n_channels: 2,
        n_samples_per_sec: rate,
        n_avg_bytes_per_sec: rate * 4,
        n_block_align: 4,
        bits_per_sample: 16,
        data: None,
    }
}

fn opus() -> AudioFormat {
    AudioFormat { format: WaveFormat::OPUS, ..pcm(48_000) }
}

impl SessionSound {
    pub fn new(control: Arc<AudioControl>, prefer_opus: bool) -> Self {
        let formats = if prefer_opus { vec![opus(), pcm(48_000), pcm(44_100)] } else { vec![pcm(48_000), pcm(44_100), opus()] };
        Self { control, formats, out: None, open_failed: false, opus: None, server_gain: 1.0 }
    }

    fn decode(&mut self, format_no: usize, data: &[u8]) -> Option<AudioChunk> {
        let f = self.formats.get(format_no)?.clone();
        let samples: Vec<i16> = match (f.format, f.bits_per_sample) {
            (WaveFormat::PCM, 16) => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(),
            (WaveFormat::PCM, 8) => data.iter().map(|&b| ((b as i16) - 128) << 8).collect(),
            (WaveFormat::OPUS, _) => {
                if self.opus.as_ref().is_none_or(|(n, _)| *n != format_no) {
                    let ch = if f.n_channels == 1 { opus2::Channels::Mono } else { opus2::Channels::Stereo };
                    match opus2::Decoder::new(f.n_samples_per_sec, ch) {
                        Ok(d) => self.opus = Some((format_no, d)),
                        Err(e) => {
                            tracing::warn!("Opus decoder: {e}");
                            return None;
                        }
                    }
                }
                let (_, dec) = self.opus.as_mut()?;
                let frames = dec.get_nb_samples(data).ok()?;
                let mut buf = vec![0i16; frames * f.n_channels as usize];
                // A bad packet (server restarting audio) is skipped, not fatal.
                let n = dec.decode(data, &mut buf, false).ok()?;
                buf.truncate(n * f.n_channels as usize);
                buf
            }
            (other, bits) => {
                tracing::debug!(?other, bits, "unsupported audio format");
                return None;
            }
        };
        Some(AudioChunk { rate: f.n_samples_per_sec, channels: f.n_channels, samples })
    }
}

impl RdpsndClientHandler for SessionSound {
    fn get_flags(&self) -> AudioFormatFlags {
        AudioFormatFlags::VOLUME
    }

    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn wave(&mut self, format_no: usize, _ts: u32, data: Cow<'_, [u8]>) {
        let Some(chunk) = self.decode(format_no, &data) else { return };
        self.control.feed_tap(&chunk);

        let gain = self.control.gain() * self.server_gain;
        if self.out.is_none() && !self.open_failed {
            match AudioOut::open() {
                Ok(out) => self.out = Some(out),
                Err(e) => {
                    // No device: keep the session (and recordings) going without sound.
                    tracing::warn!("no audio output: {e:#}");
                    self.open_failed = true;
                }
            }
        }
        let Some(out) = &self.out else { return };
        out.set_volume(gain);
        if gain <= 0.0 {
            out.clear();
            return;
        }
        out.push(&chunk.samples, chunk.rate, chunk.channels);
        if chunk.samples.iter().any(|s| s.unsigned_abs() > 64) {
            self.control.heard();
        }
    }

    fn set_volume(&mut self, volume: VolumePdu) {
        let avg = (volume.volume_left as f32 + volume.volume_right as f32) / 2.0 / 65535.0;
        self.server_gain = avg.clamp(0.0, 1.0);
        tracing::debug!(gain = self.server_gain, "remote volume");
    }

    fn set_pitch(&mut self, _pitch: PitchPdu) {}

    fn close(&mut self) {
        if let Some(out) = &self.out {
            out.clear();
        }
        self.opus = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_decodes_and_feeds_the_tap() {
        let control = Arc::new(AudioControl::default());
        control.set_muted(true); // no device needed while muted
        let (tx, rx) = std::sync::mpsc::channel();
        control.set_tap(Some(tx));
        let mut s = SessionSound::new(control, false);
        let data: Vec<u8> = [1000i16, -1000, 2000, -2000].iter().flat_map(|v| v.to_le_bytes()).collect();
        s.wave(0, 0, Cow::Borrowed(&data));
        let chunk = rx.try_recv().unwrap();
        assert_eq!((chunk.rate, chunk.channels), (48_000, 2));
        assert_eq!(chunk.samples, vec![1000, -1000, 2000, -2000]);
    }

    #[test]
    fn opus_round_trip() {
        let mut enc = opus2::Encoder::new(48_000, opus2::Channels::Stereo, opus2::Application::Audio).unwrap();
        let pcm: Vec<i16> = (0..960 * 2).map(|i| ((i as f32 * 0.03).sin() * 8000.0) as i16).collect();
        let mut packet = vec![0u8; 4000];
        let n = enc.encode(&pcm, &mut packet).unwrap();
        let mut s = SessionSound::new(Arc::new(AudioControl::default()), true);
        let chunk = s.decode(0, &packet[..n]).unwrap();
        assert_eq!(chunk.samples.len(), 960 * 2);
    }

    #[test]
    fn remote_volume_scales() {
        let mut s = SessionSound::new(Arc::new(AudioControl::default()), true);
        s.set_volume(VolumePdu { volume_left: 0x8000, volume_right: 0x8000 });
        assert!((s.server_gain - 0.5).abs() < 0.01);
    }
}
