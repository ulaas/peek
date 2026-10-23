//! RDP audio output: Peek's own `rdpsnd` static channel.
//!
//! IronRDP ships an rdpsnd client, but it is not usable as is: it answers the
//! server's format list in hash-set order (random per run) while handing the
//! server's index into *that* list to a handler that only knows its own
//! list, so PCM could be decoded as Opus and play as silence; and any
//! message it does not expect (the pre-v8 `WaveInfo` + `Wave` pair, for one)
//! stops sound for the rest of the session. This channel answers with the
//! formats it can play in the server's order, decodes every wave with the
//! format the server actually means, accepts both wave message styles and
//! ignores what it does not know.
//!
//! Decoded audio plays through [`peek_audio::AudioOut`] and is copied to the
//! session's audio tap (recording).

use ironrdp::core::{Decode as _, ReadCursor, impl_as_any};
use ironrdp::pdu::PduResult;
use ironrdp::pdu::gcc::ChannelName;
use ironrdp::rdpsnd::pdu::{
    AudioFormat, AudioFormatFlags, ClientAudioFormatPdu, ClientAudioOutputPdu, QualityMode,
    QualityModePdu, ServerAudioOutputPdu, TrainingConfirmPdu, Version, VolumePdu, WaveConfirmPdu,
    WaveFormat,
};
use ironrdp::svc::{CompressionCondition, SvcClientProcessor, SvcMessage, SvcProcessor};
use peek_audio::AudioOut;
use peek_core::{AudioChunk, AudioControl};
use std::sync::Arc;

/// Opus decoders exist for these rates only.
const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Decodes server audio and plays it.
pub struct SessionSound {
    control: Arc<AudioControl>,
    prefer_opus: bool,
    out: Option<AudioOut>,
    open_failed: bool,
    opus: Option<((u32, u16), opus2::Decoder)>,
    /// Volume set on the remote computer, 0.0 ..= 1.0.
    server_gain: f32,
    /// Waves played and waves that could not be decoded (for the log).
    played: u64,
    failed: u64,
}

impl SessionSound {
    pub fn new(control: Arc<AudioControl>, prefer_opus: bool) -> Self {
        Self {
            control,
            prefer_opus,
            out: None,
            open_failed: false,
            opus: None,
            server_gain: 1.0,
            played: 0,
            failed: 0,
        }
    }

    fn supports(f: &AudioFormat) -> bool {
        let channels_ok = matches!(f.n_channels, 1 | 2);
        match f.format {
            WaveFormat::PCM => {
                channels_ok && matches!(f.bits_per_sample, 8 | 16) && f.n_samples_per_sec >= 8_000
            }
            WaveFormat::OPUS => channels_ok && OPUS_RATES.contains(&f.n_samples_per_sec),
            _ => false,
        }
    }

    /// The formats to answer with: what the server offers that Peek can
    /// play, in the server's order. Below LAN speeds, Opus alone if offered.
    fn choose(&self, server: &[AudioFormat]) -> Vec<AudioFormat> {
        let playable: Vec<AudioFormat> = server
            .iter()
            .filter(|f| Self::supports(f))
            .cloned()
            .collect();
        let opus: Vec<AudioFormat> = playable
            .iter()
            .filter(|f| f.format == WaveFormat::OPUS)
            .cloned()
            .collect();
        if self.prefer_opus && !opus.is_empty() {
            opus
        } else {
            playable
        }
    }

    fn decode(&mut self, f: &AudioFormat, data: &[u8]) -> Option<AudioChunk> {
        let samples: Vec<i16> = match (f.format, f.bits_per_sample) {
            (WaveFormat::PCM, 16) => data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect(),
            (WaveFormat::PCM, 8) => data.iter().map(|&b| ((b as i16) - 128) << 8).collect(),
            (WaveFormat::OPUS, _) => {
                let key = (f.n_samples_per_sec, f.n_channels);
                if self.opus.as_ref().is_none_or(|(k, _)| *k != key) {
                    let ch = if f.n_channels == 1 {
                        opus2::Channels::Mono
                    } else {
                        opus2::Channels::Stereo
                    };
                    self.opus = Some((key, opus2::Decoder::new(f.n_samples_per_sec, ch).ok()?));
                }
                let (_, dec) = self.opus.as_mut()?;
                let frames = dec.get_nb_samples(data).ok()?;
                let mut buf = vec![0i16; frames * f.n_channels as usize];
                let n = dec.decode(data, &mut buf, false).ok()?;
                buf.truncate(n * f.n_channels as usize);
                buf
            }
            _ => return None,
        };
        Some(AudioChunk {
            rate: f.n_samples_per_sec,
            channels: f.n_channels,
            samples,
        })
    }

