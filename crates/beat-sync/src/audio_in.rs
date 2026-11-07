//! Audio Ingestion using `cpal` CoreAudio stream and lock-free ring buffer with Mid/Side stereo separation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use crate::dsp::AudioFocusMode;

/// Audio input device information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Active audio input stream capturing samples into dual lock-free ring buffers (Mid and Side).
/// Runs CoreAudio on a dedicated OS thread, keeping the handle Send-safe for Tokio.
pub struct AudioStream {
    cons_mid: ringbuf::HeapCons<f32>,
    cons_side: ringbuf::HeapCons<f32>,
    _stop_tx: std::sync::mpsc::Sender<()>,
    is_running: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
}

impl AudioStream {
    /// List available input devices on macOS, Linux, and Windows.
    pub fn list_input_devices() -> Vec<AudioDeviceInfo> {
        let host = cpal::default_host();
        let default_name = host.default_input_device().and_then(|d| d.name().ok());

        let mut devices = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Ok(outputs) = host.output_devices() {
                for dev in outputs {
                    if let Ok(name) = dev.name() {
                        devices.push(AudioDeviceInfo {
                            name: format!("[Loopback] {}", name),
                            is_default: false,
                        });
                    }
                }
            }
        }

        if let Ok(input_devices) = host.input_devices() {
            for dev in input_devices {
                if let Ok(name) = dev.name() {
                    #[cfg(target_os = "linux")]
                    let is_monitor = name.contains(".monitor") || name.to_lowercase().contains("monitor of");
                    #[cfg(target_os = "linux")]
                    let display_name = if is_monitor {
                        format!("{} (System Audio Loopback)", name)
                    } else {
                        name.clone()
                    };
                    #[cfg(not(target_os = "linux"))]
                    let display_name = name.clone();

                    let is_default = default_name.as_deref() == Some(&name);
                    devices.push(AudioDeviceInfo {
                        name: display_name,
                        is_default,
                    });
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            devices.sort_by_key(|d| !d.name.contains("(System Audio Loopback)"));
        }

        devices
    }

    /// Open an audio input stream from the default or named device.
    pub fn new(device_name: Option<&str>, _preferred_sample_rate: u32, buffer_capacity: usize) -> anyhow::Result<Self> {
        let rb_mid = HeapRb::<f32>::new(buffer_capacity);
        let (mut prod_mid, cons_mid) = rb_mid.split();

        let rb_side = HeapRb::<f32>::new(buffer_capacity);
        let (mut prod_side, cons_side) = rb_side.split();

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&is_running);

        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<(u32, u16)>>();

        let dev_name = device_name.map(str::to_string);

        std::thread::spawn(move || {
            let host = cpal::default_host();

            #[cfg(target_os = "windows")]
            let is_loopback = dev_name.as_deref().map(|n| n.starts_with("[Loopback] ")).unwrap_or(false);
            #[cfg(not(target_os = "windows"))]
            let is_loopback = false;

            let device = if is_loopback {
                #[cfg(target_os = "windows")]
                {
                    let raw_name = dev_name.as_deref().unwrap().trim_start_matches("[Loopback] ");
                    host.output_devices().ok().and_then(|mut devs| {
                        devs.find(|d| d.name().map(|n| n == raw_name).unwrap_or(false))
                    })
                }
                #[cfg(not(target_os = "windows"))]
                {
                    None
                }
            } else if let Some(ref name) = dev_name {
                #[cfg(target_os = "linux")]
                let clean_name = name.trim_end_matches(" (System Audio Loopback)");
                #[cfg(not(target_os = "linux"))]
                let clean_name = name.as_str();

                match host.input_devices() {
                    Ok(mut devs) => devs.find(|d| d.name().map(|n| n == clean_name).unwrap_or(false)),
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

            let default_config = if is_loopback {
                match device.default_output_config() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(e.into()));
                        return;
                    }
                }
            } else {
                match device.default_input_config() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(e.into()));
                        return;
                    }
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
                                        let _ = prod_mid.try_push(sample);
                                        let _ = prod_side.try_push(0.0);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let left = chunk[0];
                                        let right = chunk[1];
                                        let mid = (left + right) * 0.5;
                                        let side = (left - right) * 0.5;
                                        let _ = prod_mid.try_push(mid);
                                        let _ = prod_side.try_push(side);
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
                                        let sample = s as f32 / 32768.0;
                                        let _ = prod_mid.try_push(sample);
                                        let _ = prod_side.try_push(0.0);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let left = chunk[0] as f32 / 32768.0;
                                        let right = chunk[1] as f32 / 32768.0;
                                        let mid = (left + right) * 0.5;
                                        let side = (left - right) * 0.5;
                                        let _ = prod_mid.try_push(mid);
                                        let _ = prod_side.try_push(side);
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
                                        let sample = (s as f32 - 32768.0) / 32768.0;
                                        let _ = prod_mid.try_push(sample);
                                        let _ = prod_side.try_push(0.0);
                                    }
                                } else {
                                    for chunk in data.chunks_exact(ch) {
                                        let left = (chunk[0] as f32 - 32768.0) / 32768.0;
                                        let right = (chunk[1] as f32 - 32768.0) / 32768.0;
                                        let mid = (left + right) * 0.5;
                                        let side = (left - right) * 0.5;
                                        let _ = prod_mid.try_push(mid);
                                        let _ = prod_side.try_push(side);
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

            let _ = init_tx.send(Ok((actual_sample_rate, channels)));

            // Keep stream alive until stop signal received on drop
            let _ = stop_rx.recv();
        });

        let (detected_sample_rate, detected_channels) = init_rx.recv()??;

        Ok(Self {
            cons_mid,
            cons_side,
            _stop_tx: stop_tx,
            is_running,
            sample_rate: detected_sample_rate,
            channels: detected_channels,
        })
    }

    /// Read available Mid samples from the ring buffer into a vector (draining Side to stay aligned).
    pub fn read_samples(&mut self, max_samples: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(max_samples);
        while samples.len() < max_samples {
            if let Some(sample) = self.cons_mid.try_pop() {
                let _ = self.cons_side.try_pop();
                samples.push(sample);
            } else {
                break;
            }
        }
        samples
    }

    /// Read samples tailored to the active AudioFocusMode:
    /// - FullMix: Mid channel (L + R) / 2
    /// - VocalsOnly: Mid channel (L + R) / 2 (center lead vocal isolation)
    /// - InstrumentsOnly: Side channel (L - R) / 2 (vocal cancellation when stereo), or Mid when mono
    pub fn read_samples_focused(&mut self, max_samples: usize, focus_mode: AudioFocusMode) -> Vec<f32> {
        let mut samples = Vec::with_capacity(max_samples);
        let has_stereo = self.channels >= 2;

        while samples.len() < max_samples {
            match (self.cons_mid.try_pop(), self.cons_side.try_pop()) {
                (Some(m), Some(s)) => {
                    let s_val = match focus_mode {
                        AudioFocusMode::FullMix | AudioFocusMode::VocalsOnly => m,
                        AudioFocusMode::InstrumentsOnly => {
                            if has_stereo {
                                // Side channel cancels center vocals; boost slightly to match mid RMS
                                s * 1.35
                            } else {
                                m
                            }
                        }
                    };
                    samples.push(s_val);
                }
                _ => break,
            }
        }
        samples
    }

    /// Read both Mid and Side channels simultaneously for advanced multi-band stem routing.
    pub fn read_mid_side(&mut self, max_samples: usize) -> (Vec<f32>, Vec<f32>) {
        let mut mid = Vec::with_capacity(max_samples);
        let mut side = Vec::with_capacity(max_samples);
        while mid.len() < max_samples {
            match (self.cons_mid.try_pop(), self.cons_side.try_pop()) {
                (Some(m), Some(s)) => {
                    mid.push(m);
                    side.push(s);
                }
                _ => break,
            }
        }
        (mid, side)
    }

    /// Sample rate of the stream.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of channels in the input stream.
    pub fn channels(&self) -> u16 {
        self.channels
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
