use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone)]
pub struct AudioSegment {
    pub source_rate: u32,
    pub samples: Vec<i16>,
}

#[derive(Debug)]
struct TimedSample {
    at: Instant,
    value: i16,
}

#[derive(Debug)]
pub struct RollingAudioBuffer {
    samples: Mutex<VecDeque<TimedSample>>,
    max_history: Mutex<Duration>,
    source_rate: Mutex<u32>,
}

impl RollingAudioBuffer {
    pub fn new(max_history: Duration) -> Self {
        Self {
            samples: Mutex::new(VecDeque::new()),
            max_history: Mutex::new(max_history),
            source_rate: Mutex::new(TARGET_SAMPLE_RATE),
        }
    }

    pub fn set_max_history(&self, max_history: Duration) {
        *self.max_history.lock() = max_history;
    }

    pub fn push_many(&self, source_rate: u32, values: &[i16]) {
        if values.is_empty() {
            return;
        }
        *self.source_rate.lock() = source_rate;
        let now = Instant::now();
        let frame_ns = 1_000_000_000f64 / source_rate.max(1) as f64;
        let mut samples = self.samples.lock();
        for (idx, value) in values.iter().enumerate() {
            let from_end = values.len().saturating_sub(idx + 1) as f64;
            let at = now - Duration::from_nanos((from_end * frame_ns) as u64);
            samples.push_back(TimedSample { at, value: *value });
        }
        let min_at = now - *self.max_history.lock();
        while samples.front().is_some_and(|s| s.at < min_at) {
            samples.pop_front();
        }
    }

    pub fn extract(&self, start: Instant, end: Instant) -> AudioSegment {
        let samples = self.samples.lock();
        let values = samples
            .iter()
            .filter(|s| s.at >= start && s.at <= end)
            .map(|s| s.value)
            .collect();
        AudioSegment {
            source_rate: *self.source_rate.lock(),
            samples: values,
        }
    }
}

pub struct AudioManager {
    buffer: Arc<RollingAudioBuffer>,
    stop_tx: Mutex<Option<mpsc::Sender<()>>>,
    running: AtomicBool,
    status: Arc<Mutex<String>>,
}

impl AudioManager {
    pub fn new(history: Duration) -> Self {
        Self {
            buffer: Arc::new(RollingAudioBuffer::new(history)),
            stop_tx: Mutex::new(None),
            running: AtomicBool::new(false),
            status: Arc::new(Mutex::new("idle".to_string())),
        }
    }

