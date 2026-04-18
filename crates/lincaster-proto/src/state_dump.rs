//! Parser for the RØDECaster device state dump (Type 0x04).
//!
//! After the handshake, the device sends a ~186KB binary blob containing
//! the entire device state tree. This module reassembles multi-packet
//! HID reports into a contiguous buffer and then walks the tree to extract
//! pad configurations.

use std::collections::HashMap;

use crate::{
    EchoEffect, EffectConfig, FxInputSource, LatchMode, MegaphoneEffect, MixerMode, MixerPadConfig,
    PadAssignment, PadColor, PitchShiftEffect, PlayMode, ReplayMode, ReverbEffect, ReverbModel,
    RobotEffect, SoundConfig, SoundPadConfig, TriggerPadConfig, TriggerType, VoiceDisguiseEffect,
};

// ── Multi-packet reassembly ─────────────────────────────────────────

/// Reassemble multi-packet Type 0x04 HID reports into a single payload.
///
/// The first report has: `04 LL LL LL LL [payload...]`
/// Continuation reports: `04 [payload...]`
///
/// Returns the combined payload (without the type/length header).
pub fn reassemble_state_dump(reports: &[Vec<u8>]) -> Option<Vec<u8>> {
    if reports.is_empty() {
        return None;
    }

    let first = &reports[0];
    if first.len() < 5 || first[0] != 0x04 {
        return None;
    }

    let payload_len = u32::from_le_bytes([first[1], first[2], first[3], first[4]]) as usize;
    let mut payload = Vec::with_capacity(payload_len);

    // First report: skip type (1) + length (4) = 5 bytes header
    payload.extend_from_slice(&first[5..]);

    // Continuation reports: skip type byte (1)
    for report in &reports[1..] {
        if report.is_empty() {
            continue;
        }
        if report[0] != 0x04 {
            // Not a continuation of our state dump
            break;
        }
        payload.extend_from_slice(&report[1..]);
    }

    // Trim trailing zeros (each HID report is padded)
    // The actual payload_len from the header tells us how much is real
    if payload.len() > payload_len {
        payload.truncate(payload_len);
    }

    Some(payload)
}

// ── State tree parser ───────────────────────────────────────────────

/// Parsed property value from the state tree.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum PropValue {
    Bool(bool),
    U32(u32),
    F64(f64),
    String(String),
    Unknown,
}

/// A single property: name + value.
struct Property {
    name: String,
    value: PropValue,
}

/// A section in the state tree with its properties and children.
struct Section {
    name: String,
    properties: Vec<Property>,
    children: Vec<Section>,
}

/// Read a null-terminated string from `data` starting at `pos`.
/// Returns the string and the new position (past the null byte).
fn read_cstring(data: &[u8], pos: usize) -> Option<(String, usize)> {
    let start = pos;
    let mut end = pos;
    while end < data.len() {
        if data[end] == 0x00 {
            let s = String::from_utf8_lossy(&data[start..end]).into_owned();
            return Some((s, end + 1));
        }
        end += 1;
    }
    None
}

/// Read a varint-encoded integer from `data` at `pos`.
/// Returns the decoded value and the position after the varint.
fn read_varint(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    let mut result: usize = 0;
    let mut shift = 0;
    let mut cursor = pos;
    while cursor < data.len() {
        let b = data[cursor];
        cursor += 1;
        result |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            return Some((result, cursor));
        }
        shift += 7;
        if shift >= 28 {
            break; // prevent runaway
        }
    }
    None
}

/// Parse a property value encoding starting at `pos`.
/// Returns the value and new position.
///
/// General format: `01 NN [payload of NN bytes]`
/// - NN is both the type identifier (for NN ≤ 0x04) and the payload length
///   (for NN ≥ 0x05).
fn parse_value(data: &[u8], pos: usize) -> Option<(PropValue, usize)> {
    if pos + 1 >= data.len() {
        return None;
    }

    // Value always starts with 0x01 marker
    if data[pos] != 0x01 {
        return None;
    }

    let nn = data[pos + 1];

    match nn {
        // Bool: 01 01 VV — payload=1 byte (VV=0x02 true, 0x03 false)
        0x01 => {
            if pos + 2 >= data.len() {
                return None;
            }
            let val = data[pos + 2] == 0x02;
            Some((PropValue::Bool(val), pos + 3))
        }

        // Short value: 01 02 SS DD — payload=2 bytes (sub-marker + 1 data byte)
        0x02 => {
            if pos + 3 >= data.len() {
                return None;
            }
            let sub = data[pos + 2];
            if sub == 0x05 {
                // Empty string: 01 02 05 00
                Some((PropValue::String(String::new()), pos + 4))
            } else {
                Some((PropValue::U32(data[pos + 3] as u32), pos + 4))
            }
        }

        // String (null-terminated): 01 03 [STRING\0]
        0x03 => {
            let (s, next) = read_cstring(data, pos + 2)?;
            Some((PropValue::String(s), next))
        }

        // String (length-prefixed): 01 04 LL [DATA×LL]
        0x04 => {
            if pos + 2 >= data.len() {
                return None;
            }
            let len = data[pos + 2] as usize;
            if pos + 3 + len > data.len() {
                return None;
            }
            let s = String::from_utf8_lossy(&data[pos + 3..pos + 3 + len])
                .trim_end_matches('\0')
                .to_string();
            Some((PropValue::String(s), pos + 3 + len))
        }

        // NN ≥ 0x05: payload is exactly NN bytes after the type byte.
        // First payload byte is a sub-marker that determines interpretation:
        //   0x01 → u32 (4 LE bytes)
        //   0x04 → f64 (8 LE bytes)
        //   0x05 → null-terminated string (NN-2 chars + NUL)
        nn => {
            let total = pos + 2 + nn as usize;
            if total > data.len() {
                return None;
            }
            if pos + 2 >= data.len() {
                return None;
            }
            let sub = data[pos + 2];
            match sub {
                0x05 => {
                    // String: 01 NN 05 [chars] 00
                    let str_len = (nn as usize).saturating_sub(2);
                    let str_start = pos + 3;
                    let str_end = str_start + str_len;
                    if str_end > data.len() {
                        let (s, next) = read_cstring(data, pos + 3)?;
                        return Some((PropValue::String(s), next));
                    }
                    let s = String::from_utf8_lossy(&data[str_start..str_end]).to_string();
                    Some((PropValue::String(s), str_end + 1))
                }
                0x04 if nn >= 9 => {
                    // f64: 01 NN 04 VV×8
                    if pos + 10 >= data.len() {
                        return None;
                    }
                    let val = f64::from_le_bytes([
                        data[pos + 3],
                        data[pos + 4],
                        data[pos + 5],
                        data[pos + 6],
                        data[pos + 7],
                        data[pos + 8],
                        data[pos + 9],
                        data[pos + 10],
                    ]);
                    Some((PropValue::F64(val), pos + 11))
                }
                0x01 if nn >= 5 => {
                    // u32: 01 NN 01 VV VV VV VV
                    if pos + 6 >= data.len() {
                        return None;
                    }
                    let val = u32::from_le_bytes([
                        data[pos + 3],
                        data[pos + 4],
                        data[pos + 5],
                        data[pos + 6],
                    ]);
                    Some((PropValue::U32(val), pos + 7))
                }
                _ => {
                    // Unknown sub-marker — skip NN bytes of payload
                    Some((PropValue::Unknown, total))
                }
            }
        }
    }
}

