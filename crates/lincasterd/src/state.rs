use std::path::Path;

use anyhow::Result;
use lincaster_proto::{BusState, Config, PersistedState};
use tracing::{debug, info};

/// Load persisted state from disk. Falls back to defaults from config if file is missing.
pub fn load_state(path: &Path, config: &Config) -> Vec<BusState> {
    match load_state_file(path) {
        Ok(persisted) => {
            info!("Loaded persisted state from {}", path.display());
            // Merge persisted state with config: use persisted values where available,
            // add defaults for any new busses in config.
            merge_state(persisted, config)
        }
        Err(e) => {
            debug!(
                "No persisted state at {} ({}); using defaults",
                path.display(),
                e
            );
            config.busses.iter().map(BusState::from_config).collect()
        }
    }
}

fn load_state_file(path: &Path) -> Result<PersistedState> {
    let content = std::fs::read_to_string(path)?;
    let state: PersistedState = serde_json::from_str(&content)?;
    Ok(state)
}

/// Merge persisted state with current config. Preserves gain/mute/solo for existing busses,
/// adds defaults for new busses, drops state for removed busses.
fn merge_state(persisted: PersistedState, config: &Config) -> Vec<BusState> {
    config
        .busses
        .iter()
        .map(|bus_cfg| {
            persisted
                .bus_states
                .iter()
                .find(|s| s.bus_id == bus_cfg.bus_id)
                .cloned()
                .unwrap_or_else(|| BusState::from_config(bus_cfg))
        })
        .collect()
}

/// Save current bus states to disk.
pub fn save_state(path: &Path, states: &[BusState]) -> Result<()> {
    let persisted = PersistedState {
        version: 1,
        bus_states: states.to_vec(),
    };

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&persisted)?;
    // Write atomically: write to temp file, then rename
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, path)?;

    info!("Saved state to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lincaster_proto::config::BusDirection;
    use lincaster_proto::BusConfig;

    fn test_config() -> Config {
        Config {
            version: 1,
            device: lincaster_proto::DeviceConfig {
                usb_vendor_id: lincaster_proto::RODE_VENDOR_ID,
                usb_product_ids: vec![],
                alsa_card_id_hint: "test".to_string(),
                require_multitrack: false,
            },
            busses: vec![
                BusConfig {
                    bus_id: "system".into(),
                    display_name: "System".into(),
                    direction: BusDirection::Playback,
                    channels: 2,
                    default_gain: 1.0,
                    solo_safe: true,
                },
                BusConfig {
                    bus_id: "chat".into(),
                    display_name: "Chat".into(),
                    direction: BusDirection::Playback,
                    channels: 2,
                    default_gain: 1.0,
                    solo_safe: false,
                },
            ],
            routes: vec![],
            app_rules: vec![],
            latency_mode: Default::default(),
        }
    }

    #[test]
    fn test_load_missing_file() {
        let states = load_state(Path::new("/nonexistent/state.json"), &test_config());
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].bus_id, "system");
        assert_eq!(states[0].gain, 1.0);
        assert!(!states[0].mute);
    }

    #[test]
    fn test_merge_preserves_existing() {
        let persisted = PersistedState {
            version: 1,
            bus_states: vec![BusState {
                bus_id: "system".into(),
                gain: 0.5,
                mute: true,
                solo: false,
                solo_safe: true,
            }],
        };
        let config = test_config();
        let merged = merge_state(persisted, &config);

        assert_eq!(merged.len(), 2);
        // system: should keep persisted values
        assert_eq!(merged[0].gain, 0.5);
        assert!(merged[0].mute);
        // chat: should use default
        assert_eq!(merged[1].gain, 1.0);
        assert!(!merged[1].mute);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("lincaster_test_state");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.json");

        let states = vec![
            BusState {
                bus_id: "system".into(),
                gain: 0.75,
                mute: false,
                solo: true,
                solo_safe: true,
            },
            BusState {
                bus_id: "chat".into(),
                gain: 0.5,
                mute: true,
                solo: false,
                solo_safe: false,
            },
        ];

        save_state(&path, &states).unwrap();

        let loaded = load_state_file(&path).unwrap();
        assert_eq!(loaded.bus_states.len(), 2);
        assert_eq!(loaded.bus_states[0].gain, 0.75);
        assert!(loaded.bus_states[0].solo);
        assert!(loaded.bus_states[1].mute);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
