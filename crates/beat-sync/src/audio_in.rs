//! Audio Ingestion using `cpal` CoreAudio stream and lock-free ring buffer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

/// Audio input device information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Active audio input stream capturing samples into a ring buffer.
/// Runs CoreAudio on a dedicated OS thread, keeping the handle Send-safe for Tokio.
pub struct AudioStream {
    consumer: ringbuf::HeapCons<f32>,
    _stop_tx: std::sync::mpsc::Sender<()>,
    is_running: Arc<AtomicBool>,
    sample_rate: u32,
}

impl AudioStream {
    /// List available input devices on macOS (Microphone, BlackHole, etc.).
    pub fn list_input_devices() -> Vec<AudioDeviceInfo> {
        let host = cpal::default_host();
        let default_name = host.default_input_device().and_then(|d| d.name().ok());

        let mut devices = Vec::new();
        if let Ok(input_devices) = host.input_devices() {
            for dev in input_devices {
                if let Ok(name) = dev.name() {
                    let is_default = default_name.as_deref() == Some(&name);
                    devices.push(AudioDeviceInfo { name, is_default });
                }
            }
        }
        devices
    }

    /// Open an audio input stream from the default or named device.
    pub fn new(device_name: Option<&str>, _preferred_sample_rate: u32, buffer_capacity: usize) -> anyhow::Result<Self> {
        let ring_buffer = HeapRb::<f32>::new(buffer_capacity);
        let (mut prod, cons) = ring_buffer.split();

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&is_running);

        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<u32>>();

        let dev_name = device_name.map(str::to_string);

        std::thread::spawn(move || {
            let host = cpal::default_host();
            let device = if let Some(ref name) = dev_name {
                match host.input_devices() {
                    Ok(mut devs) => devs.find(|d| d.name().map(|n| &n == name).unwrap_or(false)),
                    Err(e) => {
                        let _ = init_tx.send(Err(e.into()));
                        return;
                    }
                }
            } else {
                host.default_input_device()
            };

            let Some(device) = device else {
                let _ = init_tx.send(Err(anyhow::anyhow!("No audio input device found")));
                return;
            };

            let default_config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    let _ = init_tx.send(Err(e.into()));
                    return;
                }
            };

            let channels = default_config.channels();
            let actual_sample_rate = default_config.sample_rate().0;
            let sample_format = default_config.sample_format();
            let config: cpal::StreamConfig = default_config.into();

            let err_fn = |err| log::error!("Audio stream error: {:?}", err);
            let stream_res = match sample_format {
                cpal::SampleFormat::F32 => {
                    device.build_input_stream(
                        &config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if running_clone.load(Ordering::Relaxed) {
                                let ch = channels as usize;
                                if ch == 1 {
                                    for &sample in data {
                                        let _ = prod.try_push(sample);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let sum: f32 = chunk.iter().sum();
                                        let _ = prod.try_push(sum / (ch as f32));
                                    }
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    device.build_input_stream(
                        &config,
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            if running_clone.load(Ordering::Relaxed) {
                                let ch = channels as usize;
                                if ch == 1 {
                                    for &s in data {
                                        let _ = prod.try_push(s as f32 / 32768.0);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let sum: f32 = chunk.iter().map(|&s| s as f32 / 32768.0).sum();
                                        let _ = prod.try_push(sum / (ch as f32));
                                    }
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    device.build_input_stream(
                        &config,
                        move |data: &[u16], _: &cpal::InputCallbackInfo| {
                            if running_clone.load(Ordering::Relaxed) {
                                let ch = channels as usize;
                                if ch == 1 {
                                    for &s in data {
                                        let _ = prod.try_push((s as f32 - 32768.0) / 32768.0);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let sum: f32 = chunk.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).sum();
                                        let _ = prod.try_push(sum / (ch as f32));
                                    }
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                _ => {
                    let _ = init_tx.send(Err(anyhow::anyhow!("Unsupported sample format: {:?}", sample_format)));
                    return;
                }
            };

            let stream = match stream_res {
                Ok(s) => s,
                Err(e) => {
                    let _ = init_tx.send(Err(e.into()));
                    return;
                }
            };

            if let Err(e) = stream.play() {
                let _ = init_tx.send(Err(e.into()));
                return;
            }

            let _ = init_tx.send(Ok(actual_sample_rate));

            // Keep stream alive until stop signal received on drop
            let _ = stop_rx.recv();
        });

        let detected_sample_rate = init_rx.recv()??;

        Ok(Self {
            consumer: cons,
            _stop_tx: stop_tx,
            is_running,
            sample_rate: detected_sample_rate,
        })
    }

    /// Read available samples from the ring buffer into a vector.
    pub fn read_samples(&mut self, max_samples: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(max_samples);
        while samples.len() < max_samples {
            if let Some(sample) = self.consumer.try_pop() {
                samples.push(sample);
            } else {
                break;
            }
        }
        samples
    }

    /// Sample rate of the stream.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Pause audio ingestion.
    pub fn pause(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }

    /// Resume audio ingestion.
    pub fn resume(&self) {
        self.is_running.store(true, Ordering::Relaxed);
    }
}
