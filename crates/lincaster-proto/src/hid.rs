//! USB HID protocol encoding for the RØDECaster Duo.
//!
//! Constructs binary messages for EP5 HID Interrupt endpoint following
//! the reverse-engineered protocol documented in USB_PROTOCOL.md.

use crate::PadColor;

/// HID report size (256 bytes for SET_PROPERTY commands).
const HID_REPORT_SIZE: usize = 256;

/// Message type for SET_PROPERTY commands (Host → Device).
const MSG_SET_PROPERTY: u8 = 0x03;

/// Message type for Handshake (Host → Device).
const MSG_HANDSHAKE: u8 = 0x01;

// ── Address bytes ────────────────────────────────────────────────────

/// Standard SET_PROPERTY prefix.
const ADDR_PREFIX_SET: u8 = 0x01;

/// Section announcement prefix (used before bulk resets).
const ADDR_PREFIX_ANNOUNCE: u8 = 0x03;

/// Section redirect prefix (used for clear operations).
/// Capture-verified: the Windows app sends `04 01 01 02 9a 02 01 [hw_idx]`
/// to redirect the device to the target PAD section during a clear.
const ADDR_PREFIX_REDIRECT: u8 = 0x04;

/// Pad address base path: 01 02 02 9a 02
/// Verified from pcap: full long-form = [ADDR_PREFIX_SET=01] + [01 02 02 9a 02] + [LONG=01] + [hw_index]
/// = 01 01 02 02 9a 02 01 [hw_index] (8 bytes total)
const PAD_ADDR_BASE: [u8; 5] = [0x01, 0x02, 0x02, 0x9a, 0x02];

/// Short-form pad address suffix (current/default pad): 00
const PAD_ADDR_SHORT: u8 = 0x00;

/// Long-form pad address prefix before index byte: 01
const PAD_ADDR_LONG: u8 = 0x01;

/// Effects parameter address base: 01 02 02 9d 02 (mirrors PAD_ADDR_BASE with 0x9d)
const EFFECTS_ADDR_BASE: [u8; 5] = [0x01, 0x02, 0x02, 0x9d, 0x02];

/// System property address (selectedBank, transferModeType, remountPadStorage).
const SYSTEM_ADDR_07: [u8; 5] = [0x01, 0x01, 0x01, 0x01, 0x07]; // selectedBank
const SYSTEM_ADDR_0F: [u8; 5] = [0x01, 0x01, 0x01, 0x01, 0x0f]; // transferModeType, remountPadStorage

/// Ducker address: 01 01 01 01 01
const DUCKER_ADDR: [u8; 5] = [0x01, 0x01, 0x01, 0x01, 0x01];

// ── Value encoding ──────────────────────────────────────────────────

/// Encode a bool value: `01 01 VV` where VV=0x02 (true) or 0x03 (false).
pub fn encode_bool(val: bool) -> Vec<u8> {
    vec![0x01, 0x01, if val { 0x02 } else { 0x03 }]
}

