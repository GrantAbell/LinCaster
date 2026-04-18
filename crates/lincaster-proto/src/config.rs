use serde::{Deserialize, Serialize};

/// Top-level configuration for the LinCaster daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub device: DeviceConfig,
    #[serde(default = "default_busses")]
    pub busses: Vec<BusConfig>,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    #[serde(default)]
    pub app_rules: Vec<AppRuleConfig>,
    #[serde(default)]
    pub latency_mode: LatencyMode,
}

/// Device identification and discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// USB vendor ID. RØDE = 0x19F7.
    #[serde(default = "default_vendor_id")]
    pub usb_vendor_id: u16,
    /// USB product IDs to match. Multiple IDs support different firmware modes.
    #[serde(default = "default_product_ids")]
    pub usb_product_ids: Vec<u16>,
    /// Hint for matching ALSA card names (substring match).
    #[serde(default = "default_alsa_hint")]
    pub alsa_card_id_hint: String,
    /// Whether to require multitrack mode. If true, fail if multichannel endpoints are missing.
    #[serde(default = "default_true")]
    pub require_multitrack: bool,
}

/// Virtual bus (output endpoint) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusConfig {
    pub bus_id: String,
    pub display_name: String,
    #[serde(default = "default_playback")]
    pub direction: BusDirection,
    #[serde(default = "default_channels")]
    pub channels: u32,
    #[serde(default = "default_gain")]
    pub default_gain: f32,
    /// If true, this bus is never muted by other busses' solo actions.
    #[serde(default)]
    pub solo_safe: bool,
}

/// Routing from a bus to a hardware or software target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    pub from_bus_id: String,
    pub to_target: String,
    #[serde(default)]
    pub channel_map: Option<ChannelMapConfig>,
}

/// Channel mapping for a route (maps bus channels to target channels).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMapConfig {
    /// Left channel index on the hardware endpoint.
    pub left: u32,
    /// Right channel index on the hardware endpoint.
    pub right: u32,
}

/// Per-application stream routing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRuleConfig {
    #[serde(rename = "match")]
    pub match_criteria: MatchConfig,
    pub target_bus_id: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Stream matching criteria. At least one field must be set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchConfig {
    pub process_name_regex: Option<String>,
    pub app_name_regex: Option<String>,
    pub client_name_regex: Option<String>,
    pub flatpak_app_id: Option<String>,
}

impl MatchConfig {
    pub fn has_any_criteria(&self) -> bool {
        self.process_name_regex.is_some()
            || self.app_name_regex.is_some()
            || self.client_name_regex.is_some()
            || self.flatpak_app_id.is_some()
    }
}

/// Direction of a virtual bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusDirection {
    Playback,
    Capture,
    Duplex,
}

/// Latency mode setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LatencyMode {
    UltraLow,
    #[default]
    Low,
}

impl LatencyMode {
    /// Suggested PipeWire quantum (buffer size in frames) for this mode at 48 kHz.
    pub fn suggested_quantum(&self) -> u32 {
        match self {
            LatencyMode::UltraLow => 64,
            LatencyMode::Low => 256,
        }
    }
}

/// Snapshot of an active audio stream for GUI display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSnapshot {
    pub node_id: u32,
    pub display_name: String,
    pub app_name: String,
    /// If routed to one of our virtual busses, the bus_id. None = default device.
    pub target_bus_id: Option<String>,
    /// PipeWire node name of the target sink. None = no links found.
    pub target_sink_name: Option<String>,
    /// True if this stream was auto-routed by a config.json app_rule.
    #[serde(default)]
    pub auto_routed: bool,
}

// ── Sound Pad types ─────────────────────────────────────────────────

/// Configuration for a single sound pad on the RØDECaster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundPadConfig {
    pub pad_index: u8,
    /// Pad display name from `padName` property.
    #[serde(default)]
    pub name: String,
    pub assignment: PadAssignment,
}

/// What a sound pad is assigned to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PadAssignment {
    #[default]
    Off,
    Sound(SoundConfig),
    Effect(EffectConfig),
    Mixer(MixerPadConfig),
    Trigger(TriggerPadConfig),
}

