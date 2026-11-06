# Real-Time Music Beat Sync for Tuya Smart Bulbs (Pure Rust)
### Architecture, Protocol Analysis, Feasibility Study & Implementation Specification

---

## 1. Executive Summary & Feasibility Verdict

| Metric | Assessment |
| :--- | :--- |
| **Feasibility in 100% Pure Rust** | **100% Feasible & Optimal**. Rust is ideal due to zero-cost abstractions, real-time audio safety, deterministic latency, and native macOS CoreAudio support via `cpal`. |
| **Target Hardware** | Tuya-based Wi-Fi Smart Bulbs (Wipro, Syska, Amazon Basics, etc.). |
| **Audio Source** | macOS Microphone (physical) or Loopback device (e.g., BlackHole). |
| **Control Method** | **Tuya Local LAN Protocol (v3.1 / v3.3 over TCP port 6668)**. Cloud API is completely unviable. |
| **Estimated End-to-End Latency** | **45ms – 120ms** (LAN network + audio buffer + DSP). Synchronized beats are well within the human perceptual window (~100ms) with cadence prediction. |
| **Key Bottleneck** | **Bulb Microcontroller (MCU) command rate limit** and **internal hardware fade smoothing**. |

---

## 2. Analysis of the Reference Repositories

### 2.1 Repository 1: `dinesh-it/my-light`
* **Core Technology**: Python + `tinytuya.BulbDevice` (LAN Mode).
* **Key Mechanics**:
  * Direct TCP communication on local Wi-Fi without hitting Tuya cloud servers.
  * Direct manipulation of Tuya Data Points (DPS):
    * `20`: Switch on/off (boolean)
    * `21`: Colour hex/data
    * `22`: Brightness (0–1000 or 0–255 mapped to percentage)
    * `23`: Colour temperature
  * Uses gradual stepping logic (`gradual_map`) for brightness ramping via sleep intervals.
* **Key Takeaway for Beat Sync**:
  * Local LAN control is fast enough for near-instant execution (roundtrip ping typically 15–40ms over 2.4 GHz Wi-Fi).
  * However, tight loops with blocking commands will quickly saturate the socket or trigger TCP RST from the bulb if not throttled.

### 2.2 Repository 2: `riyazwalikar/claude-lightbulb-hooks`
* **Core Technology**: Python + `tinytuya.Cloud` vs `tinytuya.BulbDevice` (LAN).
* **Key Mechanics**:
  * Implements both Cloud OpenAPI and Local LAN.
  * Demonstrates attention-pulsing loops (`--need-attention`) modulating HSV `V` (value/brightness) between 25 and 255 every 1s.
  * Documents critical protocol variations across Tuya hardware versions:
    * **Tuya v3.1 (older devices)**: DPS `'5'` (`colour_data`) rejects JSON. It expects a 14-character hex string: `RRGGBB + HHHH + SS + VV`.
    * **Tuya v3.3+ (modern devices)**: DPS `'5'` expects a JSON string: `{"h": 0.0-360.0, "s": 0.0-255.0, "v": 0.0-255.0}`.
    * Standard DPS keys: `'1'` (switch), `'2'` (mode: `white` vs `colour`), `'3'` (brightness: 25–255), `'5'` (colour data).
* **Key Takeaways for Beat Sync**:
  * **Tuya Cloud is strictly prohibited for music sync**: Cloud roundtrips take 250ms to 1500ms and are aggressively rate-limited (429 Too Many Requests).
  * The software must speak **Tuya LAN Protocol** natively, formatting DPS according to the target bulb version.

---

## 3. Real-World Limitations & Bottlenecks

### 3.1 Bulb Microcontroller & Firmware Rate Limits
* **Hardware Profile**: Tuya bulbs run inexpensive microcontrollers (ESP8266, ESP32, or Beken BK7231N/BK7231T) with low RAM and basic TCP/IP stacks (lwIP).
* **Command Flooding**:
  * If you stream color updates at 60 FPS (every 16ms), the bulb's TCP buffer overflows, resulting in dropped packets, socket disconnection, or an MCU watchdog reset.
  * **Safe Limit**: Maximum **8 to 15 commands per second (8–15 Hz)**.
  * **Strategy**: Do **not** send continuous gradient streams. Instead, use an **event-driven beat trigger** architecture (pulse on onset, smooth client-side cooldown, and state-machine transitions).