/// Encode a u32 value: `01 05 01 VV VV VV VV` (little-endian).
pub fn encode_u32(val: u32) -> Vec<u8> {
    let bytes = val.to_le_bytes();
    vec![0x01, 0x05, 0x01, bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// Encode an f64 value: `01 09 04 VV×8` (little-endian IEEE 754).
pub fn encode_f64(val: f64) -> Vec<u8> {
    let bytes = val.to_le_bytes();
    let mut v = vec![0x01, 0x09, 0x04];
    v.extend_from_slice(&bytes);
    v
}

/// Encode an empty-string clear value: `01 02 05 00` (used for clearing padName/padFilePath).
/// This is equivalent to `encode_string("")` and matches the wire format from captures.
pub fn encode_enum_clear() -> Vec<u8> {
    encode_string("")
}

/// Encode a length-prefixed string: `01 NN 05 [string] 00`
/// where NN = 1 (sub-marker) + len(string) + 1 (null terminator).
pub fn encode_string(s: &str) -> Vec<u8> {
    let nn = 1 + s.len() + 1; // sub-marker + string bytes + null
    let mut v = vec![0x01, nn as u8, 0x05];
    v.extend_from_slice(s.as_bytes());
    v.push(0x00);
    v
}

// ── Message construction ────────────────────────────────────────────

/// Build a complete 256-byte HID report for a SET_PROPERTY command.
fn build_set_property(address: &[u8], property: &str, value: &[u8]) -> Vec<u8> {
    // Payload = address + property_name + null + value
    let prop_bytes = property.as_bytes();
    let payload_len = address.len() + prop_bytes.len() + 1 + value.len();

    let mut buf = vec![0u8; HID_REPORT_SIZE];
    buf[0] = MSG_SET_PROPERTY;
    let len_bytes = (payload_len as u32).to_le_bytes();
    buf[1..5].copy_from_slice(&len_bytes);

    let mut offset = 5;
    buf[offset..offset + address.len()].copy_from_slice(address);
    offset += address.len();

    buf[offset..offset + prop_bytes.len()].copy_from_slice(prop_bytes);
    offset += prop_bytes.len();
    buf[offset] = 0x00; // null terminator
    offset += 1;

    buf[offset..offset + value.len()].copy_from_slice(value);
    // Rest is already zeroed

    buf
}

/// Build a section announcement message (03-prefixed address).
/// Protocol captures show that section operations (announce/redirect) use
/// address byte[2] = 0x01, while SET_PROPERTY uses 0x02.  e.g.:
///   SET:      01 01 02 02 9a 02 01 XX
///   ANNOUNCE: 03 01 01 02 9a 02 01 XX
fn build_section_announce(base_addr: &[u8], section_name: &str) -> Vec<u8> {
    let mut addr = vec![ADDR_PREFIX_ANNOUNCE];
    // Skip the first byte (01) of the base address
    addr.extend_from_slice(&base_addr[1..]);
    // Fix byte[2]: section ops use 0x01, not the 0x02 from SET addresses
    if addr.len() > 2 {
        addr[2] = 0x01;
    }

    let prop_bytes = section_name.as_bytes();
    let payload_len = addr.len() + prop_bytes.len() + 1 + 2; // +2 for 00 00

    let mut buf = vec![0u8; HID_REPORT_SIZE];
    buf[0] = MSG_SET_PROPERTY;
    let len_bytes = (payload_len as u32).to_le_bytes();
    buf[1..5].copy_from_slice(&len_bytes);

    let mut offset = 5;
    buf[offset..offset + addr.len()].copy_from_slice(&addr);
    offset += addr.len();

    buf[offset..offset + prop_bytes.len()].copy_from_slice(prop_bytes);
    offset += prop_bytes.len();
    buf[offset] = 0x00;
    // offset += 1; two more zeros already present from zeroed buf

    buf
}

/// Build a section redirect message (04-prefixed address).
/// Used by the clear protocol: the Windows app sends this after clearing
/// padFilePath to commit the section change on the device.
///
/// Wire format: `03 LL 00 00 00 04 01 01 02 9a 02 01 [hw_idx]`
/// (no section name — just the address with 0x04 prefix).
fn build_section_redirect(base_addr: &[u8]) -> Vec<u8> {
    let mut addr = vec![ADDR_PREFIX_REDIRECT];
    // Skip the first byte (01) of the base address
    addr.extend_from_slice(&base_addr[1..]);
    // Fix byte[2]: section ops use 0x01, not the 0x02 from SET addresses
    if addr.len() > 2 {
        addr[2] = 0x01;
    }

    let payload_len = addr.len() as u32;

    let mut buf = vec![0u8; HID_REPORT_SIZE];
    buf[0] = MSG_SET_PROPERTY;
    let len_bytes = payload_len.to_le_bytes();
    buf[1..5].copy_from_slice(&len_bytes);

    buf[5..5 + addr.len()].copy_from_slice(&addr);

    buf
}

// ── Pad address helpers ─────────────────────────────────────────────

/// Build the full address for a specific pad index (long form).
fn pad_address_long(pad_hw_index: u8) -> Vec<u8> {
    let mut addr = vec![ADDR_PREFIX_SET];
    addr.extend_from_slice(&PAD_ADDR_BASE);
    addr.push(PAD_ADDR_LONG);
    addr.push(pad_hw_index);
    addr
}

/// Build the short-form pad address (current/default pad).
fn pad_address_short() -> Vec<u8> {
    let mut addr = vec![ADDR_PREFIX_SET];
    addr.extend_from_slice(&PAD_ADDR_BASE);
    addr.push(PAD_ADDR_SHORT);
    addr
}

/// Build the address for an effects parameter slot.
fn effects_address(slot_index: u8) -> Vec<u8> {
    let mut addr = vec![ADDR_PREFIX_SET];
    addr.extend_from_slice(&EFFECTS_ADDR_BASE);
    addr.push(PAD_ADDR_LONG);
    addr.push(slot_index);
    addr
}

// ── Pad index mapping ───────────────────────────────────────────────

/// Known pad index mapping (bank, position) → hardware index.
/// Banks are 0-indexed, positions are 0-indexed (0–5 for Duo).
/// Only partially mapped — returns None for unknown bank/position combos.
pub fn pad_hw_index(bank: u8, position: u8) -> Option<u8> {
    // Bank 1 (bank=0): indices 0x13–0x18 (19–24), 6 pads
    // Bank 2 (bank=1): indices 0x05–0x0A (5–10), 6 pads
    // Bank 3 (bank=2): indices 0x08–0x0D (8–13) — wait, session 6 showed 0x05-0x07 for bank 2 and 0x08-0x0A for bank 3
    // The mapping is non-linear so we use what's been observed.
    match bank {
        0 => {
            // Bank 1: observed 0x13=19, 0x14=20, 0x15=21
            if position < 6 {
                Some(0x13 + position)
            } else {
                None
            }
        }
        1 => {
            // Bank 2: observed 0x05, 0x06, 0x07
            if position < 6 {
                Some(0x05 + position)
            } else {
                None
            }
        }
        2 => {
            // Bank 3: observed 0x08, 0x09, 0x0A
            if position < 6 {
                Some(0x08 + position)
            } else {
                None
            }
        }
        // Banks 4–8: not yet mapped — use an estimated linear layout.
        // These are guesses and should be verified via captures.
        3 => {
            if position < 6 {
                Some(0x0B + position)
            } else {
                None
            }
        }
        4 => {
            if position < 6 {
                Some(0x11 + position)
            } else {
                None
            }
        }
        5 => {
            if position < 6 {
                Some(0x19 + position)
            } else {
                None
            }
        }
        6 => {
            if position < 6 {
                Some(0x1F + position)
            } else {
                None
            }
        }
        7 => {
            if position < 6 {
                Some(0x25 + position)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ── Public API: High-level command builders ──────────────────────────

/// Build a handshake message (Type 0x01).
pub fn handshake() -> Vec<u8> {
    let mut buf = vec![0u8; 64];
    buf[0] = MSG_HANDSHAKE;
    // Payload length = 0x4e (78 bytes), content all zeros
    let len_bytes = 0x4e_u32.to_le_bytes();
    buf[1..5].copy_from_slice(&len_bytes);
    buf
}

/// Set the active bank (0-indexed: 0 = Bank 1, 7 = Bank 8).
pub fn set_selected_bank(bank: u8) -> Vec<u8> {
    build_set_property(&SYSTEM_ADDR_07, "selectedBank", &encode_u32(bank as u32))
}

/// Set transfer mode type (0 = normal, 2 = transfer/editing mode).
pub fn set_transfer_mode(mode: u32) -> Vec<u8> {
    build_set_property(&SYSTEM_ADDR_0F, "transferModeType", &encode_u32(mode))
}

/// Trigger a pad storage remount.
pub fn remount_pad_storage() -> Vec<u8> {
    build_set_property(&SYSTEM_ADDR_0F, "remountPadStorage", &encode_bool(true))
}

/// Set a pad property on the currently-selected pad (short address form).
pub fn set_current_pad_property(property: &str, value: &[u8]) -> Vec<u8> {
    build_set_property(&pad_address_short(), property, value)
}

/// Set a pad property on a specific pad by hardware index (long address form).
pub fn set_pad_property(hw_index: u8, property: &str, value: &[u8]) -> Vec<u8> {
    build_set_property(&pad_address_long(hw_index), property, value)
}

/// Set pad colour index on the current pad.
pub fn set_pad_colour(color: PadColor) -> Vec<u8> {
    set_current_pad_property("padColourIndex", &encode_u32(color.wire_index()))
}

/// Set pad colour on a specific pad.
pub fn set_pad_colour_at(hw_index: u8, color: PadColor) -> Vec<u8> {
    set_pad_property(hw_index, "padColourIndex", &encode_u32(color.wire_index()))
}

/// Set pad type on the current pad.
pub fn set_pad_type(pad_type: u32) -> Vec<u8> {
    set_current_pad_property("padType", &encode_u32(pad_type))
}

/// Set pad play mode (0=OneShot, 1=Toggle, 2=Hold).
pub fn set_pad_play_mode(mode: u32) -> Vec<u8> {
    set_current_pad_property("padPlayMode", &encode_u32(mode))
}

/// Set pad loop enabled.
pub fn set_pad_loop(enabled: bool) -> Vec<u8> {
    set_current_pad_property("padLoop", &encode_bool(enabled))
}

/// Set pad replay enabled.
pub fn set_pad_replay(enabled: bool) -> Vec<u8> {
    set_current_pad_property("padReplay", &encode_bool(enabled))
}

/// Set pad gain (dB, default -12.0).
pub fn set_pad_gain(gain_db: f64) -> Vec<u8> {
    set_current_pad_property("padGain", &encode_f64(gain_db))
}

/// Activate a pad (select it for editing).
pub fn activate_pad(hw_index: u8) -> Vec<u8> {
    set_pad_property(hw_index, "padActive", &encode_bool(true))
}

/// Deactivate a pad.
pub fn deactivate_pad(hw_index: u8) -> Vec<u8> {
    set_pad_property(hw_index, "padActive", &encode_bool(false))
}

/// Deactivate the currently-selected pad (short-form address).
///
/// This is the second command the official RØDE app sends on close, after
/// `transferModeType=0`. Together they end the host session and cause the
/// device to stop sending EP5 IN notifications. This variant uses the
/// short-form address so we don't need to track which pad was last selected.
pub fn deactivate_current_pad() -> Vec<u8> {
    set_current_pad_property("padActive", &encode_bool(false))
}

/// Set pad name via string encoding.
pub fn set_pad_name(name: &str) -> Vec<u8> {
    set_current_pad_property("padName", &encode_string(name))
}

/// Clear pad name.
pub fn clear_pad_name() -> Vec<u8> {
    set_current_pad_property("padName", &encode_enum_clear())
}

/// Set pad name to zero/reset (f64=0.0, used after type changes).
pub fn reset_pad_name() -> Vec<u8> {
    set_current_pad_property("padName", &encode_f64(0.0))
}

// ── FX pad properties ───────────────────────────────────────────────

/// Set FX effect input source (0=Mic1, 1=Mic2, 19=Wireless1, 20=Wireless2).
pub fn set_pad_effect_input(input: u32) -> Vec<u8> {
    set_current_pad_property("padEffectInput", &encode_u32(input))
}

/// Set FX effect trigger mode (0=Latch, 1=Momentary).
pub fn set_pad_effect_trigger_mode(mode: u32) -> Vec<u8> {
    set_current_pad_property("padEffectTriggerMode", &encode_u32(mode))
}

// ── Mixer pad properties ────────────────────────────────────────────

/// Set mixer mode (0=Censor, 1=TrashTalk, 2=FadeInOut, 3=BackChannel, 4=Ducking).
pub fn set_pad_mixer_mode(mode: u32) -> Vec<u8> {
    set_current_pad_property("padMixerMode", &encode_u32(mode))
}

/// Set mixer fade-in seconds.
pub fn set_pad_mixer_fade_in(seconds: f64) -> Vec<u8> {
    set_current_pad_property("padMixerFadeInSeconds", &encode_f64(seconds))
}

/// Set mixer fade-out seconds.
pub fn set_pad_mixer_fade_out(seconds: f64) -> Vec<u8> {
    set_current_pad_property("padMixerFadeOutSeconds", &encode_f64(seconds))
}

/// Set mixer fade exclude host.
pub fn set_pad_mixer_exclude_host(exclude: bool) -> Vec<u8> {
    set_current_pad_property("padMixerFadeExcludeHost", &encode_bool(exclude))
}

/// Set mixer censor custom file mode.
pub fn set_pad_mixer_censor_custom(enabled: bool) -> Vec<u8> {
    set_current_pad_property("padMixerCensorCustom", &encode_bool(enabled))
}

/// Set mixer trigger mode (0=Latch, 1=Momentary).
pub fn set_pad_mixer_trigger_mode(mode: u32) -> Vec<u8> {
    set_current_pad_property("padMixerTriggerMode", &encode_u32(mode))
}

/// Set global ducker depth (dB, range -12.0 to -6.0).
pub fn set_ducker_depth(depth_db: f64) -> Vec<u8> {
    build_set_property(&DUCKER_ADDR, "duckerDepth", &encode_f64(depth_db))
}

/// Set mixer back-channel routing target.
pub fn set_mixer_back_channel(target: &str, enabled: bool) -> Vec<u8> {
    set_current_pad_property(target, &encode_bool(enabled))
}

// ── MIDI pad properties ─────────────────────────────────────────────

/// Set MIDI trigger mode (0=Latching, 1=Momentary).
pub fn set_pad_trigger_mode(mode: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerMode", &encode_u32(mode))
}

/// Set MIDI trigger send mode (0=Press, 1=Release, 2=Press+Release).
pub fn set_pad_trigger_send(mode: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerSend", &encode_u32(mode))
}

/// Set MIDI trigger type (0=CC, 1=Note).
pub fn set_pad_trigger_type(trigger_type: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerType", &encode_u32(trigger_type))
}

/// Enable/disable custom MIDI settings.
pub fn set_pad_trigger_custom(enabled: bool) -> Vec<u8> {
    set_current_pad_property("padTriggerCustom", &encode_bool(enabled))
}

/// Set MIDI control number (CC) or note number (Note), 0–127.
pub fn set_pad_trigger_control(control: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerControl", &encode_u32(control))
}

/// Set MIDI channel (1–16).
pub fn set_pad_trigger_channel(channel: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerChannel", &encode_u32(channel))
}

/// Set MIDI on value (velocity or CC value, 0–127).
pub fn set_pad_trigger_on(value: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerOn", &encode_u32(value))
}

/// Set MIDI off value (velocity or CC value, 0–127).
pub fn set_pad_trigger_off(value: u32) -> Vec<u8> {
    set_current_pad_property("padTriggerOff", &encode_u32(value))
}

// ── Video pad properties ────────────────────────────────────────────

/// Set RODE Central Video sync pad type (0–29, see padRCVSyncPadType enum).
pub fn set_pad_rcv_sync_type(rcv_type: u32) -> Vec<u8> {
    set_current_pad_property("padRCVSyncPadType", &encode_u32(rcv_type))
}

// ── State dump request ──────────────────────────────────────────────

/// Build the "request state dump" command (Type 0x03, 4-byte payload).
///
/// Observed in Windows RØDE Central captures: after the handshake (Type 0x01)
/// and device identification response (Type 0x02), the host sends this command
/// to trigger the full state dump (Type 0x04 reports).  Without it, the device
/// never sends the state dump.
///
/// Wire bytes: `03 04 00 00 00 AD 10 A7 B0` padded to 256.
pub fn request_state_dump() -> Vec<u8> {
    let mut buf = vec![0u8; HID_REPORT_SIZE];
    buf[0] = MSG_SET_PROPERTY; // 0x03
                               // Payload length = 4 (LE u32)
    buf[1] = 0x04;
    // buf[2..5] already 0
    // 4-byte magic payload
    buf[5] = 0xAD;
    buf[6] = 0x10;
    buf[7] = 0xA7;
    buf[8] = 0xB0;
    buf
}

// ── Effects parameters ──────────────────────────────────────────────

/// Build the section announcement for an effects parameter slot.
/// This MUST be sent before writing any effects properties — the device
/// ignores property writes to a slot that hasn't been announced.
pub fn effects_section_announce(slot_index: u8) -> Vec<u8> {
    build_section_announce(&effects_address(slot_index), "EFFECTS_PARAMETERS")
}

/// Set an effects parameter on a specific effects slot.
pub fn set_effects_property(slot_index: u8, property: &str, value: &[u8]) -> Vec<u8> {
    build_set_property(&effects_address(slot_index), property, value)
}

/// Set reverb on/off on an effects slot.
pub fn set_reverb_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "reverbOn", &encode_bool(on))
}

/// Set reverb mix (0.0–1.0).
pub fn set_reverb_mix(slot: u8, mix: f64) -> Vec<u8> {
    set_effects_property(slot, "reverbMix", &encode_f64(mix))
}

/// Set reverb model (0.0, 0.2, 0.4, 0.6, 0.8 — 5 room types).
pub fn set_reverb_model(slot: u8, model: f64) -> Vec<u8> {
    set_effects_property(slot, "reverbModel", &encode_f64(model))
}

/// Set reverb low-cut filter (0.0–1.0).
pub fn set_reverb_low_cut(slot: u8, val: f64) -> Vec<u8> {
    set_effects_property(slot, "reverbLowCut", &encode_f64(val))
}

/// Set reverb high-cut filter (0.0–1.0).
pub fn set_reverb_high_cut(slot: u8, val: f64) -> Vec<u8> {
    set_effects_property(slot, "reverbHighCut", &encode_f64(val))
}

/// Set echo on/off.
pub fn set_echo_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "echoOn", &encode_bool(on))
}

/// Set echo mix (0.0–1.0).
pub fn set_echo_mix(slot: u8, mix: f64) -> Vec<u8> {
    set_effects_property(slot, "echoMix", &encode_f64(mix))
}

/// Set echo low-cut filter (0.0–1.0).
pub fn set_echo_low_cut(slot: u8, val: f64) -> Vec<u8> {
    set_effects_property(slot, "echoLowCut", &encode_f64(val))
}

/// Set echo high-cut filter (0.0–1.0).
pub fn set_echo_high_cut(slot: u8, val: f64) -> Vec<u8> {
    set_effects_property(slot, "echoHighCut", &encode_f64(val))
}

/// Set echo delay (0.0–1.0).
pub fn set_echo_delay(slot: u8, delay: f64) -> Vec<u8> {
    set_effects_property(slot, "echoDelay", &encode_f64(delay))
}

/// Set echo decay/feedback (0.0–1.0).
pub fn set_echo_decay(slot: u8, decay: f64) -> Vec<u8> {
    set_effects_property(slot, "echoDecay", &encode_f64(decay))
}

/// Set pitch shift on/off.
pub fn set_pitch_shift_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "pitchShiftOn", &encode_bool(on))
}

