# IOT-Lights 💡🎵
### Real-Time Music Beat-Sync for Tuya Smart Bulbs • Pure Rust • 100% Local LAN

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg?style=flat&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Local Control](https://img.shields.io/badge/Cloud%20Dependency-0%25%20(Local%20LAN)-brightgreen.svg)]()
[![Latency](https://img.shields.io/badge/Beat%20Latency-%3C15ms-success.svg)]()

**IOT-Lights** is an ultra-low latency, real-time audio-reactive lighting controller written in pure Rust. It transforms ordinary Tuya and Smart Life Wi-Fi smart bulbs (Wipro, Syska, Gosund, Treatlife, Lumary, Avatar, generic OEM, etc.) into a responsive music light show.

Unlike traditional cloud integrations that suffer from 200–800ms internet delays and rate limits, **IOT-Lights operates 100% locally over your Wi-Fi network (Port 6668)** with zero cloud reliance. It features a built-in DSP spectral flux engine, intelligent kick/snare detection, multi-palette lighting choreographies, a mobile-optimized Web Dashboard, and a fast CLI.

---

## 📑 Table of Contents

- [System Architecture](#-system-architecture)
- [Minimum Prerequisites](#-minimum-prerequisites)
- [Obtaining Your Bulb's Local Key (`localkey`)](#-obtaining-your-bulbs-local-key-localkey)
  - [Path 1: Tuya Developer Cloud + TinyTuya (Recommended)](#path-1-tuya-developer-cloud--tinytuya-recommended)
  - [Path 2: Wireless ADB / Android Backup (Alternative)](#path-2-wireless-adb--android-backup-alternative)
  - [Path 3: Network Proxy / Packet Sniffing (Legacy)](#path-3-network-proxy--packet-sniffing-legacy)
- [Quick Start & Setup](#-quick-start--setup)
  - [1. Clone & Build](#1-clone--build)
  - [2. Configure Devices](#2-configure-devices)
  - [3. Start the Web Dashboard](#3-start-the-web-dashboard)
  - [4. Direct Command-Line Control](#4-direct-command-line-control)
  - [5. Standalone Beat-Sync CLI](#5-standalone-beat-sync-cli)
- [Frequently Asked Questions (FAQ) & Troubleshooting](#-frequently-asked-questions-faq--troubleshooting)
  - [Local Key Questions](#local-key-questions)
  - [Connectivity & Protocol Versions](#connectivity--protocol-versions)
  - [Which Devices Will Work?](#which-devices-will-work)
  - [Tested Hardware](#tested-hardware)
  - [Why DP 27 Music Mode is Special](#why-dp-27-music-mode-is-special)
- [Contributing](#-contributing)
- [Acknowledgments & Shout-Outs](#-acknowledgments--shout-outs)

---

## ⚡ System Architecture

```
[ Microphone / BlackHole Virtual Audio Loopback ]
                       │
                       ▼
           [ cpal Audio Ingestion ]  (512 samples buffer @ 44.1 kHz, ~11.6ms)
                       │
                       ▼
         [ RealFFT DSP Beat Detector ]
         - Spectral Flux (half-wave rectified)
         - Sub-Bass (20 - 150 Hz) Kick Detection
         - Mids (250 - 2500 Hz) Snare & Vocal Detection
         - Dynamic Adaptive Energy Thresholding
                       │
                       ▼
         [ Lighting Choreographer & Rate Limiter ]
         - Neon Cyberpunk / Synthwave / Club Rave / Vaporwave Palettes
         - Drop, Build, and Idle State Tracking
         - Hardware Safety Coalescing (Protects ESP8266/BK7231 MCUs)
                       │
                       ▼
     [ BulbFleet Connection Pool & Tuya LAN Driver ]
     - High-speed Tuya DP 27 (`music_data`) jump streaming frames
     - Persistent TCP keep-alive sockets (Port 6668)
     - AES-128-CBC / ECB encryption & HMAC-SHA256 framing (v3.1 - v3.5)
                       │
                       ▼
        [ Wi-Fi Smart Bulbs (Wipro / Syska / Tuya) ]
```

---

## 📋 Minimum Prerequisites

Before setting up IOT-Lights, ensure you have:

1. **Tuya / Smart Life Wi-Fi Bulbs**:
   - Paired and operating in the official **Smart Life** or **Tuya Smart** mobile app.
   - Connected to your local **2.4 GHz Wi-Fi** network.
2. **Network Setup**:
   - Your computer running IOT-Lights and the smart bulbs must be on the **same local subnet** (e.g., both on `192.168.1.x`).
   - Router **AP Isolation / Client Isolation** must be turned **OFF** so devices can talk to each other over LAN.
3. **Rust Toolchain**:
   - Rust 1.75+ installed via [rustup](https://rustup.rs/):
     ```bash
     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
     ```
4. **Audio Subsystem**:
   - **macOS**: Built-in CoreAudio works out of the box. To sync system sound (Spotify, YouTube, DAW) instead of the microphone, install [BlackHole 2ch](https://github.com/ExistentialAudio/BlackHole) and create a Multi-Output Device.
   - **Linux**: Install ALSA development libraries:
     ```bash
     sudo apt update && sudo apt install libasound2-dev pkg-config
     ```
5. **Python 3 & TinyTuya** (for one-time local key extraction):
   ```bash
   pip install tinytuya
   ```

---

## 🔑 Obtaining Your Bulb's Local Key (`localkey`)

Every Tuya device requires a 16-character AES cryptographic key (`localkey`) to communicate directly over LAN without going through Tuya's cloud servers.

Here are the paths to obtain your keys:

### Path 1: Tuya Developer Cloud + TinyTuya (Recommended)

This is the cleanest, non-destructive official method. It takes ~5 minutes and does not require rooting or modifying your phone:

1. **Create a Free Account on Tuya IoT Platform**:
   - Register at [iot.tuya.com](https://iot.tuya.com/).
2. **Create a Cloud Development Project**:
   - Go to **Cloud** -> **Development Projects** -> **Create Cloud Project**.
   - Enter project name `IOT-Lights`, select **Smart Home** industry, and select **Custom Development**.
   - > [!IMPORTANT]
     > **Select the correct Data Center region** matching your mobile app registration (e.g., *India Data Center*, *Western America*, *Central Europe*). If mismatched, device linking will fail.
3. **Link Your Smart Life App Account**:
   - In your cloud project, open the **Devices** tab -> **Link Tuya App Account**.
   - Click **Add App Account**. A QR code will appear.
   - Open the **Smart Life** app on your phone -> tap **Me** -> tap the **QR scan icon** (top right) -> Scan the QR code and authorize.
   - All your bulbs will now appear under the Cloud Project!
4. **Extract Keys with TinyTuya**:
   - Run the automated key extractor:
     ```bash
     python -m tinytuya wizard
     ```
   - Provide your Project **API Key** (Client ID), **API Secret**, and **Region code** (e.g., `in`, `us`, `eu`).
   - TinyTuya will output `devices.json` with all device names, `id`, and `key`!

---

### Path 2: Wireless ADB / Android Backup (Alternative)

For developers without a Tuya Cloud account who have an Android phone with Smart Life installed:

1. Enable **Developer Options** and **Wireless Debugging** in Android settings.
2. Note the Wi-Fi pairing code and port, then pair and connect from your computer:
   ```bash
   adb pair 192.168.1.X:<pairing-port> <pairing-code>
   adb connect 192.168.1.X:<debugging-port>
   ```
3. Extract Tuya app preferences or SQLite databases:
   - Older Android versions / rooted devices store keys in:
     `/data/data/com.tuya.smartlife/shared_prefs/preferences_global_key...xml`
   - *Note*: On modern Android 11+ and unrooted phones (e.g., Google Pixel), SELinux blocks direct access to application sandboxes without root. **Path 1 is strongly recommended instead.**

---

### Path 3: Network Proxy / Packet Sniffing (Legacy)

- Using `tuya-cli list-app` or tools like `mitmproxy` / `Charles Proxy` with an older version of the Smart Life APK (v3.12 or older) before Tuya enforced SSL certificate pinning.

---

## 🚀 Quick Start & Setup

### 1. Clone & Build
```bash
git clone https://github.com/your-username/IOT-Lights.git
cd IOT-Lights

# Compile optimized release binaries
cargo build --release
```

### 2. Configure Devices
Create or edit `devices.json` in the root of the project with your bulbs:
```json
[
  {
    "name": "Living Room Light",
    "id": "d79466ba2de1ef6565hrzd",
    "key": "qxkIfvV3p?KjPZ#7",
    "ip": "192.168.1.2",
    "version": "3.5",
    "bulb_type": "TypeB"
  },
  {
    "name": "Desk Lamp",
    "id": "d788ae729054b621degigj",
    "key": "zJxOPzO<n[xUoT_J",
    "ip": "192.168.1.5",
    "version": "3.5",
    "bulb_type": "TypeB"
  }
]
```

*(You can also add and scan for bulbs directly from the Web Dashboard!)*

### 3. Start the Web Dashboard
Run the single-script runner:
```bash
./start.sh
# Or specify a custom port:
./start.sh 8080
```
Open your browser to:
- **Desktop**: [http://localhost:8080](http://localhost:8080)
- **Mobile**: `http://<your-computer-ip>:8080` (accessible from your phone on the same Wi-Fi!)

The dashboard gives you:
- Live audio visualizer and beat telemetry via WebSockets.
- One-click Start/Stop Beat Sync.
- Audio input device selector (Microphone, Loopback, BlackHole).
- Color theme presets: **Cyberpunk**, **Synthwave**, **Club Rave**, **Vaporwave**, **Ambient Chill**.
- Local UDP network scanner to discover active bulbs.

### 4. Direct Command-Line Control
To turn a bulb on/off or change its hue directly:
```bash
# Turn ON and set color to Cyan (Hue 180°)
target/release/iot-lights control \
  --id "d79466ba2de1ef6565hrzd" \
  --key 'qxkIfvV3p?KjPZ#7' \
  --ip "192.168.1.2" \
  --power true --hue 180 --version 3.5

# Set warm white mode
target/release/iot-lights control \
  --id "d79466ba2de1ef6565hrzd" \
  --key 'qxkIfvV3p?KjPZ#7' \
  --ip "192.168.1.2" \
  --brightness 800 --version 3.5

# Turn OFF
target/release/iot-lights control \
  --id "d79466ba2de1ef6565hrzd" \
  --key 'qxkIfvV3p?KjPZ#7' \
  --ip "192.168.1.2" \
  --power false --version 3.5
```

### 5. Standalone Beat-Sync CLI
Sync a single bulb directly from the terminal without opening the dashboard:
```bash
target/release/iot-lights sync \
  --id "d79466ba2de1ef6565hrzd" \
  --key 'qxkIfvV3p?KjPZ#7' \
  --ip "192.168.1.2" \
  --palette cyberpunk \
  --version 3.5
```

---

## ❓ Frequently Asked Questions (FAQ) & Troubleshooting

### Local Key Questions

#### Q: What is a `localkey` and why is it needed?
Tuya devices use a pre-shared 16-character AES key for all encrypted communications over the local network (Port 6668). Without this key, local commands cannot be signed or decrypted.

#### Q: Does my local key expire?
**No.** As long as your bulb remains paired with the Smart Life app and you do not reset it, the key is permanent and valid indefinitely.

#### Q: What happens if I reset or re-pair my bulb in the Smart Life app?
**Tuya generates a new random `localkey` every time a device is factory reset or re-paired.** If you delete and re-add the bulb, you must re-run `tinytuya` or check the Tuya Cloud API Explorer to get the newly generated key.

#### Q: I get `decode error` or `decrypt failed`. What's wrong?
This means the `localkey` provided does not match the key on the bulb. Re-extract your keys using Path 1.

---

### Connectivity & Protocol Versions

#### Q: Why do I get `request timed out` or `connection refused`?
The most common reasons are:
1. **The Single TCP Connection Limit (Most Common!)**:
   Tuya Wi-Fi microcontrollers only allow **one concurrent TCP client** on port 6668. If your **Smart Life mobile app is open on your phone**, it holds the TCP connection open and blocks IOT-Lights!
   > **Fix**: Force-close or background the Smart Life app on your phone, and ensure no other Python/CLI scripts are running.
2. **Subnet / AP Isolation**:
   Your computer and bulbs must be on the same subnet (e.g. `192.168.1.x`). Ensure your router does not have "AP Isolation" or "Guest Network Isolation" enabled.
3. **Bulb is Offline / Switched Off**:
   Ensure the physical wall switch is turned on and the bulb is powered.

#### Q: What protocol version should I choose (`3.1`, `3.3`, `3.4`, `3.5`)?
- **Version 3.5 (Recommended for modern bulbs)**: Bulbs purchased from 2022 onwards (including Wipro, Syska, and generic 12W/14W bulbs) typically run Tuya protocol 3.5 with enhanced HMAC-SHA256 framing.
- **Version 3.3**: Older Tuya hardware (prior to 2022).
- **Version 3.1**: Very old legacy devices.
- By default, IOT-Lights uses `3.5`. If a device fails to handshake, try `3.3`.

---

### Which Devices Will Work?

IOT-Lights works with any Wi-Fi smart bulb, strip, or ceiling light compatible with the **Tuya / Smart Life** ecosystem.

#### Tested Hardware
The following hardware devices have been physically tested and verified with IOT-Lights:
- **Wipro RGBCW 12.5W Smart Bulb** (`v3.5`, Type B DPS 20-27)
- **Smart Bulb 14 Watt (Tuya Generic / OEM)** (`v3.5`, Type B DPS 20-26 / 41)

#### Compatible Brands & Devices:
- **Wipro** Smart LED Bulbs & Batten Lights
- **Syska** Smart Lights
- **Treatlife** Smart Bulbs & Dimmer Switches
- **Gosund / NiteBird** Smart Bulbs
- **Lumary** Recessed LED Lights
- **Avatar Controls** Smart Bulbs
- **Teckin**, **Novostella**, and generic **Tuya / Smart Life** OEM lighting hardware

---

### Why DP 27 Music Mode is Special

Traditional smart bulb controllers send color updates via DP 24 (or DP 5). The bulb's internal firmware applies an automatic 300–500ms PWM smooth ramp to color transitions, creating noticeable visual lag during fast music.

IOT-Lights takes advantage of **Tuya DP 27 (`music_data`)**:
- A specialized 21-character real-time streaming frame: `0` (jump action) + 4-hex Hue + 4-hex Saturation + 4-hex Value + 4-hex Brightness + 4-hex Color Temperature.
- Bypasses the MCU's internal fade curve, allowing true zero-delay strobe and beat synchronicity up to 15–20 updates per second!

---

## 🤝 Contributing

We welcome contributions to IOT-Lights! Whether you are adding support for new Tuya DPS schemas, optimizing the DSP beat-detection algorithm, creating new color choreographies, or expanding documentation, check out our [**CONTRIBUTING.md**](CONTRIBUTING.md) for full development guidelines, testing instructions, and PR requirements.

---

## 🌟 Acknowledgments & Shout-Outs

IOT-Lights stands on the shoulders of fantastic open-source projects:

- [**TinyTuya**](https://github.com/jasonacox/tinytuya) by **Jason Cox**: The gold standard for Tuya local protocol reverse-engineering, network scanning, and Python local key extraction.
- [**rustuya**](https://github.com/robtaylor/rustuya) by **Rob Taylor**: Sans-I/O Rust state machine implementation of the Tuya protocol framing and cryptographic primitives.
- [**cpal**](https://github.com/RustAudio/cpal): Cross-platform, low-latency audio ingestion in Rust.
- [**realfft**](https://github.com/ejmahler/RustFFT): Ultra-fast real-to-complex Fast Fourier Transforms in pure Rust.

---

<p align="center">
  Made with ❤️ and pure Rust for low-latency lighting.
</p>
