//! Bulb Fleet Management, Connection Pooling, and Synchronized Broadcasting.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use tuya_bulb::{BulbColor, BulbDevice, BulbType, ColorFormat};
use rustuya_tokio::{ConnectLimiter, Device, Version};
use serde::{Deserialize, Serialize};

/// Configuration for adding a bulb to the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulbConfig {
    pub id: String,
    pub name: Option<String>,
    pub local_key: String,
    pub address: String,
    pub port: Option<u16>,
    pub version: Option<String>,
    pub bulb_type: Option<BulbType>,
    pub color_format: Option<ColorFormat>,
    pub mac: Option<String>,
}

/// Information about a bulb in the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetBulbInfo {
    pub id: String,
    pub name: Option<String>,
    pub address: String,
    pub is_connected: bool,
    pub power: bool,
    pub bulb_type: BulbType,
    pub mac: Option<String>,
    #[serde(default)]
    pub sync_excluded: bool,
}

/// A fleet connection pool managing synchronized control of multiple Tuya smart bulbs.
#[derive(Clone)]
pub struct BulbFleet {
    bulbs: Arc<RwLock<HashMap<String, Arc<BulbDevice>>>>,
    bulb_addrs: Arc<RwLock<HashMap<String, String>>>,
    bulb_types: Arc<RwLock<HashMap<String, BulbType>>>,
    bulb_names: Arc<RwLock<HashMap<String, Option<String>>>>,
    bulb_macs: Arc<RwLock<HashMap<String, Option<String>>>>,
    sync_excluded: Arc<RwLock<HashMap<String, bool>>>,
    limiter: ConnectLimiter,
}

impl BulbFleet {
    /// Create a new BulbFleet with an establishment concurrency limiter.
    pub fn new(max_concurrent_connections: usize) -> Self {
        Self {
            bulbs: Arc::new(RwLock::new(HashMap::new())),
            bulb_addrs: Arc::new(RwLock::new(HashMap::new())),
            bulb_types: Arc::new(RwLock::new(HashMap::new())),
            bulb_names: Arc::new(RwLock::new(HashMap::new())),
            bulb_macs: Arc::new(RwLock::new(HashMap::new())),
            sync_excluded: Arc::new(RwLock::new(HashMap::new())),
            limiter: ConnectLimiter::new(max_concurrent_connections),
        }
    }

    /// Get a specific bulb device by ID.
    pub async fn get_bulb(&self, id: &str) -> Option<Arc<BulbDevice>> {
        let guard = self.bulbs.read().await;
        guard.get(id).cloned()
    }

    /// Add and connect a bulb to the fleet.
    pub async fn add_bulb(&self, cfg: BulbConfig) -> anyhow::Result<Arc<BulbDevice>> {
        let key_trimmed = cfg.local_key.trim();
        if key_trimmed.len() != 16 {
            anyhow::bail!(
                "Tuya local_key must be exactly 16 ASCII characters, got {} chars",
                key_trimmed.len()
            );
        }

        let version = match cfg.version.as_deref() {
            Some("3.1") => Version::V3_1,
            Some("3.2") => Version::V3_2,
            Some("3.4") => Version::V3_4,
            Some("3.5") => Version::V3_5,
            _ => Version::V3_3,
        };

        let port = cfg.port.unwrap_or(6668);
        let btype = cfg.bulb_type.unwrap_or(BulbType::TypeB);
        let cformat = cfg.color_format.unwrap_or(ColorFormat::Hex12);

        let dev = Device::builder(&cfg.id, key_trimmed.as_bytes())
            .address(&cfg.address)
            .port(port)
            .version(version)
            .connect_limiter(&self.limiter)
            .auto_reconnect(true)
            .heartbeat(Some(Duration::from_secs(7)))
            .connect()?;

        let bulb = Arc::new(BulbDevice::new(dev, btype, cformat));

        self.bulbs.write().await.insert(cfg.id.clone(), Arc::clone(&bulb));
        self.bulb_addrs.write().await.insert(cfg.id.clone(), cfg.address);
        self.bulb_types.write().await.insert(cfg.id.clone(), btype);
        self.bulb_names.write().await.insert(cfg.id.clone(), cfg.name);
        self.bulb_macs.write().await.insert(cfg.id.clone(), cfg.mac);

        Ok(bulb)
    }