/// Parse a node (section) from the binary tree starting at `pos`.
///
/// Format: `[NAME\0] MARKER [properties?] [children?]`
///
/// - MARKER = 0x00: no properties, check next byte for children
/// - MARKER = 0x01: properties follow (varint count, then name/value pairs),
///   then check next byte for children
/// - MARKER = 0x02: children follow immediately (varint count, then children)
///
/// After properties, children are indicated by the next byte being 0x01 or 0x02
/// (children marker), followed by a varint child count.
///
/// Children are separated by 0x00 bytes and parsed recursively.
fn parse_node(data: &[u8], pos: usize) -> Option<(Section, usize)> {
    if pos >= data.len() || data[pos] == 0x00 {
        return None;
    }

    let (name, mut cursor) = read_cstring(data, pos)?;
    if name.is_empty() {
        return None;
    }

    let mut properties = Vec::new();
    let mut children = Vec::new();

    if cursor >= data.len() {
        return Some((
            Section {
                name,
                properties,
                children,
            },
            cursor,
        ));
    }

    let marker = data[cursor];
    cursor += 1;

    // Parse properties if marker is 0x01
    if marker == 0x01 {
        let (prop_count, next) = read_varint(data, cursor)?;
        cursor = next;

        for _ in 0..prop_count {
            if cursor >= data.len() {
                break;
            }
            let (prop_name, next) = match read_cstring(data, cursor) {
                Some(v) => v,
                None => break,
            };
            cursor = next;

            let (val, next) = match parse_value(data, cursor) {
                Some(v) => v,
                None => break,
            };
            cursor = next;

            properties.push(Property {
                name: prop_name,
                value: val,
            });
        }
    }

    // Check for children block.
    // If the initial marker was 0x02, children follow immediately.
    // Otherwise, look at the current byte for a children marker (0x01 or 0x02).
    let has_children = if marker == 0x02 {
        true
    } else if cursor < data.len() && (data[cursor] == 0x01 || data[cursor] == 0x02) {
        cursor += 1; // consume children marker byte
        true
    } else {
        false
    };

    if has_children {
        let (child_count, next) = read_varint(data, cursor)?;
        cursor = next;

        for i in 0..child_count {
            // Children are separated by 0x00
            if i > 0 && cursor < data.len() && data[cursor] == 0x00 {
                cursor += 1;
            }

            match parse_node(data, cursor) {
                Some((child, next)) => {
                    children.push(child);
                    cursor = next;
                }
                None => break,
            }
        }
    }

    Some((
        Section {
            name,
            properties,
            children,
        },
        cursor,
    ))
}

/// Extract pad properties from a parsed PAD section into a hashmap.
fn extract_pad_props(section: &Section) -> HashMap<String, PropValue> {
    let mut map = HashMap::new();
    for prop in &section.properties {
        map.insert(prop.name.clone(), prop.value.clone());
    }
    map
}

/// Convert a u32 PropValue to u32, returning None for other variants.
fn prop_u32(props: &HashMap<String, PropValue>, key: &str) -> Option<u32> {
    match props.get(key)? {
        PropValue::U32(v) => Some(*v),
        _ => None,
    }
}

/// Convert a bool PropValue to bool.
fn prop_bool(props: &HashMap<String, PropValue>, key: &str) -> Option<bool> {
    match props.get(key)? {
        PropValue::Bool(v) => Some(*v),
        _ => None,
    }
}

/// Convert an f64 PropValue to f64.
fn prop_f64(props: &HashMap<String, PropValue>, key: &str) -> Option<f64> {
    match props.get(key)? {
        PropValue::F64(v) => Some(*v),
        _ => None,
    }
}