/// Sound file playback configuration for a pad.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundConfig {
    /// Path to the audio file on device storage (from `padFilePath`).
    pub file_path: String,
    pub play_mode: PlayMode,
    /// Pad output gain in dB (from `padGain`, default -12.0).
    #[serde(default = "default_gain_db")]
    pub gain_db: f64,
    #[serde(default)]
    pub color: PadColor,
    #[serde(default)]
    pub loop_enabled: bool,
    /// Replay: restart from beginning; Continue: resume from last position.
    #[serde(default)]
    pub replay_mode: ReplayMode,
}

/// Whether a sound replays from the start or continues where it left off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    #[default]
    Replay,
    Continue,
}

/// How a sound pad triggers playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    #[default]
    OneShot,
    Toggle,
    Hold,
}

/// Voice FX configuration for a pad.
///
/// Each effect can be independently enabled — a single FX pad can have
/// multiple effects active simultaneously (e.g., reverb + robot + echo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectConfig {
    #[serde(default)]
    pub reverb: ReverbEffect,
    #[serde(default)]
    pub echo: EchoEffect,
    #[serde(default)]
    pub megaphone: MegaphoneEffect,
    #[serde(default)]
    pub robot: RobotEffect,
    #[serde(default)]
    pub voice_disguise: VoiceDisguiseEffect,
    #[serde(default)]
    pub pitch_shift: PitchShiftEffect,
    #[serde(default)]
    pub color: PadColor,
    /// Latch: effect stays on after release; Momentary: only while held.
    #[serde(default)]
    pub latch_mode: LatchMode,
    /// Which input source the FX processes (from `padEffectInput`).
    #[serde(default)]
    pub input_source: FxInputSource,
}

impl EffectConfig {
    /// Short display name summarising which effects are enabled.
    pub fn active_effects_summary(&self) -> String {
        let mut names = Vec::new();
        if self.reverb.enabled {
            names.push("Reverb");
        }
        if self.echo.enabled {
            names.push("Echo");
        }
        if self.megaphone.enabled {
            names.push("Megaphone");
        }
        if self.robot.enabled {
            names.push("Robot");
        }
        if self.voice_disguise.enabled {
            names.push("Disguise");
        }
        if self.pitch_shift.enabled {
            names.push("Pitch");
        }
        if names.is_empty() {
            "FX (none)".into()
        } else {
            names.join("+")
        }
    }
}

impl Default for EffectConfig {
    fn default() -> Self {
        Self {
            reverb: ReverbEffect::default(),
            echo: EchoEffect::default(),
            megaphone: MegaphoneEffect::default(),
            robot: RobotEffect::default(),
            voice_disguise: VoiceDisguiseEffect::default(),
            pitch_shift: PitchShiftEffect::default(),
            color: PadColor::default(),
            latch_mode: LatchMode::default(),
            input_source: FxInputSource::default(),
        }
    }
}

/// Latch behavior for effect pads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LatchMode {
    #[default]
    Latch,
    Momentary,
}

/// Mixer pad configuration matching `padMixerMode` protocol.
/// All mode-specific fields are stored flat (matching device state storage).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerPadConfig {
    pub mode: MixerMode,
    #[serde(default)]
    pub color: PadColor,
    /// Latch: mixer stays active after release; Momentary: only while held.
    #[serde(default)]
    pub latch_mode: LatchMode,
    // ── Censor mode (padMixerMode=0) ──
    #[serde(default)]
    pub censor_custom: bool,
    #[serde(default)]
    pub censor_file_path: String,
    /// Beep tone trim in dB (padGain when in censor mode, default -12.0).
    #[serde(default = "default_gain_db")]
    pub beep_gain_db: f64,
    // ── Fade In/Out mode (padMixerMode=2) ──
    #[serde(default = "default_fade_seconds")]
    pub fade_in_seconds: f64,
    #[serde(default = "default_fade_seconds")]
    pub fade_out_seconds: f64,
    #[serde(default)]
    pub fade_exclude_host: bool,
    // ── Back Channel routing (padMixerMode=3) ──
    #[serde(default)]
    pub back_channel_mic2: bool,
    #[serde(default)]
    pub back_channel_mic3: bool,
    #[serde(default)]
    pub back_channel_mic4: bool,
    #[serde(default)]
    pub back_channel_usb1_comms: bool,
    #[serde(default)]
    pub back_channel_usb2_main: bool,
    #[serde(default)]
    pub back_channel_bluetooth: bool,
    #[serde(default)]
    pub back_channel_callme1: bool,
    #[serde(default)]
    pub back_channel_callme2: bool,
    #[serde(default)]
    pub back_channel_callme3: bool,
    // ── Ducking mode (padMixerMode=4) ──
    /// Global ducking depth in dB (duckerDepth, default -9.0, range -12.0 to -6.0).
    #[serde(default = "default_ducker_depth")]
    pub ducker_depth_db: f64,
}

