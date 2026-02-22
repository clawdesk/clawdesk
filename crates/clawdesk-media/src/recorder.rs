//! Native audio recorder using cpal.
//!
//! Records from the system's default input device, writing samples into
//! a lock-free ring buffer. A dedicated writer thread drains the buffer
//! and writes WAV to a temp file via `hound`. The final WAV bytes are
//! returned when recording stops.
//!
//! This avoids the browser `navigator.mediaDevices` limitation in Tauri WebView.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat as HoundSampleFormat, WavSpec, WavWriter};
use parking_lot::Mutex;
use ringbuf::{HeapRb, traits::{Producer, Consumer, Split}};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Recording state visible to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    Idle,
    Recording,
    Transcribing,
    Error,
}

/// Native audio recorder backed by cpal.
pub struct AudioRecorder {
    /// Whether we are actively recording.
    is_recording: Arc<AtomicBool>,
    /// The cpal input stream (kept alive while recording).
    stream: Option<cpal::Stream>,
    /// Handle to the consumer/writer thread.
    writer_handle: Option<std::thread::JoinHandle<Result<PathBuf, String>>>,
    /// Signal for the writer thread to stop.
    stop_writer: Arc<AtomicBool>,
    /// Path to the temp WAV file being written.
    output_path: Option<PathBuf>,
    /// Current state.
    state: RecordingState,
    /// Sample rate of the recording device.
    device_sample_rate: u32,
}

// cpal::Stream is Send but not Sync — we guard via Mutex in AppState
unsafe impl Send for AudioRecorder {}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            is_recording: Arc::new(AtomicBool::new(false)),
            stream: None,
            writer_handle: None,
            stop_writer: Arc::new(AtomicBool::new(false)),
            output_path: None,
            state: RecordingState::Idle,
            device_sample_rate: 0,
        }
    }

    pub fn state(&self) -> RecordingState {
        self.state
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::Relaxed)
    }

    /// Start recording from the default input device.
    ///
    /// Returns the sample rate of the input device.
    pub fn start_recording(&mut self) -> Result<u32, String> {
        if self.state == RecordingState::Recording {
            return Err("already recording".into());
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or("no default input audio device found")?;

        let device_name = device.name().unwrap_or_else(|_| "unknown".into());
        info!(device = %device_name, "using input audio device");

        let supported_config = device
            .default_input_config()
            .map_err(|e| format!("no supported input config: {e}"))?;

        let sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels() as usize;
        self.device_sample_rate = sample_rate;

        info!(sample_rate, channels, "input device config");

        // Create temp file for WAV output
        let tmp_dir = std::env::temp_dir();
        let output_path = tmp_dir.join(format!("clawdesk_voice_{}.wav", std::process::id()));
        self.output_path = Some(output_path.clone());

        // Ring buffer: 5 seconds of audio at device sample rate
        let ring_size = sample_rate as usize * channels * 5;
        let ring = HeapRb::<f32>::new(ring_size);
        let (producer, consumer) = ring.split();
        let producer = Arc::new(Mutex::new(producer));

        // Flags
        self.is_recording.store(true, Ordering::Release);
        self.stop_writer.store(false, Ordering::Release);

        let is_recording = Arc::clone(&self.is_recording);
        let stop_writer = Arc::clone(&self.stop_writer);

        // Build input stream
        let producer_clone = Arc::clone(&producer);
        let is_rec_clone = Arc::clone(&is_recording);

        let stream_config: cpal::StreamConfig = supported_config.into();

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !is_rec_clone.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(mut prod) = producer_clone.try_lock() {
                        // Push as many samples as possible; drop overflow silently
                        prod.push_slice(data);
                    }
                },
                move |err| {
                    error!("cpal input stream error: {err}");
                },
                None,
            )
            .map_err(|e| format!("failed to build input stream: {e}"))?;

        stream.play().map_err(|e| format!("failed to start stream: {e}"))?;
        self.stream = Some(stream);

        // Writer thread: drain ring buffer → WAV file
        let wav_path = output_path.clone();
        let writer_stop = Arc::clone(&stop_writer);
        let writer_channels = stream_config.channels as u16;
        let writer_sr = sample_rate;

        let handle = std::thread::Builder::new()
            .name("audio-writer".into())
            .spawn(move || {
                write_wav_from_consumer(consumer, &wav_path, writer_sr, writer_channels, writer_stop)
            })
            .map_err(|e| format!("failed to spawn writer thread: {e}"))?;

        self.writer_handle = Some(handle);
        self.state = RecordingState::Recording;

        info!("recording started");
        Ok(sample_rate)
    }

    /// Stop recording and return the path to the WAV file.
    pub fn stop_recording(&mut self) -> Result<PathBuf, String> {
        if self.state != RecordingState::Recording {
            return Err("not recording".into());
        }

        // 1. Gate the callback
        self.is_recording.store(false, Ordering::Release);

        // 2. Signal writer thread to stop
        self.stop_writer.store(true, Ordering::Release);

        // 3. Drop the cpal stream
        self.stream = None;

        // 4. Join writer thread (with timeout)
        if let Some(handle) = self.writer_handle.take() {
            match handle.join() {
                Ok(Ok(path)) => {
                    info!(path = %path.display(), "recording saved");
                    self.state = RecordingState::Idle;
                    return Ok(path);
                }
                Ok(Err(e)) => {
                    self.state = RecordingState::Error;
                    return Err(format!("writer error: {e}"));
                }
                Err(_) => {
                    self.state = RecordingState::Error;
                    return Err("writer thread panicked".into());
                }
            }
        }

        self.state = RecordingState::Idle;
        self.output_path
            .clone()
            .ok_or_else(|| "no output path".into())
    }

    /// Cancel a recording without saving.
    pub fn cancel_recording(&mut self) {
        self.is_recording.store(false, Ordering::Release);
        self.stop_writer.store(true, Ordering::Release);
        self.stream = None;
        if let Some(handle) = self.writer_handle.take() {
            let _ = handle.join();
        }
        // Clean up temp file
        if let Some(path) = &self.output_path {
            let _ = std::fs::remove_file(path);
        }
        self.output_path = None;
        self.state = RecordingState::Idle;
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        if self.state == RecordingState::Recording {
            self.cancel_recording();
        }
    }
}