### 3.2 Hardware Transition Smoothing (Fade Latency)
* Stock Tuya bulb firmware includes a hardcoded PWM transition ramp (typically 100ms–250ms) to ensure lights turn on/off and shift colors softly without jarring the human eye.
* **Impact**: Rapid 20Hz strobes will be smoothed out into a muddy glow.
* **Workaround**:
  * Target low-frequency structural elements of music: **Kick drums (bass hits)**, **snare onsets**, and **major drop transitions**.
  * Use high-contrast color jumps (e.g., jumping from Deep Blue to Bright Amber on a beat) rather than subtle micro-adjustments.

### 3.3 Network Jitter & 2.4 GHz Wi-Fi
* Tuya bulbs operate exclusively on 2.4 GHz Wi-Fi. In crowded RF environments, latency can spike from 15ms to 120ms.
* **Remedy**:
  * Keep a persistent TCP connection open with Tuya Heartbeat packets (`0x09`) every 5–9 seconds. Opening and closing sockets per beat adds 50–100ms of TCP handshake overhead.

### 3.4 macOS Audio Capture Permissions & Sources
* **Physical Microphone (Built-in Mac Mic)**:
  * Prone to acoustic feedback, speaker distortion, and room reverberation.
  * Requires granting Terminal/App `Microphone` permission in macOS System Settings (`TCC`).
* **System Audio Loopback (Recommended for Pure Sound)**:
  * macOS does not natively provide a "stereo mix" loopback device for internal audio.
  * To capture the exact audio stream coming from Spotify, Apple Music, or YouTube without mic distortion, use a virtual audio driver like **BlackHole (2ch)** or capture via macOS 13+ ScreenCaptureKit audio stream.

---

## 4. End-to-End Latency Budget

To feel "synced" to a human observer, the light change should occur within **80ms – 100ms** of the audible transient:

```
[Mac Mic / Audio Stream]
        │  (Buffer size: 512 samples @ 44.1kHz = 11.6 ms)
        ▼
[cpal Audio Ingestion] (~12 ms)
        │
        ▼
[FFT & Spectral Flux Peak Detection] (< 2 ms in Rust)
        │
        ▼
[Choreography & Rate-Limiter Engine] (< 0.1 ms)
        │
        ▼
[Tuya LAN TCP Packet Framing & AES-128] (< 0.5 ms)
        │
        ▼
[Local Wi-Fi Network Transport] (15 - 35 ms)
        │
        ▼
[Bulb PWM Transition & Illumination] (30 - 60 ms)
────────────────────────────────────────────────────
Total Latency: ~60 ms – 110 ms  (Perceptually Synchronous)
```

---

## 5. System Architecture (Pure Rust)

```
┌────────────────────────────────────────────────────────────────────────┐
│                              macOS Host                                │
│                                                                        │
│   ┌──────────────────────────┐                                         │
│   │   Audio Capture (cpal)   │ CoreAudio stream (Mic or BlackHole)     │
│   └─────────────┬────────────┘                                         │
│                 │ ringbuf (f32 samples)                                │
│                 ▼                                                      │
│   ┌──────────────────────────┐                                         │
│   │   DSP Beat Detector      │ FFT (realfft) + Spectral Flux           │
│   │   - 20-150Hz Sub-Bass    │ Dynamic threshold & onset peak          │
│   │   - 250-2500Hz Mids      │                                         │
│   └─────────────┬────────────┘                                         │
│                 │ crossbeam / tokio::sync::mpsc (BeatEvent)            │
│                 ▼                                                      │
│   ┌──────────────────────────┐                                         │
│   │   Lighting Engine        │ Energy mapping, HSV generation,         │
│   │   - Color Palette Gen    │ Rate-limiter / Coalescer (max 10-12 Hz) │
│   │   - Drop / Build Tracker │                                         │
│   └─────────────┬────────────┘                                         │
│                 │ TuyaCommand (DPS JSON / Hex)                         │
│                 ▼                                                      │
│   ┌──────────────────────────┐                                         │
│   │   Tuya LAN Client        │ Persistent TCP Socket (Port 6668)       │
│   │   - Tokio Async TCP      │ AES-128-CBC/ECB + HMAC-SHA256           │
│   │   - Heartbeat Keep-Alive │ Binary protocol framing                 │
│   └─────────────┬────────────┘                                         │
└─────────────────┼──────────────────────────────────────────────────────┘
                  │ Wi-Fi (TCP 6668)
                  ▼
         ┌───────────────────┐
         │  Tuya Smart Bulb  │ (Wipro / Syska / Smart Life)
         └───────────────────┘
```