/// Mixer operating modes matching `padMixerMode` protocol values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MixerMode {
    #[default]
    Censor,      // padMixerMode=0
    TrashTalk,   // padMixerMode=1
    FadeInOut,   // padMixerMode=2
    BackChannel, // padMixerMode=3
    Ducking,     // padMixerMode=4
}

/// Trigger pad configuration (MIDI or other external trigger).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerPadConfig {
    pub trigger_type: TriggerType,
    #[serde(default)]
    pub color: PadColor,
}

/// Types of triggers a pad can send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerType {
    MidiNote {
        #[serde(default = "default_midi_channel")]
        channel: u8,
        #[serde(default = "default_midi_note")]
        note: u8,
        #[serde(default = "default_midi_velocity")]
        velocity: u8,
    },
}

impl Default for TriggerType {
    fn default() -> Self {
        Self::MidiNote {
            channel: default_midi_channel(),
            note: default_midi_note(),
            velocity: default_midi_velocity(),
        }
    }
}

fn default_midi_channel() -> u8 {
    1
}

fn default_midi_note() -> u8 {
    60
}

fn default_midi_velocity() -> u8 {
    127
}

/// Reverb effect parameters (from EFFECTS_PARAMETERS section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReverbEffect {
    #[serde(default)]
    pub enabled: bool,
    /// Wet/dry mix (reverbMix, 0.0–1.0, default 0.5).
    #[serde(default = "default_half")]
    pub mix: f64,
    /// Low-cut filter (reverbLowCut, 0.0–1.0, default ~0.666).
    #[serde(default = "default_reverb_low_cut")]
    pub low_cut: f64,
    /// High-cut filter (reverbHighCut, 0.0–1.0, default ~0.333).
    #[serde(default = "default_reverb_high_cut")]
    pub high_cut: f64,
    /// Room model (reverbModel, 5 discrete f64 values).
    #[serde(default)]
    pub model: ReverbModel,
}

impl Default for ReverbEffect {
    fn default() -> Self {
        Self {
            enabled: false,
            mix: default_half(),
            low_cut: default_reverb_low_cut(),
            high_cut: default_reverb_high_cut(),
            model: ReverbModel::default(),
        }
    }
}

/// Echo effect parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoEffect {
    #[serde(default)]
    pub enabled: bool,
    /// Wet/dry mix (echoMix, 0.0–1.0, default 0.5).
    #[serde(default = "default_half")]
    pub mix: f64,
    /// Low-cut filter (echoLowCut, 0.0–1.0, default 0.5).
    #[serde(default = "default_half")]
    pub low_cut: f64,
    /// High-cut filter (echoHighCut, 0.0–1.0, default 0.5).
    #[serde(default = "default_half")]
    pub high_cut: f64,
    /// Delay time (echoDelay, 0.0–1.0, default 0.165).
    #[serde(default = "default_echo_delay")]
    pub delay: f64,
    /// Feedback/decay (echoDecay, 0.0–1.0, default 0.5).
    #[serde(default = "default_half")]
    pub decay: f64,
}

impl Default for EchoEffect {
    fn default() -> Self {
        Self {
            enabled: false,
            mix: default_half(),
            low_cut: default_half(),
            high_cut: default_half(),
            delay: default_echo_delay(),
            decay: default_half(),
        }
    }
}