    /// Rename a bulb in the fleet.
    pub async fn rename_bulb(&self, id: &str, new_name: &str) -> bool {
        let mut names = self.bulb_names.write().await;
        if names.contains_key(id) {
            names.insert(id.to_string(), Some(new_name.to_string()));
            true
        } else {
            false
        }
    }

    /// Remove a bulb from the fleet.
    pub async fn remove_bulb(&self, id: &str) -> Option<Arc<BulbDevice>> {
        self.bulb_addrs.write().await.remove(id);
        self.bulb_types.write().await.remove(id);
        self.bulb_names.write().await.remove(id);
        self.bulb_macs.write().await.remove(id);
        self.sync_excluded.write().await.remove(id);
        let bulb = self.bulbs.write().await.remove(id);
        if let Some(ref b) = bulb {
            b.close().await;
        }
        bulb
    }

    /// Check whether a bulb is excluded from synchronization.
    pub async fn is_sync_excluded(&self, id: &str) -> bool {
        let guard = self.sync_excluded.read().await;
        guard.get(id).copied().unwrap_or(false)
    }

    /// Set whether a bulb is excluded from synchronization.
    pub async fn set_sync_excluded(&self, id: &str, excluded: bool) {
        self.sync_excluded.write().await.insert(id.to_string(), excluded);
        if excluded {
            if let Some(bulb) = self.get_bulb(id).await {
                if bulb.is_connected() {
                    let _ = bulb.exit_music_mode().await;
                }
            }
        }
    }

    /// Toggle sync exclusion for a bulb. Returns the new state (true if excluded).
    pub async fn toggle_sync_excluded(&self, id: &str) -> bool {
        let new_state = {
            let mut guard = self.sync_excluded.write().await;
            let current = guard.get(id).copied().unwrap_or(false);
            let next = !current;
            guard.insert(id.to_string(), next);
            next
        };
        if new_state {
            if let Some(bulb) = self.get_bulb(id).await {
                if bulb.is_connected() {
                    let _ = bulb.exit_music_mode().await;
                }
            }
        }
        new_state
    }

    /// Get list of bulbs currently in the fleet and their connection states.
    pub async fn list_bulbs(&self) -> Vec<FleetBulbInfo> {
        let bulbs = self.bulbs.read().await;
        let addrs = self.bulb_addrs.read().await;
        let types = self.bulb_types.read().await;
        let names = self.bulb_names.read().await;
        let macs = self.bulb_macs.read().await;
        let excluded = self.sync_excluded.read().await;

        let mut list = Vec::new();
        for (id, bulb) in bulbs.iter() {
            let address = addrs.get(id).cloned().unwrap_or_default();
            let bulb_type = types.get(id).copied().unwrap_or(BulbType::TypeB);
            let name = names.get(id).cloned().flatten();
            let mac = macs.get(id).cloned().flatten();
            let sync_excluded = excluded.get(id).copied().unwrap_or(false);
            list.push(FleetBulbInfo {
                id: id.clone(),
                name,
                address,
                is_connected: bulb.is_connected(),
                power: bulb.is_power_on(),
                bulb_type,
                mac,
                sync_excluded,
            });
        }
        list
    }

