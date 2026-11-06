//! `tuya-bulb`: High-level Tuya smart bulb control, color modeling, and DPS formatting.
//!
//! Handles:
//! - **Type A bulbs** (DPS 1=power, 2=mode, 3=brightness, 4=color temp, 5=color)
//! - **Type B bulbs** (DPS 20=power, 21=mode, 22=brightness, 23=color temp, 24=color, 27=music)
//! - Multi-format color encoding: 12-char hex (`HHHHSSSSVVVV`), 14-char hex (`RRGGBBHHHHSSVV`), and JSON
//! - High-speed real-time DP 27 `music_data` streaming packet generation
//! - Thread-safe [`BulbDevice`] handle wrapping `rustuya_tokio::Device`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustuya_tokio::{Device, Result as TokioResult, Value};
use serde::{Deserialize, Serialize};

/// Type of Tuya smart bulb DP mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BulbType {
    /// Legacy bulbs: DP 1 (power), 2 (mode), 3 (brightness), 4 (temp), 5 (colour)
    TypeA,
    /// Modern bulbs (v3.3/v3.4/v3.5, Wipro, Syska, Gosund, Treatlife, Lumary, etc.):
    /// DP 20 (power), 21 (mode), 22 (brightness), 23 (temp), 24 (colour), 27 (music)
    #[default]
    TypeB,
}

/// Work mode for Tuya smart bulbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BulbMode {
    #[default]
    White,
    Colour,
    Scene,
    Music,
}

impl BulbMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            BulbMode::White => "white",
            BulbMode::Colour => "colour",
            BulbMode::Scene => "scene",
            BulbMode::Music => "music",
        }
    }
}

/// Color encoding format for bulb color data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorFormat {
    /// 12 hex digits: `HHHH` (0-360) + `SSSS` (0-1000) + `VVVV` (0-1000). Standard modern Tuya.
    #[default]
    Hex12,
    /// 14 hex digits: `RRGGBB` + `HHHH` + `SS` + `VV`. Older v3.1 Tuya bulbs.
    Hex14,
    /// JSON string: `{"h": 0-360, "s": 0-1000, "v": 0-1000}`.
    Json,
}

/// Normalized HSV / RGB color representation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BulbColor {
    /// Hue in degrees: 0.0 to 360.0
    pub h: f32,
    /// Saturation: 0.0 to 1.0 (0% to 100%)
    pub s: f32,
    /// Value / Brightness: 0.0 to 1.0 (0% to 100%)
    pub v: f32,
}

impl Default for BulbColor {
    fn default() -> Self {
        Self {
            h: 0.0,
            s: 1.0,
            v: 1.0,
        }
    }
}

