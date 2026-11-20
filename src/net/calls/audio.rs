//! Call audio: Opus 48 kHz mono in 20 ms frames, linear resampling to the
//! device rate, and `cpal` capture/playback on a dedicated thread.
//!
//! No acoustic echo cancellation yet: with loudspeakers the peer may hear
//! itself. Headphones avoid it (documented in the UI and `docs/protocol.md`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::jitter::{Frame, JitterBuffer};

pub const RATE: u32 = 48_000;
pub const FRAME: usize = 960; // 20 ms
const MAX_OPUS: usize = 1275;

/// State shared between the network engine and the audio callbacks.
#[derive(Debug, Default)]
pub struct Shared {
    pub jitter: Mutex<JitterBuffer>,
    pub muted: AtomicBool,
    /// Peak of the last captured frame (0..=32767), for a level meter.
    pub mic_level: AtomicU32,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

pub struct OpusEncoder(opus::Encoder);

impl OpusEncoder {
    pub fn new() -> Option<Self> {
        let mut e = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).ok()?;
        let _ = e.set_inband_fec(true);
        let _ = e.set_packet_loss_perc(10);
        let _ = e.set_bitrate(opus::Bitrate::Bits(32_000));
        Some(Self(e))
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Option<Vec<u8>> {
        if pcm.len() != FRAME {
            return None;
        }
        let mut out = vec![0u8; MAX_OPUS];
        let n = self.0.encode(pcm, &mut out).ok()?;
        out.truncate(n);
        Some(out)
    }
}

pub struct OpusDecoder(opus::Decoder);

impl OpusDecoder {
    pub fn new() -> Option<Self> {
        opus::Decoder::new(RATE, opus::Channels::Mono).ok().map(Self)
    }

    /// Decode one jitter-buffer slot into exactly [`FRAME`] samples.
    pub fn decode(&mut self, frame: &Frame) -> Vec<i16> {
        let mut out = vec![0i16; FRAME];
        let ok = match frame {
            Frame::Packet(p) => self.0.decode(p, &mut out, false).ok(),
            Frame::Lost { next: Some(n) } => self.0.decode(n, &mut out, true).ok(),
            Frame::Lost { next: None } => self.0.decode(&[], &mut out, false).ok(),
            Frame::Silence => Some(FRAME),
        };
        if ok != Some(FRAME) {
            out.iter_mut().for_each(|s| *s = 0);
        }
        out
    }
}

/// Streaming linear resampler (mono).
#[derive(Debug, Clone)]
pub struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from.max(1)) / f64::from(to.max(1)),
            pos: 0.0,
            prev: 0.0,
        }
    }

    /// Resample `input`, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < f64::EPSILON {
            out.extend_from_slice(input);
            return;
        }
        // Sample k of the virtual signal: k = 0 is `prev`, k >= 1 is
        // `input[k - 1]`; `pos` interpolates between k and k + 1.
        let at = |k: usize| if k == 0 { self.prev } else { input[k - 1] };
        let n = input.len() as f64;
        while self.pos < n {
            let k = self.pos.floor();
            let frac = (self.pos - k) as f32;
            let k = k as usize;
            let (a, b) = (at(k), at(k + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        if let Some(&last) = input.last() {
            self.prev = last;
        }
    }
}

/// Mixes interleaved device samples to mono 48 kHz frames and encodes them.
pub struct CapturePipeline {
    channels: usize,
    resampler: Resampler,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    pending: Vec<i16>,
    encoder: OpusEncoder,
}

impl CapturePipeline {
    pub fn new(device_rate: u32, channels: u16) -> Option<Self> {
        Some(Self {
            channels: usize::from(channels.max(1)),
            resampler: Resampler::new(device_rate, RATE),
            mono: Vec::new(),
            resampled: Vec::new(),
            pending: Vec::with_capacity(FRAME * 2),
            encoder: OpusEncoder::new()?,
        })
    }

    /// Feed interleaved samples; every complete 20 ms frame is encoded and
    /// passed to `emit`. `muted` sends silence (the stream keeps running, as
    /// in the official apps).
    pub fn push(&mut self, data: &[f32], muted: bool, shared: &Shared, mut emit: impl FnMut(Vec<u8>)) {
        self.mono.clear();
        self.mono.extend(
            data.chunks(self.channels)
                .map(|c| c.iter().sum::<f32>() / c.len() as f32),
        );
        self.resampled.clear();
        self.resampler.process(&self.mono, &mut self.resampled);
        for &s in &self.resampled {
            let v = if muted { 0.0 } else { s };
            self.pending.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            if self.pending.len() == FRAME {
                let peak = self.pending.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                shared.mic_level.store(u32::from(peak), Ordering::Relaxed);
                if let Some(p) = self.encoder.encode(&self.pending) {
                    emit(p);
                }
                self.pending.clear();
            }
        }
    }
}

/// Pulls frames from the jitter buffer, decodes and resamples them to the
/// device format.
pub struct PlaybackPipeline {
    channels: usize,
    resampler: Resampler,
    decoder: OpusDecoder,
    queue: std::collections::VecDeque<f32>,
    scratch: Vec<f32>,
}

impl PlaybackPipeline {
    pub fn new(device_rate: u32, channels: u16) -> Option<Self> {
        Some(Self {
            channels: usize::from(channels.max(1)),
            resampler: Resampler::new(RATE, device_rate),
            decoder: OpusDecoder::new()?,
            queue: std::collections::VecDeque::new(),
            scratch: Vec::new(),
        })
    }

    /// Fill an interleaved output buffer.
    pub fn fill(&mut self, out: &mut [f32], shared: &Shared) {
        let frames = out.len() / self.channels;
        while self.queue.len() < frames {
            let frame = match shared.jitter.lock() {
                Ok(mut j) => j.pop(),
                Err(_) => Frame::Silence,
            };
            let pcm = self.decoder.decode(&frame);
            let f: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
            self.scratch.clear();
            self.resampler.process(&f, &mut self.scratch);
            self.queue.extend(self.scratch.iter().copied());
        }
        for chunk in out.chunks_mut(self.channels) {
            let v = self.queue.pop_front().unwrap_or(0.0);
            chunk.iter_mut().for_each(|s| *s = v);
        }
    }
}

// ---------------------------------------------------------------------------
// Devices (cpal)

/// Input and output device names of the default host.
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let names = |it: Result<cpal::InputDevices<cpal::Devices>, cpal::DevicesError>| -> Vec<String> {
        it.map(|d| d.filter_map(|x| x.name().ok()).take(32).collect())
            .unwrap_or_default()
    };
    let inputs = names(host.input_devices());
    let outputs = host
        .output_devices()
        .map(|d| d.filter_map(|x| x.name().ok()).take(32).collect())
        .unwrap_or_default();
    (inputs, outputs)
}