    /// Broadcast a real-time music sync frame concurrently across all connected, non-excluded bulbs in the fleet.
    pub async fn broadcast_music_frame(
        &self,
        color: BulbColor,
        brightness: u32,
        temp: u32,
        gradient: bool,
    ) {
        let (bulbs, excluded_set): (Vec<(String, Arc<BulbDevice>)>, HashMap<String, bool>) = {
            let guard = self.bulbs.read().await;
            let excluded = self.sync_excluded.read().await;
            (
                guard.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
                excluded.clone(),
            )
        };

        let futures = bulbs.into_iter().filter_map(|(id, bulb)| {
            if excluded_set.get(&id).copied().unwrap_or(false) {
                None
            } else {
                let color = color;
                Some(async move {
                    if bulb.is_connected() {
                        let _ = bulb.stream_music_frame(color, brightness, temp, gradient).await;
                    }
                })
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Set all bulbs to a specific color.
    pub async fn broadcast_color(&self, color: BulbColor) {
        let bulbs: Vec<Arc<BulbDevice>> = {
            let guard = self.bulbs.read().await;
            guard.values().cloned().collect()
        };

        let futures = bulbs.into_iter().map(|bulb| {
            async move {
                if bulb.is_connected() {
                    let _ = bulb.set_color(color).await;
                }
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Set all bulbs to white mode with brightness and temperature.
    pub async fn broadcast_white(&self, brightness: u32, temp: u32) {
        let bulbs: Vec<Arc<BulbDevice>> = {
            let guard = self.bulbs.read().await;
            guard.values().cloned().collect()
        };

        let futures = bulbs.into_iter().map(|bulb| {
            async move {
                if bulb.is_connected() {
                    let _ = bulb.set_white(brightness, temp).await;
                }
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Turn all bulbs in the fleet on.
    pub async fn broadcast_turn_on(&self) {
        let bulbs: Vec<Arc<BulbDevice>> = {
            let guard = self.bulbs.read().await;
            guard.values().cloned().collect()
        };

        let futures = bulbs.into_iter().map(|bulb| {
            async move {
                let _ = bulb.turn_on().await;
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Turn all bulbs in the fleet off.
    pub async fn broadcast_turn_off(&self) {
        let bulbs: Vec<Arc<BulbDevice>> = {
            let guard = self.bulbs.read().await;
            guard.values().cloned().collect()
        };

        let futures = bulbs.into_iter().map(|bulb| {
            async move {
                let _ = bulb.turn_off().await;
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Put all bulbs in the fleet into music synchronization mode (excluding bulbs marked sync_excluded).
    pub async fn broadcast_enter_music_mode(&self) {
        let (bulbs, excluded_set): (Vec<(String, Arc<BulbDevice>)>, HashMap<String, bool>) = {
            let guard = self.bulbs.read().await;
            let excluded = self.sync_excluded.read().await;
            (
                guard.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
                excluded.clone(),
            )
        };

        let futures = bulbs.into_iter().filter_map(|(id, bulb)| {
            if excluded_set.get(&id).copied().unwrap_or(false) {
                None
            } else {
                Some(async move {
                    if bulb.is_connected() {
                        let _ = bulb.enter_music_mode().await;
                    }
                })
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Restore all bulbs in the fleet from music synchronization mode.
    pub async fn broadcast_exit_music_mode(&self) {
        let bulbs: Vec<Arc<BulbDevice>> = {
            let guard = self.bulbs.read().await;
            guard.values().cloned().collect()
        };

        let futures = bulbs.into_iter().map(|bulb| {
            async move {
                if bulb.is_connected() {
                    let _ = bulb.exit_music_mode().await;
                }
            }
        });

        futures::future::join_all(futures).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bulb_fleet_sync_exclusion() {
        let fleet = BulbFleet::new(4);
        assert!(!fleet.is_sync_excluded("bulb_1").await);

        fleet.set_sync_excluded("bulb_1", true).await;
        assert!(fleet.is_sync_excluded("bulb_1").await);

        let toggled = fleet.toggle_sync_excluded("bulb_1").await;
        assert!(!toggled);
        assert!(!fleet.is_sync_excluded("bulb_1").await);

        let toggled_again = fleet.toggle_sync_excluded("bulb_1").await;
        assert!(toggled_again);
        assert!(fleet.is_sync_excluded("bulb_1").await);
    }
}