/// Convert a String PropValue to &str.
fn prop_str<'a>(props: &'a HashMap<String, PropValue>, key: &str) -> Option<&'a str> {
    match props.get(key)? {
        PropValue::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Convert parsed pad properties into a `SoundPadConfig`.
/// `effects_slots` maps effectsIdx → effect properties (from EFFECTS_PARAMETERS sections).
fn pad_props_to_config(
    props: &HashMap<String, PropValue>,
    position: u8,
    fx_slot: Option<&HashMap<String, PropValue>>,
) -> SoundPadConfig {
    let pad_type = prop_u32(props, "padType").unwrap_or(0);
    let color_idx = prop_u32(props, "padColourIndex").unwrap_or(0);
    let color = PadColor::from_wire_index(color_idx).unwrap_or_default();
    let name = prop_str(props, "padName")
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();

    let assignment = match pad_type {
        1 => {
            // Sound (assigned) — padType=1 means a sound file is assigned
            let play_mode = match prop_u32(props, "padPlayMode").unwrap_or(0) {
                0 => PlayMode::OneShot,
                1 => PlayMode::Toggle,
                2 => PlayMode::Hold,
                _ => PlayMode::OneShot,
            };
            let loop_enabled = prop_bool(props, "padLoop").unwrap_or(false);
            let replay_mode = if prop_bool(props, "padReplay").unwrap_or(false) {
                ReplayMode::Replay
            } else {
                ReplayMode::Continue
            };
            let gain_db = prop_f64(props, "padGain").unwrap_or(-12.0);

            PadAssignment::Sound(SoundConfig {
                file_path: prop_str(props, "padFilePath")
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string(),
                play_mode,
                gain_db,
                color,
                loop_enabled,
                replay_mode,
            })
        }
        2 => {
            // FX pad — read all effect on/off states and parameters from linked effects slot
            // Device wire values: 0 = Momentary (hold to activate),
            // 1 = Latch (toggle on tap). Confirmed by user testing.
            let latch_mode = match prop_u32(props, "padEffectTriggerMode").unwrap_or(1) {
                0 => LatchMode::Momentary,
                1 => LatchMode::Latch,
                _ => LatchMode::Latch,
            };
            let input_source =
                FxInputSource::from_wire(prop_u32(props, "padEffectInput").unwrap_or(0));

            let mut effect_config = read_effects_state(fx_slot);
            effect_config.color = color;
            effect_config.latch_mode = latch_mode;
            effect_config.input_source = input_source;

            PadAssignment::Effect(effect_config)
        }
        3 => {
            // Mixer pad — parse all mixer-specific properties
            let mode = match prop_u32(props, "padMixerMode").unwrap_or(0) {
                0 => MixerMode::Censor,
                1 => MixerMode::TrashTalk,
                2 => MixerMode::FadeInOut,
                3 => MixerMode::BackChannel,
                4 => MixerMode::Ducking,
                _ => MixerMode::Censor,
            };
            let latch_mode = match prop_u32(props, "padMixerTriggerMode").unwrap_or(1) {
                0 => LatchMode::Momentary,
                1 => LatchMode::Latch,
                _ => LatchMode::Latch,
            };
            PadAssignment::Mixer(MixerPadConfig {
                mode,
                color,
                latch_mode,
                censor_custom: prop_bool(props, "padMixerCensorCustom").unwrap_or(false),
                censor_file_path: prop_str(props, "padMixerCensorFilePath")
                    .unwrap_or("")
                    .to_string(),
                beep_gain_db: prop_f64(props, "padGain").unwrap_or(-12.0),
                fade_in_seconds: prop_f64(props, "padMixerFadeInSeconds").unwrap_or(3.0),
                fade_out_seconds: prop_f64(props, "padMixerFadeOutSeconds").unwrap_or(3.0),
                fade_exclude_host: prop_bool(props, "padMixerFadeExcludeHost").unwrap_or(false),
                back_channel_mic2: prop_bool(props, "padMixerBackChannelMic2").unwrap_or(false),
                back_channel_mic3: prop_bool(props, "padMixerBackChannelMic3").unwrap_or(false),
                back_channel_mic4: prop_bool(props, "padMixerBackChannelMic4").unwrap_or(false),
                back_channel_usb1_comms: prop_bool(props, "padMixerBackChannelUsb1Comms")
                    .unwrap_or(false),
                back_channel_usb2_main: prop_bool(props, "padMixerBackChannelUsb2Main")
                    .unwrap_or(false),
                back_channel_bluetooth: prop_bool(props, "padMixerBackChannelBluetooth")
                    .unwrap_or(false),
                back_channel_callme1: prop_bool(props, "padMixerBackChannelCallMe1")
                    .unwrap_or(false),
                back_channel_callme2: prop_bool(props, "padMixerBackChannelCallMe2")
                    .unwrap_or(false),
                back_channel_callme3: prop_bool(props, "padMixerBackChannelCallMe3")
                    .unwrap_or(false),
                ducker_depth_db: -9.0, // Global property, set separately
            })
        }
        4 => {
            // MIDI trigger pad
            let trigger_type = match prop_u32(props, "padTriggerType").unwrap_or(0) {
                1 => TriggerType::MidiNote {
                    channel: prop_u32(props, "padTriggerChannel").unwrap_or(1) as u8,
                    note: prop_u32(props, "padTriggerControl").unwrap_or(60) as u8,
                    velocity: prop_u32(props, "padTriggerOn").unwrap_or(127) as u8,
                },
                _ => TriggerType::MidiNote {
                    channel: prop_u32(props, "padTriggerChannel").unwrap_or(1) as u8,
                    note: prop_u32(props, "padTriggerControl").unwrap_or(0) as u8,
                    velocity: prop_u32(props, "padTriggerOn").unwrap_or(127) as u8,
                },
            };
            PadAssignment::Trigger(TriggerPadConfig {
                trigger_type,
                color,
            })
        }
        _ => {
            // padType 0 = default/off, padType 5 = unknown, padType 6 = Video (not modeled)
            PadAssignment::Off
        }
    };

    SoundPadConfig {
        pad_index: position,
        name,
        assignment,
    }
}

/// Read all effect enable states and parameters from an EFFECTS_PARAMETERS section.
/// Each effect has an independent `*On` boolean — multiple can be active simultaneously.
fn read_effects_state(fx_props: Option<&HashMap<String, PropValue>>) -> EffectConfig {
    let fx = match fx_props {
        Some(fx) => fx,
        None => return EffectConfig::default(),
    };

    EffectConfig {
        reverb: ReverbEffect {
            enabled: prop_bool(fx, "reverbOn").unwrap_or(false),
            mix: prop_f64(fx, "reverbMix").unwrap_or(0.5),
            low_cut: prop_f64(fx, "reverbLowCut").unwrap_or(0.666146),
            high_cut: prop_f64(fx, "reverbHighCut").unwrap_or(0.333325),
            model: ReverbModel::from_wire(prop_f64(fx, "reverbModel").unwrap_or(0.6)),
        },
        echo: EchoEffect {
            enabled: prop_bool(fx, "echoOn").unwrap_or(false),
            mix: prop_f64(fx, "echoMix").unwrap_or(0.5),
            low_cut: prop_f64(fx, "echoLowCut").unwrap_or(0.5),
            high_cut: prop_f64(fx, "echoHighCut").unwrap_or(0.5),
            delay: prop_f64(fx, "echoDelay").unwrap_or(0.165),
            decay: prop_f64(fx, "echoDecay").unwrap_or(0.5),
        },
        megaphone: MegaphoneEffect {
            enabled: prop_bool(fx, "distortionOn").unwrap_or(false),
            intensity: prop_f64(fx, "distortionIntensity").unwrap_or(0.7),
        },
        robot: RobotEffect {
            enabled: prop_bool(fx, "robotOn").unwrap_or(false),
            mix: prop_f64(fx, "robotMix").unwrap_or(0.0),
        },
        voice_disguise: VoiceDisguiseEffect {
            enabled: prop_bool(fx, "voiceDisguiseOn").unwrap_or(false),
        },
        pitch_shift: PitchShiftEffect {
            enabled: prop_bool(fx, "pitchShiftOn").unwrap_or(false),
            semitones: prop_f64(fx, "pitchShiftSemitones").unwrap_or(7.0),
        },
        color: PadColor::default(),
        latch_mode: LatchMode::default(),
        input_source: FxInputSource::default(),
    }
}

/// Parse the root of the state tree.
///
/// The root has a special `0x02` prefix byte, then behaves like a normal node.
/// The root child count may not reflect the actual number of children in the
/// dump, so we parse until the data is exhausted.
fn parse_root(payload: &[u8]) -> Option<Section> {
    if payload.is_empty() || payload[0] != 0x02 {
        return None;
    }

    // Skip the 0x02 root prefix, then read root name + marker
    let (name, mut cursor) = read_cstring(payload, 1)?;

    let marker = *payload.get(cursor)?;
    cursor += 1;

    // Root marker is 0x00 (no properties), then children marker 0x02
    let has_children = if marker == 0x02 {
        true
    } else if cursor < payload.len() && payload[cursor] == 0x02 {
        cursor += 1;
        true
    } else {
        false
    };

    let mut children = Vec::new();
    if has_children {
        // Read the encoded child count — may be varint or u16 LE.
        // In practice the count doesn't always match, so we also try to
        // keep going past the declared count.
        let (_count, next) = read_varint(payload, cursor)?;
        cursor = next;

        // Parse children until we hit the end of valid data
        loop {
            // Skip separator bytes between children
            if !children.is_empty() && cursor < payload.len() && payload[cursor] == 0x00 {
                cursor += 1;
            }
            if cursor >= payload.len() {
                break;
            }
            match parse_node(payload, cursor) {
                Some((child, next)) if next > cursor => {
                    children.push(child);
                    cursor = next;
                }
                _ => break,
            }
        }
    }

    Some(Section {
        name,
        properties: Vec::new(),
        children,
    })
}

/// Recursively find a section by name in the tree.
fn find_section<'a>(section: &'a Section, target: &str) -> Option<&'a Section> {
    if section.name == target {
        return Some(section);
    }
    for child in &section.children {
        if let Some(found) = find_section(child, target) {
            return Some(found);
        }
    }
    None
}