    fn play(&mut self, format: Option<&AudioFormat>, data: &[u8]) {
        let chunk = format.and_then(|f| self.decode(f, data));
        let Some(chunk) = chunk else {
            self.failed += 1;
            if self.failed == 1 || self.failed.is_multiple_of(500) {
                tracing::warn!(
                    failed = self.failed,
                    ?format,
                    bytes = data.len(),
                    "audio: cannot decode wave"
                );
            }
            return;
        };
        if self.played == 0 {
            tracing::info!(
                rate = chunk.rate,
                channels = chunk.channels,
                bytes = data.len(),
                "audio: first wave"
            );
        }
        self.played += 1;
        self.control.feed_tap(&chunk);

        if self.out.is_none() && !self.open_failed {
            match AudioOut::open() {
                Ok(out) => {
                    let (rate, ch) = out.device_format();
                    tracing::info!(rate, channels = ch, "audio: output opened");
                    self.out = Some(out);
                }
                Err(e) => {
                    // No device: keep the session (and recordings) going without sound.
                    tracing::warn!("audio: no output device: {e:#}");
                    self.open_failed = true;
                }
            }
        }
        let Some(out) = &self.out else { return };
        let gain = self.control.gain() * self.server_gain;
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
        tracing::debug!(gain = self.server_gain, "audio: remote volume");
    }

    fn close(&mut self) {
        if let Some(out) = &self.out {
            out.clear();
        }
        self.opus = None;
    }
}

/// The `rdpsnd` static virtual channel.
pub struct SoundChannel {
    sound: SessionSound,
    /// Formats agreed with the server; waves index into this list.
    agreed: Vec<AudioFormat>,
}

impl std::fmt::Debug for SoundChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundChannel")
            .field("agreed", &self.agreed.len())
            .field("played", &self.sound.played)
            .finish()
    }
}

impl_as_any!(SoundChannel);

impl SoundChannel {
    pub fn new(sound: SessionSound) -> Self {
        Self {
            sound,
            agreed: Vec::new(),
        }
    }

    fn reply(pdu: ClientAudioOutputPdu) -> SvcMessage {
        SvcMessage::from(pdu)
    }

    fn wave(
        &mut self,
        format_no: u16,
        data: &[u8],
        timestamp: u16,
        block_no: u8,
    ) -> Vec<SvcMessage> {
        let format = self.agreed.get(usize::from(format_no)).cloned();
        self.sound.play(format.as_ref(), data);
        vec![Self::reply(ClientAudioOutputPdu::WaveConfirm(
            WaveConfirmPdu {
                timestamp,
                block_no,
            },
        ))]
    }
}

impl SvcProcessor for SoundChannel {
    fn channel_name(&self) -> ChannelName {
        ChannelName::from_static(b"rdpsnd\0\0")
    }

    fn compression_condition(&self) -> CompressionCondition {
        CompressionCondition::Never
    }