/// Set pitch shift semitones (-12.0 to 12.0).
pub fn set_pitch_shift_semitones(slot: u8, semitones: f64) -> Vec<u8> {
    set_effects_property(slot, "pitchShiftSemitones", &encode_f64(semitones))
}

/// Set distortion/megaphone on/off.
pub fn set_distortion_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "distortionOn", &encode_bool(on))
}

/// Set distortion/megaphone intensity (0.0–1.0, 10 discrete levels).
pub fn set_distortion_intensity(slot: u8, intensity: f64) -> Vec<u8> {
    set_effects_property(slot, "distortionIntensity", &encode_f64(intensity))
}

/// Set robot voice on/off.
pub fn set_robot_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "robotOn", &encode_bool(on))
}

/// Set robot voice mix (0.0, 0.333, 0.667 — 3 discrete levels).
pub fn set_robot_mix(slot: u8, mix: f64) -> Vec<u8> {
    set_effects_property(slot, "robotMix", &encode_f64(mix))
}

/// Set voice disguise on/off.
pub fn set_voice_disguise_on(slot: u8, on: bool) -> Vec<u8> {
    set_effects_property(slot, "voiceDisguiseOn", &encode_bool(on))
}

// ── Pad clear/reset ─────────────────────────────────────────────────

/// Generate the 3-command clear sequence matching the Windows RØDE Central
/// app's clear protocol (from captures/soundpad_clear_sound.pcapng):
///
///   1. Set padFilePath = "" (clear the file association)
///   2. Section redirect (04-prefix) to the PAD section
///   3. remountPadStorage = true (trigger firmware re-scan)
///
/// This is dramatically simpler than the full 48-command property reset
/// (`pad_clear_sequence`), and is what the Windows app actually sends.
/// No transfer mode, no file deletion, no property resets needed.
pub fn pad_clear_simple(hw_index: u8) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    vec![
        build_set_property(&addr, "padFilePath", &encode_string("")),
        build_section_redirect(&addr),
        remount_pad_storage(),
    ]
}