/// Result of parsing the state dump: pad configs + index map for HID addressing.
pub struct ParsedPadState {
    /// Pad configurations organized by bank: `banks[bank][position]`.
    pub banks: Vec<Vec<SoundPadConfig>>,
    /// Mapping from logical padIdx (= bank*pads_per_bank + position) to the
    /// **absolute** child index within the SOUNDPADS section.
    /// This index counts ALL children (PAD and non-PAD) and is the byte used
    /// in HID long-form pad addresses (`9a 02 01 XX`) to target a specific pad.
    pub hid_index_map: Vec<Option<u8>>,
    /// Mapping from effectsIdx (= padIdx for FX pads) to the child index
    /// within the PADEFFECTS section.  This child index is the byte used in
    /// HID effects addresses (`9d 02 01 XX`) to target an effects slot.
    pub effects_slot_map: std::collections::HashMap<u32, u8>,
    /// Total number of children in SOUNDPADS (PAD + non-PAD).
    pub total_children: usize,
    /// Number of PAD children specifically (for diagnostics).
    pub num_pad_children: usize,
    /// Total number of children in PADEFFECTS (for fabricating new effects slots).
    pub effects_total_children: usize,
}

/// Parse the full state dump payload and extract pad configurations.
///
/// Returns 8 banks × N pads (6 for Duo, 8 for Pro) of `SoundPadConfig`,
/// along with a mapping from logical padIdx to HID address index.
/// Pads not found in the dump are left as `Off`.
pub fn parse_pad_configs(payload: &[u8], pads_per_bank: usize) -> ParsedPadState {
    let total_pads = 8 * pads_per_bank;

    // Initialize all-Off
    let mut banks: Vec<Vec<SoundPadConfig>> = (0..8)
        .map(|_| {
            (0..pads_per_bank)
                .map(|i| SoundPadConfig {
                    pad_index: i as u8,
                    name: String::new(),
                    assignment: PadAssignment::Off,
                })
                .collect()
        })
        .collect();

    // HID index map: hid_index_map[padIdx] = sequential position in SOUNDPADS
    let mut hid_index_map: Vec<Option<u8>> = vec![None; total_pads];

    // Parse the root of the state tree
    let root = match parse_root(payload) {
        Some(section) => section,
        None => {
            return ParsedPadState {
                banks,
                hid_index_map,
                effects_slot_map: HashMap::new(),
                total_children: 0,
                num_pad_children: 0,
                effects_total_children: 0,
            }
        }
    };

    // ── Collect EFFECTS_PARAMETERS sections ──
    // Each EFFECTS_PARAMETERS section has an `effectsIdx` property.
    // Build a map: effectsIdx → properties.
    let mut effects_slots: HashMap<u32, HashMap<String, PropValue>> = HashMap::new();
    collect_effects_sections(&root, &mut effects_slots);

    // ── Build effects slot map from PADEFFECTS section ──
    // The PADEFFECTS section contains EFFECTS_PARAMETERS children.
    // Each child's position (child index) within PADEFFECTS is the byte used
    // in HID effects addresses (9d 02 01 XX).  The effectsIdx property links
    // the slot to a pad (effectsIdx == padIdx for FX pads).
    let mut effects_slot_map: HashMap<u32, u8> = HashMap::new();
    let effects_total_children;
    if let Some(padeffects) = find_section(&root, "PADEFFECTS") {
        effects_total_children = padeffects.children.len();
        for (i, child) in padeffects.children.iter().enumerate() {
            if child.name == "EFFECTS_PARAMETERS" {
                let props = extract_pad_props(child);
                if let Some(eidx) = prop_u32(&props, "effectsIdx") {
                    effects_slot_map.insert(eidx, i as u8);
                }
            }
        }
    } else {
        effects_total_children = 0;
    }

    // PAD sections are nested inside SOUNDPADS
    let soundpads = match find_section(&root, "SOUNDPADS") {
        Some(section) => section,
        None => {
            return ParsedPadState {
                banks,
                hid_index_map,
                effects_slot_map,
                total_children: 0,
                num_pad_children: 0,
                effects_total_children,
            }
        }
    };

    // ── Also read global duckerDepth from DUCKER section ──
    let ducker_depth = find_section(&root, "DUCKER")
        .and_then(|s| {
            s.properties
                .iter()
                .find(|p| p.name == "duckerDepth")
                .and_then(|p| match &p.value {
                    PropValue::F64(v) => Some(*v),
                    _ => None,
                })
        })
        .unwrap_or(-9.0);

    // Each PAD child has a `padIdx` property that determines its logical
    // bank and position:  bank = padIdx / pads_per_bank,
    //                     pos  = padIdx % pads_per_bank.
    //
    // CRITICAL: The HID address byte (9a 02 01 XX) uses the ABSOLUTE child
    // index within SOUNDPADS — counting ALL children, not just PAD ones.
    // SOUNDPADS may contain non-PAD children (e.g. metadata sections) that
    // occupy positions in the tree.  Using a PAD-only counter produces wrong
    // indices, causing clears/assigns to target the wrong child.
    //
    // This matches how PADEFFECTS is handled (absolute enumerate() index).
    let total_children = soundpads.children.len();
    let mut num_pad_children = 0usize;
    let mut stolen_positions: Vec<u8> = Vec::new();
    for (child_index, child) in soundpads.children.iter().enumerate() {
        if child.name == "PAD" {
            num_pad_children += 1;
            let props = extract_pad_props(child);
            let pad_idx = prop_u32(&props, "padIdx").unwrap_or(child_index as u32) as usize;
            let pad_type = prop_u32(&props, "padType").unwrap_or(0);

            if pad_idx < total_pads {
                let bank = pad_idx / pads_per_bank;
                let pos = pad_idx % pads_per_bank;

                // Skip if this padIdx already has a non-Off assignment set by
                // an earlier PAD entry. The device dump can contain duplicate
                // padIdx values where later entries are stale/shadow copies.
                // Track these "stolen" absolute positions so they can be
                // reassigned to missing padIdx values (the duplicates in the
                // dump replace entries that should hold other padIdx values).
                let existing_is_assigned = banks
                    .get(bank)
                    .and_then(|b| b.get(pos))
                    .map_or(false, |p| !matches!(p.assignment, PadAssignment::Off));
                if existing_is_assigned {
                    stolen_positions.push(child_index as u8);
                    continue;
                }

                // Link FX pads to their effects slot by padIdx.
                // Each pad has an EFFECTS_PARAMETERS section keyed by
                // effectsIdx == padIdx in the state dump.
                let fx_slot = if pad_type == 2 {
                    effects_slots.get(&(pad_idx as u32))
                } else {
                    None
                };

                let mut config = pad_props_to_config(&props, pos as u8, fx_slot);

                // Inject global duckerDepth into Mixer/Ducking pads
                if let PadAssignment::Mixer(ref mut m) = config.assignment {
                    m.ducker_depth_db = ducker_depth;
                }

                if let Some(bank_pads) = banks.get_mut(bank) {
                    if let Some(slot) = bank_pads.get_mut(pos) {
                        *slot = config;
                    }
                }

                hid_index_map[pad_idx] = Some(child_index as u8);
            }
        }
    }

    // The state dump can have duplicate padIdx values that steal absolute
    // positions from other padIdx values.  Assign those stolen positions to
    // the padIdx values that ended up with no mapping.
    if !stolen_positions.is_empty() {
        let mut unmapped: Vec<usize> = (0..total_pads.min(total_children))
            .filter(|i| hid_index_map[*i].is_none())
            .collect();
        unmapped.sort_unstable();
        for (pos, pad_idx) in stolen_positions.into_iter().zip(unmapped.into_iter()) {
            hid_index_map[pad_idx] = Some(pos);
        }
    }

    // Pads that never had entries in the state dump (never assigned) still
    // need HID indices so the host can create new assignments.  The device
    // only accepts new PAD children at the NEXT available position
    // (total_children), so ALL unassigned pads share that single index.
    //
    // Only ONE new pad can be created per operation — after creating it the
    // daemon must refresh the state dump so total_children advances.  This
    // means the caller must refresh state after any assign/clear that
    // targets an unassigned pad.
    let next_index = total_children as u8;
    for i in 0..total_pads {
        if hid_index_map[i].is_none() {
            hid_index_map[i] = Some(next_index);
        }
    }

    ParsedPadState {
        banks,
        hid_index_map,
        effects_slot_map,
        total_children,
        num_pad_children,
        effects_total_children,
    }
}

