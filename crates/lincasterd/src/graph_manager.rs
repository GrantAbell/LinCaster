use tracing::info;

#[allow(dead_code)]
const NODE_PREFIX: &str = "lincaster";

/// Determine the hardware channel pair for a given bus ID in multitrack mode.
/// Returns (left_ch, right_ch) indices into the hardware's playback channels.
#[allow(dead_code)]
pub fn bus_to_hw_channels(bus_id: &str) -> Option<(u32, u32)> {
    match bus_id {
        "system" => Some((0, 1)),
        "game" => Some((2, 3)),
        "music" => Some((4, 5)),
        "a" => Some((6, 7)),
        "b" => Some((8, 9)),
        // Chat uses a separate playback device, not multitrack channels
        "chat" => None,
        _ => None,
    }
}

/// Log the multitrack routing table (informational).
#[allow(dead_code)]
pub fn log_multitrack_routing() {
    info!("Multitrack routing table:");
    for bus_id in &["system", "game", "music", "a", "b", "chat"] {
        match bus_to_hw_channels(bus_id) {
            Some((l, r)) => info!("  {} -> hw channels [{}, {}]", bus_id, l, r),
            None => info!("  {} -> separate device", bus_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bus_to_hw_channels() {
        assert_eq!(bus_to_hw_channels("system"), Some((0, 1)));
        assert_eq!(bus_to_hw_channels("game"), Some((2, 3)));
        assert_eq!(bus_to_hw_channels("music"), Some((4, 5)));
        assert_eq!(bus_to_hw_channels("a"), Some((6, 7)));
        assert_eq!(bus_to_hw_channels("b"), Some((8, 9)));
        assert_eq!(bus_to_hw_channels("chat"), None);
        assert_eq!(bus_to_hw_channels("unknown"), None);
    }

    #[test]
    fn test_node_prefix() {
        let name = format!("{}.system", NODE_PREFIX);
        assert_eq!(name, "lincaster.system");
    }
}
