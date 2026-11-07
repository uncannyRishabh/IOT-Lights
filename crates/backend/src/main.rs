//! IOT-Bulb Sync: Backend server, CLI, and IMAX Prominent Color Screen Sync controller for Tuya smart bulbs.

pub mod local_identity;
pub mod tui;

use std::collections::VecDeque;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message as AxumWsMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;

use beat_sync::{
    AudioDeviceInfo, AudioFocusMode, AudioStream, BeatDetector, BulbAmbienceMode, BulbConfig,
    BulbFleet, Choreographer, ColorPalette, DisplayInfo, DspConfig, FleetBulbInfo, ImaxColorMode,
    ImaxConfig, ImaxEngine, TransitionMode,
};
use local_identity::{
    infer_friendly_bulb_name, load_devices_json, lookup_arp_mac, resolve_fleet_config_path,
    save_devices_json, SavedDeviceRecord,
};
use rustuya_tokio::{Discovery, Version};
use tuya_bulb::{BulbColor, BulbType, ColorFormat};

#[derive(Parser, Debug)]
#[command(
    name = "iot-bulb-sync",
    about = "Real-time music beat-sync & IMAX prominent-color screen sync controller for Tuya smart bulbs"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the REST & WebSocket backend server with Web Dashboard
    Serve {
        #[arg(short, long, default_value = "8080")]
        port: u16,
        /// Launch the 24-bit Truecolor terminal visualizer & controller
        #[arg(long)]
        tui: bool,
        /// Suppress interactive TUI and run headless server logs only
        #[arg(long)]
        no_tui: bool,
    },
    /// Interactive 24-bit Truecolor ANSI terminal visualizer & controller
    Tui {
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
    /// Scan and discover Tuya smart devices on the local Wi-Fi network
    Scan {
        #[arg(short, long, default_value = "5")]
        seconds: u64,
    },
    /// Direct command-line music beat-sync to a bulb
    Sync {
        #[arg(long)]
        id: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        ip: String,
        #[arg(long, default_value = "cyberpunk")]
        palette: String,
        #[arg(long)]
        device: Option<String>,
        #[arg(long, default_value = "3.5")]
        version: String,
    },
    /// High-performance CLI screen sync (IMAX Mode with multi-display prompt & prominent color engine)
    Imax {
        #[arg(long, default_value = "20")]
        fps: u32,
        /// Display index (if omitted and multiple displays are detected, prompts interactively)
        #[arg(long)]
        display: Option<u32>,
        #[arg(long, default_value = "0.82")]
        smoothing: f32,
        #[arg(long, default_value = "1.35")]
        saturation_boost: f32,
        #[arg(long, default_value = "25")]
        min_brightness: u32,
        #[arg(long, default_value = "1000")]
        max_brightness: u32,
        /// Color mode: "prominent" (dominant chromatic cluster) or "ambient" (screen average)
        #[arg(long, default_value = "prominent")]
        color_mode: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        ip: Option<String>,
        #[arg(long, default_value = "3.5")]
        version: String,
        /// Launch the 24-bit Truecolor terminal visualizer & controller
        #[arg(long)]
        tui: bool,
    },
    /// Directly control a bulb (turn on/off, color, white)
    Control {
        #[arg(long)]
        id: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        ip: String,
        #[arg(long)]
        power: Option<bool>,
        #[arg(long)]
        hue: Option<f32>,
        #[arg(long)]
        brightness: Option<u32>,
        #[arg(long, default_value = "3.5")]
        version: String,
    },
}

fn default_custom_palette() -> Vec<BulbColor> {
    vec![
        BulbColor::from_rgb(245, 158, 11), // Amber #F59E0B
        BulbColor::from_rgb(244, 63, 94),  // Coral #F43F5E
        BulbColor::from_rgb(99, 102, 241), // Indigo #6366F1
        BulbColor::from_rgb(16, 185, 129), // Emerald #10B981
        BulbColor::from_rgb(0, 240, 255),  // Cyan #00F0FF
        BulbColor::from_rgb(168, 85, 247), // Purple #A855F7
    ]
}

fn default_transition_speed() -> f32 {
    0.65
}

fn default_color_frequency() -> f32 {
    0.70
}

fn default_noise_gate_db() -> f32 {
    -45.0
}

fn default_sensitivity() -> f32 {
    1.0
}

fn default_imax_fps() -> u32 {
    20
}

fn default_imax_smoothing() -> f32 {
    0.82
}

fn default_imax_saturation() -> f32 {
    1.35
}

/// Active engine mode (`BeatSync` vs `Imax`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ActiveEngineMode {
    #[default]
    BeatSync,
    Imax,
}

/// Dynamic synchronization, audio-reactive, and IMAX screen settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    #[serde(default)]
    pub engine_mode: ActiveEngineMode,
    pub palette: ColorPalette,
    #[serde(default = "default_custom_palette")]
    pub custom_palette: Vec<BulbColor>,
    pub ambience_mode: BulbAmbienceMode,
    pub master_brightness: f32,
    #[serde(default)]
    pub transition_mode: TransitionMode,
    #[serde(default)]
    pub invert_auto_gradual: bool,
    #[serde(default = "default_transition_speed")]
    pub transition_speed: f32,
    #[serde(default = "default_color_frequency")]
    pub color_frequency: f32,
    pub auto_normalize: bool,
    #[serde(default = "default_noise_gate_db")]
    pub noise_gate_db: f32,
    #[serde(default = "default_sensitivity")]
    pub sensitivity: f32,
    #[serde(default)]
    pub focus_mode: AudioFocusMode,
    pub audio_device: Option<String>,
    // IMAX Mode settings (shared between CLI and Web UI)
    #[serde(default)]
    pub imax_display: u32,
    #[serde(default = "default_imax_fps")]
    pub imax_fps: u32,
    #[serde(default = "default_imax_smoothing")]
    pub imax_smoothing: f32,
    #[serde(default = "default_imax_saturation")]
    pub imax_saturation_boost: f32,
    #[serde(default)]
    pub imax_color_mode: ImaxColorMode,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            engine_mode: ActiveEngineMode::BeatSync,
            palette: ColorPalette::Cyberpunk,
            custom_palette: default_custom_palette(),
            ambience_mode: BulbAmbienceMode::DarkMode,
            master_brightness: 1.0,
            transition_mode: TransitionMode::Auto,
            invert_auto_gradual: false,
            transition_speed: default_transition_speed(),
            color_frequency: default_color_frequency(),
            auto_normalize: true,
            noise_gate_db: default_noise_gate_db(),
            sensitivity: default_sensitivity(),
            focus_mode: AudioFocusMode::FullMix,
            audio_device: None,
            imax_display: 0,
            imax_fps: default_imax_fps(),
            imax_smoothing: default_imax_smoothing(),
            imax_saturation_boost: default_imax_saturation(),
            imax_color_mode: ImaxColorMode::Prominent,
        }
    }
}

/// WebSocket broadcast packet to live web clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsTelemetry {
    pub engine_mode: ActiveEngineMode,
    pub imax_cli_active: bool,
    pub is_kick: bool,
    pub is_snare: bool,
    pub is_vocal_onset: bool,
    pub has_instruments: bool,
    pub instrument_score: f32,
    pub active_gradual: bool,
    pub current_db: f32,
    pub gate_open: bool,
    pub bass_intensity: f32,
    pub snare_intensity: f32,
    pub instrument_intensity: f32,
    pub vocal_intensity: f32,
    pub overall_energy: f32,
    pub active_hue: f32,
    pub active_brightness: u32,
    pub active_rgb: (u8, u8, u8),
    pub agc_gain: f32,
    #[serde(default = "default_effective_freq")]
    pub effective_color_frequency: f32,
    #[serde(default = "default_effective_interval")]
    pub effective_color_interval_ms: u32,
}

fn default_effective_freq() -> f32 {
    0.70
}

fn default_effective_interval() -> u32 {
    160
}