---

## 6. Implementation Blueprint in Rust

### 6.1 Recommended Crates (`Cargo.toml`)
```toml
[package]
name = "tuya-beat-sync"
version = "0.1.0"
edition = "2021"

[dependencies]
# Audio Capture
cpal = "0.15"
ringbuf = "0.3"

# DSP & Math
realfft = "3.3"
num-complex = "0.4"

# Async & Networking
tokio = { version = "1.38", features = ["full"] }
tokio-util = "0.7"

# Cryptography & Tuya Protocol
aes = "0.8"
cbc = "0.1"
ecb = "0.1"
cipher = { version = "0.4", features = ["block-padding"] }
hmac = "0.12"
sha2 = "0.10"
md-5 = "0.10"
crc32fast = "1.4"

# Serialization & Config
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
clap = { version = "4.5", features = ["derive"] }
log = "0.4"
env_logger = "0.11"
```

---

### 6.2 Module 1: Low-Latency Audio Ingestion (`audio_in.rs`)
Captures samples from the Mac default microphone or virtual soundcard using `cpal`:

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::sync::Arc;

pub struct AudioStream {
    _stream: cpal::Stream,
    pub consumer: ringbuf::HeapCons<f32>,
}

impl AudioStream {
    pub fn new(sample_rate: u32, buffer_capacity: usize) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input audio device found"))?;

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Fixed(512),
        };

        let ring_buffer = HeapRb::<f32>::new(buffer_capacity);
        let (mut prod, cons) = ring_buffer.split();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for &sample in data {
                    let _ = prod.try_push(sample);
                }
            },
            |err| eprintln!("Audio stream error: {:?}", err),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            _stream: stream,
            consumer: cons,
        })
    }
}
```

---

### 6.3 Module 2: Beat & Onset Detection (`dsp.rs`)
Uses Real-to-Complex Fast Fourier Transform (`realfft`) to calculate the spectral energy in low frequency bands (20Hz–150Hz for kicks, 250Hz–2500Hz for snares/vocals) and detects onsets via **Spectral Flux**:

```rust
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct BeatDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    window_size: usize,
    sample_rate: f32,
    scratch: Vec<num_complex::Complex<f32>>,
    prev_spectrum: Vec<f32>,
    moving_energy_avg: f32,
    alpha: f32, // Moving average decay factor
}

#[derive(Debug, Clone)]
pub struct BeatEvent {
    pub is_kick: bool,
    pub is_snare: bool,
    pub bass_intensity: f32,   // 0.0 to 1.0
    pub overall_energy: f32,   // 0.0 to 1.0
}

impl BeatDetector {
    pub fn new(window_size: usize, sample_rate: f32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(window_size);
        let scratch = fft.make_scratch_vec();
        let spectrum_len = window_size / 2 + 1;

        Self {
            fft,
            window_size,
            sample_rate,
            scratch,
            prev_spectrum: vec![0.0; spectrum_len],
            moving_energy_avg: 0.0,
            alpha: 0.05,
        }
    }

