# Contributing to IOT-Lights

Thank you for your interest in contributing to **IOT-Lights**! We welcome contributions of all kinds: bug fixes, documentation improvements, new lighting choreography algorithms, expanded Tuya device profile support, and performance optimizations.

---

## Workspace Architecture

IOT-Lights is organized as a Cargo workspace with clean separation of concerns:

| Crate | Responsibility |
| :--- | :--- |
| [`crates/rustuya-core`](crates/rustuya-core) | Pure sans-I/O Tuya protocol state machine, crypto (AES-128-ECB, AES-128-CBC, HMAC-SHA256, GCM), command generators, and frame codecs. |
| [`crates/rustuya-tokio`](crates/rustuya-tokio) | Asynchronous Tokio runtime driver, UDP network discovery, TCP actor loop, and connection rate limiters. |
| [`crates/tuya-bulb`](crates/tuya-bulb) | Smart bulb abstractions, Type A & Type B DPS data point models, HSV/RGB color space conversions, and DP 27 high-speed music streaming frames. |
| [`crates/beat-sync`](crates/beat-sync) | CPAL audio stream ingestion, RealFFT spectral flux beat detector, multi-palette lighting choreographer, and `BulbFleet` connection manager. |
| [`crates/backend`](crates/backend) | Axum REST API server, WebSocket real-time telemetry broadcaster, mobile-optimized SVG dashboard, and CLI tool (`iot-lights`). |

---

## Development Setup

### 1. Prerequisites
- **Rust Toolchain**: Rust 1.75+ (install via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).
- **macOS Audio**: Built-in CoreAudio works out of the box. For desktop audio loopback, install [BlackHole 2ch](https://github.com/ExistentialAudio/BlackHole).
- **Linux Audio**: ALSA development headers:
  ```bash
  sudo apt install libasound2-dev pkg-config
  ```

### 2. Building and Testing
```bash
# Clone the repository
git clone https://github.com/your-username/IOT-Lights.git
cd IOT-Lights

# Build the entire workspace
cargo build

# Run unit and integration tests
cargo test

# Check formatting and linter
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

---

## How to Contribute

### 1. Adding Support for New Bulbs or DPS Schemas
Tuya manufacturers use slightly different Data Point (DPS) mappings for their firmware. If you encounter a bulb with non-standard DPS mappings:
1. Probe the device DPS using `tinytuya.OutletDevice(id, ip, key).status()` or `iot-lights scan`.
2. Inspect or extend `BulbType` or `BulbState::from_dps` in [`crates/tuya-bulb/src/lib.rs`](crates/tuya-bulb/src/lib.rs).
3. Add a unit test verifying serialization and deserialization of the new payload.

### 2. Creating New Lighting Choreographies & Palettes
Lighting transitions and color palettes live in [`crates/beat-sync/src/choreographer.rs`](crates/beat-sync/src/choreographer.rs):
- Add a new `ColorPalette` enum variant.
- Define dominant, accent, drop, and ambient HSV colors.
- Tune the energy curve or kick/snare sensitivity thresholds.

### 3. Improving the Web Dashboard
The web dashboard is served directly from [`crates/backend/src/main.rs`](crates/backend/src/main.rs) as an ultra-fast, zero-dependency embedded HTML/CSS/SVG SPA.
- Keep assets pure SVG and vanilla CSS (no external CDN dependencies required for offline local network operation).
- Ensure mobile responsiveness for phones controlling lights over Wi-Fi.

---

## Pull Request Guidelines

1. **Create a Topic Branch**:
   ```bash
   git checkout -b feature/my-cool-feature
   ```
2. **Write Clean, Idiomatic Code**:
   - Maintain documentation integrity and preserve existing docstrings.
   - Run `cargo fmt` and ensure `cargo clippy` produces no warnings.
3. **Test with Physical Hardware (if applicable)**:
   - If modifying protocol or timing code, verify that bulbs don't suffer TCP disconnects or frame drops.
4. **Submit Your PR**:
   - Write a clear description of the problem solved and test steps conducted.
   - Reference any related issues.

---

## Community & Code of Conduct

Please be respectful and constructive in all discussions, issues, and code reviews. We are here to build great open-source tools together!