/// Recursively collect all EFFECTS_PARAMETERS sections and index by effectsIdx.
fn collect_effects_sections(section: &Section, map: &mut HashMap<u32, HashMap<String, PropValue>>) {
    if section.name == "EFFECTS_PARAMETERS" {
        let props = extract_pad_props(section);
        if let Some(idx) = prop_u32(&props, "effectsIdx") {
            map.insert(idx, props);
        }
    }
    for child in &section.children {
        collect_effects_sections(child, map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_cstring() {
        let data = b"hello\0world";
        let (s, pos) = read_cstring(data, 0).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(pos, 6);
    }

    #[test]
    fn test_parse_value_bool() {
        let data = [0x01, 0x01, 0x02]; // true
        let (val, pos) = parse_value(&data, 0).unwrap();
        assert_eq!(pos, 3);
        assert!(matches!(val, PropValue::Bool(true)));

        let data = [0x01, 0x01, 0x03]; // false
        let (val, _) = parse_value(&data, 0).unwrap();
        assert!(matches!(val, PropValue::Bool(false)));
    }

    #[test]
    fn test_parse_value_u32() {
        let data = [0x01, 0x05, 0x01, 0x14, 0x00, 0x00, 0x00]; // u32(20)
        let (val, pos) = parse_value(&data, 0).unwrap();
        assert_eq!(pos, 7);
        match val {
            PropValue::U32(v) => assert_eq!(v, 20),
            _ => panic!("Expected U32"),
        }
    }

    #[test]
    fn test_parse_value_f64() {
        let data = {
            let mut d = vec![0x01, 0x09, 0x04];
            d.extend_from_slice(&(-12.0_f64).to_le_bytes());
            d
        };
        let (val, pos) = parse_value(&data, 0).unwrap();
        assert_eq!(pos, 11);
        match val {
            PropValue::F64(v) => assert!((v - (-12.0)).abs() < f64::EPSILON),
            _ => panic!("Expected F64"),
        }
    }

    #[test]
    fn test_parse_value_short_string_empty() {
        // 01 02 05 00 = empty string (sub=0x05, NUL terminator)
        let data = [0x01, 0x02, 0x05, 0x00];
        let (val, pos) = parse_value(&data, 0).unwrap();
        assert_eq!(pos, 4);
        match val {
            PropValue::String(s) => assert!(s.is_empty()),
            _ => panic!("Expected empty String"),
        }
    }

    #[test]
    fn test_parse_value_string() {
        // 01 05 05 46 54 42 00 = "FTB"
        let data = [0x01, 0x05, 0x05, 0x46, 0x54, 0x42, 0x00];
        let (val, pos) = parse_value(&data, 0).unwrap();
        assert_eq!(pos, 7);
        match val {
            PropValue::String(s) => assert_eq!(s, "FTB"),
            _ => panic!("Expected String, got {:?}", val),
        }
    }

    #[test]
    fn test_reassemble_empty() {
        assert!(reassemble_state_dump(&[]).is_none());
    }

    #[test]
    fn test_reassemble_single() {
        let mut report = vec![0x04];
        report.extend_from_slice(&5u32.to_le_bytes()); // payload_len=5
        report.extend_from_slice(&[0x41, 0x42, 0x43, 0x44, 0x45]); // "ABCDE"
        report.extend_from_slice(&[0x00; 10]); // padding

        let payload = reassemble_state_dump(&[report]).unwrap();
        assert_eq!(payload, vec![0x41, 0x42, 0x43, 0x44, 0x45]);
    }

    #[test]
    fn test_pad_props_to_config_off() {
        let props = HashMap::new();
        let config = pad_props_to_config(&props, 0, None);
        assert!(matches!(config.assignment, PadAssignment::Off));
    }

    #[test]
    fn test_pad_props_to_config_sound() {
        let mut props = HashMap::new();
        props.insert("padType".to_string(), PropValue::U32(1));
        props.insert("padPlayMode".to_string(), PropValue::U32(1));
        props.insert("padLoop".to_string(), PropValue::Bool(true));
        props.insert("padReplay".to_string(), PropValue::Bool(true));
        props.insert("padGain".to_string(), PropValue::F64(-12.0));
        props.insert("padColourIndex".to_string(), PropValue::U32(5));
        props.insert("padName".to_string(), PropValue::String("TestSound".into()));

        let config = pad_props_to_config(&props, 2, None);
        assert_eq!(config.pad_index, 2);
        assert_eq!(config.name, "TestSound");
        match &config.assignment {
            PadAssignment::Sound(s) => {
                assert_eq!(s.play_mode, PlayMode::Toggle);
                assert!(s.loop_enabled);
                assert_eq!(s.replay_mode, ReplayMode::Replay);
                assert_eq!(s.color, PadColor::Green);
                assert!((s.gain_db - (-12.0)).abs() < 0.01);
            }
            _ => panic!("Expected Sound assignment"),
        }
    }

    #[test]
    fn test_pad_props_to_config_fx() {
        let mut props = HashMap::new();
        props.insert("padType".to_string(), PropValue::U32(2));
        props.insert("padEffectTriggerMode".to_string(), PropValue::U32(1));
        props.insert("padEffectInput".to_string(), PropValue::U32(19));
        props.insert("padColourIndex".to_string(), PropValue::U32(8));

        // Provide an effects slot with voiceDisguise + reverb enabled
        let mut fx_props = HashMap::new();
        fx_props.insert("voiceDisguiseOn".to_string(), PropValue::Bool(true));
        fx_props.insert("reverbOn".to_string(), PropValue::Bool(true));

        let config = pad_props_to_config(&props, 0, Some(&fx_props));
        match &config.assignment {
            PadAssignment::Effect(e) => {
                assert_eq!(e.latch_mode, LatchMode::Latch);
                assert_eq!(e.color, PadColor::Blue);
                assert_eq!(e.input_source, FxInputSource::Wireless1);
                assert!(e.voice_disguise.enabled);
                assert!(e.reverb.enabled);
                assert!(!e.echo.enabled);
                assert!(!e.robot.enabled);
                assert!(!e.megaphone.enabled);
                assert!(!e.pitch_shift.enabled);
            }
            _ => panic!("Expected Effect assignment"),
        }
    }

    #[test]
    fn test_pad_props_to_config_midi() {
        let mut props = HashMap::new();
        props.insert("padType".to_string(), PropValue::U32(4));
        props.insert("padTriggerType".to_string(), PropValue::U32(1)); // Note
        props.insert("padTriggerChannel".to_string(), PropValue::U32(3));
        props.insert("padTriggerControl".to_string(), PropValue::U32(64));
        props.insert("padTriggerOn".to_string(), PropValue::U32(100));

        let config = pad_props_to_config(&props, 1, None);
        match &config.assignment {
            PadAssignment::Trigger(t) => match &t.trigger_type {
                TriggerType::MidiNote {
                    channel,
                    note,
                    velocity,
                } => {
                    assert_eq!(*channel, 3);
                    assert_eq!(*note, 64);
                    assert_eq!(*velocity, 100);
                }
            },
            _ => panic!("Expected Trigger assignment"),
        }
    }

    #[test]
    fn test_parse_real_state_dump() {
        // Skip if the real state dump file is not available (CI, etc.)
        let path = "/tmp/lincaster_state_dump.bin";
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => return, // Skip test
        };

        let root = parse_root(&data).expect("Failed to parse root section");
        eprintln!(
            "Root: name='{}', {} children",
            root.name,
            root.children.len(),
        );

        // Find SOUNDPADS and count PAD children
        let soundpads = find_section(&root, "SOUNDPADS");
        let pad_count =
            soundpads.map_or(0, |s| s.children.iter().filter(|c| c.name == "PAD").count());
        eprintln!("SOUNDPADS PAD children: {}", pad_count);

        // Show first few PAD configs
        if let Some(sp) = soundpads {
            // Dump all property names for first PAD
            if let Some(first_pad) = sp.children.iter().find(|c| c.name == "PAD") {
                eprintln!("First PAD property names:");
                for prop in &first_pad.properties {
                    eprintln!("  {} = {:?}", prop.name, prop.value);
                }
            }
            for (i, child) in sp.children.iter().filter(|c| c.name == "PAD").enumerate() {
                let props = extract_pad_props(child);
                let pt = prop_u32(&props, "padType");
                let pi = prop_u32(&props, "padIdx");
                let ci = prop_u32(&props, "padColourIndex");
                let active = prop_bool(&props, "padActive");
                eprintln!(
                    "  PAD #{}: padIdx={:?} padType={:?} padColourIndex={:?} padActive={:?} (bank={}, pos={})",
                    i, pi, pt, ci, active,
                    i / 6, i % 6
                );
            }
        }

        let parsed = parse_pad_configs(&data, 6);
        let configs = &parsed.banks;
        assert_eq!(configs.len(), 8, "Should have 8 banks");

        // Verify HID index map was populated
        let mapped_count = parsed.hid_index_map.iter().filter(|x| x.is_some()).count();
        eprintln!("HID index map: {} entries mapped", mapped_count);

        let total_assigned: usize = configs
            .iter()
            .flat_map(|b| b.iter())
            .filter(|p| !matches!(p.assignment, PadAssignment::Off))
            .count();
        eprintln!("Total assigned pads: {}", total_assigned);

        // Print summary for manual inspection
        for (bank_idx, bank) in configs.iter().enumerate() {
            for pad in bank {
                if !matches!(pad.assignment, PadAssignment::Off) {
                    eprintln!(
                        "Bank {} Pad {}: {:?}",
                        bank_idx + 1,
                        pad.pad_index,
                        pad.assignment
                    );
                }
            }
        }

        // Verify JSON round-trip (same path as D-Bus GetPadConfigs)
        let json = serde_json::to_string(&configs).expect("Failed to serialize pad configs");
        let deserialized: Vec<Vec<SoundPadConfig>> =
            serde_json::from_str(&json).expect("Failed to deserialize pad configs");
        let rt_assigned: usize = deserialized
            .iter()
            .flat_map(|b| b.iter())
            .filter(|p| !matches!(p.assignment, PadAssignment::Off))
            .count();
        assert_eq!(
            rt_assigned, total_assigned,
            "JSON round-trip lost assigned pads"
        );
        eprintln!(
            "JSON round-trip OK: {} assigned pads preserved",
            rt_assigned
        );
    }

    #[test]
    fn test_soundpads_children_order() {
        let path = "/tmp/lincaster_state_dump.bin";
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let root = parse_root(&data).expect("parse root");
        let sp = find_section(&root, "SOUNDPADS").expect("find SOUNDPADS");
        eprintln!("SOUNDPADS total children: {}", sp.children.len());
        for (i, child) in sp.children.iter().enumerate() {
            let extra = if child.name == "PAD" {
                let props = extract_pad_props(child);
                format!(
                    " padIdx={:?} padType={:?} children={}",
                    prop_u32(&props, "padIdx"),
                    prop_u32(&props, "padType"),
                    child.children.len()
                )
            } else if child.name == "EFFECTS_PARAMETERS" {
                let props = extract_pad_props(child);
                format!(" effectsIdx={:?}", prop_u32(&props, "effectsIdx"))
            } else {
                format!(" children={}", child.children.len())
            };
            eprintln!("  child {:3}: {}{}", i, child.name, extra);
        }

        // Also check where EFFECTS_PARAMETERS live
        fn find_effects(section: &Section, path: &str) {
            if section.name == "EFFECTS_PARAMETERS" {
                let props = extract_pad_props(section);
                eprintln!(
                    "  EFFECTS at path={} effectsIdx={:?}",
                    path,
                    prop_u32(&props, "effectsIdx")
                );
            }
            for (i, child) in section.children.iter().enumerate() {
                find_effects(child, &format!("{}/{}", path, i));
            }
        }
        eprintln!("\nAll EFFECTS_PARAMETERS locations:");
        find_effects(&root, "root");
    }

    #[test]
    fn test_effects_slot_mapping() {
        // Find the tree positions of SOUNDPADS and PADEFFECTS to understand address encoding
        let duo_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../captures/state_dump.bin");
        let data = match std::fs::read(duo_path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let root = parse_root(&data).expect("parse root");

        // Print ALL root children names to find SOUNDPADS and PADEFFECTS positions
        for (i, child) in root.children.iter().enumerate() {
            if child.name == "SOUNDPADS"
                || child.name == "PADEFFECTS"
                || child.name == "EFFECTS_PARAMETERS"
            {
                eprintln!(
                    "root child {}: {} (children={})",
                    i,
                    child.name,
                    child.children.len()
                );
            }
        }

        // Print hex: what is 669 in hex?
        eprintln!("\n669 decimal = 0x{:X}", 669);

        // Also show the section at 0x9a and 0x9d if they exist
        if root.children.len() > 0x9a {
            eprintln!("root[0x9a=154]: {}", root.children[0x9a].name);
        }
        if root.children.len() > 0x9d {
            eprintln!("root[0x9d=157]: {}", root.children[0x9d].name);
        }

        // Now understand how pad hw_index maps:
        // SOUNDPADS children are PAD entries, hw_index = sequential position
        let soundpads = find_section(&root, "SOUNDPADS").unwrap();
        eprintln!("\nSOUNDPADS has {} children", soundpads.children.len());

        // PADEFFECTS children are EFFECTS_PARAMETERS entries
        let padeffects = find_section(&root, "PADEFFECTS").unwrap();
        eprintln!("PADEFFECTS has {} children", padeffects.children.len());

        // Build inverse map: effectsIdx → child_index_in_PADEFFECTS
        let mut eidx_to_child: Vec<(u32, usize)> = Vec::new();
        for (i, child) in padeffects.children.iter().enumerate() {
            if child.name == "EFFECTS_PARAMETERS" {
                let props = extract_pad_props(child);
                if let Some(eidx) = prop_u32(&props, "effectsIdx") {
                    eidx_to_child.push((eidx, i));
                }
            }
        }
        eidx_to_child.sort_by_key(|(eidx, _)| *eidx);

        eprintln!("\neffectsIdx → child_index_in_PADEFFECTS (sorted by effectsIdx):");
        for (eidx, cidx) in &eidx_to_child {
            eprintln!("  effectsIdx={} → child_index={}", eidx, cidx);
        }
    }

    #[test]
    fn test_dbus_json_deserialize() {
        // Read the JSON that D-Bus actually serves (captured from a live daemon)
        let json = match std::fs::read_to_string("/tmp/pad_json.txt") {
            Ok(j) => j,
            Err(_) => return, // Skip if not available
        };

        eprintln!("JSON length: {} bytes", json.len());
        eprintln!("First 200 chars: {}", &json[..json.len().min(200)]);

        let configs: Vec<Vec<SoundPadConfig>> =
            serde_json::from_str(&json).expect("Failed to deserialize D-Bus pad JSON");

        let total: usize = configs.iter().map(|b| b.len()).sum();
        let assigned: usize = configs
            .iter()
            .flat_map(|b| b.iter())
            .filter(|p| !matches!(p.assignment, PadAssignment::Off))
            .count();

        eprintln!(
            "Deserialized: {} banks, {} total pads, {} assigned",
            configs.len(),
            total,
            assigned
        );
        assert!(
            assigned > 0,
            "Expected some assigned pads from live D-Bus data"
        );
    }
}