    fn process(&mut self, payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        let pdu = match ServerAudioOutputPdu::decode(&mut ReadCursor::new(payload)) {
            Ok(p) => p,
            Err(e) => {
                // One odd message must not end sound for the session.
                tracing::warn!("audio: undecodable message ({} bytes): {e}", payload.len());
                return Ok(Vec::new());
            }
        };
        Ok(match pdu {
            ServerAudioOutputPdu::AudioFormat(af) => {
                self.sound.close();
                self.agreed = self.sound.choose(&af.formats);
                let offered: Vec<String> = af.formats.iter().map(describe).collect();
                let chosen: Vec<String> = self.agreed.iter().map(describe).collect();
                tracing::info!(version = ?af.version, ?offered, ?chosen, "audio: formats");
                let mut msgs = vec![Self::reply(ClientAudioOutputPdu::AudioFormat(
                    ClientAudioFormatPdu {
                        version: af.version,
                        flags: AudioFormatFlags::ALIVE | AudioFormatFlags::VOLUME,
                        formats: self.agreed.clone(),
                        volume_left: 0xFFFF,
                        volume_right: 0xFFFF,
                        pitch: 0x0001_0000,
                        dgram_port: 0,
                    },
                ))];
                if af.version >= Version::V6 {
                    msgs.push(Self::reply(ClientAudioOutputPdu::QualityMode(
                        QualityModePdu {
                            quality_mode: QualityMode::High,
                        },
                    )));
                }
                msgs
            }
            ServerAudioOutputPdu::Training(t) => {
                let pack_size = u16::try_from(t.data.len()).unwrap_or(u16::MAX);
                vec![Self::reply(ClientAudioOutputPdu::TrainingConfirm(
                    TrainingConfirmPdu {
                        timestamp: t.timestamp,
                        pack_size,
                    },
                ))]
            }
            ServerAudioOutputPdu::Wave2(w) => {
                self.wave(w.format_no, &w.data, w.timestamp, w.block_no)
            }
            ServerAudioOutputPdu::Wave(w) => {
                self.wave(w.format_no, &w.data, w.timestamp, w.block_no)
            }
            ServerAudioOutputPdu::Volume(v) => {
                self.sound.set_volume(v);
                Vec::new()
            }
            ServerAudioOutputPdu::Close => {
                self.sound.close();
                Vec::new()
            }
            other => {
                tracing::debug!("audio: ignored {}", short(&format!("{other:?}")));
                Vec::new()
            }
        })
    }
}

impl SvcClientProcessor for SoundChannel {}

impl Drop for SoundChannel {
    fn drop(&mut self) {
        if self.sound.played > 0 || self.sound.failed > 0 {
            tracing::info!(
                played = self.sound.played,
                failed = self.sound.failed,
                "audio: channel closed"
            );
        }
    }
}

fn describe(f: &AudioFormat) -> String {
    let name = match f.format {
        WaveFormat::PCM => "PCM".to_string(),
        WaveFormat::OPUS => "Opus".to_string(),
        other => format!("{other:?}"),
    };
    format!(
        "{name} {} Hz {}ch {}bit",
        f.n_samples_per_sec, f.n_channels, f.bits_per_sample
    )
}