    pub fn buffer(&self) -> Arc<RollingAudioBuffer> {
        self.buffer.clone()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> String {
        self.status.lock().clone()
    }

    pub fn start(&self, device_name: &str) -> anyhow::Result<()> {
        if self.is_running() {
            return Ok(());
        }

        let buffer = self.buffer.clone();
        let status = self.status.clone();
        let device_name = device_name.trim().to_string();
        let (stop_tx, stop_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();

        thread::Builder::new()
            .name("voice-keyboard-audio".to_string())
            .spawn(move || {
                let init_error_tx = init_tx.clone();
                let result =
                    run_audio_thread(buffer, status.clone(), stop_rx, init_tx, device_name);
                if let Err(err) = result {
                    let message = format!("audio stopped: {err:#}");
                    *status.lock() = message.clone();
                    let _ = init_error_tx.send(Err(message));
                }
            })?;

        match init_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(message)) => {
                *self.status.lock() = message;
            }
            Ok(Err(err)) => return Err(anyhow!(err)),
            Err(_) => return Err(anyhow!("audio stream did not initialize")),
        }
        *self.stop_tx.lock() = Some(stop_tx);
        self.running.store(true, Ordering::Relaxed);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        if let Some(tx) = self.stop_tx.lock().take() {
            let _ = tx.send(());
        }
        self.running.store(false, Ordering::Relaxed);
        *self.status.lock() = "stopped".to_string();
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioInputDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn input_devices() -> anyhow::Result<Vec<AudioInputDevice>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let mut devices = Vec::new();
    for device in host.input_devices()? {
        let name = device
            .name()
            .unwrap_or_else(|_| "Unknown microphone".to_string());
        if devices
            .iter()
            .any(|existing: &AudioInputDevice| existing.name == name)
        {
            continue;
        }
        devices.push(AudioInputDevice {
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
        });
    }
    devices.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(devices)
}

fn run_audio_thread(
    buffer: Arc<RollingAudioBuffer>,
    status: Arc<Mutex<String>>,
    stop_rx: mpsc::Receiver<()>,
    init_tx: mpsc::Sender<Result<String, String>>,
    selected_device_name: String,
) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = if selected_device_name.is_empty() {
        host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?
    } else {
        host.input_devices()?
            .find(|device| {
                device
                    .name()
                    .map(|name| name == selected_device_name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("selected input device not found: {selected_device_name}"))?
    };
    let device_name = device
        .name()
        .unwrap_or_else(|_| "default microphone".to_string());
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config = supported.config();
    let err_fn = {
        let status = status.clone();
        move |err| {
            *status.lock() = format!("audio stream error: {err}");
        }
    };

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| push_f32(&buffer, sample_rate, channels, data),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| push_i16(&buffer, sample_rate, channels, data),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| push_u16(&buffer, sample_rate, channels, data),
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    };

    stream.play().context("failed to start microphone stream")?;
    let message = format!("{device_name}, {sample_rate} Hz, {channels} channel(s)");
    *status.lock() = message.clone();
    let _ = init_tx.send(Ok(message));
    let _ = stop_rx.recv();
    drop(stream);
    Ok(())
}

fn push_f32(buffer: &RollingAudioBuffer, sample_rate: u32, channels: usize, data: &[f32]) {
    let samples = downmix(data.chunks(channels.max(1)).map(|frame| {
        let sum: f32 = frame.iter().copied().sum();
        sum / frame.len().max(1) as f32
    }));
    buffer.push_many(sample_rate, &samples);
}

fn push_i16(buffer: &RollingAudioBuffer, sample_rate: u32, channels: usize, data: &[i16]) {
    let samples = downmix(data.chunks(channels.max(1)).map(|frame| {
        let sum: f32 = frame.iter().map(|v| *v as f32 / i16::MAX as f32).sum();
        sum / frame.len().max(1) as f32
    }));
    buffer.push_many(sample_rate, &samples);
}

fn push_u16(buffer: &RollingAudioBuffer, sample_rate: u32, channels: usize, data: &[u16]) {
    let samples = downmix(data.chunks(channels.max(1)).map(|frame| {
        let sum: f32 = frame.iter().map(|v| (*v as f32 - 32768.0) / 32768.0).sum();
        sum / frame.len().max(1) as f32
    }));
    buffer.push_many(sample_rate, &samples);
}

fn downmix(values: impl Iterator<Item = f32>) -> Vec<i16> {
    values
        .map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

pub fn to_mono_16k(segment: &AudioSegment) -> Vec<i16> {
    if segment.samples.is_empty() || segment.source_rate == TARGET_SAMPLE_RATE {
        return segment.samples.clone();
    }
    let ratio = segment.source_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let mut pos = 0.0;
    let mut out = Vec::new();
    while (pos as usize) < segment.samples.len() {
        out.push(segment.samples[pos as usize]);
        pos += ratio;
    }
    out
}

pub fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|v| {
            let f = *v as f64 / i16::MAX as f64;
            f * f
        })
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}

pub fn max_window_rms(samples: &[i16], window_ms: u64) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let window = ((TARGET_SAMPLE_RATE as u64 * window_ms.max(1)) / 1000)
        .max(1)
        .min(samples.len() as u64) as usize;
    samples
        .chunks(window)
        .map(rms)
        .fold(0.0, |best, value| best.max(value))
}

pub fn write_wav_16k(path: &Path, samples: &[i16]) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_rms_rejects_silence_and_accepts_signal() {
        assert!(rms(&vec![0; 1600]) < 0.001);
        assert!(rms(&vec![1200; 1600]) > 0.008);
    }

    #[test]
    fn max_window_rms_finds_speech_inside_silence() {
        let mut samples = vec![0; TARGET_SAMPLE_RATE as usize];
        samples[4800..6400].fill(600);
        assert!(rms(&samples) < 0.008);
        assert!(max_window_rms(&samples, 100) > 0.015);
    }

    #[test]
    fn resamples_to_16k() {
        let segment = AudioSegment {
            source_rate: 48_000,
            samples: vec![1; 48_000],
        };
        let out = to_mono_16k(&segment);
        assert!((15_900..=16_100).contains(&out.len()));
    }

    #[test]
    fn rolling_buffer_extracts_bounds() {
        let buffer = RollingAudioBuffer::new(Duration::from_secs(5));
        let start = Instant::now();
        buffer.push_many(TARGET_SAMPLE_RATE, &vec![42; 1600]);
        let segment = buffer.extract(start, Instant::now());
        assert!(!segment.samples.is_empty());
        assert!(segment.samples.iter().all(|v| *v == 42));
    }
}
