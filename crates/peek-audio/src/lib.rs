//! Audio output for remote sessions and recordings.
//!
//! Sources push 16-bit PCM at whatever rate and channel count they have;
//! [`AudioOut`] converts it to the output device's own format (the device is
//! never asked to switch rates, which many Windows devices refuse) and plays
//! it from a small queue. The device callback never blocks: an empty queue
//! plays silence. Latency stays low: playback starts after a short cushion,
//! and a queue that grows too long (after a network stall) is cut back.

use anyhow::{Context, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

/// Audio queued before playback starts (and restarts after running dry).
const CUSHION_MS: u32 = 60;
/// Queue length that triggers catching up, and what it is cut back to.
const MAX_QUEUE_MS: u32 = 300;
const CATCH_UP_TO_MS: u32 = 120;

struct Shared {
    queue: Mutex<Queue>,
    /// Linear gain as f32 bits.
    gain: AtomicU32,
    /// Something was audible recently (drives the speaker indicator).
    playing: AtomicBool,
    rate: u32,
    channels: u16,
}

struct Queue {
    samples: VecDeque<f32>,
    primed: bool,
    resampler: Resampler,
}

/// Plays PCM on the default output device. Cheap to clone; the device is
/// released when the last clone is dropped.
#[derive(Clone)]
pub struct AudioOut {
    shared: Arc<Shared>,
    _stream: Arc<StreamThread>,
}

impl AudioOut {
    /// Open the default output device in its default format.
    pub fn open() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        // cpal streams are not Send: the stream lives on its own thread.
        let thread = std::thread::Builder::new().name("peek-audio".into()).spawn(move || match build_stream() {
            Ok((stream, shared)) => {
                let _ = tx.send(Ok(shared));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        })?;
        let shared = rx.recv().map_err(|_| anyhow!("audio thread ended"))??;
        Ok(Self { shared, _stream: Arc::new(StreamThread { stop: Some(stop_tx), thread: Some(thread) }) })
    }

    /// Queue interleaved 16-bit samples.
    pub fn push(&self, samples: &[i16], rate: u32, channels: u16) {
        if rate == 0 || channels == 0 {
            return;
        }
        let s = &self.shared;
        let mut q = s.queue.lock();
        let Queue { samples: out, resampler, .. } = &mut *q;
        resampler.process(samples, rate, channels, s.rate, s.channels, out);
        let frames = out.len() / s.channels as usize;
        if frames > ms_to_frames(MAX_QUEUE_MS, s.rate) {
            let keep = ms_to_frames(CATCH_UP_TO_MS, s.rate) * s.channels as usize;
            let drop = out.len() - keep;
            out.drain(..drop);
        }
    }

    /// 0.0 ..= 1.0 (anything above is clamped); applies immediately.
    pub fn set_volume(&self, gain: f32) {
        self.shared.gain.store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Drop everything queued (seek, pause, mute).
    pub fn clear(&self) {
        let mut q = self.shared.queue.lock();
        q.samples.clear();
        q.primed = false;
        q.resampler.reset();
    }

    /// Queued audio in milliseconds.
    pub fn queued_ms(&self) -> u32 {
        let q = self.shared.queue.lock();
        (q.samples.len() as u64 * 1000 / (self.shared.rate as u64 * self.shared.channels as u64)) as u32
    }

    /// True while sound is coming out.
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn device_format(&self) -> (u32, u16) {
        (self.shared.rate, self.shared.channels)
    }
}

struct StreamThread {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn ms_to_frames(ms: u32, rate: u32) -> usize {
    (rate as u64 * ms as u64 / 1000) as usize
}

fn build_stream() -> anyhow::Result<(cpal::Stream, Arc<Shared>)> {
    let host = cpal::default_host();
    let device = host.default_output_device().context("no audio output device")?;
    let config = device.default_output_config().context("cannot read the output format")?;
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue { samples: VecDeque::new(), primed: false, resampler: Resampler::default() }),
        gain: AtomicU32::new(1.0f32.to_bits()),
        playing: AtomicBool::new(false),
        rate: config.sample_rate,
        channels: config.channels,
    });
    tracing::debug!(rate = config.sample_rate, channels = config.channels, ?format, "audio output");
    let err = |e| tracing::warn!("audio output: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(&config, writer::<f32>(shared.clone()), err, None),
        cpal::SampleFormat::I16 => device.build_output_stream(&config, writer::<i16>(shared.clone()), err, None),
        cpal::SampleFormat::U16 => device.build_output_stream(&config, writer::<u16>(shared.clone()), err, None),
        cpal::SampleFormat::I32 => device.build_output_stream(&config, writer::<i32>(shared.clone()), err, None),
        other => return Err(anyhow!("unsupported output sample format {other:?}")),
    }
    .context("cannot open the audio output")?;
    stream.play().context("cannot start audio playback")?;
    Ok((stream, shared))
}