/// Keeps the capture and playback streams alive on their own thread.
pub struct AudioHandle {
    stop: Option<std_mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for AudioHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioHandle")
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioHandle {
    /// Open the devices (`None` = system default) and start streaming:
    /// captured Opus packets go to `out`, playback reads `shared.jitter`.
    pub fn start(
        input: Option<String>,
        output: Option<String>,
        shared: Arc<Shared>,
        out: mpsc::Sender<Vec<u8>>,
    ) -> Result<Self, String> {
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("ferrogram-call-audio".into())
            .spawn(move || {
                let streams = open_streams(input.as_deref(), output.as_deref(), shared, out);
                match streams {
                    Ok(s) => {
                        let _ = ready_tx.send(Ok(()));
                        // Park until the handle is dropped.
                        let _ = stop_rx.recv();
                        drop(s);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| format!("audio thread: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("audio thread died".into()),
        }
    }
}

fn find_device(
    devices: impl Iterator<Item = cpal::Device>,
    name: Option<&str>,
) -> Option<cpal::Device> {
    use cpal::traits::DeviceTrait;
    let name = name?;
    devices
        .take(64)
        .find(|d| d.name().is_ok_and(|n| n == name))
}

fn open_streams(
    input: Option<&str>,
    output: Option<&str>,
    shared: Arc<Shared>,
    out: mpsc::Sender<Vec<u8>>,
) -> Result<(Option<cpal::Stream>, Option<cpal::Stream>), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let in_dev = host
        .input_devices()
        .ok()
        .and_then(|d| find_device(d, input))
        .or_else(|| host.default_input_device());
    let out_dev = host
        .output_devices()
        .ok()
        .and_then(|d| find_device(d, output))
        .or_else(|| host.default_output_device());
    if in_dev.is_none() && out_dev.is_none() {
        return Err("no audio devices".into());
    }
    let err_fn = |e: cpal::StreamError| warn!(error = %e, "audio stream error");

    let capture = in_dev.and_then(|dev| {
        let cfg = dev.default_input_config().ok()?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels();
        let mut pipe = CapturePipeline::new(rate, channels)?;
        let shared = Arc::clone(&shared);
        let out = out.clone();
        let mut buf: Vec<f32> = Vec::new();
        let stream_cfg: cpal::StreamConfig = cfg.clone().into();
        let mut on_data = move |data: &[f32]| {
            let muted = shared.muted.load(Ordering::Relaxed);
            pipe.push(data, muted, &shared, |p| {
                let _ = out.try_send(p);
            });
        };
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_input_stream(
                &stream_cfg,
                move |d: &[f32], _: &_| on_data(d),
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => dev.build_input_stream(
                &stream_cfg,
                move |d: &[i16], _: &_| {
                    buf.clear();
                    buf.extend(d.iter().map(|&s| f32::from(s) / 32768.0));
                    on_data(&buf);
                },
                err_fn,
                None,
            ),
            other => {
                warn!(?other, "unsupported capture sample format");
                return None;
            }
        }
        .ok()?;
        stream.play().ok()?;
        debug!(rate, channels, "capture started");
        Some(stream)
    });

    let playback = out_dev.and_then(|dev| {
        let cfg = dev.default_output_config().ok()?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels();
        let mut pipe = PlaybackPipeline::new(rate, channels)?;
        let shared = Arc::clone(&shared);
        let stream_cfg: cpal::StreamConfig = cfg.clone().into();
        let mut buf: Vec<f32> = Vec::new();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_output_stream(
                &stream_cfg,
                move |d: &mut [f32], _: &_| pipe.fill(d, &shared),
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => dev.build_output_stream(
                &stream_cfg,
                move |d: &mut [i16], _: &_| {
                    buf.resize(d.len(), 0.0);
                    pipe.fill(&mut buf, &shared);
                    for (o, s) in d.iter_mut().zip(&buf) {
                        *o = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                err_fn,
                None,
            ),
            other => {
                warn!(?other, "unsupported playback sample format");
                return None;
            }
        }
        .ok()?;
        stream.play().ok()?;
        debug!(rate, channels, "playback started");
        Some(stream)
    });

    if capture.is_none() && playback.is_none() {
        return Err("could not open audio devices".into());
    }
    Ok((capture, playback))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sine(freq: f32, rate: u32, n: usize, phase: &mut f32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                let v = (*phase * std::f32::consts::TAU).sin() * 0.5;
                *phase = (*phase + freq / rate as f32).fract();
                v
            })
            .collect()
    }

    #[test]
    fn resampler_lengths() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (16_000, 48_000), (48_000, 48_000)] {
            let mut r = Resampler::new(from, to);
            let mut out = Vec::new();
            for _ in 0..100 {
                r.process(&[0.25; 441], &mut out);
            }
            let expect = 44_100.0 * f64::from(to) / f64::from(from);
            assert!((out.len() as f64 - expect).abs() < 3.0, "{from}->{to}: {}", out.len());
            assert!(out.iter().all(|v| (v - 0.25).abs() < 0.26));
        }
    }

    #[test]
    fn capture_to_playback_keeps_the_tone() {
        let shared = Shared::new();
        let mut cap = CapturePipeline::new(44_100, 2).unwrap();
        let mut phase = 0.0;
        let mut packets = Vec::new();
        for _ in 0..50 {
            let mono = sine(440.0, 44_100, 441, &mut phase);
            let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
            cap.push(&stereo, false, &shared, |p| packets.push(p));
        }
        assert!(packets.len() >= 22, "{}", packets.len());
        assert!(shared.mic_level.load(Ordering::Relaxed) > 10_000);
        {
            let mut j = shared.jitter.lock().unwrap();
            for (i, p) in packets.into_iter().enumerate() {
                j.push(i as u16, p);
            }
        }
        let mut play = PlaybackPipeline::new(48_000, 1).unwrap();
        let mut out = vec![0.0f32; 960 * 20];
        play.fill(&mut out, &shared);
        // Skip codec warm-up; the tone must come through with real energy.
        let rms = (out[4800..].iter().map(|s| s * s).sum::<f32>() / (out.len() - 4800) as f32).sqrt();
        assert!(rms > 0.15, "rms {rms}");
    }

    #[test]
    fn muted_capture_sends_silence() {
        let shared = Shared::new();
        let mut cap = CapturePipeline::new(48_000, 1).unwrap();
        let mut phase = 0.0;
        let mut n = 0;
        for _ in 0..10 {
            cap.push(&sine(440.0, 48_000, 960, &mut phase), true, &shared, |_| n += 1);
        }
        assert_eq!(n, 10);
        assert_eq!(shared.mic_level.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn decoder_survives_garbage() {
        let mut d = OpusDecoder::new().unwrap();
        for f in [
            Frame::Packet(vec![0xff; 3]),
            Frame::Packet(vec![]),
            Frame::Lost { next: Some(vec![1, 2, 3]) },
            Frame::Lost { next: None },
            Frame::Silence,
        ] {
            assert_eq!(d.decode(&f).len(), FRAME);
        }
    }
}
