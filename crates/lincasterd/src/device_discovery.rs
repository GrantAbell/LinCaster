use anyhow::{Context, Result};
use lincaster_proto::DeviceConfig;
use lincaster_proto::DeviceIdentity;
use tracing::{debug, info, warn};

/// Probe for a connected RØDECaster device using rusb (libusb).
/// Returns `Ok(Some(identity))` if found, `Ok(None)` if not connected.
pub fn probe_device(config: &DeviceConfig) -> Result<Option<DeviceIdentity>> {
    info!(
        "Probing for RØDECaster device (vendor_id=0x{:04X})",
        config.usb_vendor_id
    );

    let vendor_id = config.usb_vendor_id;
    let product_ids = &config.usb_product_ids;

    // Try USB enumeration first
    match probe_via_usb(vendor_id, product_ids) {
        Ok(Some(identity)) => {
            info!(
                "Found device via USB: vendor=0x{:04X} product=0x{:04X} playback_ch={} capture_ch={}",
                identity.usb_vendor_id, identity.usb_product_id,
                identity.playback_channels, identity.capture_channels
            );
            return Ok(Some(identity));
        }
        Ok(None) => {
            debug!("No device found via USB enumeration");
        }
        Err(e) => {
            warn!("USB probe failed (libusb may be unavailable): {}", e);
        }
    }

    // Fallback: probe via ALSA card names in /proc/asound
    match probe_via_alsa_proc(&config.alsa_card_id_hint) {
        Ok(Some(identity)) => {
            info!(
                "Found device via ALSA: card='{}' playback_ch={} capture_ch={}",
                identity.alsa_card_name.as_deref().unwrap_or("?"),
                identity.playback_channels,
                identity.capture_channels
            );
            return Ok(Some(identity));
        }
        Ok(None) => {
            debug!("No device found via ALSA /proc scan");
        }
        Err(e) => {
            warn!("ALSA probe failed: {}", e);
        }
    }

    Ok(None)
}

/// Probe for device using rusb (libusb wrapper).
fn probe_via_usb(vendor_id: u16, product_ids: &[u16]) -> Result<Option<DeviceIdentity>> {
    let devices = rusb::devices().context("Failed to enumerate USB devices")?;

    for device in devices.iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };

        if desc.vendor_id() != vendor_id {
            continue;
        }

        // If product IDs are specified, match against them. Otherwise accept any RØDE device.
        if !product_ids.is_empty() && !product_ids.contains(&desc.product_id()) {
            debug!(
                "Skipping RØDE device with unrecognized product ID: 0x{:04X}",
                desc.product_id()
            );
            continue;
        }

        debug!(
            "Found RØDE USB device: vendor=0x{:04X} product=0x{:04X}",
            desc.vendor_id(),
            desc.product_id()
        );

        // Try to get serial number
        let serial = device.open().ok().and_then(|handle| {
            handle
                .read_string_descriptor_ascii(desc.serial_number_string_index()?)
                .ok()
        });

        // Count audio streaming endpoints to estimate channel counts
        let (playback_ch, capture_ch) = count_audio_channels(&device, &desc);

        // Try to find corresponding ALSA card
        let alsa_info = find_alsa_card_for_usb(vendor_id, desc.product_id());

        return Ok(Some(DeviceIdentity {
            usb_vendor_id: desc.vendor_id(),
            usb_product_id: desc.product_id(),
            serial,
            alsa_card_name: alsa_info.as_ref().map(|(name, _)| name.clone()),
            alsa_card_index: alsa_info.map(|(_, idx)| idx),
            playback_channels: playback_ch,
            capture_channels: capture_ch,
        }));
    }

    Ok(None)
}