/// Writer thread: drains ring buffer consumer → WAV file.
fn write_wav_from_consumer(
    mut consumer: impl Consumer<Item = f32>,
    wav_path: &Path,
    sample_rate: u32,
    channels: u16,
    stop_flag: Arc<AtomicBool>,
) -> Result<PathBuf, String> {
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: HoundSampleFormat::Int,
    };

    let mut writer = WavWriter::create(wav_path, spec)
        .map_err(|e| format!("failed to create WAV writer: {e}"))?;

    let mut buf = vec![0.0f32; 4096];
    let mut total_samples: u64 = 0;

    loop {
        let n = consumer.pop_slice(&mut buf);
        if n > 0 {
            for &sample in &buf[..n] {
                let clamped = sample.max(-1.0).min(1.0);
                let i16_sample = if clamped < 0.0 {
                    (clamped * 32768.0) as i16
                } else {
                    (clamped * 32767.0) as i16
                };
                writer.write_sample(i16_sample)
                    .map_err(|e| format!("WAV write error: {e}"))?;
            }
            total_samples += n as u64;
        } else if stop_flag.load(Ordering::Relaxed) {
            // No more data and told to stop
            break;
        } else {
            // No data yet, sleep briefly to avoid busy-spin
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    writer.finalize().map_err(|e| format!("WAV finalize error: {e}"))?;

    let duration_secs = total_samples as f64 / (sample_rate as f64 * channels as f64);
    info!(
        path = %wav_path.display(),
        total_samples,
        duration_secs = format!("{:.1}", duration_secs),
        "WAV file written"
    );

    Ok(wav_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorder_initial_state() {
        let recorder = AudioRecorder::new();
        assert_eq!(recorder.state(), RecordingState::Idle);
        assert!(!recorder.is_recording());
    }

    #[test]
    fn test_stop_when_not_recording() {
        let mut recorder = AudioRecorder::new();
        assert!(recorder.stop_recording().is_err());
    }

    #[test]
    fn test_recording_state_serde() {
        let state = RecordingState::Recording;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"recording\"");

        let parsed: RecordingState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, RecordingState::Recording);
    }
}