/// Megaphone / distortion effect parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MegaphoneEffect {
    #[serde(default)]
    pub enabled: bool,
    /// Intensity (distortionIntensity, 10 discrete levels: 0.0, 1/9, …, 1.0).
    #[serde(default = "default_megaphone_intensity")]
    pub intensity: f64,
}

impl Default for MegaphoneEffect {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: default_megaphone_intensity(),
        }
    }
}

/// Robot voice effect parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobotEffect {
    #[serde(default)]
    pub enabled: bool,
    /// Robot mix (robotMix, 3 discrete levels: 0.0, 0.333, 0.667).
    #[serde(default)]
    pub mix: f64,
}

impl Default for RobotEffect {
    fn default() -> Self {
        Self {
            enabled: false,
            mix: 0.0,
        }
    }
}

/// Voice disguise effect — no configurable parameters (just on/off).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoiceDisguiseEffect {
    #[serde(default)]
    pub enabled: bool,
}

/// Pitch shift effect parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PitchShiftEffect {
    #[serde(default)]
    pub enabled: bool,
    /// Semitone shift (pitchShiftSemitones, -12.0–12.0, default 7.0).
    #[serde(default = "default_pitch_semitones")]
    pub semitones: f64,
}

impl Default for PitchShiftEffect {
    fn default() -> Self {
        Self {
            enabled: false,
            semitones: default_pitch_semitones(),
        }
    }
}

/// Reverb room model matching `reverbModel` f64 wire values (5 discrete steps).
/// Names match the RØDECaster Pro II touchscreen labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReverbModel {
    SmallRoom,  // reverbModel = 0.0
    MediumRoom, // reverbModel = 0.2
    LargeRoom,  // reverbModel = 0.4
    #[default]
    SmallHall,  // reverbModel = 0.6
    LargeHall,  // reverbModel = 0.8
}

impl ReverbModel {
    /// Convert from the wire f64 value to enum.
    pub fn from_wire(val: f64) -> Self {
        if val < 0.1 {
            ReverbModel::SmallRoom
        } else if val < 0.3 {
            ReverbModel::MediumRoom
        } else if val < 0.5 {
            ReverbModel::LargeRoom
        } else if val < 0.7 {
            ReverbModel::SmallHall
        } else {
            ReverbModel::LargeHall
        }
    }

    /// Convert to the wire f64 value.
    pub fn to_wire(self) -> f64 {
        match self {
            ReverbModel::SmallRoom => 0.0,
            ReverbModel::MediumRoom => 0.2,
            ReverbModel::LargeRoom => 0.4,
            ReverbModel::SmallHall => 0.6,
            ReverbModel::LargeHall => 0.8,
        }
    }
}

/// FX input source matching `padEffectInput` wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FxInputSource {
    #[default]
    Mic1,      // padEffectInput = 0
    Mic2,      // padEffectInput = 1
    Wireless1, // padEffectInput = 19
    Wireless2, // padEffectInput = 20
}

impl FxInputSource {
    pub fn from_wire(val: u32) -> Self {
        match val {
            0 => FxInputSource::Mic1,
            1 => FxInputSource::Mic2,
            19 => FxInputSource::Wireless1,
            20 => FxInputSource::Wireless2,
            _ => FxInputSource::Mic1,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            FxInputSource::Mic1 => 0,
            FxInputSource::Mic2 => 1,
            FxInputSource::Wireless1 => 19,
            FxInputSource::Wireless2 => 20,
        }
    }
}

/// Pad LED colour matching the RØDECaster hardware colour wheel.
/// Values correspond to the `padColourIndex` u32 sent over USB HID (0–11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum PadColor {
    #[default]
    Red = 0,
    Orange = 1,
    Amber = 2,
    Yellow = 3,
    Lime = 4,
    Green = 5,
    Teal = 6,
    Cyan = 7,
    Blue = 8,
    Purple = 9,
    Magenta = 10,
    Pink = 11,
}

impl PadColor {
    pub const ALL: [PadColor; 12] = [
        PadColor::Red,
        PadColor::Orange,
        PadColor::Amber,
        PadColor::Yellow,
        PadColor::Lime,
        PadColor::Green,
        PadColor::Teal,
        PadColor::Cyan,
        PadColor::Blue,
        PadColor::Purple,
        PadColor::Magenta,
        PadColor::Pink,
    ];