/// Count audio channels by inspecting USB audio class interface descriptors.
fn count_audio_channels(
    device: &rusb::Device<rusb::GlobalContext>,
    desc: &rusb::DeviceDescriptor,
) -> (u32, u32) {
    let mut playback_ch: u32 = 0;
    let mut capture_ch: u32 = 0;

    let config = match device.active_config_descriptor() {
        Ok(c) => c,
        Err(_) => return (playback_ch, capture_ch),
    };

    for interface in config.interfaces() {
        for iface_desc in interface.descriptors() {
            // USB Audio Class: class 1 (audio), subclass 2 (streaming)
            if iface_desc.class_code() != 1 || iface_desc.sub_class_code() != 2 {
                continue;
            }
            for endpoint in iface_desc.endpoint_descriptors() {
                let max_packet = endpoint.max_packet_size();
                // Audio streaming endpoints use isochronous transfer
                if endpoint.transfer_type() == rusb::TransferType::Isochronous {
                    // Estimate channels from endpoint: at 48kHz/16bit stereo, packet ~= 192 bytes
                    // For multichannel, larger packets indicate more channels.
                    // This is a rough heuristic; actual channel count comes from ALSA UCM.
                    let _max_packet = max_packet;

                    match endpoint.direction() {
                        rusb::Direction::Out => {
                            // Host to device = playback
                            let num_endpoints_out = iface_desc
                                .endpoint_descriptors()
                                .filter(|e| e.direction() == rusb::Direction::Out)
                                .count();
                            debug!(
                                "Found audio playback interface {}, alt {}, {} OUT endpoints, max_packet={}",
                                interface.number(), iface_desc.setting_number(), num_endpoints_out, max_packet
                            );
                        }
                        rusb::Direction::In => {
                            // Device to host = capture
                            let num_endpoints_in = iface_desc
                                .endpoint_descriptors()
                                .filter(|e| e.direction() == rusb::Direction::In)
                                .count();
                            debug!(
                                "Found audio capture interface {}, alt {}, {} IN endpoints, max_packet={}",
                                interface.number(), iface_desc.setting_number(), num_endpoints_in, max_packet
                            );
                        }
                    }
                }
            }
        }
    }

    // USB descriptor channel counting is unreliable for multi-config devices.
    // Fall back to ALSA for accurate channel counts.
    if let Some((_name, card_idx)) = find_alsa_card_for_usb(desc.vendor_id(), desc.product_id()) {
        let (p, c) = get_alsa_channel_counts(card_idx);
        if p > 0 {
            playback_ch = p;
        }
        if c > 0 {
            capture_ch = c;
        }
    }

    (playback_ch, capture_ch)
}

/// Find an ALSA card that matches the given USB vendor/product ID.
fn find_alsa_card_for_usb(vendor_id: u16, product_id: u16) -> Option<(String, u32)> {
    // Scan /proc/asound/cards for card names, then check USB IDs
    let cards_content = std::fs::read_to_string("/proc/asound/cards").ok()?;

    for line in cards_content.lines() {
        let line = line.trim();
        // Lines look like: " 0 [System         ]: HDA-Intel - HD Audio ..."
        // or               " 1 [RODECasterDuo  ]: USB-Audio - RODECaster Duo"
        if !line.contains("USB-Audio") && !line.contains("usb-audio") {
            continue;
        }
        // Parse card index
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let card_idx: u32 = match parts[0].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };

        // Check if this card's USB IDs match
        let id_path = format!("/proc/asound/card{}/usbid", card_idx);
        if let Ok(usb_id_str) = std::fs::read_to_string(&id_path) {
            let usb_id_str = usb_id_str.trim();
            let expected = format!("{:04x}:{:04x}", vendor_id, product_id);
            if usb_id_str.to_lowercase() == expected {
                // Extract card name from the bracket
                let card_name = parts
                    .iter()
                    .find(|p| p.starts_with('['))
                    .map(|p| p.trim_matches(|c| c == '[' || c == ']'))
                    .unwrap_or("Unknown")
                    .to_string();
                return Some((card_name, card_idx));
            }
        }
    }

    None
}

