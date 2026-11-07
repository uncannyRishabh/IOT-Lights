use std::io::{stdout, Write};
use std::time::{Duration, Instant};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::{
    local_identity::{load_devices_json, lookup_arp_mac, save_devices_json, SavedDeviceRecord},
    ActiveEngineMode, AppState, WsTelemetry,
};
use beat_sync::{BulbConfig, FleetBulbInfo};
use rustuya_tokio::Discovery;
use tuya_bulb::{BulbColor, BulbType, ColorFormat};

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        crate::set_tui_running(true);
        terminal::enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, cursor::Hide)?;
        out.flush()?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        let _ = out.flush();
        crate::set_tui_running(false);
    }
}

#[derive(Default)]
struct PairingState {
    active: bool,
    scanning: bool,
    discovered: Vec<DiscoveredBulb>,
    selected_idx: Option<usize>,
    key_buffer: String,
    message: String,
}

struct DiscoveredBulb {
    id: String,
    ip: String,
    mac: Option<String>,
    name: String,
    version: String,
}

#[derive(Clone, Copy, Debug)]
pub struct PredefinedColor {
    pub name: &'static str,
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
    pub is_white: bool,
    pub temp: u32,
}

pub const PREDEFINED_COLORS: &[PredefinedColor] = &[
    PredefinedColor { name: "Neon Cyan", hue: 180.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Electric Blue", hue: 220.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Deep Violet", hue: 270.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Hot Magenta", hue: 320.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Laser Red", hue: 0.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Sunset Orange", hue: 28.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Amber Gold", hue: 45.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Acid Yellow", hue: 60.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Neon Lime", hue: 90.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Emerald Green", hue: 135.0, sat: 1.0, val: 1.0, is_white: false, temp: 0 },
    PredefinedColor { name: "Warm White (2700K)", hue: 35.0, sat: 0.2, val: 1.0, is_white: true, temp: 100 },
    PredefinedColor { name: "Natural White (4000K)", hue: 45.0, sat: 0.1, val: 1.0, is_white: true, temp: 500 },
    PredefinedColor { name: "Cool White (6500K)", hue: 200.0, sat: 0.1, val: 1.0, is_white: true, temp: 1000 },
];

/// Runs the interactive 24-bit Truecolor terminal visualizer and controller.
pub async fn run_tui(state: AppState) -> anyhow::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut ws_rx = state.ws_tx.subscribe();

    let mut latest_telemetry = WsTelemetry {
        engine_mode: ActiveEngineMode::BeatSync,
        imax_cli_active: false,
        is_kick: false,
        is_snare: false,
        is_vocal_onset: false,
        has_instruments: false,
        instrument_score: 0.0,
        active_gradual: false,
        current_db: -60.0,
        gate_open: false,
        bass_intensity: 0.0,
        snare_intensity: 0.0,
        instrument_intensity: 0.0,
        vocal_intensity: 0.0,
        overall_energy: 0.0,
        active_hue: 180.0,
        active_brightness: 500,
        active_rgb: (0, 240, 255),
        agc_gain: 1.0,
        effective_color_frequency: 0.70,
        effective_color_interval_ms: 160,
    };

    let mut flash_notice: Option<(String, Instant)> = None;
    let mut pairing = PairingState::default();
    let mut frame_tick: u64 = 0;
    let mut selected_bulb_idx: Option<usize> = None;
    let mut selected_color_idx: usize = 0;

    const PALETTES: [beat_sync::ColorPalette; 6] = [
        beat_sync::ColorPalette::Cyberpunk,
        beat_sync::ColorPalette::Synthwave,
        beat_sync::ColorPalette::ClubRave,
        beat_sync::ColorPalette::Vaporwave,
        beat_sync::ColorPalette::AmbientChill,
        beat_sync::ColorPalette::Custom,
    ];

    let mut render_buf = String::with_capacity(8192);

    loop {
        // Drain any incoming WebSocket telemetry
        while let Ok(msg) = ws_rx.try_recv() {
            latest_telemetry = msg;
        }

        // Poll keyboard input
        if event::poll(Duration::from_millis(15))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if pairing.active {
                        handle_pairing_input(&mut pairing, key.code, &state).await;
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                break;
                            }
                            KeyCode::Esc => {
                                if selected_bulb_idx.is_some() {
                                    selected_bulb_idx = None;
                                } else {
                                    break;
                                }
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break;
                            }
                            KeyCode::Left => {
                                // Cycle predefined colors backward (for selected bulb or whole fleet)
                                selected_color_idx = if selected_color_idx == 0 {
                                    PREDEFINED_COLORS.len() - 1
                                } else {
                                    selected_color_idx - 1
                                };
                                let c = &PREDEFINED_COLORS[selected_color_idx];
                                let bulbs = state.fleet.list_bulbs().await;

                                if let Some(idx) = selected_bulb_idx {
                                    if idx < bulbs.len() {
                                        let b = &bulbs[idx];
                                        let bname = b.name.as_deref().unwrap_or("Bulb");
                                        if let Some(device) = state.fleet.get_bulb(&b.id).await {
                                            if c.is_white {
                                                let _ = device.set_white(1000, c.temp).await;
                                            } else {
                                                let _ = device.set_color(BulbColor::from_hsv(c.hue, c.sat, c.val)).await;
                                            }
                                        }
                                        flash_notice = Some((
                                            format!("🎨 [{}] Color: {}", bname, c.name).to_uppercase(),
                                            Instant::now(),
                                        ));
                                    }
                                } else {
                                    if c.is_white {
                                        state.fleet.broadcast_white(1000, c.temp).await;
                                    } else {
                                        state.fleet.broadcast_color(BulbColor::from_hsv(c.hue, c.sat, c.val)).await;
                                    }
                                    flash_notice = Some((
                                        format!("🎨 Fleet Color: {}", c.name).to_uppercase(),
                                        Instant::now(),
                                    ));
                                }
                            }
                            KeyCode::Right => {
                                // Cycle predefined colors forward (for selected bulb or whole fleet)
                                selected_color_idx = (selected_color_idx + 1) % PREDEFINED_COLORS.len();
                                let c = &PREDEFINED_COLORS[selected_color_idx];
                                let bulbs = state.fleet.list_bulbs().await;

                                if let Some(idx) = selected_bulb_idx {
                                    if idx < bulbs.len() {
                                        let b = &bulbs[idx];
                                        let bname = b.name.as_deref().unwrap_or("Bulb");
                                        if let Some(device) = state.fleet.get_bulb(&b.id).await {
                                            if c.is_white {
                                                let _ = device.set_white(1000, c.temp).await;
                                            } else {
                                                let _ = device.set_color(BulbColor::from_hsv(c.hue, c.sat, c.val)).await;
                                            }
                                        }
                                        flash_notice = Some((
                                            format!("🎨 [{}] Color: {}", bname, c.name).to_uppercase(),
                                            Instant::now(),
                                        ));
                                    }
                                } else {
                                    if c.is_white {
                                        state.fleet.broadcast_white(1000, c.temp).await;
                                    } else {
                                        state.fleet.broadcast_color(BulbColor::from_hsv(c.hue, c.sat, c.val)).await;
                                    }
                                    flash_notice = Some((
                                        format!("🎨 Fleet Color: {}", c.name).to_uppercase(),
                                        Instant::now(),
                                    ));
                                }
                            }
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                // Cycle Beat Sync color palette
                                let mut settings = state.settings.write().await;
                                let cur = settings.palette;
                                let idx = PALETTES.iter().position(|&p| p == cur).unwrap_or(0);
                                let next_idx = (idx + 1) % PALETTES.len();
                                settings.palette = PALETTES[next_idx];
                                flash_notice = Some((
                                    format!("🎵 Palette: {:?}", settings.palette).to_uppercase(),
                                    Instant::now(),
                                ));
                            }
                            KeyCode::Up => {
                                // Navigate fleet bulb selection upward
                                let bulbs = state.fleet.list_bulbs().await;
                                if !bulbs.is_empty() {
                                    selected_bulb_idx = Some(match selected_bulb_idx {
                                        Some(i) => if i == 0 { bulbs.len() - 1 } else { i - 1 },
                                        None => bulbs.len() - 1,
                                    });
                                }
                            }
                            KeyCode::Down => {
                                // Navigate fleet bulb selection downward
                                let bulbs = state.fleet.list_bulbs().await;
                                if !bulbs.is_empty() {
                                    selected_bulb_idx = Some(match selected_bulb_idx {
                                        Some(i) => (i + 1) % bulbs.len(),
                                        None => 0,
                                    });
                                }
                            }
                            KeyCode::Char('x') | KeyCode::Char('X') => {
                                // Toggle sync exclusion for selected bulb and save to disk
                                let bulbs = state.fleet.list_bulbs().await;
                                if let Some(idx) = selected_bulb_idx {
                                    if idx < bulbs.len() {
                                        let b = &bulbs[idx];
                                        let excluded = state.fleet.toggle_sync_excluded(&b.id).await;
                                        let _ = save_fleet_to_disk(&state).await;
                                        let bname = b.name.as_deref().unwrap_or("Bulb");
                                        if excluded {
                                            flash_notice = Some((
                                                format!("⏸ {} EXCLUDED from sync (saved)", bname),
                                                Instant::now(),
                                            ));
                                        } else {
                                            flash_notice = Some((
                                                format!("⚡ {} INCLUDED in sync (saved)", bname),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                } else {
                                    flash_notice = Some((
                                        "Use ↑/↓ to select a bulb, then [X] to toggle sync exclusion".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                // Toggle Mode
                                let mut settings = state.settings.write().await;
                                settings.engine_mode = match settings.engine_mode {
                                    ActiveEngineMode::BeatSync => ActiveEngineMode::Imax,
                                    ActiveEngineMode::Imax => ActiveEngineMode::BeatSync,
                                };
                                let mode_str = match settings.engine_mode {
                                    ActiveEngineMode::BeatSync => "BEAT SYNC",
                                    ActiveEngineMode::Imax => "IMAX MODE",
                                };
                                flash_notice = Some((format!("Switched to {}", mode_str), Instant::now()));
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') => {
                                // Cycle IMAX Display
                                let displays = beat_sync::imax::screen_capture::list_displays();
                                if !displays.is_empty() {
                                    let mut settings = state.settings.write().await;
                                    let next_idx = (settings.imax_display + 1) % displays.len() as u32;
                                    settings.imax_display = next_idx;
                                    let d_name = displays
                                        .iter()
                                        .find(|d| d.index == next_idx)
                                        .map(|d| d.name.clone())
                                        .unwrap_or_else(|| format!("Display #{}", next_idx));
                                    flash_notice = Some((format!("Target: {}", d_name), Instant::now()));
                                } else {
                                    flash_notice = Some(("No displays found".to_string(), Instant::now()));
                                }
                            }
                            KeyCode::Char(' ') => {
                                // Toggle power for selected bulb, or all if none selected
                                let bulbs = state.fleet.list_bulbs().await;
                                if let Some(idx) = selected_bulb_idx {
                                    if idx < bulbs.len() {
                                        let b = &bulbs[idx];
                                        let bname = b.name.as_deref().unwrap_or("Bulb");
                                        if let Some(device) = state.fleet.get_bulb(&b.id).await {
                                            if b.power {
                                                let _ = device.turn_off().await;
                                                flash_notice = Some((format!("{} Power OFF", bname), Instant::now()));
                                            } else {
                                                let _ = device.turn_on().await;
                                                flash_notice = Some((format!("{} Power ON", bname), Instant::now()));
                                            }
                                        }
                                    }
                                } else {
                                    let any_on = bulbs.iter().any(|b| b.power);
                                    if any_on {
                                        state.fleet.broadcast_turn_off().await;
                                        flash_notice = Some(("Fleet Power OFF".to_string(), Instant::now()));
                                    } else {
                                        state.fleet.broadcast_turn_on().await;
                                        flash_notice = Some(("Fleet Power ON".to_string(), Instant::now()));
                                    }
                                }
                            }
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                // Atomic save to bulbs.json
                                match save_fleet_to_disk(&state).await {
                                    Ok(count) => {
                                        flash_notice = Some((
                                            format!("✓ Saved {} bulb(s) to bulbs.json", count),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(err) => {
                                        flash_notice = Some((format!("❌ Save failed: {}", err), Instant::now()));
                                    }
                                }
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                // Trigger LAN Discovery & Pairing mode
                                pairing.active = true;
                                pairing.scanning = true;
                                pairing.message = "Scanning local network (UDP 6666/6667)...".to_string();
                                pairing.discovered.clear();
                                pairing.selected_idx = None;
                                pairing.key_buffer.clear();

                                // Launch discovery task
                                let scan_devices = tokio::task::spawn(async move {
                                    if let Ok(disco) = Discovery::builder().build() {
                                        let items = disco.scan(Duration::from_millis(2500)).await;
                                        disco.close().await;
                                        items
                                    } else {
                                        vec![]
                                    }
                                })
                                .await
                                .unwrap_or_default();

                                let saved = load_devices_json(&state.devices_path);
                                pairing.discovered = scan_devices
                                    .into_iter()
                                    .map(|info| {
                                        let ip_str = info.ip.to_string();
                                        let mac = lookup_arp_mac(&ip_str);
                                        let name = crate::local_identity::infer_friendly_bulb_name(
                                            &info.id,
                                            &ip_str,
                                            mac.as_deref(),
                                            &saved,
                                        );
                                        let ver = info.version.map(|v| v.as_str()).unwrap_or("3.5").to_string();
                                        DiscoveredBulb {
                                            id: info.id,
                                            ip: ip_str,
                                            mac,
                                            name,
                                            version: ver,
                                        }
                                    })
                                    .collect();

                                pairing.scanning = false;
                                if pairing.discovered.is_empty() {
                                    pairing.message = "No responsive Tuya devices found. Press [Esc] to return.".to_string();
                                } else {
                                    pairing.message = format!(
                                        "Found {} device(s). Press [1..{}] to pair, or [Esc] to cancel:",
                                        pairing.discovered.len(),
                                        pairing.discovered.len().min(9)
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Render Frame
        frame_tick += 1;
        render_buf.clear();

        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let settings = state.settings.read().await.clone();
        let bulbs = state.fleet.list_bulbs().await;

        render_tui_view(
            &mut render_buf,
            cols,
            rows,
            &settings,
            &bulbs,
            &latest_telemetry,
            &flash_notice,
            &pairing,
            selected_bulb_idx,
            selected_color_idx,
            frame_tick,
        );

        let mut out = stdout();
        execute!(out, cursor::MoveTo(0, 0))?;
        out.write_all(render_buf.as_bytes())?;
        out.flush()?;

        // Clear expired flash notices (after 2.5s)
        if let Some((_, ts)) = flash_notice {
            if ts.elapsed() > Duration::from_millis(2500) {
                flash_notice = None;
            }
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    Ok(())
}

async fn handle_pairing_input(pairing: &mut PairingState, code: KeyCode, state: &AppState) {
    if pairing.selected_idx.is_none() {
        // Selection stage: pick device 1-9
        match code {
            KeyCode::Esc => {
                pairing.active = false;
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let idx = (c as usize) - ('1' as usize);
                if idx < pairing.discovered.len() {
                    pairing.selected_idx = Some(idx);
                    pairing.key_buffer.clear();
                    pairing.message = format!(
                        "Enter 16-char Local Key for {}: [                ]",
                        pairing.discovered[idx].name
                    );
                }
            }
            _ => {}
        }
    } else {
        // Key entry stage
        match code {
            KeyCode::Esc => {
                pairing.selected_idx = None;
                pairing.key_buffer.clear();
                pairing.message = "Cancelled key input. Select 1-9 or [Esc] to exit.".to_string();
            }
            KeyCode::Backspace => {
                pairing.key_buffer.pop();
            }
            KeyCode::Char(c) if pairing.key_buffer.len() < 16 && (c.is_alphanumeric() || c == '_' || c == '-') => {
                pairing.key_buffer.push(c);
            }
            KeyCode::Enter => {
                if pairing.key_buffer.len() == 16 {
                    let idx = pairing.selected_idx.unwrap();
                    let target = &pairing.discovered[idx];
                    let bulb_cfg = BulbConfig {
                        id: target.id.clone(),
                        name: Some(target.name.clone()),
                        local_key: pairing.key_buffer.clone(),
                        address: target.ip.clone(),
                        port: Some(6668),
                        version: Some(target.version.clone()),
                        bulb_type: Some(BulbType::TypeB),
                        color_format: Some(ColorFormat::Hex12),
                        mac: target.mac.clone(),
                    };

                    match state.fleet.add_bulb(bulb_cfg).await {
                        Ok(_) => {
                            let mut records = load_devices_json(&state.devices_path);
                            if let Some(rec) = records.iter_mut().find(|r| r.id == target.id) {
                                rec.key = pairing.key_buffer.clone();
                                rec.ip = target.ip.clone();
                                rec.name = Some(target.name.clone());
                                rec.mac = target.mac.clone();
                            } else {
                                records.push(SavedDeviceRecord {
                                    name: Some(target.name.clone()),
                                    id: target.id.clone(),
                                    key: pairing.key_buffer.clone(),
                                    ip: target.ip.clone(),
                                    version: target.version.clone(),
                                    bulb_type: Some(BulbType::TypeB),
                                    mac: target.mac.clone(),
                                    sync_excluded: false,
                                });
                            }
                            let bulbs_json_path = std::path::Path::new("bulbs.json");
                            let _ = save_devices_json(bulbs_json_path, &records);
                            if state.devices_path != bulbs_json_path {
                                let _ = save_devices_json(&state.devices_path, &records);
                            }
                            pairing.message = format!("✓ Paired & Saved {} to bulbs.json!", target.name);
                            pairing.active = false;
                        }
                        Err(e) => {
                            pairing.message = format!("❌ Connection failed: {}", e);
                        }
                    }
                } else {
                    pairing.message = format!(
                        "Key must be exactly 16 characters! (Currently {} chars)",
                        pairing.key_buffer.len()
                    );
                }
            }
            _ => {}
        }
    }
}

async fn save_fleet_to_disk(state: &AppState) -> anyhow::Result<usize> {
    let mut records = load_devices_json(&state.devices_path);
    let fleet_bulbs = state.fleet.list_bulbs().await;

    // Persist to bulbs.json in current directory, preserving existing keys and adding new bulbs
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

    let bulbs_json_path = std::path::Path::new("bulbs.json");
    save_devices_json(bulbs_json_path, &records)?;
    if state.devices_path != bulbs_json_path {
        let _ = save_devices_json(&state.devices_path, &records);
    }
    Ok(records.len())
}

#[allow(clippy::too_many_arguments)]
fn render_tui_view(
    buf: &mut String,
    cols: u16,
    _rows: u16,
    settings: &crate::SyncSettings,
    bulbs: &[FleetBulbInfo],
    t: &WsTelemetry,
    flash: &Option<(String, Instant)>,
    pairing: &PairingState,
    selected_idx: Option<usize>,
    selected_color_idx: usize,
    _tick: u64,
) {
    let w = (cols as usize).max(80);

    // 1. Header Bar
    let mode_pill = match settings.engine_mode {
        ActiveEngineMode::BeatSync => "\x1b[1;30;42m [🎵 BEAT SYNC] \x1b[0m",
        ActiveEngineMode::Imax => "\x1b[1;30;45m [🎬 IMAX MODE] \x1b[0m",
    };

    let palette_name = format!("{:?}", settings.palette).to_uppercase();
    let palette_badge = format!("\x1b[1;37;48;2;147;51;234m [🎨 {}] \x1b[0m", palette_name);

    let online_count = bulbs.iter().filter(|b| b.is_connected).count();
    let fleet_badge = format!(
        "\x1b[1;37;44m ({}/{} Online) \x1b[0m",
        online_count,
        bulbs.len()
    );

    let config_status = if std::path::Path::new("bulbs.json").exists() {
        "\x1b[32mbulbs.json: SAVED ✓\x1b[0m"
    } else {
        "\x1b[33mbulbs.json: NOT SAVED\x1b[0m"
    };

    buf.push_str("\x1b[1;37;48;2;15;23;42m  IOT-BULB SYNC v3.0  \x1b[0m ");
    buf.push_str(mode_pill);
    buf.push(' ');
    buf.push_str(&palette_badge);
    buf.push(' ');
    buf.push_str(&fleet_badge);
    buf.push_str(" | ");
    buf.push_str(config_status);
    buf.push_str("\x1b[K\r\n");

    // Divider
    buf.push_str(&format!("{}\x1b[K\r\n", "─".repeat(w)));

    if pairing.active {
        // Pairing Sub-Screen
        buf.push_str("\x1b[1;36m[📡 LOCAL NETWORK DISCOVERY & DEVICE PAIRING]\x1b[0m\x1b[K\r\n");
        buf.push_str(&format!("\x1b[33m{}\x1b[0m\x1b[K\r\n\r\n", pairing.message));

        for (i, d) in pairing.discovered.iter().enumerate().take(9) {
            let selected_mark = if pairing.selected_idx == Some(i) { "▶ " } else { "  " };
            buf.push_str(&format!(
                "{}[{}] {:<24} {:<15} MAC: {:<17} (v{})\x1b[K\r\n",
                selected_mark,
                i + 1,
                d.name,
                d.ip,
                d.mac.as_deref().unwrap_or("--:--:--:--:--:--"),
                d.version
            ));
        }

        if pairing.selected_idx.is_some() {
            buf.push_str("\r\n");
            let key_display: String = pairing.key_buffer.chars().map(|_| '*').collect();
            let pad = 16usize.saturating_sub(pairing.key_buffer.len());
            buf.push_str(&format!(
                "\x1b[1;32mLocal Key (16 chars): [{}{}]\x1b[0m\x1b[K\r\n",
                key_display,
                " ".repeat(pad)
            ));
            buf.push_str("\x1b[90m(Type 16 alphanumeric characters, then press [Enter] to pair & save to bulbs.json)\x1b[0m\x1b[K\r\n");
        }
    } else {
        // Main Visualizer View
        let (r, g, b) = t.active_rgb;

        // 2. 24-Bit Truecolor Waveform Spectrum Bar (32 columns)
        buf.push_str("24-BIT TRUECOLOR SPECTRUM (Real-Time Audio Stem Lockstep):\x1b[K\r\n  ");
        for col in 0..32 {
            let band_val = if col < 8 {
                let frac = col as f32 / 7.0;
                let kick_boost = if t.is_kick { 0.45 } else { 0.0 };
                (t.bass_intensity * (0.8 + 0.2 * (1.0 - frac)) + kick_boost).clamp(0.0, 1.0)
            } else if col < 16 {
                let frac = (col - 8) as f32 / 7.0;
                let mid_boost = t.instrument_score * 0.2;
                (t.instrument_intensity * (0.85 + 0.15 * frac.sin()) + mid_boost).clamp(0.0, 1.0)
            } else if col < 24 {
                let frac = (col - 16) as f32 / 7.0;
                let vocal_boost = if t.is_vocal_onset { 0.35 } else { 0.0 };
                (t.vocal_intensity * (0.80 + 0.20 * (1.0 - frac)) + vocal_boost).clamp(0.0, 1.0)
            } else {
                let snare_boost = if t.is_snare { 0.45 } else { 0.0 };
                (t.snare_intensity * 0.75 + t.overall_energy * 0.25 + snare_boost).clamp(0.0, 1.0)
            };

            let val = if t.gate_open { band_val } else { band_val * 0.12 };

            let char_block = if val > 0.85 {
                "█"
            } else if val > 0.70 {
                "▇"
            } else if val > 0.55 {
                "▆"
            } else if val > 0.40 {
                "▅"
            } else if val > 0.25 {
                "▄"
            } else if val > 0.12 {
                "▃"
            } else if val > 0.04 {
                "▂"
            } else {
                " "
            };

            let hue_shift = (col as f32 / 31.0 - 0.5) * 60.0;
            let col_hue = (t.active_hue + hue_shift).rem_euclid(360.0);
            let (cr, cg, cb) = hsv_to_rgb(col_hue, 0.95, (0.35 + 0.65 * val).clamp(0.0, 1.0));

            buf.push_str(&format!("\x1b[38;2;{};{};{}m{}\x1b[0m", cr, cg, cb, char_block));
        }

        // Active Color Swatch readout
        buf.push_str(&format!(
            "  \x1b[48;2;{};{};{}m  \x1b[0m \x1b[1;37mRGB({},{},{})\x1b[0m | HUE: {:>3.0}° | BRI: {:>4}/1000\x1b[K\r\n\r\n",
            r, g, b, r, g, b, t.active_hue, t.active_brightness
        ));

        // 3. Dedicated Horizontal Stem Meters
        let inst_pct = (t.instrument_intensity * 100.0).round() as u32;
        let vocal_pct = (t.vocal_intensity * 100.0).round() as u32;

        let inst_bar = render_progress_bar(t.instrument_intensity, 14);
        let vocal_bar = render_progress_bar(t.vocal_intensity, 14);

        let gate_str = if t.gate_open {
            format!("\x1b[32m{:>5.1} dB ● OPEN\x1b[0m", t.current_db)
        } else {
            format!("\x1b[31m{:>5.1} dB ○ CLOSED\x1b[0m", t.current_db)
        };

        let auto_bpm = (60000.0 / (t.effective_color_interval_ms as f32 * 2.0).max(10.0)).round() as u32;
        let auto_freq_str = format!(
            "{}ms / {} BPM",
            t.effective_color_interval_ms,
            auto_bpm.clamp(60, 240)
        );

        buf.push_str(&format!(
            "  INSTRUMENT  [{}] {:>3}%    GATE       [{}]\x1b[K\r\n",
            inst_bar, inst_pct, gate_str
        ));
        buf.push_str(&format!(
            "  VOCAL       [{}] {:>3}%    AUTO FREQ  [{:<14}]\x1b[K\r\n\r\n",
            vocal_bar, vocal_pct, auto_freq_str
        ));

        // 4. Fleet Status Rail with Viewport Scrolling
        let total = bulbs.len();
        let page_size = 6;
        let start_idx = if let Some(sel) = selected_idx {
            if sel >= page_size {
                sel.saturating_sub(page_size - 1)
            } else {
                0
            }
        } else {
            0
        };
        let end_idx = (start_idx + page_size).min(total);

        let active_c = &PREDEFINED_COLORS[selected_color_idx % PREDEFINED_COLORS.len()];
        let (cr, cg, cb) = if active_c.is_white {
            (255, 245, 230)
        } else {
            hsv_to_rgb(active_c.hue, active_c.sat, active_c.val)
        };

        buf.push_str(&format!(
            "\x1b[1;37mCONFIGURED FLEET ({}/{} shown | ↑/↓ Select, ←/→ Color, [Space] Power, [X] Exclude):\x1b[0m\x1b[K\r\n",
            if total > 0 { end_idx - start_idx } else { 0 },
            total
        ));

        if bulbs.is_empty() {
            buf.push_str("  \x1b[90m(No bulbs configured. Press [P] to scan LAN & pair)\x1b[0m\x1b[K\r\n");
        } else {
            for (i, b) in bulbs.iter().enumerate().skip(start_idx).take(page_size) {
                let is_selected = selected_idx == Some(i);
                let sel_mark = if is_selected { "\x1b[1;33m▶\x1b[0m " } else { "  " };

                let status_icon = if b.is_connected {
                    "\x1b[32m● CONNECTED\x1b[0m"
                } else {
                    "\x1b[31m○ OFFLINE\x1b[0m"
                };
                let power_icon = if b.power { "\x1b[33m⚡ ON\x1b[0m " } else { "\x1b[90m○ OFF\x1b[0m" };
                let sync_badge = if b.sync_excluded {
                    "\x1b[1;33m⏸ EXCLUDED\x1b[0m"
                } else {
                    "\x1b[1;36m⚡ SYNCING\x1b[0m"
                };

                let name = b.name.as_deref().unwrap_or("Smart Bulb");
                let name_formatted = if is_selected {
                    format!("\x1b[1;37;48;2;30;41;59m {:<22} \x1b[0m", name)
                } else {
                    format!("{:<24}", name)
                };

                buf.push_str(&format!(
                    "{}{} {:<15} {:<13} {:<8} {}\x1b[K\r\n",
                    sel_mark, name_formatted, b.address, status_icon, power_icon, sync_badge
                ));
            }
        }

        // Active Color / Bulb selection hint rail
        buf.push_str("\r\n");
        if let Some(sel) = selected_idx {
            if sel < bulbs.len() {
                let bname = bulbs[sel].name.as_deref().unwrap_or("Bulb");
                buf.push_str(&format!(
                    "  \x1b[1;33m▶ BULB TARGET:\x1b[0m \x1b[1;37m{}\x1b[0m  |  \x1b[48;2;{};{};{}m  \x1b[0m \x1b[1;36mColor: {}\x1b[0m  \x1b[90m(←/→ cycle color, [Space] power, [X] exclude, [Esc] deselect)\x1b[0m\x1b[K\r\n",
                    bname, cr, cg, cb, active_c.name
                ));
            }
        } else {
            buf.push_str(&format!(
                "  \x1b[1;37mFLEET COLOR:\x1b[0m  \x1b[48;2;{};{};{}m  \x1b[0m \x1b[1;36m{}\x1b[0m  \x1b[90m(←/→ broadcast color to all, ↑/↓ select bulb, [C] Beat Sync palette)\x1b[0m\x1b[K\r\n",
                cr, cg, cb, active_c.name
            ));
        }
    }

    // 5. Flash Notification or Recent Log
    buf.push_str("\r\n");
    if let Some((msg, _)) = flash {
        buf.push_str(&format!("\x1b[1;33;48;2;30;41;59m  {}  \x1b[0m\x1b[K\r\n", msg));
    } else if let Some(last_log) = crate::get_latest_log() {
        let max_len = w.saturating_sub(12);
        let truncated = if last_log.len() > max_len {
            format!("{}...", &last_log[..max_len])
        } else {
            last_log
        };
        buf.push_str(&format!("\x1b[90m  LOG: {}\x1b[0m\x1b[K\r\n", truncated));
    } else {
        buf.push_str("\x1b[K\r\n");
    }

    // 6. Interactive Hotkey Bar (Bottom Row)
    buf.push_str(&format!("{}\x1b[K\r\n", "─".repeat(w)));
    buf.push_str("\x1b[1;37;48;2;15;23;42m");
    buf.push_str(" [↑/↓] Select Bulb  [←/→] Color  [Space] Power  [X] Sync Exclude  [C] Palette  [M] Mode  [D] Display  [P] Pair  [S] Save  [Q] Quit ");
    buf.push_str("\x1b[0m\x1b[K\r\n");
}

fn render_progress_bar(val: f32, len: usize) -> String {
    let filled = ((val.clamp(0.0, 1.0) * len as f32).round() as usize).min(len);
    let empty = len.saturating_sub(filled);
    format!(
        "\x1b[36m{}\x1b[90m{}\x1b[0m",
        "█".repeat(filled),
        "░".repeat(empty)
    )
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = if hp < 1.0 {
        (c, x, 0.0)
    } else if hp < 2.0 {
        (x, c, 0.0)
    } else if hp < 3.0 {
        (0.0, c, x)
    } else if hp < 4.0 {
        (0.0, x, c)
    } else if hp < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predefined_colors_coverage() {
        assert!(PREDEFINED_COLORS.len() >= 10);
        for c in PREDEFINED_COLORS {
            assert!(!c.name.is_empty());
            assert!(c.hue >= 0.0 && c.hue <= 360.0);
            assert!(c.sat >= 0.0 && c.sat <= 1.0);
            assert!(c.val >= 0.0 && c.val <= 1.0);
        }
    }

    #[test]
    fn test_tui_render_with_scrolling_fleet() {
        let settings = crate::SyncSettings::default();
        let telemetry = WsTelemetry {
            engine_mode: ActiveEngineMode::BeatSync,
            imax_cli_active: false,
            is_kick: false,
            is_snare: false,
            is_vocal_onset: false,
            has_instruments: true,
            instrument_score: 0.8,
            active_gradual: false,
            current_db: -24.0,
            gate_open: true,
            bass_intensity: 0.5,
            snare_intensity: 0.3,
            instrument_intensity: 0.6,
            vocal_intensity: 0.4,
            overall_energy: 0.55,
            active_hue: 180.0,
            active_brightness: 800,
            active_rgb: (0, 240, 255),
            agc_gain: 1.0,
            effective_color_frequency: 0.70,
            effective_color_interval_ms: 160,
        };
        let pairing = PairingState::default();

        // 1. Render empty fleet
        let mut buf = String::new();
        render_tui_view(
            &mut buf,
            100,
            30,
            &settings,
            &[],
            &telemetry,
            &None,
            &pairing,
            None,
            0,
            0,
        );
        assert!(buf.contains("IOT-BULB SYNC"));
        assert!(buf.contains("(No bulbs configured"));
        assert!(buf.contains("FLEET COLOR"));

        // 2. Render large fleet (10 bulbs) with selected index 8 (tests viewport scroll window)
        let mut bulbs = Vec::new();
        for i in 0..10 {
            bulbs.push(FleetBulbInfo {
                id: format!("bulb_{:02}", i),
                name: Some(format!("Living Room {:02}", i)),
                address: format!("192.168.1.{}", 10 + i),
                is_connected: i % 2 == 0,
                power: true,
                bulb_type: tuya_bulb::BulbType::TypeB,
                mac: None,
                sync_excluded: i == 3,
            });
        }

        buf.clear();
        render_tui_view(
            &mut buf,
            120,
            30,
            &settings,
            &bulbs,
            &telemetry,
            &None,
            &pairing,
            Some(8),
            2, // Deep Violet
            0,
        );

        assert!(buf.contains("CONFIGURED FLEET (6/10 shown"));
        assert!(buf.contains("Living Room 08"));
        assert!(buf.contains("BULB TARGET"));
        assert!(buf.contains("Color: Deep Violet"));
        assert!(buf.contains("⏸ EXCLUDED"));
    }
}