/// Generate the full sequence of HID reports needed to clear/reset a pad.
/// Returns a Vec of 256-byte reports to send in order.
///
/// This full reset sequence is used BEFORE assigning a new pad type.
/// The Windows app sends this as the first phase of every pad assignment.
/// For simply clearing a pad (removing its sound), use `pad_clear_simple()`.
///
/// The Windows RØDE Central app does NOT send a section redirect (04-prefix)
/// for pad setup — it goes directly to section announce + property resets.
/// See captures/sound_assignment_analysis.md for details.
pub fn pad_clear_sequence(hw_index: u8, pad_idx: u8) -> Vec<Vec<u8>> {
    let addr_long = pad_address_long(hw_index);
    let mut cmds = Vec::with_capacity(50);

    // Step 1: Section announcement (03-prefix, "PAD") — NO section redirect!
    cmds.push(build_section_announce(&addr_long, "PAD"));

    // Step 2: All properties set to defaults (exact order from protocol)
    let a = &addr_long;
    cmds.push(build_set_property(a, "padColourIndex", &encode_u32(0)));
    cmds.push(build_set_property(a, "padActive", &encode_bool(false)));
    cmds.push(build_set_property(a, "padLoop", &encode_bool(false)));
    cmds.push(build_set_property(a, "padReplay", &encode_bool(false)));
    cmds.push(build_set_property(a, "padType", &encode_u32(0)));
    cmds.push(build_set_property(a, "padName", &encode_enum_clear()));
    cmds.push(build_set_property(a, "padProgress", &encode_f64(0.0)));
    cmds.push(build_set_property(a, "padFilePath", &encode_enum_clear()));
    cmds.push(build_set_property(a, "padPlayMode", &encode_u32(0)));
    cmds.push(build_set_property(a, "padEnvStart", &encode_f64(0.0)));
    cmds.push(build_set_property(a, "padEnvFadeIn", &encode_f64(0.0)));
    cmds.push(build_set_property(a, "padEnvFadeOut", &encode_f64(0.0)));
    cmds.push(build_set_property(a, "padEnvStop", &encode_f64(0.0)));
    cmds.push(build_set_property(a, "padMixerMode", &encode_u32(0)));
    cmds.push(build_set_property(a, "padMixerTriggerMode", &encode_u32(0)));
    cmds.push(build_set_property(
        a,
        "padMixerCensorCustom",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerCensorFilePath",
        &encode_enum_clear(),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerFadeInSeconds",
        &encode_f64(0.0),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerFadeOutSeconds",
        &encode_f64(0.0),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerFadeExcludeHost",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelMic2",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelMic3",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelMic4",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelUsb1Comms",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelUsb2Main",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelBluetooth",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelCallMe1",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelCallMe2",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padMixerBackChannelCallMe3",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(a, "padRCVSyncPadType", &encode_u32(0)));
    cmds.push(build_set_property(a, "padEffectInput", &encode_u32(0)));
    cmds.push(build_set_property(
        a,
        "padEffectTriggerMode",
        &encode_u32(0),
    ));
    cmds.push(build_set_property(
        a,
        "padSIPPhoneBookEntry",
        &encode_u32(0),
    ));
    cmds.push(build_set_property(a, "padSIPCallSlot", &encode_u32(0)));
    cmds.push(build_set_property(a, "padSIPFlashState", &encode_u32(0)));
    cmds.push(build_set_property(a, "padSIPQdLock", &encode_bool(false)));
    cmds.push(build_set_property(a, "padTriggerMode", &encode_u32(1))); // default=1
    cmds.push(build_set_property(a, "padTriggerSend", &encode_u32(2))); // default=2
    cmds.push(build_set_property(a, "padTriggerType", &encode_u32(0)));
    cmds.push(build_set_property(
        a,
        "padTriggerCustom",
        &encode_bool(false),
    ));
    cmds.push(build_set_property(
        a,
        "padTriggerControl",
        &encode_u32(pad_idx as u32),
    )); // defaults to padIdx
    cmds.push(build_set_property(a, "padTriggerChannel", &encode_u32(1))); // default=1
    cmds.push(build_set_property(a, "padTriggerOn", &encode_u32(127))); // full velocity
    cmds.push(build_set_property(a, "padTriggerOff", &encode_u32(0)));
    cmds.push(build_set_property(a, "padIsInternal", &encode_bool(false)));
    cmds.push(build_set_property(a, "padGain", &encode_f64(-12.0)));
    cmds.push(build_set_property(a, "padIdx", &encode_u32(pad_idx as u32)));

    cmds
}

/// Generate the command sequence to assign a Sound pad (padType=1) after a clear.
/// `pad_idx` is used to generate the auto placeholder name "Sound NN" (padIdx+1).
pub fn pad_assign_sound(hw_index: u8, pad_idx: u8, color: PadColor) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    let auto_name = format!("Sound {}", pad_idx as u32 + 1);
    vec![
        build_set_property(&addr, "padType", &encode_u32(1)),
        build_set_property(&addr, "padEnvStop", &encode_f64(1.0)),
        build_set_property(&addr, "padEnvFadeOut", &encode_f64(1.0)),
        build_set_property(&addr, "padName", &encode_string(&auto_name)),
        build_set_property(&addr, "padColourIndex", &encode_u32(color.wire_index())),
    ]
}