/// The device callback: drain the queue, silence when it is short.
fn writer<T: cpal::SizedSample + cpal::FromSample<f32>>(
    shared: Arc<Shared>,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static {
    move |out: &mut [T], _| {
        let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
        let cushion = ms_to_frames(CUSHION_MS, shared.rate) * shared.channels as usize;
        // try_lock: never wait in the audio callback; a missed lock is one silent buffer.
        let mut audible = false;
        match shared.queue.try_lock() {
            Some(mut q) => {
                if !q.primed && q.samples.len() >= cushion {
                    q.primed = true;
                }
                for s in out.iter_mut() {
                    let v = if q.primed { q.samples.pop_front() } else { None };
                    if v.is_none() {
                        q.primed = false;
                    }
                    let v = v.unwrap_or(0.0) * gain;
                    audible |= v.abs() > 1e-4;
                    *s = T::from_sample(v);
                }
            }
            None => out.iter_mut().for_each(|s| *s = T::from_sample(0.0)),
        }
        shared.playing.store(audible, Ordering::Relaxed);
    }
}

/// Converts interleaved i16 at any rate / channel count to interleaved f32 at
/// the device's, with linear interpolation. Keeps its phase across calls so
/// consecutive packets join without clicks.
#[derive(Default)]
pub struct Resampler {
    /// Pending source frames, already in the output channel layout.
    frames: Vec<f32>,
    /// Read position in `frames`, in frames.
    pos: f64,
    from: (u32, u16),
}

impl Resampler {
    pub fn reset(&mut self) {
        self.frames.clear();
        self.pos = 0.0;
    }

    /// Convert to `out_rate` / `ch_out` as 16-bit samples.
    pub fn process_i16(&mut self, input: &[i16], rate: u32, ch_in: u16, out_rate: u32, ch_out: u16) -> Vec<i16> {
        let mut out = VecDeque::new();
        self.process(input, rate, ch_in, out_rate, ch_out, &mut out);
        out.into_iter().map(|v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).collect()
    }

    /// Convert interleaved `input` and append it to `out` (f32, -1.0 ..= 1.0).
    pub fn process(
        &mut self,
        input: &[i16],
        rate: u32,
        ch_in: u16,
        out_rate: u32,
        ch_out: u16,
        out: &mut VecDeque<f32>,
    ) {
        if self.from != (rate, ch_in) {
            self.reset();
            self.from = (rate, ch_in);
        }
        let (ci, co) = (ch_in as usize, ch_out as usize);
        for frame in input.chunks_exact(ci) {
            let f = |i: usize| frame[i] as f32 / 32768.0;
            for c in 0..co {
                let v = match (ci, c) {
                    (1, _) => f(0),
                    // Down-mix to mono.
                    (_, _) if co == 1 => frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ci as f32,
                    (_, c) if c < ci => f(c),
                    // More outputs than inputs (e.g. 5.1 device): front pair only.
                    _ => 0.0,
                };
                self.frames.push(v);
            }
        }
        let n = self.frames.len() / co;
        let step = rate as f64 / out_rate as f64;
        while self.pos + 1.0 < n as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..co {
                let a = self.frames[i * co + c];
                let b = self.frames[(i + 1) * co + c];
                out.push_back(a + (b - a) * t);
            }
            self.pos += step;
        }
        let used = (self.pos as usize).min(n);
        self.frames.drain(..used * co);
        self.pos -= used as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[i16], rate: u32, ch: u16, out_rate: u32, out_ch: u16, chunk: usize) -> Vec<f32> {
        let mut r = Resampler::default();
        let mut out = VecDeque::new();
        for c in input.chunks(chunk * ch as usize) {
            r.process(c, rate, ch, out_rate, out_ch, &mut out);
        }
        out.into_iter().collect()
    }

    #[test]
    fn rate_conversion_keeps_duration() {
        // One second of 44.1 kHz stereo becomes about one second at 48 kHz.
        let input = vec![1000i16; 44_100 * 2];
        let out = run(&input, 44_100, 2, 48_000, 2, 441);
        let frames = out.len() / 2;
        assert!((47_990..=48_000).contains(&frames), "{frames}");
        assert!(out.iter().all(|&v| (v - 1000.0 / 32768.0).abs() < 1e-6));
    }

    #[test]
    fn chunking_does_not_change_output() {
        let input: Vec<i16> = (0..9600).map(|i| ((i as f32 * 0.05).sin() * 20000.0) as i16).collect();
        let a = run(&input, 44_100, 2, 48_000, 2, 4800);
        let b = run(&input, 44_100, 2, 48_000, 2, 17);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5));
    }

    #[test]
    fn channel_mapping() {
        // Mono to stereo duplicates; stereo to mono averages.
        assert_eq!(run(&[16384, 16384], 48_000, 1, 48_000, 2, 2)[..2], [0.5, 0.5]);
        let m = run(&[16384, 0, 16384, 0, 16384, 0], 48_000, 2, 48_000, 1, 3);
        assert!((m[0] - 0.25).abs() < 1e-6, "{m:?}");
    }
}

#[cfg(test)]
mod device_tests {
    /// Needs a sound device; plays only silence. `cargo test -p peek-audio -- --ignored`
    #[test]
    #[ignore]
    fn opens_default_device() {
        let out = super::AudioOut::open().expect("open");
        let (rate, ch) = out.device_format();
        println!("device: {rate} Hz, {ch} channels");
        out.set_volume(0.0);
        out.push(&vec![0i16; 4410 * 2], 44_100, 2);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(out.queued_ms() < 100, "queue drains: {} ms left", out.queued_ms());
    }
}