/// Probe for the device using ALSA /proc filesystem (no libusb required).
fn probe_via_alsa_proc(card_hint: &str) -> Result<Option<DeviceIdentity>> {
    let cards_content = std::fs::read_to_string("/proc/asound/cards")
        .context("Failed to read /proc/asound/cards")?;

    let hint_lower = card_hint.to_lowercase();

    for line in cards_content.lines() {
        let line_lower = line.to_lowercase();
        if !line_lower.contains(&hint_lower) {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let card_idx: u32 = match parts[0].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };

        let card_name = parts
            .iter()
            .find(|p| p.starts_with('['))
            .map(|p| p.trim_matches(|c| c == '[' || c == ']').to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        // Read USB ID if available
        let (vendor_id, product_id) = read_usb_ids(card_idx);
        let (playback_ch, capture_ch) = get_alsa_channel_counts(card_idx);

        return Ok(Some(DeviceIdentity {
            usb_vendor_id: vendor_id,
            usb_product_id: product_id,
            serial: None,
            alsa_card_name: Some(card_name),
            alsa_card_index: Some(card_idx),
            playback_channels: playback_ch,
            capture_channels: capture_ch,
        }));
    }

    Ok(None)
}

/// Read USB vendor/product IDs from /proc/asound/cardN/usbid.
fn read_usb_ids(card_idx: u32) -> (u16, u16) {
    let id_path = format!("/proc/asound/card{}/usbid", card_idx);
    let content = match std::fs::read_to_string(&id_path) {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };
    let content = content.trim();
    let parts: Vec<&str> = content.split(':').collect();
    if parts.len() != 2 {
        return (0, 0);
    }
    let vendor = u16::from_str_radix(parts[0], 16).unwrap_or(0);
    let product = u16::from_str_radix(parts[1], 16).unwrap_or(0);
    (vendor, product)
}

/// Get playback and capture channel counts for an ALSA card by reading /proc/asound/cardN/streamN.
/// Scans all stream files (stream0, stream1, ...) and returns the maximum channel counts found,
/// since multi-profile USB audio devices expose different profiles on different streams
/// (e.g. stream0 = stereo chat, stream1 = multitrack).
fn get_alsa_channel_counts(card_idx: u32) -> (u32, u32) {
    let mut playback_ch: u32 = 0;
    let mut capture_ch: u32 = 0;

    for stream_idx in 0..8 {
        let stream_path = format!("/proc/asound/card{}/stream{}", card_idx, stream_idx);
        let content = match std::fs::read_to_string(&stream_path) {
            Ok(c) => c,
            Err(_) => break, // No more stream files
        };

        let mut in_playback = false;
        let mut in_capture = false;

        for line in content.lines() {
            let line = line.trim();
            if line.contains("Playback:") {
                in_playback = true;
                in_capture = false;
            } else if line.contains("Capture:") {
                in_capture = true;
                in_playback = false;
            }

            // Look for "Channels: N" lines
            if line.starts_with("Channels:") {
                if let Some(ch_str) = line.strip_prefix("Channels:") {
                    if let Ok(ch) = ch_str.trim().parse::<u32>() {
                        if in_playback && ch > playback_ch {
                            playback_ch = ch;
                        }
                        if in_capture && ch > capture_ch {
                            capture_ch = ch;
                        }
                    }
                }
            }
        }
    }

    debug!(
        "ALSA card {} channel counts: playback={}, capture={}",
        card_idx, playback_ch, capture_ch
    );
    (playback_ch, capture_ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_usb_ids_missing_file() {
        // Card 99 shouldn't exist
        let (v, p) = read_usb_ids(99);
        assert_eq!(v, 0);
        assert_eq!(p, 0);
    }

    #[test]
    fn test_get_alsa_channel_counts_missing_card() {
        let (p, c) = get_alsa_channel_counts(99);
        assert_eq!(p, 0);
        assert_eq!(c, 0);
    }
}