fn short(s: &str) -> &str {
    &s[..s.len().min(60)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp::core::encode_vec;
    use ironrdp::rdpsnd::pdu::{ServerAudioFormatPdu, TrainingPdu, Wave2Pdu};
    use std::borrow::Cow;

    fn pcm(rate: u32, ch: u16) -> AudioFormat {
        AudioFormat {
            format: WaveFormat::PCM,
            n_channels: ch,
            n_samples_per_sec: rate,
            n_avg_bytes_per_sec: rate * ch as u32 * 2,
            n_block_align: ch * 2,
            bits_per_sample: 16,
            data: None,
        }
    }

    fn adpcm() -> AudioFormat {
        AudioFormat {
            format: WaveFormat::ADPCM,
            n_channels: 2,
            n_samples_per_sec: 22_050,
            n_avg_bytes_per_sec: 22_311,
            n_block_align: 1024,
            bits_per_sample: 4,
            data: Some(vec![0; 32]),
        }
    }

    fn server(pdu: ServerAudioOutputPdu<'_>) -> Vec<u8> {
        encode_vec(&pdu).unwrap()
    }

    /// The whole handshake as a Windows server runs it, then a wave at index 1
    /// of the agreed list: it must be decoded with that format and reach the tap.
    #[test]
    fn handshake_and_wave_use_the_agreed_list() {
        let control = Arc::new(AudioControl::default());
        control.set_muted(true); // no sound device needed
        let (tx, rx) = std::sync::mpsc::channel();
        control.set_tap(Some(tx));
        let mut ch = SoundChannel::new(SessionSound::new(control, false));

        // Server offers ADPCM (unsupported), PCM 22.05 kHz, PCM 44.1 kHz.
        let offer = ServerAudioFormatPdu {
            version: Version::V8,
            formats: vec![adpcm(), pcm(22_050, 2), pcm(44_100, 2)],
        };
        let replies = ch
            .process(&server(ServerAudioOutputPdu::AudioFormat(offer)))
            .unwrap();
        assert_eq!(replies.len(), 2, "formats + quality mode");
        assert_eq!(
            ch.agreed,
            vec![pcm(22_050, 2), pcm(44_100, 2)],
            "server order, unsupported dropped"
        );

        let replies = ch
            .process(&server(ServerAudioOutputPdu::Training(TrainingPdu {
                timestamp: 7,
                data: vec![],
            })))
            .unwrap();
        assert_eq!(replies.len(), 1);

        let data: Vec<u8> = [100i16, -100, 200, -200]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let wave = Wave2Pdu {
            timestamp: 1,
            format_no: 1,
            block_no: 3,
            audio_timestamp: 0,
            data: Cow::Borrowed(&data),
        };
        let replies = ch
            .process(&server(ServerAudioOutputPdu::Wave2(wave)))
            .unwrap();
        assert_eq!(replies.len(), 1, "wave confirm");
        let chunk = rx.try_recv().unwrap();
        assert_eq!(
            (chunk.rate, chunk.channels),
            (44_100, 2),
            "format 1 of the agreed list"
        );
        assert_eq!(chunk.samples, vec![100, -100, 200, -200]);

        // Garbage does not stop the channel.
        assert!(ch.process(&[0xEE, 0, 1, 0, 9]).unwrap().is_empty());
        let wave = Wave2Pdu {
            timestamp: 2,
            format_no: 0,
            block_no: 4,
            audio_timestamp: 0,
            data: Cow::Borrowed(&data),
        };
        ch.process(&server(ServerAudioOutputPdu::Wave2(wave)))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap().rate, 22_050);
    }

    #[test]
    fn opus_preferred_only_when_asked_and_offered() {
        let opus = AudioFormat {
            format: WaveFormat::OPUS,
            ..pcm(48_000, 2)
        };
        let offer = [pcm(44_100, 2), opus.clone()];
        let s = SessionSound::new(Arc::new(AudioControl::default()), true);
        assert_eq!(s.choose(&offer), vec![opus.clone()]);
        let s = SessionSound::new(Arc::new(AudioControl::default()), false);
        assert_eq!(s.choose(&offer), offer.to_vec());
        let s = SessionSound::new(Arc::new(AudioControl::default()), true);
        assert_eq!(
            s.choose(&[pcm(44_100, 2)]),
            vec![pcm(44_100, 2)],
            "no Opus offered: PCM"
        );
    }

    #[test]
    fn opus_round_trip() {
        let mut enc =
            opus2::Encoder::new(48_000, opus2::Channels::Stereo, opus2::Application::Audio)
                .unwrap();
        let pcm_in: Vec<i16> = (0..960 * 2)
            .map(|i| ((i as f32 * 0.03).sin() * 8000.0) as i16)
            .collect();
        let mut packet = vec![0u8; 4000];
        let n = enc.encode(&pcm_in, &mut packet).unwrap();
        let mut s = SessionSound::new(Arc::new(AudioControl::default()), true);
        let opus = AudioFormat {
            format: WaveFormat::OPUS,
            ..pcm(48_000, 2)
        };
        assert_eq!(
            s.decode(&opus, &packet[..n]).unwrap().samples.len(),
            960 * 2
        );
    }

    #[test]
    fn remote_volume_scales() {
        let mut s = SessionSound::new(Arc::new(AudioControl::default()), true);
        s.set_volume(VolumePdu {
            volume_left: 0x8000,
            volume_right: 0x8000,
        });
        assert!((s.server_gain - 0.5).abs() < 0.01);
    }
}