    pub fn process_frame(&mut self, mut samples: Vec<f32>) -> Option<BeatEvent> {
        if samples.len() < self.window_size {
            return None;
        }

        // Apply Hann window
        for (i, sample) in samples.iter_mut().enumerate() {
            let multiplier = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (self.window_size - 1) as f32).cos());
            *sample *= multiplier;
        }

        let mut output = self.fft.make_output_vec();
        self.fft.process_with_scratch(&mut samples, &mut output, &mut self.scratch).ok()?;

        let bin_hz = self.sample_rate / (self.window_size as f32);
        let mut bass_energy = 0.0;
        let mut snare_energy = 0.0;
        let mut total_flux = 0.0;

        for (i, comp) in output.iter().enumerate() {
            let freq = i as f32 * bin_hz;
            let mag = comp.norm();

            // Spectral Flux (half-wave rectified)
            let diff = mag - self.prev_spectrum[i];
            if diff > 0.0 {
                total_flux += diff;
            }
            self.prev_spectrum[i] = mag;

            if (20.0..=150.0).contains(&freq) {
                bass_energy += mag;
            } else if (250.0..=2500.0).contains(&freq) {
                snare_energy += mag;
            }
        }

        // Dynamic thresholding
        self.moving_energy_avg = (1.0 - self.alpha) * self.moving_energy_avg + self.alpha * bass_energy;
        let kick_threshold = self.moving_energy_avg * 1.5;
        let is_kick = bass_energy > kick_threshold && bass_energy > 5.0;

        Some(BeatEvent {
            is_kick,
            is_snare: snare_energy > 20.0,
            bass_intensity: (bass_energy / (self.moving_energy_avg * 2.0 + 1e-4)).clamp(0.0, 1.0),
            overall_energy: (total_flux / 500.0).clamp(0.0, 1.0),
        })
    }
}
```

---

### 6.4 Module 3: Choreography & Rate-Limiter Engine (`engine.rs`)
Prevents bulb MCU crash by capping output to **10 Hz** while maintaining responsive beat flashes:

```rust
use std::time::{Duration, Instant};

pub struct LightingChoreographer {
    last_command_time: Instant,
    min_interval: Duration, // e.g. 100ms (10 Hz max)
    current_hue: f32,
}

impl LightingChoreographer {
    pub fn new(max_fps: u32) -> Self {
        Self {
            last_command_time: Instant::now() - Duration::from_secs(1),
            min_interval: Duration::from_millis(1000 / max_fps as u64),
            current_hue: 0.0,
        }
    }

    pub fn handle_beat(&mut self, event: &BeatEvent) -> Option<BulbCommand> {
        let now = Instant::now();
        if now.duration_since(self.last_command_time) < self.min_interval {
            return None; // Drop packet to protect bulb TCP buffer
        }

        if event.is_kick {
            self.last_command_time = now;
            // Shift color by 60 degrees on strong kicks
            self.current_hue = (self.current_hue + 45.0) % 360.0;
            let brightness = 150 + (event.bass_intensity * 105.0) as u32; // 150 to 255

            Some(BulbCommand::SetColor {
                hue: self.current_hue,
                saturation: 255,
                value: brightness,
            })
        } else {
            None
        }
    }
}

pub enum BulbCommand {
    SetColor { hue: f32, saturation: u32, value: u32 },
    SetWhite { brightness: u32, temp: u32 },
}
```

---

### 6.5 Module 4: Native Pure-Rust Tuya LAN Client (`tuya.rs`)
Implements the binary protocol framing over TCP directly:

#### Protocol Packet Structure:
1. **Prefix (4 bytes)**: `0x000055AA`
2. **Sequence Number (4 bytes)**: Big-endian incremental counter
3. **Command ID (4 bytes)**:
   * `0x07`: Control (DPS send, v3.1/v3.3)
   * `0x08`: Status request
   * `0x09`: Heartbeat (Ping)
   * `0x10`: Query DP
4. **Payload Length (4 bytes)**: Length of payload + suffix/tag
5. **Payload Data**:
   * For v3.3: `"3.3" + 12-byte zero padding + AES-128-CBC(JSON)`
   * For v3.1: `AES-128-ECB(JSON)`
6. **Integrity Hash (16 or 32 bytes)**: HMAC-SHA256 (v3.3) or MD5 (v3.1) of packet
7. **Suffix (4 bytes)**: `0x0000AA55`

#### Rust Implementation Snippet:
```rust
use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type HmacSha256 = Hmac<Sha256>;

pub struct TuyaBulbClient {
    device_id: String,
    local_key: Vec<u8>,
    stream: TcpStream,
    seq: u32,
}