/// Generate the command sequence to assign an FX pad (padType=2) after a clear.
/// Only sets padType and colour — trigger mode, padActive, padName, and
/// padEffectInput are sent individually afterward by the caller.
pub fn pad_assign_fx(hw_index: u8, color: PadColor) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    vec![
        build_set_property(&addr, "padType", &encode_u32(2)),
        build_set_property(&addr, "padColourIndex", &encode_u32(color.wire_index())),
    ]
}

/// Generate the command sequence to assign a Mixer pad (padType=3) after a clear.
pub fn pad_assign_mixer(hw_index: u8, color: PadColor) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    vec![
        build_set_property(&addr, "padType", &encode_u32(3)),
        build_set_property(&addr, "padName", &encode_string("")),
        build_set_property(&addr, "padColourIndex", &encode_u32(color.wire_index())),
    ]
}

/// Generate the command sequence to assign a MIDI pad (padType=4) after a clear.
pub fn pad_assign_midi(hw_index: u8, color: PadColor) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    vec![
        build_set_property(&addr, "padType", &encode_u32(4)),
        build_set_property(&addr, "padName", &encode_string("")),
        build_set_property(&addr, "padColourIndex", &encode_u32(color.wire_index())),
    ]
}

/// Generate the command sequence to assign a Video pad (padType=6) after a clear.
pub fn pad_assign_video(hw_index: u8, rcv_type: u32, name: &str, color: PadColor) -> Vec<Vec<u8>> {
    let addr = pad_address_long(hw_index);
    vec![
        build_set_property(&addr, "padType", &encode_u32(6)),
        build_set_property(&addr, "padName", &encode_string(name)),
        build_set_property(&addr, "padRCVSyncPadType", &encode_u32(rcv_type)),
        build_set_property(&addr, "padColourIndex", &encode_u32(color.wire_index())),
    ]
}