impl BulbColor {
    /// Create a color from HSV values.
    /// `h`: 0.0..=360.0, `s`: 0.0..=1.0 (or 0-100%), `v`: 0.0..=1.0 (or 0-100%)
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let s_norm = if s > 1.0 && s <= 100.0 { s / 100.0 } else { s };
        let v_norm = if v > 1.0 && v <= 100.0 { v / 100.0 } else { v };
        Self {
            h: h.rem_euclid(360.0),
            s: s_norm.clamp(0.0, 1.0),
            v: v_norm.clamp(0.0, 1.0),
        }
    }

    /// Create a color from RGB 8-bit values.
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        let rf = r as f32 / 255.0;
        let gf = g as f32 / 255.0;
        let bf = b as f32 / 255.0;

        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        let delta = max - min;

        let v = max;
        let s = if max == 0.0 { 0.0 } else { delta / max };

        let h = if delta == 0.0 {
            0.0
        } else if max == rf {
            60.0 * (((gf - bf) / delta).rem_euclid(6.0))
        } else if max == gf {
            60.0 * (((bf - rf) / delta) + 2.0)
        } else {
            60.0 * (((rf - gf) / delta) + 4.0)
        };

        Self {
            h: h.rem_euclid(360.0),
            s,
            v,
        }
    }

    /// Convert to RGB 8-bit values.
    pub fn to_rgb(&self) -> (u8, u8, u8) {
        let c = self.v * self.s;
        let x = c * (1.0 - ((self.h / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = self.v - c;

        let (r1, g1, b1) = match (self.h / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };

        (
            ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
            ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
            ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Encode into 12-char hex string: `HHHHSSSSVVVV` (0-360, 0-1000, 0-1000).
    pub fn to_hex12(&self) -> String {
        let h_val = self.h.round().clamp(0.0, 360.0) as u16;
        let s_val = (self.s * 1000.0).round().clamp(0.0, 1000.0) as u16;
        let v_val = (self.v * 1000.0).round().clamp(0.0, 1000.0) as u16;
        format!("{h_val:04x}{s_val:04x}{v_val:04x}")
    }

    /// Encode into 14-char hex string: `RRGGBBHHHHSSVV` (v3.1 older bulbs).
    pub fn to_hex14(&self) -> String {
        let (r, g, b) = self.to_rgb();
        let h_val = self.h.round().clamp(0.0, 360.0) as u16;
        let s_val = (self.s * 255.0).round().clamp(0.0, 255.0) as u8;
        let v_val = (self.v * 255.0).round().clamp(0.0, 255.0) as u8;
        format!("{r:02x}{g:02x}{b:02x}{h_val:04x}{s_val:02x}{v_val:02x}")
    }

    /// Encode into JSON string: `{"h": 0-360, "s": 0-1000, "v": 0-1000}`.
    pub fn to_json_str(&self) -> String {
        let h_val = self.h.round().clamp(0.0, 360.0) as u32;
        let s_val = (self.s * 1000.0).round().clamp(0.0, 1000.0) as u32;
        let v_val = (self.v * 1000.0).round().clamp(0.0, 1000.0) as u32;
        serde_json::json!({
            "h": h_val,
            "s": s_val,
            "v": v_val
        })
        .to_string()
    }

    /// Parse 12-char or 14-char hex, or JSON into `BulbColor`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.starts_with('{') {
            let val: serde_json::Value = serde_json::from_str(s).ok()?;
            let h = val.get("h")?.as_f64()? as f32;
            let s_raw = val.get("s")?.as_f64()? as f32;
            let v_raw = val.get("v")?.as_f64()? as f32;
            let s_norm = if s_raw > 1.0 { s_raw / 1000.0 } else { s_raw };
            let v_norm = if v_raw > 1.0 { v_raw / 1000.0 } else { v_raw };
            return Some(Self::from_hsv(h, s_norm, v_norm));
        }

        if s.len() == 12 {
            let h = u16::from_str_radix(&s[0..4], 16).ok()? as f32;
            let s_val = u16::from_str_radix(&s[4..8], 16).ok()? as f32 / 1000.0;
            let v_val = u16::from_str_radix(&s[8..12], 16).ok()? as f32 / 1000.0;
            return Some(Self::from_hsv(h, s_val, v_val));
        }

        if s.len() == 14 {
            let h = u16::from_str_radix(&s[6..10], 16).ok()? as f32;
            let s_val = u8::from_str_radix(&s[10..12], 16).ok()? as f32 / 255.0;
            let v_val = u8::from_str_radix(&s[12..14], 16).ok()? as f32 / 255.0;
            return Some(Self::from_hsv(h, s_val, v_val));
        }

        None
    }
}

/// Generates the 21-character Tuya DP 27 `music_data` string.
/// Format: `[mode 1 char: 0=jump, 1=gradient]` + `HHHH` (hue 0-360) + `SSSS` (sat 0-1000) +
/// `VVVV` (val 0-1000) + `BBBB` (brightness 0-1000) + `TTTT` (temp 0-1000).
pub fn format_music_data(color: BulbColor, brightness: u32, temp: u32, gradient: bool) -> String {
    let mode_char = if gradient { '1' } else { '0' };
    let h_val = color.h.round().clamp(0.0, 360.0) as u16;
    let s_val = (color.s * 1000.0).round().clamp(0.0, 1000.0) as u16;
    let v_val = (color.v * 1000.0).round().clamp(0.0, 1000.0) as u16;
    let b_val = brightness.clamp(10, 1000) as u16;
    let t_val = temp.clamp(0, 1000) as u16;

    format!("{mode_char}{h_val:04x}{s_val:04x}{v_val:04x}{b_val:04x}{t_val:04x}")
}

/// Extracted state of a Tuya bulb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BulbState {
    pub power: bool,
    pub mode: BulbMode,
    pub brightness: u32,
    pub color_temp: u32,
    pub color: BulbColor,
}

impl Default for BulbState {
    fn default() -> Self {
        Self {
            power: true,
            mode: BulbMode::White,
            brightness: 1000,
            color_temp: 500,
            color: BulbColor::default(),
        }
    }
}

impl BulbState {
    /// Parse state from Tuya DPS JSON dictionary with tolerance for string, numeric, and boolean types.
    pub fn from_dps(dps: &serde_json::Map<String, Value>) -> Self {
        let mut state = Self::default();

        let parse_bool = |v: &Value| {
            v.as_bool().or_else(|| {
                v.as_str().map(|s| s.eq_ignore_ascii_case("true") || s == "1")
            })
        };

        let parse_u32 = |v: &Value| {
            v.as_u64()
                .map(|n| n as u32)
                .or_else(|| v.as_f64().map(|f| f.round() as u32))
                .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
        };

        // Check Type B first (DPS 20+)
        if let Some(p) = dps.get("20").and_then(parse_bool) {
            state.power = p;
        } else if let Some(p) = dps.get("1").and_then(parse_bool) {
            state.power = p;
        }

        if let Some(m) = dps.get("21").and_then(Value::as_str) {
            state.mode = match m {
                "colour" => BulbMode::Colour,
                "scene" => BulbMode::Scene,
                "music" => BulbMode::Music,
                _ => BulbMode::White,
            };
        } else if let Some(m) = dps.get("2").and_then(Value::as_str) {
            state.mode = match m {
                "colour" => BulbMode::Colour,
                "scene" => BulbMode::Scene,
                "music" => BulbMode::Music,
                _ => BulbMode::White,
            };
        }

        if let Some(b) = dps.get("22").and_then(parse_u32) {
            state.brightness = b;
        } else if let Some(b) = dps.get("3").and_then(parse_u32) {
            state.brightness = if b <= 255 {
                b * 1000 / 255
            } else {
                b
            };
        }

        if let Some(t) = dps.get("23").and_then(parse_u32) {
            state.color_temp = t;
        } else if let Some(t) = dps.get("4").and_then(parse_u32) {
            state.color_temp = if t <= 255 {
                t * 1000 / 255
            } else {
                t
            };
        }

        if let Some(c_str) = dps.get("24").and_then(Value::as_str) {
            if let Some(c) = BulbColor::parse(c_str) {
                state.color = c;
            }
        } else if let Some(c_str) = dps.get("5").and_then(Value::as_str) {
            if let Some(c) = BulbColor::parse(c_str) {
                state.color = c;
            }
        }

        state
    }
}

/// A high-level smart bulb handle wrapping `Device`.
#[derive(Clone, Debug)]
pub struct BulbDevice {
    device: Device,
    bulb_type: BulbType,
    color_format: ColorFormat,
    is_on: Arc<AtomicBool>,
}

impl BulbDevice {
    /// Create a new BulbDevice controller.
    pub fn new(device: Device, bulb_type: BulbType, color_format: ColorFormat) -> Self {
        Self {
            device,
            bulb_type,
            color_format,
            is_on: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Underlying raw Tuya device handle.
    pub fn raw_device(&self) -> &Device {
        &self.device
    }

    /// Device ID.
    pub fn id(&self) -> &str {
        self.device.id()
    }

    /// Whether the device connection is currently live.
    pub fn is_connected(&self) -> bool {
        self.device.is_connected()
    }

    /// Wait for device connection to come up.
    pub async fn wait_connected(&self, timeout: Duration) -> TokioResult<()> {
        self.device.wait_connected(timeout).await
    }

    /// Turn bulb power on.
    pub async fn turn_on(&self) -> TokioResult<()> {
        let dp_key = match self.bulb_type {
            BulbType::TypeA => "1",
            BulbType::TypeB => "20",
        };
        let mut map = serde_json::Map::new();
        map.insert(dp_key.to_string(), Value::Bool(true));
        self.is_on.store(true, Ordering::Relaxed);
        self.device.set_dps(Value::Object(map)).await
    }

    /// Turn bulb power off.
    pub async fn turn_off(&self) -> TokioResult<()> {
        let dp_key = match self.bulb_type {
            BulbType::TypeA => "1",
            BulbType::TypeB => "20",
        };
        let mut map = serde_json::Map::new();
        map.insert(dp_key.to_string(), Value::Bool(false));
        self.is_on.store(false, Ordering::Relaxed);
        self.device.set_dps(Value::Object(map)).await
    }

    /// Toggle bulb power.
    pub async fn toggle(&self) -> TokioResult<()> {
        if self.is_on.load(Ordering::Relaxed) {
            self.turn_off().await
        } else {
            self.turn_on().await
        }
    }

    /// Set mode: "white", "colour", "scene", "music".
    pub async fn set_mode(&self, mode: BulbMode) -> TokioResult<()> {
        let dp_key = match self.bulb_type {
            BulbType::TypeA => "2",
            BulbType::TypeB => "21",
        };
        let mut map = serde_json::Map::new();
        map.insert(dp_key.to_string(), Value::String(mode.as_str().to_string()));
        self.device.set_dps(Value::Object(map)).await
    }

    /// Set white light with brightness (10..=1000) and color temperature (0..=1000).
    pub async fn set_white(&self, brightness: u32, temp: u32) -> TokioResult<()> {
        let mut map = serde_json::Map::new();
        match self.bulb_type {
            BulbType::TypeA => {
                let b = ((brightness.clamp(10, 1000) as f32 / 1000.0) * 255.0).round() as u64;
                let t = ((temp.clamp(0, 1000) as f32 / 1000.0) * 255.0).round() as u64;
                map.insert("1".to_string(), Value::Bool(true));
                map.insert("2".to_string(), Value::String("white".to_string()));
                map.insert("3".to_string(), Value::from(b));
                map.insert("4".to_string(), Value::from(t));
            }
            BulbType::TypeB => {
                map.insert("20".to_string(), Value::Bool(true));
                map.insert("21".to_string(), Value::String("white".to_string()));
                map.insert("22".to_string(), Value::from(brightness.clamp(10, 1000) as u64));
                map.insert("23".to_string(), Value::from(temp.clamp(0, 1000) as u64));
            }
        }
        self.is_on.store(true, Ordering::Relaxed);
        self.device.set_dps(Value::Object(map)).await
    }

    /// Set color with HSV. Automatically ensures bulb is in 'colour' mode.
    pub async fn set_color(&self, color: BulbColor) -> TokioResult<()> {
        let color_payload = match self.color_format {
            ColorFormat::Hex12 => Value::String(color.to_hex12()),
            ColorFormat::Hex14 => Value::String(color.to_hex14()),
            ColorFormat::Json => Value::String(color.to_json_str()),
        };

        let mut map = serde_json::Map::new();
        match self.bulb_type {
            BulbType::TypeA => {
                map.insert("1".to_string(), Value::Bool(true));
                map.insert("2".to_string(), Value::String("colour".to_string()));
                map.insert("5".to_string(), color_payload);
            }
            BulbType::TypeB => {
                map.insert("20".to_string(), Value::Bool(true));
                map.insert("21".to_string(), Value::String("colour".to_string()));
                map.insert("24".to_string(), color_payload);
            }
        }
        self.is_on.store(true, Ordering::Relaxed);
        self.device.set_dps(Value::Object(map)).await
    }

    /// Stream a high-speed music sync frame.
    /// On Type B bulbs, utilizes DP 27 (`music_data`) to bypass PWM smoothing ramp delays!
    /// On Type A bulbs, emits an optimized DP 5 color jump.
    pub async fn stream_music_frame(
        &self,
        color: BulbColor,
        brightness: u32,
        temp: u32,
        gradient: bool,
    ) -> TokioResult<()> {
        let mut map = serde_json::Map::new();
        match self.bulb_type {
            BulbType::TypeB => {
                let music_str = format_music_data(color, brightness, temp, gradient);
                map.insert("27".to_string(), Value::String(music_str));
            }
            BulbType::TypeA => {
                let color_payload = match self.color_format {
                    ColorFormat::Hex12 => Value::String(color.to_hex12()),
                    ColorFormat::Hex14 => Value::String(color.to_hex14()),
                    ColorFormat::Json => Value::String(color.to_json_str()),
                };
                map.insert("5".to_string(), color_payload);
            }
        }
        self.device.set_dps(Value::Object(map)).await
    }

    /// Enter music synchronization mode.
    /// Sets Type B bulbs to DP 20=true, 21='music', and Type A bulbs to DP 1=true, 2='colour'.
    pub async fn enter_music_mode(&self) -> TokioResult<()> {
        let mut map = serde_json::Map::new();
        match self.bulb_type {
            BulbType::TypeA => {
                map.insert("1".to_string(), Value::Bool(true));
                map.insert("2".to_string(), Value::String("colour".to_string()));
            }
            BulbType::TypeB => {
                map.insert("20".to_string(), Value::Bool(true));
                map.insert("21".to_string(), Value::String("music".to_string()));
            }
        }
        self.is_on.store(true, Ordering::Relaxed);
        self.device.set_dps(Value::Object(map)).await
    }

    /// Exit music synchronization mode and restore standard colour mode.
    pub async fn exit_music_mode(&self) -> TokioResult<()> {
        self.set_mode(BulbMode::Colour).await
    }

    /// Query the bulb state asynchronously.
    pub async fn query_state(&self) -> TokioResult<()> {
        self.device.query().await
    }

    /// Read latest received state from `watch_status`.
    pub fn current_state(&self) -> Option<BulbState> {
        let msg = self.device.watch_status().borrow().clone()?;
        let dps = msg.dps()?;
        match dps {
            Value::Object(map) => Some(BulbState::from_dps(&map)),
            _ => None,
        }
    }

    /// Close the underlying device connection.
    pub async fn close(&self) {
        self.device.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_to_rgb_and_back_roundtrip() {
        let red = BulbColor::from_hsv(0.0, 1.0, 1.0);
        let (r, g, b) = red.to_rgb();
        assert_eq!((r, g, b), (255, 0, 0));

        let green = BulbColor::from_hsv(120.0, 1.0, 1.0);
        let (r, g, b) = green.to_rgb();
        assert_eq!((r, g, b), (0, 255, 0));

        let blue = BulbColor::from_hsv(240.0, 1.0, 1.0);
        let (r, g, b) = blue.to_rgb();
        assert_eq!((r, g, b), (0, 0, 255));

        let from_blue = BulbColor::from_rgb(0, 0, 255);
        assert!((from_blue.h - 240.0).abs() < 1.0);
        assert!((from_blue.s - 1.0).abs() < 0.01);
        assert!((from_blue.v - 1.0).abs() < 0.01);
    }

    #[test]
    fn hex12_encoding_and_parsing() {
        // Red: H=0 (0x0000), S=1000 (0x03e8), V=1000 (0x03e8) -> 000003e803e8
        let red = BulbColor::from_hsv(0.0, 1.0, 1.0);
        assert_eq!(red.to_hex12(), "000003e803e8");

        let parsed = BulbColor::parse("000003e803e8").expect("parse hex12");
        assert_eq!(parsed.h, 0.0);
        assert!((parsed.s - 1.0).abs() < 0.01);
        assert!((parsed.v - 1.0).abs() < 0.01);

        // Cyan: H=180 (0x00b4), S=500 (0x01f4), V=800 (0x0320)
        let cyan = BulbColor::from_hsv(180.0, 0.5, 0.8);
        assert_eq!(cyan.to_hex12(), "00b401f40320");
        let parsed_cyan = BulbColor::parse("00b401f40320").expect("parse cyan");
        assert_eq!(parsed_cyan.h, 180.0);
        assert!((parsed_cyan.s - 0.5).abs() < 0.01);
        assert!((parsed_cyan.v - 0.8).abs() < 0.01);
    }

    #[test]
    fn hex14_encoding_and_parsing() {
        let green = BulbColor::from_hsv(120.0, 1.0, 1.0);
        let h14 = green.to_hex14();
        assert_eq!(h14, "00ff000078ffff");

        let parsed = BulbColor::parse(&h14).expect("parse hex14");
        assert_eq!(parsed.h, 120.0);
        assert!((parsed.s - 1.0).abs() < 0.01);
        assert!((parsed.v - 1.0).abs() < 0.01);
    }

    #[test]
    fn json_color_format_roundtrip() {
        let amber = BulbColor::from_hsv(35.0, 0.9, 0.85);
        let json_str = amber.to_json_str();
        let parsed = BulbColor::parse(&json_str).expect("parse json");
        assert_eq!(parsed.h, 35.0);
        assert!((parsed.s - 0.9).abs() < 0.01);
        assert!((parsed.v - 0.85).abs() < 0.01);
    }

    #[test]
    fn music_data_format_21_chars() {
        // Test DP 27 formatting
        let color = BulbColor::from_hsv(120.0, 1.0, 1.0); // H=120 (0x0078), S=1000 (0x03e8), V=1000 (0x03e8)
        let data = format_music_data(color, 1000, 500, false);
        // mode '0' + 0078 + 03e8 + 03e8 + 03e8 (bright=1000) + 01f4 (temp=500)
        assert_eq!(data.len(), 21);
        assert_eq!(data, "0007803e803e803e801f4");

        let gradient_data = format_music_data(color, 800, 0, true);
        assert_eq!(gradient_data.len(), 21);
        assert_eq!(gradient_data, "1007803e803e803200000");
    }

    #[test]
    fn bulb_state_dps_parsing() {
        let mut dps = serde_json::Map::new();
        dps.insert("20".to_string(), Value::Bool(true));
        dps.insert("21".to_string(), Value::String("colour".to_string()));
        dps.insert("22".to_string(), Value::from(850u64));
        dps.insert("23".to_string(), Value::from(400u64));
        dps.insert(
            "24".to_string(),
            Value::String("000003e803e8".to_string()),
        );

        let state = BulbState::from_dps(&dps);
        assert!(state.power);
        assert_eq!(state.mode, BulbMode::Colour);
        assert_eq!(state.brightness, 850);
        assert_eq!(state.color_temp, 400);
        assert_eq!(state.color.h, 0.0);
    }

    #[test]
    fn robust_dps_string_and_float_parsing() {
        let mut dps = serde_json::Map::new();
        dps.insert("20".to_string(), Value::String("true".to_string()));
        dps.insert("21".to_string(), Value::String("music".to_string()));
        dps.insert("22".to_string(), Value::String("750".to_string()));
        dps.insert("23".to_string(), serde_json::json!(450.0));

        let state = BulbState::from_dps(&dps);
        assert!(state.power);
        assert_eq!(state.mode, BulbMode::Music);
        assert_eq!(state.brightness, 750);
        assert_eq!(state.color_temp, 450);
    }

    #[test]
    fn hsv_percentage_input_normalization() {
        // Saturation and value passed as 0-100 percentages
        let color = BulbColor::from_hsv(180.0, 80.0, 90.0);
        assert_eq!(color.h, 180.0);
        assert!((color.s - 0.8).abs() < 0.001);
        assert!((color.v - 0.9).abs() < 0.001);
    }
}
