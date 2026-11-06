//! IOT-Lights: Backend server and CLI for real-time music beat-sync Tuya smart lighting.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as AxumWsMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;

use beat_sync::{
    AudioDeviceInfo, AudioStream, BeatDetector, BulbAmbienceMode, BulbConfig, BulbFleet,
    Choreographer, ColorPalette, DspConfig, FleetBulbInfo,
};
use rustuya_tokio::{Discovery, Version};
use tuya_bulb::{BulbColor, BulbType, ColorFormat};

#[derive(Parser, Debug)]
#[command(name = "iot-lights", about = "Real-time music beat-sync lighting controller for Tuya smart bulbs")]
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

/// Dynamic synchronization and audio-reactive settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    pub palette: ColorPalette,
    pub ambience_mode: BulbAmbienceMode,
    pub master_brightness: f32,
    pub smooth_transitions: bool,
    pub auto_normalize: bool,
    pub audio_device: Option<String>,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            palette: ColorPalette::Cyberpunk,
            ambience_mode: BulbAmbienceMode::DarkMode,
            master_brightness: 1.0,
            smooth_transitions: true,
            auto_normalize: true,
            audio_device: None,
        }
    }
}

/// WebSocket broadcast packet to live web clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsTelemetry {
    pub is_kick: bool,
    pub is_snare: bool,
    pub bass_intensity: f32,
    pub snare_intensity: f32,
    pub overall_energy: f32,
    pub active_hue: f32,
    pub active_brightness: u32,
    pub active_rgb: (u8, u8, u8),
    pub agc_gain: f32,
}

/// Shared application state for axum.
#[derive(Clone)]
pub struct AppState {
    pub fleet: BulbFleet,
    pub sync_running: Arc<AtomicBool>,
    pub settings: Arc<RwLock<SyncSettings>>,
    pub ws_tx: broadcast::Sender<WsTelemetry>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Serve { port: 8080 }) {
        Commands::Serve { port } => run_server(port).await?,
        Commands::Scan { seconds } => run_scan(seconds).await?,
        Commands::Sync {
            id,
            key,
            ip,
            palette,
            device,
            version,
        } => run_cli_sync(id, key, ip, palette, device, version).await?,
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
    for info in &devices {
        let ver = info.version.map(|v| v.as_str()).unwrap_or("unknown");
        println!("{:<24} {:<18} {:<10}", info.id, info.ip, ver);
    }

    println!("{}\nScan complete. Found {} device(s).", "-".repeat(56), devices.len());
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

    println!("\n[beat-sync] Starting IOT-Lights Beat-Sync");
    println!("Target Bulb: {} ({}) v{}", id, ip, version);
    println!("Palette    : {:?}", palette);
    println!("Audio Dev  : {}\n", audio_dev.as_deref().unwrap_or("Default System Input"));

    let fleet = BulbFleet::new(4);
    let bulb = fleet
        .add_bulb(BulbConfig {
            id: id.clone(),
            local_key: key,
            address: ip,
            port: Some(6668),
            version: Some(version),
            bulb_type: Some(BulbType::TypeB),
            color_format: Some(ColorFormat::Hex12),
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
    let mut choreographer = Choreographer::new(12, palette);

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

                print!("\r\x1b[K{pulse_tag} Hue: {:>5.1}° | Brightness: {:>4} | RGB: ({:>3}, {:>3}, {:>3})",
                    cmd.color.h, cmd.brightness, r, g, b);
                std::io::Write::flush(&mut std::io::stdout())?;

                fleet.broadcast_music_frame(cmd.color, cmd.brightness, cmd.temp, false).await;
            }
        }
    }
}