impl TuyaBulbClient {
    pub async fn connect(ip: &str, device_id: String, local_key: &str) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(format!("{}:6668", ip)).await?;
        Ok(Self {
            device_id,
            local_key: local_key.as_bytes().to_vec(),
            stream,
            seq: 0,
        })
    }

    pub async fn send_heartbeat(&mut self) -> anyhow::Result<()> {
        let packet = self.build_packet(0x09, &[])?;
        self.stream.write_all(&packet).await?;
        Ok(())
    }

    pub async fn set_color_v33(&mut self, h: f32, s: f32, v: f32) -> anyhow::Result<()> {
        let colour_data = serde_json::json!({
            "h": h as u32,
            "s": s as u32,
            "v": v as u32
        });

        let dps_payload = serde_json::json!({
            "protocol": 4,
            "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "data": {
                "dps": {
                    "1": true,
                    "2": "colour",
                    "5": colour_data.to_string()
                }
            }
        });

        let json_bytes = serde_json::to_vec(&dps_payload)?;
        let encrypted = self.encrypt_v33(&json_bytes)?;

        let mut payload = b"3.3".to_vec();
        payload.extend_from_slice(&[0u8; 12]); // 12-byte zero header
        payload.extend_from_slice(&encrypted);

        let packet = self.build_packet(0x07, &payload)?;
        self.stream.write_all(&packet).await?;
        Ok(())
    }

    fn encrypt_v33(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let iv = &self.local_key[0..16];
        let cipher = Aes128CbcEnc::new_from_slices(&self.local_key, iv)?;
        let mut buf = vec![0u8; data.len() + 16];
        let ct = cipher.encrypt_padded_b2b_mut::<Pkcs7>(data, &mut buf)
            .map_err(|e| anyhow::anyhow!("Encryption error: {:?}", e))?;
        Ok(ct.to_vec())
    }

    fn build_packet(&mut self, cmd: u32, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.seq += 1;
        let mut packet = Vec::with_capacity(payload.len() + 48);

        packet.extend_from_slice(&0x000055AA_u32.to_be_bytes()); // Prefix
        packet.extend_from_slice(&self.seq.to_be_bytes());
        packet.extend_from_slice(&cmd.to_be_bytes());

        // Length = payload length + 32 (HMAC) + 4 (Suffix)
        let total_len = (payload.len() + 32 + 4) as u32;
        packet.extend_from_slice(&total_len.to_be_bytes());
        packet.extend_from_slice(payload);

        // HMAC-SHA256 signature
        let mut mac = HmacSha256::new_from_slice(&self.local_key)?;
        mac.update(&packet);
        let hmac_result = mac.finalize().into_bytes();
        packet.extend_from_slice(&hmac_result);

        packet.extend_from_slice(&0x0000AA55_u32.to_be_bytes()); // Suffix
        Ok(packet)
    }
}
```

---

## 7. Operational Workflow & Device Setup

### Prerequisites Checklist:
1. **Extract Bulb Credentials**:
   * Follow the standard procedure (documented in `dinesh-it/my-light` & `claude-lightbulb-hooks`):
     * Create a free developer account at [iot.tuya.com](https://iot.tuya.com).
     * Link the Smart Life / Tuya Smart app account under **Cloud -> Development -> Link Tuya App Account**.
     * Run `tinytuya wizard` or Tuya Cloud API Explorer to retrieve the **`device_id`**, **`local_key`**, and bulb's **`local_ip`**.
2. **Assign a Static DHCP Reservation**:
   * Assign a fixed IP to the smart bulb on your Wi-Fi router (avoids IP changes breaking the TCP socket).
3. **Configure macOS Audio In**:
   * If using microphone: Grant your terminal app permission under **System Settings -> Privacy & Security -> Microphone**.
   * If using internal music without room echo: Install BlackHole via Homebrew (`brew install blackhole-2ch`) and set Mac audio output to a Multi-Output Device (Speakers + BlackHole), then select BlackHole as the input device.

---

## 8. Summary Comparison: Python vs Pure Rust

| Aspect | Python (`tinytuya` + `pyaudio`) | Pure Rust (`cpal` + `tokio` + `realfft`) |
| :--- | :--- | :--- |
| **Audio Jitter** | High (GIL latency, GC pauses, 30–60ms buffer spikes) | Minimal (<1ms deterministic DSP processing) |
| **TCP Keep-Alive** | Often spawns new sockets or blocks main thread | Fully asynchronous via `tokio::net::TcpStream` |
| **CPU Usage** | 10% – 25% CPU on continuous FFT | < 1% CPU utilization |
| **Distribution** | Requires Python, pip, virtualenv, PyAudio C-bindings | Single standalone static binary (`cargo build --release`) |
| **Overall Sync Feel** | Noticeable delay (often out-of-rhythm on fast songs) | Crisp, immediate kick/bass tracking |