/// Runtime state written by CLI `iot-bulb-sync imax` so Web UI knows when CLI IMAX is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImaxCliRuntimeState {
    pub active: bool,
    pub pid: u32,
    pub timestamp_ms: u64,
    pub display: u32,
    pub fps: u32,
    pub smoothing: f32,
    pub saturation_boost: f32,
    pub color_mode: ImaxColorMode,
    pub active_rgb: (u8, u8, u8),
    pub active_brightness: u32,
}

/// Shared application state for axum.
#[derive(Clone)]
pub struct AppState {
    pub fleet: BulbFleet,
    pub sync_running: Arc<AtomicBool>,
    pub settings: Arc<RwLock<SyncSettings>>,
    pub ws_tx: broadcast::Sender<WsTelemetry>,
    pub devices_path: std::path::PathBuf,
    pub settings_path: std::path::PathBuf,
}

const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">
  <defs>
    <linearGradient id="gearGrad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#F59E0B"/>
      <stop offset="55%" stop-color="#D97706"/>
      <stop offset="100%" stop-color="#94A3B8"/>
    </linearGradient>
    <linearGradient id="domeGrad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F0FF"/>
      <stop offset="55%" stop-color="#F59E0B"/>
      <stop offset="100%" stop-color="#F43F5E"/>
    </linearGradient>
  </defs>
  <rect width="64" height="64" rx="14" fill="#090A0D" stroke="#222631" stroke-width="2"/>
  <path d="M30 6 L34 6 L35 11 L39 12.5 L43 9.5 L46 12.5 L43 16.5 L44.5 20.5 L49.5 21.5 L49.5 25.5 L44.5 26.5 L43 30.5 L46 34.5 L43 37.5 L39 34.5 L35 36 L34 41 L30 41 L29 36 L25 34.5 L21 37.5 L18 34.5 L21 30.5 L19.5 26.5 L14.5 25.5 L14.5 21.5 L19.5 20.5 L21 16.5 L18 12.5 L21 9.5 L25 12.5 L29 11 Z"
        transform="translate(0, 4)" fill="none" stroke="url(#gearGrad)" stroke-width="2.4" stroke-linejoin="round" opacity="0.9"/>
  <path d="M23 38 C18 33 18 22 25 17 C29 13.5 35 13.5 39 17 C46 22 46 33 41 38 C38.5 40.5 37.5 43 37.5 46 L26.5 46 C26.5 43 25.5 40.5 23 38 Z"
        fill="url(#domeGrad)" fill-opacity="0.24" stroke="url(#domeGrad)" stroke-width="2.5" stroke-linejoin="round"/>
  <circle cx="32" cy="27" r="4.5" fill="none" stroke="#F8FAFC" stroke-width="2"/>
  <line x1="29" y1="30.5" x2="29" y2="45" stroke="#F59E0B" stroke-width="2" stroke-linecap="round"/>
  <line x1="35" y1="30.5" x2="35" y2="45" stroke="#F59E0B" stroke-width="2" stroke-linecap="round"/>
  <line x1="27" y1="50" x2="37" y2="50" stroke="#CBD5E1" stroke-width="2.6" stroke-linecap="round"/>
  <line x1="29" y1="54" x2="35" y2="54" stroke="#94A3B8" stroke-width="2.6" stroke-linecap="round"/>
</svg>"##;

static TUI_RUNNING: AtomicBool = AtomicBool::new(false);
static LOG_RING_BUFFER: Mutex<Option<VecDeque<String>>> = Mutex::new(None);

pub fn set_tui_running(running: bool) {
    TUI_RUNNING.store(running, Ordering::SeqCst);
}

pub fn get_latest_log() -> Option<String> {
    if let Ok(guard) = LOG_RING_BUFFER.lock() {
        guard.as_ref().and_then(|q| q.back().cloned())
    } else {
        None
    }
}

struct TuiAwareLogger {
    default_level: log::LevelFilter,
}

impl log::Log for TuiAwareLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.default_level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let msg = format!("{}", record.args());
        if let Ok(mut guard) = LOG_RING_BUFFER.lock() {
            let q = guard.get_or_insert_with(|| VecDeque::with_capacity(32));
            if q.len() >= 32 {
                q.pop_front();
            }
            q.push_back(msg.clone());
        }

        if !TUI_RUNNING.load(Ordering::Relaxed) {
            eprintln!(
                "[{:5} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logger = TuiAwareLogger {
        default_level: log::LevelFilter::Info,
    };
    let _ = log::set_boxed_logger(Box::new(logger));
    log::set_max_level(log::LevelFilter::Info);

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Serve {
        port: 8080,
        tui: false,
        no_tui: false,
    }) {
        Commands::Serve { port, tui, no_tui } => {
            if tui && !no_tui {
                run_tui_mode(port).await?;
            } else {
                run_server(port).await?;
            }
        }
        Commands::Tui { port } => run_tui_mode(port).await?,
        Commands::Scan { seconds } => run_scan(seconds).await?,
        Commands::Sync {
            id,
            key,
            ip,
            palette,
            device,
            version,
        } => run_cli_sync(id, key, ip, palette, device, version).await?,
        Commands::Imax {
            fps,
            display,
            smoothing,
            saturation_boost,
            min_brightness,
            max_brightness,
            color_mode,
            id,
            key,
            ip,
            version,
            tui,
        } => {
            if tui {
                run_tui_mode(8080).await?;
            } else {
                run_cli_imax(
                    fps,
                    display,
                    smoothing,
                    saturation_boost,
                    min_brightness,
                    max_brightness,
                    color_mode,
                    id,
                    key,
                    ip,
                    version,
                )
                .await?
            }
        }
        Commands::Control {
            id,
            key,
            ip,
            power,
            hue,
            brightness,
            version,
        } => run_cli_control(id, key, ip, power, hue, brightness, version).await?,
    }

    Ok(())
}

async fn run_scan(seconds: u64) -> anyhow::Result<()> {
    log::info!("Scanning local network for Tuya smart devices for {seconds}s (UDP 6666/6667)...");
    let disco = Discovery::builder()
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to bind discovery socket: {:?}", e))?;

    let devices = disco.scan(Duration::from_secs(seconds)).await;
    let saved = load_devices_json(&resolve_fleet_config_path());

    println!(
        "\n{:<24} {:<16} {:<18} {:<8} {:<24}",
        "DEVICE ID", "IP ADDRESS", "MAC ADDRESS", "VERSION", "FRIENDLY NAME"
    );
    println!("{}", "-".repeat(92));

    for info in &devices {
        let ip_str = info.ip.to_string();
        let mac = lookup_arp_mac(&ip_str);
        let name = infer_friendly_bulb_name(&info.id, &ip_str, mac.as_deref(), &saved);
        let ver = info.version.map(|v| v.as_str()).unwrap_or("3.3");
        println!(
            "{:<24} {:<16} {:<18} {:<8} {:<24}",
            info.id,
            ip_str,
            mac.as_deref().unwrap_or("--:--:--:--:--:--"),
            ver,
            name
        );
    }

    println!("{}\nScan complete. Found {} device(s).", "-".repeat(92), devices.len());
    disco.close().await;
    Ok(())
}