async fn run_server(port: u16) -> anyhow::Result<()> {
    let (ws_tx, _) = broadcast::channel(128);

    let state = AppState {
        fleet: BulbFleet::new(16),
        sync_running: Arc::new(AtomicBool::new(false)),
        settings: Arc::new(RwLock::new(SyncSettings::default())),
        ws_tx,
    };

    // Spawn background beat-sync supervisor task
    tokio::spawn(beat_sync_background_worker(state.clone()));

    let app = build_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    log::info!("IOT-Lights server running on http://localhost:{}", port);
    log::info!("Web Dashboard available at http://localhost:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard_html))
        .route("/api/audio/devices", get(get_audio_devices))
        .route("/api/bulbs", get(list_bulbs).post(add_bulb))
        .route("/api/bulbs/:id", axum::routing::delete(remove_bulb))
        .route("/api/bulbs/:id/power", post(set_bulb_power))
        .route("/api/bulbs/:id/color", post(set_bulb_color))
        .route("/api/bulbs/:id/white", post(set_bulb_white))
        .route("/api/bulbs/broadcast/power", post(broadcast_power))
        .route("/api/bulbs/broadcast/color", post(broadcast_color))
        .route("/api/bulbs/broadcast/white", post(broadcast_white))
        .route("/api/sync/start", post(start_sync))
        .route("/api/sync/stop", post(stop_sync))
        .route("/api/sync/status", get(get_sync_status))
        .route("/api/sync/settings", get(get_sync_settings).post(update_sync_settings))
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
    let mut detector = BeatDetector::new(window_size, sample_rate as f32, DspConfig::default());
    let mut choreographer = Choreographer::new(14, ColorPalette::Cyberpunk);

    let mut current_device_name: Option<String> = None;
    let mut was_running = false;

    loop {
        let is_running = state.sync_running.load(Ordering::Relaxed);
        if !is_running {
            if was_running {
                was_running = false;
                audio_opt = None;
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
        detector.set_auto_normalize(settings.auto_normalize);
        choreographer.set_palette(settings.palette);
        choreographer.set_ambience_mode(settings.ambience_mode);
        choreographer.set_master_brightness(settings.master_brightness);
        choreographer.set_smooth_transitions(settings.smooth_transitions);

        let dev_name = settings.audio_device.clone();
        if audio_opt.is_none() || dev_name != current_device_name {
            current_device_name = dev_name.clone();
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
            let samples = audio.read_samples(window_size);
            if samples.len() >= window_size {
                if let Some(event) = detector.process_frame(&samples) {
                    if let Some(cmd) = choreographer.handle_event(&event) {
                        let (r, g, b) = cmd.color.to_rgb();

                        let telemetry = WsTelemetry {
                            is_kick: cmd.is_kick,
                            is_snare: cmd.is_snare,
                            bass_intensity: event.bass_intensity,
                            snare_intensity: event.snare_intensity,
                            overall_energy: event.overall_energy,
                            active_hue: cmd.color.h,
                            active_brightness: cmd.brightness,
                            active_rgb: (r, g, b),
                            agc_gain: detector.current_gain(),
                        };

                        let _ = state.ws_tx.send(telemetry);
                        state.fleet.broadcast_music_frame(cmd.color, cmd.brightness, cmd.temp, cmd.is_gradual).await;
                    }
                }
            }
        }
    }
}

// REST Handlers

async fn get_audio_devices() -> Json<Vec<AudioDeviceInfo>> {
    Json(AudioStream::list_input_devices())
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

    match state.fleet.add_bulb(cfg).await {
        Ok(bulb) => Ok(Json(FleetBulbInfo {
            id,
            address: addr,
            is_connected: bulb.is_connected(),
            bulb_type: btype,
        })),
        Err(e) => Err((axum::http::StatusCode::BAD_REQUEST, e.to_string())),
    }
}

async fn remove_bulb(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let removed = state.fleet.remove_bulb(&id).await.is_some();
    Json(serde_json::json!({ "success": removed, "id": id }))
}

#[derive(Deserialize)]
struct PowerPayload {
    power: bool,
}

async fn set_bulb_power(
    State(state): State<AppState>,
    Path(id): Path<String>,
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
struct ColorPayload {
    h: f32,
    s: f32,
    v: f32,
}

async fn set_bulb_color(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ColorPayload>,
) -> Json<serde_json::Value> {
    if let Some(bulb) = state.fleet.get_bulb(&id).await {
        let color = BulbColor::from_hsv(body.h, body.s, body.v);
        match bulb.set_color(color).await {
            Ok(_) => Json(serde_json::json!({ "success": true, "id": id })),
            Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
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
    Path(id): Path<String>,
    Json(body): Json<WhitePayload>,
) -> Json<serde_json::Value> {
    if let Some(bulb) = state.fleet.get_bulb(&id).await {
        match bulb.set_white(body.brightness, body.temp).await {
            Ok(_) => Json(serde_json::json!({ "success": true, "id": id, "brightness": body.brightness, "temp": body.temp })),
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
    let color = BulbColor::from_hsv(body.h, body.s, body.v);
    state.fleet.broadcast_color(color).await;
    Json(serde_json::json!({ "success": true, "broadcast": true }))
}

async fn broadcast_white(
    State(state): State<AppState>,
    Json(body): Json<WhitePayload>,
) -> Json<serde_json::Value> {
    state.fleet.broadcast_white(body.brightness, body.temp).await;
    Json(serde_json::json!({ "success": true, "broadcast": true, "brightness": body.brightness, "temp": body.temp }))
}

#[derive(Deserialize)]
struct StartSyncPayload {
    device: Option<String>,
    palette: Option<ColorPalette>,
    ambience_mode: Option<BulbAmbienceMode>,
    master_brightness: Option<f32>,
    smooth_transitions: Option<bool>,
    auto_normalize: Option<bool>,
}

async fn start_sync(
    State(state): State<AppState>,
    Json(body): Json<StartSyncPayload>,
) -> Json<serde_json::Value> {
    {
        let mut s = state.settings.write().await;
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
        if let Some(st) = body.smooth_transitions {
            s.smooth_transitions = st;
        }
        if let Some(an) = body.auto_normalize {
            s.auto_normalize = an;
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
    settings: SyncSettings,
}

async fn get_sync_status(State(state): State<AppState>) -> Json<SyncStatus> {
    Json(SyncStatus {
        running: state.sync_running.load(Ordering::Relaxed),
        settings: state.settings.read().await.clone(),
    })
}

#[derive(Deserialize)]
struct UpdateSyncSettingsPayload {
    palette: Option<ColorPalette>,
    ambience_mode: Option<BulbAmbienceMode>,
    master_brightness: Option<f32>,
    smooth_transitions: Option<bool>,
    auto_normalize: Option<bool>,
    audio_device: Option<String>,
}

async fn get_sync_settings(State(state): State<AppState>) -> Json<SyncSettings> {
    Json(state.settings.read().await.clone())
}

async fn update_sync_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSyncSettingsPayload>,
) -> Json<SyncSettings> {
    let mut s = state.settings.write().await;
    if let Some(pal) = body.palette {
        s.palette = pal;
    }
    if let Some(amb) = body.ambience_mode {
        s.ambience_mode = amb;
    }
    if let Some(mb) = body.master_brightness {
        s.master_brightness = mb.clamp(0.01, 1.0);
    }
    if let Some(st) = body.smooth_transitions {
        s.smooth_transitions = st;
    }
    if let Some(an) = body.auto_normalize {
        s.auto_normalize = an;
    }
    if let Some(dev) = body.audio_device {
        s.audio_device = Some(dev);
    }
    Json(s.clone())
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

async fn api_scan_devices() -> Json<Vec<serde_json::Value>> {
    let disco = match Discovery::builder().build() {
        Ok(d) => d,
        Err(_) => return Json(Vec::new()),
    };

    let devices = disco.scan(Duration::from_secs(3)).await;
    let mut results = Vec::new();
    for info in devices {
        results.push(serde_json::json!({
            "id": info.id,
            "ip": info.ip.to_string(),
            "version": info.version.map(|v| v.as_str()).unwrap_or("3.3")
        }));
    }
    disco.close().await;
    Json(results)
}

// WebSocket handler

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    let mut rx = state.ws_tx.subscribe();
    while let Ok(msg) = rx.recv().await {
        if let Ok(json_str) = serde_json::to_string(&msg) {
            if socket.send(AxumWsMsg::Text(json_str.into())).await.is_err() {
                break;
            }
        }
    }
}

// Interactive Web Dashboard HTML
async fn dashboard_html() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
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
        }
    }

    #[tokio::test]
    async fn test_dashboard_serves_html_with_threejs() {
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
        assert!(html_str.contains("Master Bulb Brightness"), "Dashboard must have master brightness");
    }

    #[tokio::test]
    async fn test_sync_settings_api() {
        let state = test_app_state();
        let mut app = build_router(state);

        // GET initial settings
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

        // POST update settings (e.g. Master Brightness & Ambience)
        let payload = serde_json::json!({
            "master_brightness": 0.75,
            "ambience_mode": "LightMode",
            "auto_normalize": true,
            "smooth_transitions": true
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
        assert!(updated.auto_normalize);
        assert!(updated.smooth_transitions);
    }
}