    pub fn wire_index(self) -> u32 {
        self as u32
    }

    pub fn from_wire_index(idx: u32) -> Option<Self> {
        match idx {
            0 => Some(PadColor::Red),
            1 => Some(PadColor::Orange),
            2 => Some(PadColor::Amber),
            3 => Some(PadColor::Yellow),
            4 => Some(PadColor::Lime),
            5 => Some(PadColor::Green),
            6 => Some(PadColor::Teal),
            7 => Some(PadColor::Cyan),
            8 => Some(PadColor::Blue),
            9 => Some(PadColor::Purple),
            10 => Some(PadColor::Magenta),
            11 => Some(PadColor::Pink),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            PadColor::Red => "Red",
            PadColor::Orange => "Orange",
            PadColor::Amber => "Amber",
            PadColor::Yellow => "Yellow",
            PadColor::Lime => "Lime",
            PadColor::Green => "Green",
            PadColor::Teal => "Teal",
            PadColor::Cyan => "Cyan",
            PadColor::Blue => "Blue",
            PadColor::Purple => "Purple",
            PadColor::Magenta => "Magenta",
            PadColor::Pink => "Pink",
        }
    }
}

fn default_echo_delay() -> f64 {
    0.165
}

fn default_half() -> f64 {
    0.5
}

fn default_reverb_low_cut() -> f64 {
    0.666146
}

fn default_reverb_high_cut() -> f64 {
    0.333325
}

fn default_megaphone_intensity() -> f64 {
    0.7
}

fn default_pitch_semitones() -> f64 {
    7.0
}

fn default_fade_seconds() -> f64 {
    3.0
}

fn default_ducker_depth() -> f64 {
    -9.0
}

/// Runtime state of a single bus (mutable, persisted across restarts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusState {
    pub bus_id: String,
    pub gain: f32,
    pub mute: bool,
    pub solo: bool,
    /// If true, this bus is never muted by other busses' solo actions.
    #[serde(default)]
    pub solo_safe: bool,
}

impl BusState {
    pub fn from_config(cfg: &BusConfig) -> Self {
        Self {
            bus_id: cfg.bus_id.clone(),
            gain: cfg.default_gain,
            mute: false,
            solo: false,
            solo_safe: cfg.solo_safe,
        }
    }
}

/// Persisted daemon state (fader positions, mute states, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: u32,
    pub bus_states: Vec<BusState>,
}

/// Information about a detected hardware device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub usb_vendor_id: u16,
    pub usb_product_id: u16,
    pub serial: Option<String>,
    pub alsa_card_name: Option<String>,
    pub alsa_card_index: Option<u32>,
    pub playback_channels: u32,
    pub capture_channels: u32,
}

impl DeviceIdentity {
    /// Whether the device appears to support full multitrack mode.
    /// Duo multitrack exposes 10 playback channels.
    pub fn is_multitrack(&self) -> bool {
        self.playback_channels >= 10
    }
}

/// Capture source identity (main mix or per-fader stem).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSource {
    pub source_id: String,
    pub display_name: String,
    /// Starting channel index in the hardware capture stream.
    pub hw_channel_start: u32,
    pub channels: u32,
}

/// Default capture sources for the RØDECaster Duo multitrack capture (20 channels).
pub fn default_capture_sources() -> Vec<CaptureSource> {
    vec![
        CaptureSource {
            source_id: "main_mix".into(),
            display_name: "Main Mix".into(),
            hw_channel_start: 0,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_1".into(),
            display_name: "Fader 1 (Mic 1)".into(),
            hw_channel_start: 2,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_2".into(),
            display_name: "Fader 2 (Mic 2)".into(),
            hw_channel_start: 4,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_3".into(),
            display_name: "Fader 3 (USB Chat)".into(),
            hw_channel_start: 6,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_4".into(),
            display_name: "Fader 4 (USB System)".into(),
            hw_channel_start: 8,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_5".into(),
            display_name: "Fader 5 (USB Game)".into(),
            hw_channel_start: 10,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_6".into(),
            display_name: "Fader 6 (USB Music)".into(),
            hw_channel_start: 12,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_7".into(),
            display_name: "Fader 7 (USB Virtual A)".into(),
            hw_channel_start: 14,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_8".into(),
            display_name: "Fader 8 (USB Virtual B)".into(),
            hw_channel_start: 16,
            channels: 2,
        },
        CaptureSource {
            source_id: "fader_9".into(),
            display_name: "Fader 9 (Bluetooth)".into(),
            hw_channel_start: 18,
            channels: 2,
        },
    ]
}