// ── Re-export encoding functions for external use ───────────────────

pub use self::encode_bool as val_bool;
pub use self::encode_enum_clear as val_enum_clear;
pub use self::encode_f64 as val_f64;
pub use self::encode_string as val_string;
pub use self::encode_u32 as val_u32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_bool() {
        assert_eq!(encode_bool(true), vec![0x01, 0x01, 0x02]);
        assert_eq!(encode_bool(false), vec![0x01, 0x01, 0x03]);
    }

    #[test]
    fn test_encode_u32() {
        let encoded = encode_u32(1);
        assert_eq!(encoded, vec![0x01, 0x05, 0x01, 0x01, 0x00, 0x00, 0x00]);

        let encoded = encode_u32(11);
        assert_eq!(encoded, vec![0x01, 0x05, 0x01, 0x0b, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_encode_f64() {
        let encoded = encode_f64(-12.0);
        assert_eq!(encoded.len(), 11);
        assert_eq!(encoded[0], 0x01);
        assert_eq!(encoded[1], 0x09);
        assert_eq!(encoded[2], 0x04);
        let val = f64::from_le_bytes(encoded[3..11].try_into().unwrap());
        assert_eq!(val, -12.0);
    }

    #[test]
    fn test_encode_string() {
        // "FTB" → 01 05 05 46 54 42 00
        let encoded = encode_string("FTB");
        assert_eq!(encoded, vec![0x01, 0x05, 0x05, 0x46, 0x54, 0x42, 0x00]);
    }

    #[test]
    fn test_build_set_property_size() {
        let msg = set_pad_colour(PadColor::Blue);
        assert_eq!(msg.len(), HID_REPORT_SIZE);
        assert_eq!(msg[0], MSG_SET_PROPERTY);
    }

    #[test]
    fn test_pad_clear_sequence_length() {
        let cmds = pad_clear_sequence(0x14, 0);
        // Should be ~48 commands (announce + 47 properties) — no redirect
        assert!(
            cmds.len() >= 47,
            "Expected >= 47 commands, got {}",
            cmds.len()
        );
        // All should be 256 bytes
        for cmd in &cmds {
            assert_eq!(cmd.len(), HID_REPORT_SIZE);
        }
    }

    #[test]
    fn test_handshake_size() {
        let msg = handshake();
        assert_eq!(msg.len(), 64);
        assert_eq!(msg[0], MSG_HANDSHAKE);
    }

    #[test]
    fn test_pad_hw_index_bank1() {
        assert_eq!(pad_hw_index(0, 0), Some(0x13));
        assert_eq!(pad_hw_index(0, 1), Some(0x14));
        assert_eq!(pad_hw_index(0, 2), Some(0x15));
    }

    #[test]
    fn test_pad_hw_index_bank2() {
        assert_eq!(pad_hw_index(1, 0), Some(0x05));
        assert_eq!(pad_hw_index(1, 1), Some(0x06));
        assert_eq!(pad_hw_index(1, 2), Some(0x07));
    }

    #[test]
    fn test_set_selected_bank() {
        let msg = set_selected_bank(0);
        assert_eq!(msg[0], MSG_SET_PROPERTY);
        // Address should be SYSTEM_ADDR_07
        assert_eq!(&msg[5..10], &SYSTEM_ADDR_07);
    }
}