async fn run_cli_control(
    id: String,
    key: String,
    ip: String,
    power: Option<bool>,
    hue: Option<f32>,
    brightness: Option<u32>,
    version_str: String,
) -> anyhow::Result<()> {
    let ver = match version_str.as_str() {
        "3.1" => Version::V3_1,
        "3.2" => Version::V3_2,
        "3.4" => Version::V3_4,
        "3.5" => Version::V3_5,
        _ => Version::V3_3,
    };
    log::info!("Connecting to bulb {id} at {ip} (v{version_str})...");
    let dev = rustuya_tokio::Device::builder(&id, key.as_bytes())
        .address(&ip)
        .version(ver)
        .connect()?;

    let bulb = tuya_bulb::BulbDevice::new(dev, BulbType::TypeB, ColorFormat::Hex12);

    log::info!("Waiting for TCP connection...");
    bulb.wait_connected(Duration::from_secs(5)).await?;
    log::info!("Connected!");

    if let Some(p) = power {
        if p {
            bulb.turn_on().await?;
            println!("Bulb turned ON");
        } else {
            bulb.turn_off().await?;
            println!("Bulb turned OFF");
        }
    }

    if let Some(h) = hue {
        let color = BulbColor::from_hsv(h, 1.0, 1.0);
        bulb.set_color(color).await?;
        println!("Set color hue to {h}°");
    }

    if let Some(b) = brightness {
        bulb.set_white(b, 500).await?;
        println!("Set white brightness to {b}");
    }

    bulb.close().await;
    Ok(())
}

async fn run_cli_sync(
    id: String,
    key: String,
    ip: String,
    palette_name: String,
    audio_dev: Option<String>,
    version: String,
) -> anyhow::Result<()> {
    let palette = match palette_name.to_lowercase().as_str() {
        "synthwave" => ColorPalette::Synthwave,
        "club" | "clubrave" => ColorPalette::ClubRave,
        "vaporwave" => ColorPalette::Vaporwave,
        "ambient" | "ambientchill" => ColorPalette::AmbientChill,
        _ => ColorPalette::Cyberpunk,
    };

    println!("\n[iot-bulb-sync] Starting Beat-Sync");
    println!("Target Bulb: {} ({}) v{}", id, ip, version);
    println!("Palette    : {:?}", palette);
    println!("Audio Dev  : {}\n", audio_dev.as_deref().unwrap_or("Default System Input"));

    let fleet = BulbFleet::new(4);
    let bulb = fleet
        .add_bulb(BulbConfig {
            id: id.clone(),
            name: Some("Target Bulb".to_string()),
            local_key: key,
            address: ip,
            port: Some(6668),
            version: Some(version),
            bulb_type: Some(BulbType::TypeB),
            color_format: Some(ColorFormat::Hex12),
            mac: None,
        })
        .await?;

    println!("Connecting to smart bulb...");
    bulb.wait_connected(Duration::from_secs(5)).await?;
    println!("Connected to bulb successfully!");
    let _ = bulb.enter_music_mode().await;

    let sample_rate = 44100;
    let window_size = 512;
    let mut audio = AudioStream::new(audio_dev.as_deref(), sample_rate, 8192)?;
    let mut detector = BeatDetector::new(window_size, sample_rate as f32, DspConfig::default());
    let mut choreographer = Choreographer::new(14, palette);

    println!("Listening for beats... (Press Ctrl+C to stop)\n");

    loop {
        tokio::time::sleep(Duration::from_millis(15)).await;
        let samples = audio.read_samples(window_size);
        if samples.len() < window_size {
            continue;
        }

        if let Some(event) = detector.process_frame(&samples) {
            if let Some(cmd) = choreographer.handle_event(&event) {
                let (r, g, b) = cmd.color.to_rgb();
                let pulse_tag = if cmd.is_drop {
                    "[DROP]"
                } else if cmd.is_kick {
                    "[KICK]"
                } else if cmd.is_snare {
                    "[SNARE]"
                } else {
                    "[PULSE]"
                };

                print!(
                    "\r\x1b[K{pulse_tag} Hue: {:>5.1}° | Brightness: {:>4} | Inst: {:>3.0}% | Vocal: {:>3.0}% | RGB: ({:>3}, {:>3}, {:>3})",
                    cmd.color.h,
                    cmd.brightness,
                    event.instrument_intensity * 100.0,
                    event.vocal_intensity * 100.0,
                    r,
                    g,
                    b
                );
                io::stdout().flush()?;

                fleet
                    .broadcast_music_frame(cmd.color, cmd.brightness, cmd.temp, cmd.is_gradual)
                    .await;
            }
        }
    }
}

async fn run_cli_imax(
    fps: u32,
    display_arg: Option<u32>,
    smoothing: f32,
    saturation_boost: f32,
    min_brightness: u32,
    max_brightness: u32,
    color_mode_str: String,
    cli_id: Option<String>,
    cli_key: Option<String>,
    cli_ip: Option<String>,
    version: String,
) -> anyhow::Result<()> {
    println!("\n=======================================================");
    println!("   IOT-Bulb Sync — IMAX Prominent-Color Screen Sync    ");
    println!("=======================================================");

    // Detect connected displays and interactively ask the user if multiple displays are found
    let displays = beat_sync::imax::macos_capture::list_displays();
    let selected_display: u32 = match display_arg {
        Some(d) => d,
        None => {
            if displays.len() > 1 {
                println!("\nMultiple displays detected ({}):", displays.len());
                for d in &displays {
                    println!("  [{}] {}", d.index, d.name);
                }
                print!(
                    "\nSelect display to sync [0..{}] (Press Enter for 0): ",
                    displays.len() - 1
                );
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                input.trim().parse::<u32>().unwrap_or(0)
            } else {
                if let Some(first) = displays.first() {
                    println!("Detected single display: {}", first.name);
                }
                0
            }
        }
    };

    let color_mode = if color_mode_str.eq_ignore_ascii_case("ambient") {
        ImaxColorMode::AmbientAverage
    } else {
        ImaxColorMode::Prominent
    };

    println!(
        "Config: {} FPS | Display #{} | Mode: {:?} | Reaction: {:.2} | Saturation: {:.2}x",
        fps, selected_display, color_mode, smoothing, saturation_boost
    );
    println!("Brightness Range: {} to {} (Zero-Black Filter Active)", min_brightness, max_brightness);

    let fleet = BulbFleet::new(8);
    let mut bulbs_count = 0;

    if let (Some(id), Some(key), Some(ip)) = (cli_id, cli_key, cli_ip) {
        println!("Connecting single target bulb {} ({})...", id, ip);
        let _ = fleet
            .add_bulb(BulbConfig {
                id,
                name: Some("IMAX Bulb".to_string()),
                local_key: key,
                address: ip,
                port: Some(6668),
                version: Some(version),
                bulb_type: Some(BulbType::TypeB),
                color_format: Some(ColorFormat::Hex12),
                mac: None,
            })
            .await?;
        bulbs_count += 1;
    } else {
        let config_path = resolve_fleet_config_path();
        println!("Loading fleet from {}...", config_path.display());
        let saved = load_devices_json(&config_path);
        for rec in saved {
            println!(
                "  -> Adding bulb: {} ({})",
                rec.name.as_deref().unwrap_or("Smart Bulb"),
                rec.ip
            );
            let _ = fleet
                .add_bulb(BulbConfig {
                    id: rec.id,
                    name: rec.name,
                    local_key: rec.key,
                    address: rec.ip,
                    port: Some(6668),
                    version: Some(rec.version),
                    bulb_type: rec.bulb_type,
                    color_format: Some(ColorFormat::Hex12),
                    mac: rec.mac,
                })
                .await;
            bulbs_count += 1;
        }
    }

    if bulbs_count == 0 {
        eprintln!("Error: No bulbs found in devices.json or CLI arguments.");
        return Ok(());
    }

    println!("[*] Waiting 2 seconds for LAN connections...");
    tokio::time::sleep(Duration::from_secs(2)).await;
    fleet.broadcast_enter_music_mode().await;
    println!("[*] Fleet ready in DP 27 music mode! Compressing 64x36 frames & extracting prominent colors...");

    let cfg = ImaxConfig {
        target_fps: fps,
        display_index: selected_display,
        smoothing,
        saturation_boost,
        min_brightness,
        max_brightness,
        color_mode,
    };

    let mut engine = ImaxEngine::new(cfg);
    let runtime_path = Path::new(".imax_runtime.json");
    let settings_path = Path::new("settings.json");

    loop {
        let loop_start = std::time::Instant::now();

        // Check if user modified IMAX settings from Web UI while CLI is running
        if let Ok(s_str) = std::fs::read_to_string(settings_path) {
            if let Ok(s) = serde_json::from_str::<SyncSettings>(&s_str) {
                engine.cfg.smoothing = s.imax_smoothing;
                engine.cfg.saturation_boost = s.imax_saturation_boost;
                engine.cfg.color_mode = s.imax_color_mode;
                engine.cfg.max_brightness = (s.master_brightness * 1000.0).round().clamp(25.0, 1000.0) as u32;
                engine.cfg.min_brightness = match s.ambience_mode {
                    BulbAmbienceMode::DarkMode => 20,
                    BulbAmbienceMode::LightMode => 450,
                };
            }
        }

        let current_display = engine.cfg.display_index;
        let frame_interval =
            Duration::from_millis((1000 / engine.cfg.target_fps.max(1) as u64).max(10));

        if let Some((color, brightness, is_cut)) =
            beat_sync::imax::macos_capture::capture_screen_frame(current_display, &mut engine)
        {
            let (r, g, b) = color.to_rgb();
            let tag = if is_cut { "[IMAX CUT ]" } else { "[IMAX SYNC]" };
            print!(
                "\r\x1b[K{tag} Display #{} | RGB: ({:>3}, {:>3}, {:>3}) | Brightness: {:>4} | Hue: {:>5.1}°",
                current_display, r, g, b, brightness, color.h
            );
            let _ = io::stdout().flush();

            // Broadcast with instant jump (!is_cut -> gradient=false on cuts for minimal transition time!)
            fleet
                .broadcast_music_frame(color, brightness, 500, !is_cut)
                .await;

            // Publish live CLI state for Web UI detection
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let rt = ImaxCliRuntimeState {
                active: true,
                pid: std::process::id(),
                timestamp_ms: now_ms,
                display: current_display,
                fps: engine.cfg.target_fps,
                smoothing: engine.cfg.smoothing,
                saturation_boost: engine.cfg.saturation_boost,
                color_mode: engine.cfg.color_mode,
                active_rgb: (r, g, b),
                active_brightness: brightness,
            };
            if let Ok(json) = serde_json::to_string(&rt) {
                let _ = std::fs::write(runtime_path, json);
            }
        }

        let elapsed = loop_start.elapsed();
        if elapsed < frame_interval {
            tokio::time::sleep(frame_interval - elapsed).await;
        }
    }
}

