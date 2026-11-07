# Connecting Your Smart Lights to IOT-Lights

This guide walks you through pairing and controlling your Tuya / Smart Life compatible smart bulbs (Wipro, Syska, Gosund, Treatlife, Lumary, Avatar, etc.) with IOT-Lights for real-time, low-latency music beat synchronization.

---

## 1. Required Credentials Overview

To communicate directly over your local Wi-Fi without cloud lag, each bulb needs:

| Parameter | Length | Description | Example |
| :--- | :--- | :--- | :--- |
| **`device_id`** | 22 chars | Unique Tuya hardware identifier | `eb65b1234567890abcdef` |
| **`local_key`** | 16 chars | AES-128 local LAN encryption key | `a1b2c3d4e5f6g7h8` |
| **`ip`** | IPv4 | Local network IP address of the bulb | `192.168.1.45` |
| **`bulb_type`** | Type B / Type A | DPS mapping (Type B for modern, Type A for legacy) | `TypeB` |

---

## 2. Extracting Your `device_id` and `local_key`

### Method A: Official Tuya IoT Cloud Platform (Recommended)

This method extracts the permanent `local_key` directly from Tuya's developer backend without modifying your phone or light.

1. **Create a Free Tuya Developer Account**:
   - Go to [Tuya IoT Platform](https://iot.tuya.com/) and register or sign in.
2. **Create a Cloud Development Project**:
   - In the left sidebar, navigate to **Cloud** -> **Development Projects**.
   - Click **Create Cloud Project**.
   - Set:
     - Project Name: `IOT-Lights`
     - Industry: `Smart Home`
     - Development Method: `Custom Development`
     - Data Center: Select the region where your account was registered (e.g. *Western America Data Center*, *Central Europe*, or *India*).
3. **Authorize Cloud Services**:
   - On the service API configuration step, ensure **IoT Core**, **Authorization Token Management**, and **Device Status Notification** are selected.
4. **Link Your Smart Life / Tuya Smart Mobile App**:
   - In your newly created project, open the **Devices** tab -> **Link Tuya App Account**.
   - Click **Add App Account**. A QR code will display on the screen.
   - On your smartphone, open the **Smart Life** or **Tuya Smart** app:
     - Go to **Me** -> tap the **QR Scanner icon** in the top-right corner.
     - Scan the QR code and tap **Confirm Login**.
   - Once linked, all your paired bulbs will be visible under the project!
5. **Read the Device Keys**:
   - In the left navigation, go to **Cloud** -> **API Explorer**.
   - Select your project from the top dropdown.
   - In the API tree, navigate to:
     `Smart Home Device System` -> `Device Control` -> `Get Device Details`
   - Paste your bulb's `device_id` (obtained from the Device List or bulb settings in the mobile app) into the `device_id` field.
   - Click **Submit Request**.
   - In the JSON response, locate:
     - `"id"`: Your 22-character `device_id`
     - `"local_key"`: Your 16-character `local_key`
     - `"name"`: Device name (e.g., Living Room Light)

> **Note:** If you ever reset or re-pair the bulb with the Tuya app, Tuya will generate a new `local_key`. Repeat step 5 if you re-pair.

---

### Method B: Automated CLI Extraction via TinyTuya Wizard

If you have Python installed, you can automatically extract all device IDs and local keys across your account:

```bash
pip install tinytuya
python -m tinytuya wizard
```

Enter your Tuya Developer API Key, API Secret, and Region when prompted. The wizard downloads a `devices.json` file containing all bulbs with their `name`, `id`, `key`, and discovered `ip`.

---

## 3. Finding Your Bulb's Local IP Address

Your bulb must be on the **same local network / Wi-Fi subnet** (2.4 GHz) as your computer.

### Option 1: Built-in Network Scanner in IOT-Lights Dashboard
1. Launch the server via `./start.sh` and open `http://localhost:8080`.
2. In the **Bulb Fleet Rail** section, click **Scan LAN**.
3. Any Tuya devices broadcasting on UDP 6666/6667 will appear under discovered devices.
4. Each discovered device card contains:
   - **Fill Form**: Pre-populates the manual device modal.
   - **Inline Local Key Field & "+ Pair & Save to JSON"**: Enter your 16-character local key directly and pair the bulb with a single click, automatically writing it permanently to `bulbs.json`!
5. You can also click **Save to bulbs.json** anytime in the Fleet Rail card header to permanently persist active fleet changes.

### Option 2: Command-Line Scanner & Interactive TUI
Run the standalone scanner tool:
```bash
cargo run --bin iot-bulb-sync -- scan --seconds 5
```
Or launch the 24-bit Truecolor terminal controller:
```bash
cargo run --bin iot-bulb-sync -- tui
# or
./start.sh --tui
```
Inside the TUI:
- Press `[M]` to hot-swap between Beat Sync and IMAX Mode in-place without restarting.
- Press `[D]` to cycle target display (when multiple monitors are available in IMAX mode).
- Press `[P]` to trigger live UDP 6666/6667 LAN discovery, list responsive bulbs, and pair them with 1–9.
- Press `[S]` to atomically save the active fleet configuration permanently to `bulbs.json`.
- Press `[Space]` to toggle fleet power ON/OFF.
- Press `[Q]` or `[Esc]` to quit cleanly and restore terminal raw mode.

### Option 3: Router DHCP Table
Log into your Wi-Fi router's admin portal (usually `192.168.1.1` or `192.168.0.1`), locate the DHCP Client List, and assign a **DHCP Reservation** (static IP) to each bulb's MAC address so its IP address never changes when your router reboots.

---

## 4. Connecting Your Bulb

### Via the Web Dashboard
1. Open `http://localhost:8080` (or `http://<your-lan-ip>:8080` on mobile).
2. All devices in `bulbs.json` are automatically loaded and connected on startup.
3. To add a new bulb, open **Connect New Bulb**, enter your **Device ID**, **Local Key**, and **Local IP**, and click **Add to Fleet**.
4. Click **Save to bulbs.json** in the card header to write changes permanently.
5. Use the single **Power ON/OFF Toggle Switch** or open the **Full 360° Color & White Wheel** on any bulb card.

### Via the CLI
To launch the interactive Truecolor terminal visualizer & controller:
```bash
cargo run --release --bin iot-bulb-sync -- tui
```

To run IMAX Prominent-Color Screen Sync across your fleet:
```bash
cargo run --release --bin iot-bulb-sync -- imax --fps 20 --color-mode prominent
```

To test direct music beat-sync from your terminal:
```bash
cargo run --release --bin iot-bulb-sync -- sync \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --palette cyberpunk
```

To control single bulb attributes via CLI:
```bash
# Turn ON and set to Amber (hue 45°)
cargo run --release --bin iot-bulb-sync -- control \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --power true \
  --hue 45

# Turn OFF
cargo run --release --bin iot-bulb-sync -- control \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --power false
```

---

## 5. Audio Setup (Clean Internal Audio Loopback)

IOT-Lights analyzes audio transients (kick drums, snare snaps, vocal pitch formants, and bass drop dynamics) in real time.

### Option A: Default Built-In Microphone
- Works out of the box with zero additional configuration on macOS, Linux, and Windows.
- Best for listening to music playing in your room from external speakers.
- In the dashboard or TUI, select your default microphone.

### Option B: macOS Digital Audio Loopback (BlackHole)
1. Install BlackHole 2-Channel Virtual Loopback:
   ```bash
   brew install blackhole-2ch
   ```
2. Open **Audio MIDI Setup** (`Cmd + Space` -> `Audio MIDI Setup`).
3. Click `+` -> **Create Multi-Output Device** -> check both your physical Speakers/Headphones and **BlackHole 2ch**.
4. Set macOS output to the Multi-Output Device.
5. In IOT-Lights, select **BlackHole 2ch** from the audio dropdown.

### Option C: Linux PipeWire / PulseAudio Digital Loopback
- IOT-Lights natively queries ALSA and PulseAudio/PipeWire `.monitor` capture streams.
- Simply select the `.monitor` device corresponding to your default audio sink (e.g., `alsa_output.pci-...monitor`).

### Option D: Windows WASAPI Digital Loopback
- IOT-Lights automatically scans and exposes all active Windows WASAPI render endpoints as `[Loopback] <Device Name>`.
- Select your active speaker or headphone loopback device to sync directly from internal audio with zero microphone latency!

---

## 6. Troubleshooting & Gotchas

- **"Connection Refused" or "Timeout"**:
  - Tuya bulbs only accept **one concurrent TCP client**. If your official Tuya Smart or Smart Life app on your phone is actively viewing the bulb's control panel, close the phone app completely.
  - Verify your computer and the bulb are connected to the same router subnet (not isolated guest networks).
- **"Invalid Protocol / Decode Error"**:
  - Most modern bulbs use Tuya protocol `3.3`. Some newer bulbs use `3.4` or `3.5`. IOT-Lights automatically discovers and negotiates the proper protocol.
- **Bulb Not Responding to Music Sync**:
  - Ensure the bulb is physically turned ON via wall switch.
  - Try toggling **Type B** vs **Type A** in the configuration if color changes don't apply.