// Default value helpers for serde

fn default_vendor_id() -> u16 {
    crate::RODE_VENDOR_ID
}

fn default_product_ids() -> Vec<u16> {
    vec![crate::RODECASTER_DUO_PID, crate::RODECASTER_PRO_II_PID]
}

fn default_alsa_hint() -> String {
    "RODECaster".to_string()
}

fn default_true() -> bool {
    true
}

fn default_playback() -> BusDirection {
    BusDirection::Playback
}

fn default_channels() -> u32 {
    2
}

fn default_gain_db() -> f64 {
    -12.0
}

fn default_gain() -> f32 {
    1.0
}

fn default_priority() -> i32 {
    50
}

pub fn default_busses() -> Vec<BusConfig> {
    vec![
        BusConfig {
            bus_id: "system".into(),
            display_name: "System".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: true, // System audio should never be silenced by solo
        },
        BusConfig {
            bus_id: "chat".into(),
            display_name: "Chat".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: false,
        },
        BusConfig {
            bus_id: "game".into(),
            display_name: "Game".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: false,
        },
        BusConfig {
            bus_id: "music".into(),
            display_name: "Music".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 0.8,
            solo_safe: false,
        },
        BusConfig {
            bus_id: "a".into(),
            display_name: "Virtual A".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: false,
        },
        BusConfig {
            bus_id: "b".into(),
            display_name: "Virtual B".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: false,
        },
    ]
}