/// Checks if an external CLI `iot-bulb-sync imax` process is actively writing frames (< 1500ms old).
fn check_external_imax_cli() -> Option<ImaxCliRuntimeState> {
    let content = std::fs::read_to_string(".imax_runtime.json").ok()?;
    let st: ImaxCliRuntimeState = serde_json::from_str(&content).ok()?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    if st.active && now_ms.saturating_sub(st.timestamp_ms) < 1800 {
        Some(st)
    } else {
        None
    }
}

fn iso_utc_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let mut y = 1970;
    let mut rem_days = days;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
        let d_in_y = if leap { 366 } else { 365 };
        if rem_days < d_in_y { break; }
        rem_days -= d_in_y;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mon = 1;
    for &md in &month_days {
        if rem_days < md { break; }
        rem_days -= md;
        mon += 1;
    }
    let day = rem_days + 1;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mon, day, h, m, s)
}

async fn setup_app_state(port: u16) -> anyhow::Result<(AppState, tokio::net::TcpListener, Router)> {
    let (ws_tx, _) = broadcast::channel(128);
    let devices_path = resolve_fleet_config_path();
    let settings_path = std::path::PathBuf::from("settings.json");

    let saved_settings = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        SyncSettings::default()
    };

    let state = AppState {
        fleet: BulbFleet::new(16),
        sync_running: Arc::new(AtomicBool::new(false)),
        settings: Arc::new(RwLock::new(saved_settings)),
        ws_tx,
        devices_path: devices_path.clone(),
        settings_path,
    };

    // Auto-hydrate fleet from bulbs.json (or devices.json fallback)
    let saved_devices = load_devices_json(&devices_path);
    if !saved_devices.is_empty() {
        log::info!(
            "Auto-hydrating fleet with {} bulb(s) from {}...",
            saved_devices.len(),
            devices_path.display()
        );
        for rec in saved_devices {
            let id = rec.id.clone();
            let sync_excluded = rec.sync_excluded;
            let bulb_cfg = BulbConfig {
                id: rec.id,
                name: rec.name,
                local_key: rec.key,
                address: rec.ip,
                port: Some(6668),
                version: Some(rec.version),
                bulb_type: rec.bulb_type,
                color_format: Some(ColorFormat::Hex12),
                mac: rec.mac,
            };
            if let Err(e) = state.fleet.add_bulb(bulb_cfg).await {
                log::warn!("Auto-hydrate bulb connect issue: {e}");
            }
            if sync_excluded {
                state.fleet.set_sync_excluded(&id, true).await;
            }
        }
    }

    // Spawn unified background worker (handles Beat-Sync, in-process IMAX mode, and external IMAX CLI telemetry)
    tokio::spawn(beat_sync_background_worker(state.clone()));

    let app = build_router(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    log::info!("IOT-Bulb Sync server running on http://localhost:{}", port);
    log::info!("Web Dashboard available at http://localhost:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    Ok((state, listener, app))
}

async fn run_server(port: u16) -> anyhow::Result<()> {
    let (_state, listener, app) = setup_app_state(port).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_tui_mode(port: u16) -> anyhow::Result<()> {
    let (state, listener, app) = setup_app_state(port).await?;
    state.sync_running.store(true, Ordering::Relaxed);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tui::run_tui(state).await?;
    Ok(())
}

async fn save_fleet_config(
    State(state): State<AppState>,
    body: Option<Json<Vec<SavedDeviceRecord>>>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let bulbs_path = std::path::PathBuf::from("bulbs.json");
    let mut records = if let Some(Json(custom_records)) = body {
        if !custom_records.is_empty() {
            custom_records
        } else {
            load_devices_json(&state.devices_path)
        }
    } else {
        load_devices_json(&state.devices_path)
    };

    let fleet_bulbs = state.fleet.list_bulbs().await;
    for b in &fleet_bulbs {
        if let Some(rec) = records.iter_mut().find(|r| r.id == b.id) {
            rec.sync_excluded = b.sync_excluded;
            if b.name.is_some() {
                rec.name = b.name.clone();
            }
            rec.ip = b.address.clone();
            if b.mac.is_some() {
                rec.mac = b.mac.clone();
            }
        } else {
            records.push(SavedDeviceRecord {
                name: b.name.clone(),
                id: b.id.clone(),
                key: "".to_string(),
                ip: b.address.clone(),
                version: "3.5".to_string(),
                bulb_type: Some(b.bulb_type),
                mac: b.mac.clone(),
                sync_excluded: b.sync_excluded,
            });
        }
    }

    save_devices_json(&bulbs_path, &records).map_err(|e| {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    if state.devices_path != bulbs_path {
        let _ = save_devices_json(&state.devices_path, &records);
    }

    let timestamp = iso_utc_timestamp();

    Ok(Json(serde_json::json!({
        "saved": true,
        "path": "bulbs.json",
        "count": records.len(),
        "timestamp": timestamp
    })))
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard_html))
        .route("/favicon.svg", get(favicon_svg))
        .route("/api/audio/devices", get(get_audio_devices))
        .route("/api/imax/displays", get(get_imax_displays))
        .route("/api/bulbs", get(list_bulbs).post(add_bulb))
        .route("/api/bulbs/save-config", post(save_fleet_config))
        .route("/api/bulbs/:id", patch(rename_bulb).delete(remove_bulb))
        .route("/api/bulbs/:id/power", post(set_bulb_power))
        .route("/api/bulbs/:id/sync", post(set_bulb_sync))
        .route("/api/bulbs/:id/color", post(set_bulb_color))
        .route("/api/bulbs/:id/white", post(set_bulb_white))
        .route("/api/bulbs/broadcast/power", post(broadcast_power))
        .route("/api/bulbs/broadcast/color", post(broadcast_color))
        .route("/api/bulbs/broadcast/white", post(broadcast_white))
        .route("/api/sync/start", post(start_sync))
        .route("/api/sync/stop", post(stop_sync))
        .route("/api/sync/status", get(get_sync_status))
        .route(
            "/api/sync/settings",
            get(get_sync_settings).post(update_sync_settings),
        )
        .route("/api/sync/palette", post(set_sync_palette))
        .route("/api/devices/scan", get(api_scan_devices))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn beat_sync_background_worker(state: AppState) {
    let sample_rate = 44100;
    let window_size = 512;
    let mut audio_opt: Option<AudioStream> = None;
    let mut sample_accumulator: Vec<f32> = Vec::with_capacity(window_size * 4);
    let mut detector = BeatDetector::new(window_size, sample_rate as f32, DspConfig::default());
    let mut choreographer = Choreographer::new(15, ColorPalette::Cyberpunk);
    let mut imax_engine = ImaxEngine::new(ImaxConfig::default());

    let mut current_device_name: Option<String> = None;
    let mut was_running = false;

    loop {
        // 1. Check if an external CLI `iot-bulb-sync imax` process is actively running
        if let Some(ext_imax) = check_external_imax_cli() {
            let color = BulbColor::from_rgb(
                ext_imax.active_rgb.0,
                ext_imax.active_rgb.1,
                ext_imax.active_rgb.2,
            );
            let telemetry = WsTelemetry {
                engine_mode: ActiveEngineMode::Imax,
                imax_cli_active: true,
                is_kick: false,
                is_snare: false,
                is_vocal_onset: false,
                has_instruments: false,
                instrument_score: 0.0,
                active_gradual: false,
                current_db: -60.0,
                gate_open: true,
                bass_intensity: 0.0,
                snare_intensity: 0.0,
                instrument_intensity: 0.0,
                vocal_intensity: 0.0,
                overall_energy: (ext_imax.active_brightness as f32 / 1000.0).clamp(0.0, 1.0),
                active_hue: color.h,
                active_brightness: ext_imax.active_brightness,
                active_rgb: ext_imax.active_rgb,
                agc_gain: 1.0,
                effective_color_frequency: 0.70,
                effective_color_interval_ms: 160,
            };
            let _ = state.ws_tx.send(telemetry);
            tokio::time::sleep(Duration::from_millis(60)).await;
            continue;
        }

        let is_running = state.sync_running.load(Ordering::Relaxed);
        if !is_running {
            if was_running {
                was_running = false;
                audio_opt = None;
                sample_accumulator.clear();
                state.fleet.broadcast_exit_music_mode().await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        if !was_running {
            was_running = true;
            state.fleet.broadcast_enter_music_mode().await;
        }

        let settings = state.settings.read().await.clone();

        // 2. If running in IMAX Mode from Web Dashboard
        if settings.engine_mode == ActiveEngineMode::Imax {
            audio_opt = None;
            sample_accumulator.clear();
            imax_engine.cfg.display_index = settings.imax_display;
            imax_engine.cfg.target_fps = settings.imax_fps.clamp(8, 30);
            imax_engine.cfg.smoothing = settings.imax_smoothing;
            imax_engine.cfg.saturation_boost = settings.imax_saturation_boost;
            imax_engine.cfg.color_mode = settings.imax_color_mode;
            imax_engine.cfg.max_brightness = (settings.master_brightness * 1000.0)
                .round()
                .clamp(25.0, 1000.0) as u32;
            imax_engine.cfg.min_brightness = match settings.ambience_mode {
                BulbAmbienceMode::DarkMode => 20,
                BulbAmbienceMode::LightMode => 450,
            };

            let sleep_ms = (1000 / imax_engine.cfg.target_fps.max(1) as u64).max(15);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

            if let Some((color, brightness, is_cut)) =
                beat_sync::imax::macos_capture::capture_screen_frame(
                    settings.imax_display,
                    &mut imax_engine,
                )
            {
                let (r, g, b) = color.to_rgb();
                let telemetry = WsTelemetry {
                    engine_mode: ActiveEngineMode::Imax,
                    imax_cli_active: false,
                    is_kick: is_cut,
                    is_snare: false,
                    is_vocal_onset: false,
                    has_instruments: true,
                    instrument_score: 1.0,
                    active_gradual: !is_cut,
                    current_db: -18.0,
                    gate_open: true,
                    bass_intensity: if is_cut { 0.85 } else { 0.25 },
                    snare_intensity: 0.0,
                    instrument_intensity: (brightness as f32 / 1000.0).clamp(0.0, 1.0),
                    vocal_intensity: 0.0,
                    overall_energy: (brightness as f32 / 1000.0).clamp(0.0, 1.0),
                    active_hue: color.h,
                    active_brightness: brightness,
                    active_rgb: (r, g, b),
                    agc_gain: 1.0,
                    effective_color_frequency: 0.70,
                    effective_color_interval_ms: 160,
                };
                let _ = state.ws_tx.send(telemetry);
                state
                    .fleet
                    .broadcast_music_frame(color, brightness, 500, !is_cut)
                    .await;
            }
            continue;
        }

        // 3. Audio Beat-Sync Mode
        detector.update_config(DspConfig {
            kick_threshold_multiplier: 1.42,
            snare_threshold_multiplier: 1.50,
            min_kick_energy: 3.2,
            min_snare_energy: 6.5,
            decay_alpha: 0.06,
            auto_normalize: settings.auto_normalize,
            noise_gate_db: settings.noise_gate_db,
            sensitivity: settings.sensitivity,
            focus_mode: settings.focus_mode,
        });

        choreographer.set_palette(settings.palette);
        choreographer.set_custom_palette(settings.custom_palette);
        choreographer.set_ambience_mode(settings.ambience_mode);
        choreographer.set_master_brightness(settings.master_brightness);
        choreographer.set_transition_mode(settings.transition_mode);
        choreographer.set_invert_auto_gradual(settings.invert_auto_gradual);
        choreographer.set_transition_speed(settings.transition_speed);
        choreographer.set_color_frequency(settings.color_frequency);

        let dev_name = settings.audio_device.clone();
        if audio_opt.is_none() || dev_name != current_device_name {
            current_device_name = dev_name.clone();
            sample_accumulator.clear();
            match AudioStream::new(dev_name.as_deref(), sample_rate, 8192) {
                Ok(stream) => {
                    log::info!("Audio capture stream started for beat sync");
                    audio_opt = Some(stream);
                }
                Err(e) => {
                    log::error!("Failed to open audio stream: {e}");
                    state.sync_running.store(false, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(15)).await;

        if let Some(ref mut audio) = audio_opt {
            let chunk = audio.read_samples_focused(window_size, settings.focus_mode);
            sample_accumulator.extend(chunk);

            // Never let latency drift beyond 3 windows (~35ms max buffer)
            if sample_accumulator.len() > window_size * 3 {
                let excess = sample_accumulator.len() - window_size * 2;
                sample_accumulator.drain(..excess);
            }

            let hop_size = window_size / 2;
            while sample_accumulator.len() >= window_size {
                let frame = &sample_accumulator[..window_size];
                if let Some(event) = detector.process_frame(frame) {
                    let cmd_opt = choreographer.handle_event(&event);
                    if let Some(ref cmd) = cmd_opt {
                        if event.gate_open {
                            state
                                .fleet
                                .broadcast_music_frame(
                                    cmd.color,
                                    cmd.brightness,
                                    cmd.temp,
                                    cmd.is_gradual,
                                )
                                .await;
                        }
                    }

                    let (active_color, active_brightness, is_gradual, is_kick, is_snare) = match cmd_opt {
                        Some(ref cmd) => (cmd.color, cmd.brightness, cmd.is_gradual, cmd.is_kick, cmd.is_snare),
                        None => {
                            let live_bri = if event.gate_open {
                                (event.overall_energy * settings.master_brightness * 1000.0)
                                    .round()
                                    .clamp(20.0, 1000.0) as u32
                            } else {
                                match settings.ambience_mode {
                                    BulbAmbienceMode::DarkMode => 0,
                                    BulbAmbienceMode::LightMode => (settings.master_brightness * 450.0).round() as u32,
                                }
                            };
                            (
                                choreographer.current_color(),
                                live_bri,
                                choreographer.smooth_transitions(),
                                event.is_kick,
                                event.is_snare,
                            )
                        }
                    };
                    let (r, g, b) = active_color.to_rgb();

                    let telemetry = WsTelemetry {
                        engine_mode: ActiveEngineMode::BeatSync,
                        imax_cli_active: false,
                        is_kick,
                        is_snare,
                        is_vocal_onset: event.is_vocal_onset,
                        has_instruments: event.instrument_presence.has_instruments,
                        instrument_score: event.instrument_presence.instrument_score,
                        active_gradual: is_gradual,
                        current_db: event.current_db,
                        gate_open: event.gate_open,
                        bass_intensity: event.bass_intensity,
                        snare_intensity: event.snare_intensity,
                        instrument_intensity: event.instrument_intensity,
                        vocal_intensity: event.vocal_intensity,
                        overall_energy: event.overall_energy,
                        active_hue: active_color.h,
                        active_brightness: active_brightness,
                        active_rgb: (r, g, b),
                        agc_gain: detector.current_gain(),
                        effective_color_frequency: choreographer.effective_color_frequency(),
                        effective_color_interval_ms: choreographer.effective_color_interval_ms(),
                    };

                    let _ = state.ws_tx.send(telemetry);
                }
                sample_accumulator.drain(..hop_size);
            }
        }
    }
}

// REST Handlers

async fn get_audio_devices() -> Json<Vec<AudioDeviceInfo>> {
    Json(AudioStream::list_input_devices())
}

async fn get_imax_displays() -> Json<Vec<DisplayInfo>> {
    Json(beat_sync::imax::macos_capture::list_displays())
}

async fn list_bulbs(State(state): State<AppState>) -> Json<Vec<FleetBulbInfo>> {
    Json(state.fleet.list_bulbs().await)
}

async fn add_bulb(
    State(state): State<AppState>,
    Json(cfg): Json<BulbConfig>,
) -> Result<Json<FleetBulbInfo>, (axum::http::StatusCode, String)> {
    let id = cfg.id.clone();
    let addr = cfg.address.clone();
    let btype = cfg.bulb_type.unwrap_or(BulbType::TypeB);
    let key = cfg.local_key.clone();
    let ver = cfg.version.clone().unwrap_or_else(|| "3.5".to_string());
    let mac = cfg.mac.clone();

    let saved = load_devices_json(&state.devices_path);
    let name = cfg
        .name
        .clone()
        .or_else(|| Some(infer_friendly_bulb_name(&id, &addr, mac.as_deref(), &saved)));

    let mut bulb_cfg_with_name = cfg.clone();
    bulb_cfg_with_name.name = name.clone();

    match state.fleet.add_bulb(bulb_cfg_with_name).await {
        Ok(bulb) => {
            let mut records = load_devices_json(&state.devices_path);
            if let Some(existing) = records.iter_mut().find(|r| r.id == id) {
                existing.key = key;
                existing.ip = addr.clone();
                existing.version = ver;
                existing.bulb_type = Some(btype);
                if name.is_some() {
                    existing.name = name.clone();
                }
                if mac.is_some() {
                    existing.mac = mac.clone();
                }
            } else {
                records.push(SavedDeviceRecord {
                    name: name.clone(),
                    id: id.clone(),
                    key,
                    ip: addr.clone(),
                    version: ver,
                    bulb_type: Some(btype),
                    mac: mac.clone(),
                    sync_excluded: false,
                });
            }
            let _ = save_devices_json(&state.devices_path, &records);

            Ok(Json(FleetBulbInfo {
                id,
                name,
                address: addr,
                is_connected: bulb.is_connected(),
                power: bulb.is_power_on(),
                bulb_type: btype,
                mac,
                sync_excluded: false,
            }))
        }
        Err(e) => Err((axum::http::StatusCode::BAD_REQUEST, e.to_string())),
    }
}

#[derive(Deserialize)]
struct RenameBulbPayload {
    name: String,
}

async fn rename_bulb(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RenameBulbPayload>,
) -> Json<serde_json::Value> {
    let success = state.fleet.rename_bulb(&id, &body.name).await;
    if success {
        let mut records = load_devices_json(&state.devices_path);
        if let Some(rec) = records.iter_mut().find(|r| r.id == id) {
            rec.name = Some(body.name.clone());
            let _ = save_devices_json(&state.devices_path, &records);
        }
    }
    Json(serde_json::json!({ "success": success, "id": id, "name": body.name }))
}

async fn remove_bulb(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Json<serde_json::Value> {
    let removed = state.fleet.remove_bulb(&id).await.is_some();
    if removed {
        let mut records = load_devices_json(&state.devices_path);
        records.retain(|r| r.id != id);
        let _ = save_devices_json(&state.devices_path, &records);
    }
    Json(serde_json::json!({ "success": removed, "id": id }))
}

#[derive(Deserialize)]
struct PowerPayload {
    power: bool,
}

async fn set_bulb_power(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<PowerPayload>,
) -> Json<serde_json::Value> {
    if let Some(bulb) = state.fleet.get_bulb(&id).await {
        let res = if body.power {
            bulb.turn_on().await
        } else {
            bulb.turn_off().await
        };
        match res {
            Ok(_) => Json(serde_json::json!({ "success": true, "id": id, "power": body.power })),
            Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
        }
    } else {
        Json(serde_json::json!({ "success": false, "error": "bulb not found" }))
    }
}

#[derive(Deserialize)]
struct SyncExclusionPayload {
    excluded: Option<bool>,
}

async fn set_bulb_sync(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    payload: Option<Json<SyncExclusionPayload>>,
) -> Json<serde_json::Value> {
    let excluded = match payload {
        Some(Json(p)) => match p.excluded {
            Some(val) => {
                state.fleet.set_sync_excluded(&id, val).await;
                val
            }
            None => state.fleet.toggle_sync_excluded(&id).await,
        },
        None => state.fleet.toggle_sync_excluded(&id).await,
    };

    let mut records = load_devices_json(&state.devices_path);
    if let Some(rec) = records.iter_mut().find(|r| r.id == id) {
        rec.sync_excluded = excluded;
        let _ = save_devices_json(&state.devices_path, &records);
    }
    let bulbs_path = std::path::Path::new("bulbs.json");
    if state.devices_path != bulbs_path && bulbs_path.exists() {
        let mut b_records = load_devices_json(bulbs_path);
        if let Some(rec) = b_records.iter_mut().find(|r| r.id == id) {
            rec.sync_excluded = excluded;
            let _ = save_devices_json(bulbs_path, &b_records);
        }
    }

    Json(serde_json::json!({ "success": true, "id": id, "sync_excluded": excluded }))
}

#[derive(Deserialize)]
struct ColorPayload {
    h: f32,
    s: f32,
    v: f32,
}

async fn set_bulb_color(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ColorPayload>,
) -> Json<serde_json::Value> {
    if let Some(bulb) = state.fleet.get_bulb(&id).await {
        // When saturation is near zero (< 0.06), automatically switch bulb to crisp White mode!
        if body.s < 0.06 {
            let brightness = (body.v.clamp(0.01, 1.0) * 1000.0).round() as u32;
            match bulb.set_white(brightness, 500).await {
                Ok(_) => Json(serde_json::json!({ "success": true, "id": id, "mode": "white" })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
            }
        } else {
            let color = BulbColor::from_hsv(body.h, body.s, body.v);
            match bulb.set_color(color).await {
                Ok(_) => Json(serde_json::json!({ "success": true, "id": id, "mode": "colour" })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
            }
        }
    } else {
        Json(serde_json::json!({ "success": false, "error": "bulb not found" }))
    }
}

#[derive(Deserialize)]
struct WhitePayload {
    brightness: u32,
    temp: u32,
}

async fn set_bulb_white(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<WhitePayload>,
) -> Json<serde_json::Value> {
    if let Some(bulb) = state.fleet.get_bulb(&id).await {
        match bulb.set_white(body.brightness, body.temp).await {
            Ok(_) => Json(
                serde_json::json!({ "success": true, "id": id, "brightness": body.brightness, "temp": body.temp }),
            ),
            Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
        }
    } else {
        Json(serde_json::json!({ "success": false, "error": "bulb not found" }))
    }
}

async fn broadcast_power(
    State(state): State<AppState>,
    Json(body): Json<PowerPayload>,
) -> Json<serde_json::Value> {
    if body.power {
        state.fleet.broadcast_turn_on().await;
    } else {
        state.fleet.broadcast_turn_off().await;
    }
    Json(serde_json::json!({ "success": true, "broadcast": true, "power": body.power }))
}

async fn broadcast_color(
    State(state): State<AppState>,
    Json(body): Json<ColorPayload>,
) -> Json<serde_json::Value> {
    if body.s < 0.06 {
        let brightness = (body.v.clamp(0.01, 1.0) * 1000.0).round() as u32;
        state.fleet.broadcast_white(brightness, 500).await;
    } else {
        let color = BulbColor::from_hsv(body.h, body.s, body.v);
        state.fleet.broadcast_color(color).await;
    }
    Json(serde_json::json!({ "success": true, "broadcast": true }))
}

async fn broadcast_white(
    State(state): State<AppState>,
    Json(body): Json<WhitePayload>,
) -> Json<serde_json::Value> {
    state.fleet.broadcast_white(body.brightness, body.temp).await;
    Json(
        serde_json::json!({ "success": true, "broadcast": true, "brightness": body.brightness, "temp": body.temp }),
    )
}

#[derive(Deserialize)]
struct StartSyncPayload {
    engine_mode: Option<ActiveEngineMode>,
    device: Option<String>,
    palette: Option<ColorPalette>,
    ambience_mode: Option<BulbAmbienceMode>,
    master_brightness: Option<f32>,
    transition_mode: Option<TransitionMode>,
    smooth_transitions: Option<bool>,
    auto_normalize: Option<bool>,
    noise_gate_db: Option<f32>,
    sensitivity: Option<f32>,
    focus_mode: Option<AudioFocusMode>,
    imax_display: Option<u32>,
    imax_fps: Option<u32>,
    imax_smoothing: Option<f32>,
    imax_saturation_boost: Option<f32>,
    imax_color_mode: Option<ImaxColorMode>,
}

async fn start_sync(
    State(state): State<AppState>,
    Json(body): Json<StartSyncPayload>,
) -> Json<serde_json::Value> {
    {
        let mut s = state.settings.write().await;
        if let Some(em) = body.engine_mode {
            s.engine_mode = em;
        }
        if let Some(dev) = body.device {
            s.audio_device = Some(dev);
        }
        if let Some(pal) = body.palette {
            s.palette = pal;
        }
        if let Some(amb) = body.ambience_mode {
            s.ambience_mode = amb;
        }
        if let Some(mb) = body.master_brightness {
            s.master_brightness = mb.clamp(0.01, 1.0);
        }
        if let Some(tm) = body.transition_mode {
            s.transition_mode = tm;
        } else if let Some(st) = body.smooth_transitions {
            s.transition_mode = if st {
                TransitionMode::Gradual
            } else {
                TransitionMode::Off
            };
        }
        if let Some(an) = body.auto_normalize {
            s.auto_normalize = an;
        }
        if let Some(ng) = body.noise_gate_db {
            s.noise_gate_db = ng.clamp(-65.0, -10.0);
        }
        if let Some(sens) = body.sensitivity {
            s.sensitivity = sens.clamp(0.2, 3.0);
        }
        if let Some(focus) = body.focus_mode {
            s.focus_mode = focus;
        }
        if let Some(disp) = body.imax_display {
            s.imax_display = disp;
        }
        if let Some(fps) = body.imax_fps {
            s.imax_fps = fps.clamp(8, 30);
        }
        if let Some(sm) = body.imax_smoothing {
            s.imax_smoothing = sm.clamp(0.10, 1.0);
        }
        if let Some(sb) = body.imax_saturation_boost {
            s.imax_saturation_boost = sb.clamp(1.0, 2.0);
        }
        if let Some(cm) = body.imax_color_mode {
            s.imax_color_mode = cm;
        }
    }
    state.sync_running.store(true, Ordering::Relaxed);
    Json(serde_json::json!({ "success": true, "sync_running": true }))
}

async fn stop_sync(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.sync_running.store(false, Ordering::Relaxed);
    Json(serde_json::json!({ "success": true, "sync_running": false }))
}

#[derive(Serialize)]
struct SyncStatus {
    running: bool,
    imax_cli_active: bool,
    settings: SyncSettings,
}

async fn get_sync_status(State(state): State<AppState>) -> Json<SyncStatus> {
    let ext_imax = check_external_imax_cli();
    let mut settings = state.settings.read().await.clone();
    if let Some(ref rt) = ext_imax {
        settings.engine_mode = ActiveEngineMode::Imax;
        settings.imax_display = rt.display;
        settings.imax_fps = rt.fps;
        settings.imax_smoothing = rt.smoothing;
        settings.imax_saturation_boost = rt.saturation_boost;
        settings.imax_color_mode = rt.color_mode;
    }
    Json(SyncStatus {
        running: state.sync_running.load(Ordering::Relaxed) || ext_imax.is_some(),
        imax_cli_active: ext_imax.is_some(),
        settings,
    })
}

#[derive(Deserialize)]
struct UpdateSyncSettingsPayload {
    engine_mode: Option<ActiveEngineMode>,
    palette: Option<ColorPalette>,
    custom_palette: Option<Vec<BulbColor>>,
    ambience_mode: Option<BulbAmbienceMode>,
    master_brightness: Option<f32>,
    transition_mode: Option<TransitionMode>,
    smooth_transitions: Option<bool>,
    invert_auto_gradual: Option<bool>,
    transition_speed: Option<f32>,
    color_frequency: Option<f32>,
    auto_normalize: Option<bool>,
    noise_gate_db: Option<f32>,
    sensitivity: Option<f32>,
    focus_mode: Option<AudioFocusMode>,
    audio_device: Option<String>,
    imax_display: Option<u32>,
    imax_fps: Option<u32>,
    imax_smoothing: Option<f32>,
    imax_saturation_boost: Option<f32>,
    imax_color_mode: Option<ImaxColorMode>,
}

async fn get_sync_settings(State(state): State<AppState>) -> Json<SyncSettings> {
    Json(state.settings.read().await.clone())
}

async fn update_sync_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSyncSettingsPayload>,
) -> Json<SyncSettings> {
    let mut s = state.settings.write().await;
    if let Some(em) = body.engine_mode {
        s.engine_mode = em;
    }
    if let Some(pal) = body.palette {
        s.palette = pal;
    }
    if let Some(cp) = body.custom_palette {
        s.custom_palette = cp;
    }
    if let Some(amb) = body.ambience_mode {
        s.ambience_mode = amb;
    }
    if let Some(mb) = body.master_brightness {
        s.master_brightness = mb.clamp(0.01, 1.0);
    }
    if let Some(tm) = body.transition_mode {
        s.transition_mode = tm;
    } else if let Some(st) = body.smooth_transitions {
        s.transition_mode = if st {
            TransitionMode::Gradual
        } else {
            TransitionMode::Off
        };
    }
    if let Some(inv) = body.invert_auto_gradual {
        s.invert_auto_gradual = inv;
    }
    if let Some(spd) = body.transition_speed {
        s.transition_speed = spd.clamp(0.10, 1.00);
    }
    if let Some(freq) = body.color_frequency {
        s.color_frequency = freq.clamp(0.10, 1.00);
    }
    if let Some(an) = body.auto_normalize {
        s.auto_normalize = an;
    }
    if let Some(ng) = body.noise_gate_db {
        s.noise_gate_db = ng.clamp(-65.0, -10.0);
    }
    if let Some(sens) = body.sensitivity {
        s.sensitivity = sens.clamp(0.2, 3.0);
    }
    if let Some(fm) = body.focus_mode {
        s.focus_mode = fm;
    }
    if let Some(dev) = body.audio_device {
        s.audio_device = Some(dev);
    }
    if let Some(disp) = body.imax_display {
        s.imax_display = disp;
    }
    if let Some(fps) = body.imax_fps {
        s.imax_fps = fps.clamp(8, 30);
    }
    if let Some(sm) = body.imax_smoothing {
        s.imax_smoothing = sm.clamp(0.10, 1.0);
    }
    if let Some(sb) = body.imax_saturation_boost {
        s.imax_saturation_boost = sb.clamp(1.0, 2.0);
    }
    if let Some(cm) = body.imax_color_mode {
        s.imax_color_mode = cm;
    }

    let cloned = s.clone();
    drop(s);
    if let Ok(json) = serde_json::to_string_pretty(&cloned) {
        let _ = std::fs::write(&state.settings_path, json);
    }

    Json(cloned)
}

#[derive(Deserialize)]
struct PalettePayload {
    palette: ColorPalette,
}

async fn set_sync_palette(
    State(state): State<AppState>,
    Json(body): Json<PalettePayload>,
) -> Json<serde_json::Value> {
    state.settings.write().await.palette = body.palette;
    Json(serde_json::json!({ "success": true, "palette": body.palette }))
}

async fn api_scan_devices(State(state): State<AppState>) -> Json<Vec<serde_json::Value>> {
    let disco = match Discovery::builder().build() {
        Ok(d) => d,
        Err(_) => return Json(Vec::new()),
    };

    let devices = disco.scan(Duration::from_secs(3)).await;
    let saved = load_devices_json(&state.devices_path);
    let mut results = Vec::new();

    for info in devices {
        let ip_str = info.ip.to_string();
        let mac = lookup_arp_mac(&ip_str);
        let name = infer_friendly_bulb_name(&info.id, &ip_str, mac.as_deref(), &saved);
        let is_saved = saved.iter().any(|r| r.id == info.id || r.ip == ip_str);

        results.push(serde_json::json!({
            "id": info.id,
            "ip": ip_str,
            "version": info.version.map(|v| v.as_str()).unwrap_or("3.3"),
            "mac": mac,
            "name": name,
            "is_saved": is_saved
        }));
    }
    disco.close().await;
    Json(results)
}

// WebSocket handler

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.ws_tx.subscribe();

    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if let Ok(json_str) = serde_json::to_string(&msg) {
                if sender.send(AxumWsMsg::Text(json_str.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(_)) = receiver.next().await {}
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
}

// Interactive Web Dashboard HTML & Favicon
async fn dashboard_html() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}

async fn favicon_svg() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
        FAVICON_SVG,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::Service;

    fn test_app_state() -> AppState {
        let (ws_tx, _) = broadcast::channel(16);
        AppState {
            fleet: BulbFleet::new(4),
            sync_running: Arc::new(AtomicBool::new(false)),
            settings: Arc::new(RwLock::new(SyncSettings::default())),
            ws_tx,
            devices_path: std::path::PathBuf::from("devices_test.json"),
            settings_path: std::path::PathBuf::from("settings_test.json"),
        }
    }

    #[tokio::test]
    async fn test_dashboard_serves_html_with_gpu_shaders() {
        let state = test_app_state();
        let mut app = build_router(state);

        let response = app
            .call(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(html_str.contains("three.min.js"), "Dashboard must import Three.js");
        assert!(html_str.contains("threeCanvas"), "Dashboard must contain full-viewport canvas");
        assert!(html_str.contains("IOT-Bulb Sync"), "Dashboard must contain IOT-Bulb Sync brand");
        assert!(!html_str.contains("YouTube Ambient"), "YouTube Ambient must be removed");
        assert!(html_str.contains("uWaveGain"), "Vertex shader must include uWaveGain");
        assert!(html_str.contains("uMeshFrequency"), "Vertex shader must include uMeshFrequency");
        assert!(html_str.contains("uFlowVelocity"), "Vertex shader must include uFlowVelocity");
        assert!(html_str.contains("uSwirlTurbulence"), "Vertex shader must include uSwirlTurbulence");
        assert!(html_str.contains("uMasterLuminance"), "Shader must include uMasterLuminance");
        assert!(html_str.contains("uStemGeometryBias"), "Vertex shader must include uStemGeometryBias");
        assert!(html_str.contains("uColorTertiary"), "Fragment shader must include 3-stop uColorTertiary");
        assert!(html_str.contains("saveConfigJsonBtn"), "Dashboard must include Save bulbs.json button");
        assert!(html_str.contains("PALETTE_STOPS"), "Dashboard must define canonical PALETTE_STOPS");
    }

    #[tokio::test]
    async fn test_sync_settings_api() {
        let state = test_app_state();
        let mut app = build_router(state);

        let response = app
            .call(
                Request::builder()
                    .uri("/api/sync/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let payload = serde_json::json!({
            "master_brightness": 0.75,
            "ambience_mode": "LightMode",
            "auto_normalize": true,
            "transition_mode": "Gradual",
            "noise_gate_db": -42.0,
            "sensitivity": 1.25,
            "focus_mode": "VocalsOnly",
            "imax_color_mode": "Prominent"
        });

        let response = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/sync/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let updated: SyncSettings = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated.master_brightness, 0.75);
        assert_eq!(updated.ambience_mode, BulbAmbienceMode::LightMode);
        assert_eq!(updated.transition_mode, TransitionMode::Gradual);
        assert_eq!(updated.noise_gate_db, -42.0);
        assert_eq!(updated.sensitivity, 1.25);
        assert_eq!(updated.focus_mode, AudioFocusMode::VocalsOnly);
        assert_eq!(updated.imax_color_mode, ImaxColorMode::Prominent);

        let _ = std::fs::remove_file("settings_test.json");
    }

    #[tokio::test]
    async fn test_save_bulbs_config_api() {
        let state = test_app_state();
        let mut app = build_router(state);

        let payload = serde_json::json!([
            {
                "id": "test_bulb_01",
                "name": "Living Room Key Light",
                "key": "0123456789abcdef",
                "ip": "192.168.1.105",
                "version": "3.5"
            }
        ]);

        let response = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/bulbs/save-config")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["saved"], true);
        assert_eq!(json_val["path"], "bulbs.json");
        assert_eq!(json_val["count"], 1);
        assert!(json_val["timestamp"].is_string());

        let _ = std::fs::remove_file("bulbs.json");
        let _ = std::fs::remove_file("devices_test.json");
    }

    #[tokio::test]
    async fn test_bulb_sync_exclusion_api() {
        let state = test_app_state();
        let mut app = build_router(state);

        // Explicit set to excluded = true
        let payload = serde_json::json!({
            "excluded": true
        });

        let response = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/bulbs/test_bulb_01/sync")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["success"], true);
        assert_eq!(json_val["id"], "test_bulb_01");
        assert_eq!(json_val["sync_excluded"], true);

        // Toggle via empty body or None
        let response = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/bulbs/test_bulb_01/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["success"], true);
        assert_eq!(json_val["id"], "test_bulb_01");
        assert_eq!(json_val["sync_excluded"], false);

        let _ = std::fs::remove_file("devices_test.json");
    }
}

