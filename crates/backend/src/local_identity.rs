//! Local Bulb Identity, ARP MAC Resolution, and devices.json Persistence.

use std::path::Path;
use std::process::Command;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDeviceRecord {
    pub name: Option<String>,
    pub id: String,
    pub key: String,
    pub ip: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub bulb_type: Option<tuya_bulb::BulbType>,
    #[serde(default)]
    pub mac: Option<String>,
}

fn default_version() -> String {
    "3.5".to_string()
}

pub fn load_devices_json(path: &Path) -> Vec<SavedDeviceRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_devices_json(path: &Path, records: &[SavedDeviceRecord]) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(records)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Resolves MAC address from the OS ARP cache for a local IPv4 address without root privileges.
pub fn lookup_arp_mac(ip: &str) -> Option<String> {
    let out = Command::new("arp").args(["-n", ip]).output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for token in stdout.split_whitespace() {
        if token.chars().filter(|&c| c == ':').count() == 5 && token.len() >= 11 {
            // Normalize macOS single-digit hex octets (e.g. 38:2c:e5:86:83:7b)
            let normalized = token
                .split(':')
                .map(|part| format!("{:0>2}", part.to_lowercase()))
                .collect::<Vec<_>>()
                .join(":");
            return Some(normalized);
        }
    }
    None
}

/// Infers a friendly hardware name from MAC OUI and Tuya protocol version when no user alias exists yet.
pub fn infer_friendly_bulb_name(
    id: &str,
    ip: &str,
    mac: Option<&str>,
    saved: &[SavedDeviceRecord],
) -> String {
    if let Some(rec) = saved.iter().find(|r| r.id == id || r.ip == ip) {
        if let Some(ref name) = rec.name {
            if !name.trim().is_empty() {
                return name.clone();
            }
        }
    }

    let suffix = &id[id.len().saturating_sub(4)..].to_uppercase();
    if let Some(m) = mac {
        let prefix = m.get(0..8).unwrap_or("").to_lowercase();
        match prefix.as_str() {
            "38:2c:e5" | "10:d5:61" => return format!("Wipro RGBCW Bulb ({suffix})"),
            "e4:ae:e4" | "50:8a:06" => return format!("Smart Bulb 14W ({suffix})"),
            "d8:f1:5b" | "cc:50:e3" | "84:f3:eb" => return format!("Tuya Smart Light ({suffix})"),
            "68:57:2d" => return format!("Syska Smart Bulb ({suffix})"),
            _ => {}
        }
    }

    format!("Smart Bulb • {suffix}")
}
