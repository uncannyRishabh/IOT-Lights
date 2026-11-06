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
2. In the **Bulb Fleet** section, click **Scan Network**.
3. Any Tuya devices broadcasting on UDP 6666/6667 will appear under "DISCOVERED ON NETWORK".
4. Click **Use This** to automatically fill in the Device ID and IP address!

### Option 2: Command-Line Scanner
Run the standalone scanner tool:
```bash
cargo run --bin iot-lights -- scan --seconds 5
```
Output:
```text
eb65b1234567890abcdef    192.168.1.45       3.3
Scan complete. Found 1 device(s).
```

### Option 3: Router DHCP Table
Log into your Wi-Fi router's admin portal (usually `192.168.1.1` or `192.168.0.1`), locate the DHCP Client List, and assign a **DHCP Reservation** (static IP) to each bulb's MAC address so its IP address never changes when your router reboots.

---

## 4. Connecting Your Bulb

### Via the Web Dashboard
1. Open `http://localhost:8080` (or `http://<your-lan-ip>:8080` on mobile).
2. Scroll to the **Connect New Bulb** form.
3. Enter:
   - **Device ID**: 22 characters
   - **Local Key**: 16 characters
   - **Local IP**: Bulb IP (e.g. `192.168.1.45`)
   - **Bulb Type**: `Type B` (for almost all modern bulbs made after 2019) or `Type A` (legacy)
4. Click **Add to Fleet**.
5. The bulb card will appear with a green **Connected** badge.
6. Test instant connectivity by clicking **ON**, **OFF**, **Cyan**, **Pink**, or **White**.

### Via the CLI
To test direct music beat-sync from your terminal without opening a browser:
```bash
cargo run --bin iot-lights -- sync \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --palette cyberpunk
```

To control single bulb attributes via CLI:
```bash
# Turn ON and set to Amber (hue 45°)
cargo run --bin iot-lights -- control \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --power true \
  --hue 45

# Turn OFF
cargo run --bin iot-lights -- control \
  --id "eb65b1234567890abcdef" \
  --key "a1b2c3d4e5f6g7h8" \
  --ip "192.168.1.45" \
  --power false
```

---

## 5. macOS Audio Setup (Clean Internal Audio Sync)

IOT-Lights analyzes audio transients (kick drums, snare snaps, and bass drop dynamics) in real time.

### Option A: Default Built-In Microphone
- Works out of the box with zero additional configuration.
- Best for listening to music playing in your room from external speakers.
- In the dashboard, select **Built-in Microphone (Default)**.

### Option B: BlackHole Virtual Audio Driver (Studio Quality, Zero Room Echo)
If you are playing music directly on your Mac (Spotify, Apple Music, YouTube, DJ software) and want pure, noise-free beat synchronization:

1. **Install BlackHole 2-Channel Virtual Loopback**:
   ```bash
   brew install blackhole-2ch
   ```
2. **Create a Multi-Output Device** (so you can hear the music while sending it to IOT-Lights):
   - Open macOS **Audio MIDI Setup** (press `Cmd + Space`, search `Audio MIDI Setup`).
   - Click the **`+`** icon in the bottom-left corner -> select **Create Multi-Output Device**.
   - Check both:
     - [x] **Your Speakers / Headphones** (e.g. MacBook Pro Speakers, External Headphones)
     - [x] **BlackHole 2ch**
   - Ensure your physical speakers are set as the *Master Device* and *Drift Correction* is checked for BlackHole.
3. **Set System Output**:
   - In macOS System Settings -> **Sound** -> **Output**, select **Multi-Output Device**.
4. **Configure IOT-Lights**:
   - In the IOT-Lights Web Dashboard, select **BlackHole 2ch** from the **Audio Capture Input** dropdown.
   - Click **Start Beat Sync**.
   - Your smart lights will now pulse and color-shift to the pure digital audio stream with zero room echo or microphone feedback!

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