impl Config {
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, crate::error::RodeError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::error::RodeError::ConfigLoad(path.display().to_string(), e.to_string())
        })?;
        let config: Config = serde_json::from_str(&content)
            .map_err(|e| crate::error::RodeError::ConfigParse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), crate::error::RodeError> {
        if self.version != 1 {
            return Err(crate::error::RodeError::ConfigParse(format!(
                "Unsupported config version: {} (expected 1)",
                self.version
            )));
        }
        for rule in &self.app_rules {
            if !rule.match_criteria.has_any_criteria() {
                return Err(crate::error::RodeError::ConfigParse(format!(
                    "App rule for bus '{}' has no match criteria",
                    rule.target_bus_id
                )));
            }
            // Validate regexes
            if let Some(ref re) = rule.match_criteria.process_name_regex {
                regex::Regex::new(re).map_err(|e| {
                    crate::error::RodeError::ConfigParse(format!("Invalid regex '{}': {}", re, e))
                })?;
            }
            if let Some(ref re) = rule.match_criteria.app_name_regex {
                regex::Regex::new(re).map_err(|e| {
                    crate::error::RodeError::ConfigParse(format!("Invalid regex '{}': {}", re, e))
                })?;
            }
            if let Some(ref re) = rule.match_criteria.client_name_regex {
                regex::Regex::new(re).map_err(|e| {
                    crate::error::RodeError::ConfigParse(format!("Invalid regex '{}': {}", re, e))
                })?;
            }
        }
        for bus in &self.busses {
            if bus.default_gain < 0.0 || bus.default_gain > 1.0 {
                return Err(crate::error::RodeError::ConfigParse(format!(
                    "Bus '{}' gain {} out of range [0.0, 1.0]",
                    bus.bus_id, bus.default_gain
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_busses() {
        let busses = default_busses();
        assert_eq!(busses.len(), 6);
        assert_eq!(busses[0].bus_id, "system");
        assert_eq!(busses[5].bus_id, "b");
    }

    #[test]
    fn test_latency_mode_quantum() {
        assert_eq!(LatencyMode::UltraLow.suggested_quantum(), 64);
        assert_eq!(LatencyMode::Low.suggested_quantum(), 256);
    }

    #[test]
    fn test_device_identity_multitrack() {
        let device = DeviceIdentity {
            usb_vendor_id: crate::RODE_VENDOR_ID,
            usb_product_id: crate::RODECASTER_DUO_PID,
            serial: None,
            alsa_card_name: Some("RODECaster Duo".into()),
            alsa_card_index: Some(1),
            playback_channels: 10,
            capture_channels: 20,
        };
        assert!(device.is_multitrack());

        let stereo_device = DeviceIdentity {
            playback_channels: 2,
            capture_channels: 2,
            ..device
        };
        assert!(!stereo_device.is_multitrack());
    }

    #[test]
    fn test_config_parse() {
        let json = r#"{
            "version": 1,
            "device": {
                "usb_vendor_id": 6647,
                "usb_product_ids": [19],
                "alsa_card_id_hint": "RODECaster",
                "require_multitrack": true
            },
            "busses": [
                { "bus_id": "system", "display_name": "System", "direction": "playback", "channels": 2, "default_gain": 1.0 }
            ],
            "routes": [],
            "app_rules": [],
            "latency_mode": "ultra_low"
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.busses.len(), 1);
        assert_eq!(config.latency_mode, LatencyMode::UltraLow);
        config.validate().unwrap();
    }

    #[test]
    fn test_config_invalid_version() {
        let json = r#"{
            "version": 99,
            "device": { "alsa_card_id_hint": "test" }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_invalid_regex() {
        let json = r#"{
            "version": 1,
            "device": { "alsa_card_id_hint": "test" },
            "app_rules": [{
                "match": { "process_name_regex": "[invalid" },
                "target_bus_id": "system",
                "priority": 100,
                "enabled": true
            }]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_invalid_gain() {
        let json = r#"{
            "version": 1,
            "device": { "alsa_card_id_hint": "test" },
            "busses": [{
                "bus_id": "test",
                "display_name": "Test",
                "direction": "playback",
                "channels": 2,
                "default_gain": 1.5
            }]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_match_config_requires_criteria() {
        let empty = MatchConfig::default();
        assert!(!empty.has_any_criteria());

        let with_process = MatchConfig {
            process_name_regex: Some("firefox".into()),
            ..Default::default()
        };
        assert!(with_process.has_any_criteria());
    }

    #[test]
    fn test_default_capture_sources() {
        let sources = default_capture_sources();
        assert_eq!(sources.len(), 10);
        assert_eq!(sources[0].source_id, "main_mix");
        assert_eq!(sources[0].hw_channel_start, 0);
        assert_eq!(sources[9].hw_channel_start, 18);
    }

    #[test]
    fn test_bus_state_from_config_solo_safe() {
        let cfg = BusConfig {
            bus_id: "system".into(),
            display_name: "System".into(),
            direction: BusDirection::Playback,
            channels: 2,
            default_gain: 1.0,
            solo_safe: true,
        };
        let state = BusState::from_config(&cfg);
        assert!(state.solo_safe);
        assert_eq!(state.gain, 1.0);
        assert!(!state.mute);
        assert!(!state.solo);
    }

    #[test]
    fn test_bus_state_solo_safe_serde_default() {
        // Old persisted state without solo_safe should deserialize with solo_safe=false
        let json = r#"{"bus_id":"chat","gain":0.5,"mute":false,"solo":false}"#;
        let state: BusState = serde_json::from_str(json).unwrap();
        assert!(!state.solo_safe); // default is false
    }

    #[test]
    fn test_bus_state_solo_safe_serde_roundtrip() {
        let state = BusState {
            bus_id: "system".into(),
            gain: 1.0,
            mute: false,
            solo: false,
            solo_safe: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        let restored: BusState = serde_json::from_str(&json).unwrap();
        assert!(restored.solo_safe);
    }

    #[test]
    fn test_default_busses_solo_safe() {
        let busses = default_busses();
        // Only system should be solo_safe by default
        assert!(
            busses
                .iter()
                .find(|b| b.bus_id == "system")
                .unwrap()
                .solo_safe
        );
        for bus in busses.iter().filter(|b| b.bus_id != "system") {
            assert!(
                !bus.solo_safe,
                "Bus '{}' should not be solo_safe",
                bus.bus_id
            );
        }
    }
}
