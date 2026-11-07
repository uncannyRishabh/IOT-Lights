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
    #[serde(default)]
    pub sync_excluded: bool,
}

fn default_version() -> String {
    "3.5".to_string()
}

pub fn resolve_fleet_config_path() -> std::path::PathBuf {
    let bulbs = Path::new("bulbs.json");
    if bulbs.exists() {
        bulbs.to_path_buf()
    } else {
        std::path::PathBuf::from("devices.json")
    }
}

pub fn load_devices_json(path: &Path) -> Vec<SavedDeviceRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_devices_json(path: &Path, records: &[SavedDeviceRecord]) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(records)?;
    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Resolves MAC address from the OS ARP cache for a local IPv4 address without root privileges.
pub fn lookup_arp_mac(ip: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // 1. Direct /proc/net/arp read (zero subprocess spawn!)
        if let Ok(arp_table) = std::fs::read_to_string("/proc/net/arp") {
            for line in arp_table.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 && parts[0] == ip {
                    let mac = parts[3].to_lowercase();
                    if mac != "00:00:00:00:00:00" && mac.contains(':') {
                        return Some(mac);
                    }
                }
            }
        }
        // Fallback: ip neigh show <ip>
        if let Ok(out) = Command::new("ip").args(["neigh", "show", ip]).output() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for token in stdout.split_whitespace() {
                if token.chars().filter(|&c| c == ':').count() == 5 {
                    return Some(token.to_lowercase());
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(out) = Command::new("arp").args(["-a", ip]).output() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for token in stdout.split_whitespace() {
                if token.chars().filter(|&c| c == '-').count() == 5 && token.len() >= 17 {
                    let normalized = token.replace('-', ":").to_lowercase();
                    return Some(normalized);
                }
            }
        }
    }

    // macOS / BSD / fallback
    if let Ok(out) = Command::new("arp").args(["-n", ip]).output() {
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
