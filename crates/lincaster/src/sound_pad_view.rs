use std::path::PathBuf;

use egui::{Color32, Rounding, Stroke, Vec2};
use lincaster_proto::{
    EffectConfig, FxInputSource, LatchMode, MixerMode, MixerPadConfig, PadAssignment, PadColor,
    PlayMode, ReplayMode, ReverbEffect, ReverbModel, SoundConfig, SoundPadConfig, TriggerPadConfig,
    TriggerType,
};

const NUM_PADS_DUO: usize = 6;
const NUM_PADS_PRO_II: usize = 8;
const NUM_BANKS: usize = 8;
const PAD_SIZE: f32 = 80.0;
const PAD_SPACING: f32 = 12.0;

/// Actions returned by the sound pad view for the main loop to dispatch.
pub enum PadAction {
    /// Apply the given pad config to the device.
    ApplyConfig {
        bank: u8,
        position: u8,
        config: SoundPadConfig,
    },
    /// Clear/reset a pad to Off.
    ClearPad { bank: u8, position: u8 },
    /// Change the active bank on the device.
    SetBank(u8),
    /// A Sound pad was selected or a pad was changed to Sound type;
    /// the main loop should ensure transfer mode is active.
    NeedTransferMode,
    /// Live colour change — send immediately to the device.
    SetPadColor {
        bank: u8,
        position: u8,
        color: PadColor,
    },
}

/// State for the sound pad configuration view.
pub struct SoundPadState {
    /// All pads across all banks: banks[bank_index][pad_index]
    pub banks: Vec<Vec<SoundPadConfig>>,
    pub current_bank: usize,
    pub selected_pad: Option<usize>,
    /// Number of pads per bank (6 for Duo, 8 for Pro).
    pub pads_per_bank: usize,
    /// Mount point of the RØDECaster's internal storage, if detected.
    pub device_mount: Option<PathBuf>,
    /// Timer for periodic storage detection.
    mount_check_counter: u32,
}

impl SoundPadState {
    pub fn new(pads_per_bank: usize) -> Self {
        let banks = (0..NUM_BANKS)
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
        Self {
            banks,
            current_bank: 0,
            selected_pad: None,
            pads_per_bank,
            device_mount: lincaster_proto::storage::find_device_mount(),
            mount_check_counter: 0,
        }
    }

    /// Create for RØDECaster Duo (6 pads).
    pub fn new_duo() -> Self {
        Self::new(NUM_PADS_DUO)
    }

    /// Create for RØDECaster Pro (8 pads).
    #[allow(dead_code)]
    pub fn new_pro_ii() -> Self {
        Self::new(NUM_PADS_PRO_II)
    }
}

impl Default for SoundPadState {
    fn default() -> Self {
        Self::new_duo()
    }
}

pub fn draw_sound_pad_view(ui: &mut egui::Ui, state: &mut SoundPadState) -> Vec<PadAction> {
    let mut actions: Vec<PadAction> = Vec::new();
    ui.add_space(8.0);

    // Periodically re-check for device storage mount (every ~50 frames ≈ 10s)
    state.mount_check_counter += 1;
    if state.mount_check_counter >= 50 || state.device_mount.is_none() {
        state.mount_check_counter = 0;
        state.device_mount = lincaster_proto::storage::find_device_mount();
    }

    // Bank selector with prev/next buttons
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.label(
            egui::RichText::new("SMART Pads")
                .size(16.0)
                .color(Color32::from_rgb(200, 200, 210)),
        );
        ui.add_space(16.0);

        // Storage status indicator
        if state.device_mount.is_some() {
            ui.label(
                egui::RichText::new("Storage: mounted")
                    .size(10.0)
                    .color(Color32::from_rgb(80, 180, 80)),
            );
        } else {
            ui.label(
                egui::RichText::new("Storage: not found")
                    .size(10.0)
                    .color(Color32::from_rgb(180, 130, 60)),
            );
        }
        ui.add_space(8.0);

        if ui.button("◀").clicked() && state.current_bank > 0 {
            state.current_bank -= 1;
            state.selected_pad = None;
            actions.push(PadAction::SetBank(state.current_bank as u8));
        }

        for bank in 0..NUM_BANKS {
            let label = format!("{}", bank + 1);
            let selected = state.current_bank == bank;
            if ui.selectable_label(selected, &label).clicked() && !selected {
                state.current_bank = bank;
                state.selected_pad = None;
                actions.push(PadAction::SetBank(state.current_bank as u8));
            }
        }

        if ui.button("▶").clicked() && state.current_bank < NUM_BANKS - 1 {
            state.current_bank += 1;
            state.selected_pad = None;
            actions.push(PadAction::SetBank(state.current_bank as u8));
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("Bank {}", state.current_bank + 1))
                .size(12.0)
                .color(Color32::from_rgb(140, 140, 150)),
        );
    });

    ui.add_space(8.0);

    let pads_per_col = state.pads_per_bank / 2;
    let prev_selected = state.selected_pad;

    // Pad grid: left column (1-3 or 1-4), right column (4-6 or 5-8)
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        // Left column
        ui.vertical(|ui| {
            for i in 0..pads_per_col {
                draw_pad_button(ui, state, i);
                if i < pads_per_col - 1 {
                    ui.add_space(PAD_SPACING);
                }
            }
        });
        ui.add_space(PAD_SPACING);
        // Right column
        ui.vertical(|ui| {
            for i in pads_per_col..state.pads_per_bank {
                draw_pad_button(ui, state, i);
                if i < state.pads_per_bank - 1 {
                    ui.add_space(PAD_SPACING);
                }
            }
        });
    });

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    // If the user just selected a Sound pad, request transfer mode
    if state.selected_pad != prev_selected {
        if let Some(idx) = state.selected_pad {
            let bank = state.current_bank;
            if matches!(state.banks[bank][idx].assignment, PadAssignment::Sound(_)) {
                actions.push(PadAction::NeedTransferMode);
            }
        }
    }

    // Configuration panel for selected pad
    if let Some(idx) = state.selected_pad {
        let bank = state.current_bank;
        let pad_idx = bank * state.pads_per_bank + idx;
        let device_mount = state.device_mount.clone();
        let is_sound = matches!(state.banks[bank][idx].assignment, PadAssignment::Sound(_));
        for pad_action in draw_pad_config(
            ui,
            &mut state.banks[bank][idx],
            bank as u8,
            idx as u8,
            pad_idx,
            device_mount.as_deref(),
        ) {
            // If the pad was just changed to Sound, request transfer mode
            if matches!(&pad_action, PadAction::ApplyConfig { config, .. } if matches!(config.assignment, PadAssignment::Sound(_)))
            {
                actions.push(PadAction::NeedTransferMode);
            }
            actions.push(pad_action);
        }
        // Check if the pad type just changed to Sound (user clicked "Sound" type selector)
        if !is_sound && matches!(state.banks[bank][idx].assignment, PadAssignment::Sound(_)) {
            actions.push(PadAction::NeedTransferMode);
        }
    } else {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new("Select a pad above to configure it")
                    .size(13.0)
                    .color(Color32::from_rgb(140, 140, 150)),
            );
        });
    }

    actions
}

fn draw_pad_button(ui: &mut egui::Ui, state: &mut SoundPadState, index: usize) {
    let pad = &state.banks[state.current_bank][index];
    let is_selected = state.selected_pad == Some(index);

    let color = pad_assignment_color(&pad.assignment);
    let bg = if is_selected {
        lighten(color, 0.3)
    } else {
        darken(color, 0.2)
    };
    let border = if is_selected {
        lighten(color, 0.2)
    } else {
        color
    };

    let label = if !pad.name.is_empty() {
        truncate_str(&pad.name, 10)
    } else {
        pad_short_label(pad)
    };

    let (response, painter) =
        ui.allocate_painter(Vec2::new(PAD_SIZE, PAD_SIZE), egui::Sense::click());
    let rect = response.rect;

    painter.rect_filled(rect, Rounding::same(8.0), bg);
    painter.rect_stroke(
        rect,
        Rounding::same(8.0),
        Stroke::new(if is_selected { 2.5 } else { 1.0 }, border),
    );

    // Pad number
    painter.text(
        egui::Pos2::new(rect.min.x + 8.0, rect.min.y + 6.0),
        egui::Align2::LEFT_TOP,
        format!("{}", index + 1),
        egui::FontId::proportional(11.0),
        Color32::from_rgb(180, 180, 190),
    );

    // Type indicator (top-right)
    let type_label = pad_type_label(&pad.assignment);
    if !type_label.is_empty() {
        painter.text(
            egui::Pos2::new(rect.max.x - 6.0, rect.min.y + 6.0),
            egui::Align2::RIGHT_TOP,
            type_label,
            egui::FontId::proportional(9.0),
            Color32::from_rgb(150, 150, 160),
        );
    }

    // Assignment label
    painter.text(
        egui::Pos2::new(rect.center().x, rect.center().y + 6.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        Color32::from_rgb(220, 220, 230),
    );

    if response.clicked() {
        state.selected_pad = Some(index);
    }
}

fn draw_pad_config(
    ui: &mut egui::Ui,
    pad: &mut SoundPadConfig,
    bank: u8,
    position: u8,
    pad_idx: usize,
    device_mount: Option<&std::path::Path>,
) -> Vec<PadAction> {
    let mut actions: Vec<PadAction> = Vec::new();
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(format!("Pad {} Configuration", pad.pad_index + 1))
                    .size(15.0)
                    .color(Color32::from_rgb(200, 200, 210)),
            );
            ui.add_space(4.0);

            // Pad name
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut pad.name);
            });

            ui.add_space(8.0);

            // Assignment type selector
            ui.horizontal(|ui| {
                ui.label("Type:");
                let is_off = matches!(pad.assignment, PadAssignment::Off);
                let is_sound = matches!(pad.assignment, PadAssignment::Sound(_));
                let is_fx = matches!(pad.assignment, PadAssignment::Effect(_));
                let is_mixer = matches!(pad.assignment, PadAssignment::Mixer(_));
                let is_trigger = matches!(pad.assignment, PadAssignment::Trigger(_));

                if ui.selectable_label(is_off, "Off").clicked() && !is_off {
                    pad.assignment = PadAssignment::Off;
                }
                if ui.selectable_label(is_sound, "Sound").clicked() && !is_sound {
                    pad.assignment = PadAssignment::Sound(SoundConfig {
                        file_path: String::new(),
                        play_mode: PlayMode::OneShot,
                        gain_db: -12.0,
                        color: PadColor::Blue,
                        loop_enabled: false,
                        replay_mode: ReplayMode::Replay,
                    });
                }
                if ui.selectable_label(is_fx, "FX").clicked() && !is_fx {
                    let fx = EffectConfig {
                        color: PadColor::Green,
                        reverb: ReverbEffect {
                            enabled: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    pad.assignment = PadAssignment::Effect(fx);
                }
                if ui.selectable_label(is_mixer, "Mixer").clicked() && !is_mixer {
                    pad.assignment = PadAssignment::Mixer(MixerPadConfig {
                        mode: MixerMode::Censor,
                        color: PadColor::Yellow,
                        latch_mode: LatchMode::Latch,
                        censor_custom: false,
                        censor_file_path: String::new(),
                        beep_gain_db: -12.0,
                        fade_in_seconds: 3.0,
                        fade_out_seconds: 3.0,
                        fade_exclude_host: false,
                        back_channel_mic2: false,
                        back_channel_mic3: false,
                        back_channel_mic4: false,
                        back_channel_usb1_comms: false,
                        back_channel_usb2_main: false,
                        back_channel_bluetooth: false,
                        back_channel_callme1: false,
                        back_channel_callme2: false,
                        back_channel_callme3: false,
                        ducker_depth_db: -9.0,
                    });
                }
                if ui.selectable_label(is_trigger, "MIDI").clicked() && !is_trigger {
                    pad.assignment = PadAssignment::Trigger(TriggerPadConfig {
                        trigger_type: TriggerType::MidiNote {
                            channel: 1,
                            note: 60,
                            velocity: 127,
                        },
                        color: PadColor::Purple,
                    });
                }
            });

            ui.add_space(6.0);

            match &mut pad.assignment {
                PadAssignment::Off => {
                    ui.label(
                        egui::RichText::new("Pad is disabled")
                            .color(Color32::from_rgb(140, 140, 150)),
                    );
                }
                PadAssignment::Sound(sound) => {
                    draw_sound_config(ui, sound, &mut pad.name, device_mount, pad_idx);
                }
                PadAssignment::Effect(effect) => {
                    draw_effect_config(ui, effect);
                }
                PadAssignment::Mixer(mixer) => {
                    draw_mixer_config(ui, mixer);
                }
                PadAssignment::Trigger(trigger) => {
                    draw_trigger_config(ui, trigger);
                }
            }

            ui.add_space(12.0);

            // Color picker — send live colour changes to the device
            if let Some(color) = draw_color_picker(ui, &mut pad.assignment) {
                actions.push(PadAction::SetPadColor {
                    bank,
                    position,
                    color,
                });
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Apply to Device").clicked() {
                    actions.push(PadAction::ApplyConfig {
                        bank,
                        position,
                        config: pad.clone(),
                    });
                }
                if !matches!(pad.assignment, PadAssignment::Off) && ui.button("Clear Pad").clicked()
                {
                    pad.assignment = PadAssignment::Off;
                    pad.name.clear();
                    actions.push(PadAction::ClearPad { bank, position });
                }
            });
        });
    });
    actions
}

fn draw_sound_config(
    ui: &mut egui::Ui,
    sound: &mut SoundConfig,
    pad_name: &mut String,
    device_mount: Option<&std::path::Path>,
    pad_idx: usize,
) {
    ui.horizontal(|ui| {
        ui.label("File:");
        ui.add(egui::TextEdit::singleline(&mut sound.file_path).desired_width(200.0));

        if ui.button("Browse...").clicked() {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("Audio", &["wav", "mp3"])
                .set_title("Select sound file to import");
            // Start in home directory so users pick local files, not device files
            if let Ok(home) = std::env::var("HOME") {
                dialog = dialog.set_directory(&home);
            }
            if let Some(path) = dialog.pick_file() {
                // Reject paths on the device mount — those are already on the device
                let path_str = path.display().to_string();
                if path_str.starts_with("/run/media/") {
                    tracing::warn!("Selected file is on device storage, not a local file");
                } else {
                    sound.file_path = path_str;
                    if pad_name.is_empty() {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            *pad_name = stem.to_string();
                        }
                    }
                }
            }
        }

        // Export button — only shown when there's a sound file on the device
        if device_mount.is_some() {
            let has_file = device_mount
                .and_then(|m| lincaster_proto::storage::find_pad_sound_file(m, pad_idx))
                .is_some();
            if has_file && ui.button("Export").clicked() {
                let default_name = sound
                    .file_path
                    .rsplit('/')
                    .next()
                    .unwrap_or("sound.wav")
                    .to_string();
                if let Some(dest) = rfd::FileDialog::new()
                    .add_filter("Audio", &["wav", "mp3"])
                    .set_file_name(&default_name)
                    .set_title("Export sound from pad")
                    .save_file()
                {
                    if let Some(mount) = device_mount {
                        if let Err(e) =
                            lincaster_proto::storage::export_sound_file(mount, pad_idx, &dest)
                        {
                            tracing::error!("Failed to export sound file: {}", e);
                        }
                    }
                }
            }
        }
    });

    // Default pad name to filename if name is empty and a file path is entered
    if pad_name.is_empty() && !sound.file_path.is_empty() {
        if let Some(filename) = sound.file_path.rsplit('/').next() {
            if let Some(stem) = filename.rsplit_once('.') {
                *pad_name = stem.0.to_string();
            } else {
                *pad_name = filename.to_string();
            }
        }
    }

    ui.horizontal(|ui| {
        ui.label("Play Mode:");
        egui::ComboBox::from_id_salt("play_mode")
            .selected_text(play_mode_label(sound.play_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sound.play_mode, PlayMode::OneShot, "One Shot");
                ui.selectable_value(&mut sound.play_mode, PlayMode::Toggle, "Toggle");
                ui.selectable_value(&mut sound.play_mode, PlayMode::Hold, "Hold");
            });
    });

    ui.horizontal(|ui| {
        ui.checkbox(&mut sound.loop_enabled, "Loop");
    });

    ui.horizontal(|ui| {
        ui.label("On Re-trigger:");
        egui::ComboBox::from_id_salt("replay_mode")
            .selected_text(replay_mode_label(sound.replay_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut sound.replay_mode,
                    ReplayMode::Replay,
                    "Replay (restart)",
                );
                ui.selectable_value(
                    &mut sound.replay_mode,
                    ReplayMode::Continue,
                    "Continue (resume)",
                );
            });
    });

    ui.horizontal(|ui| {
        ui.label("Gain:");
        ui.add(egui::Slider::new(&mut sound.gain_db, -60.0..=0.0).suffix(" dB"));
    });
}

fn draw_effect_config(ui: &mut egui::Ui, effect: &mut EffectConfig) {
    // Trigger mode
    ui.horizontal(|ui| {
        ui.label("Trigger:");
        egui::ComboBox::from_id_salt("latch_mode")
            .selected_text(latch_mode_label(effect.latch_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut effect.latch_mode, LatchMode::Latch, "Latch (toggle)");
                ui.selectable_value(
                    &mut effect.latch_mode,
                    LatchMode::Momentary,
                    "Momentary (hold)",
                );
            });
    });

    // Input source
    ui.horizontal(|ui| {
        ui.label("Input:");
        egui::ComboBox::from_id_salt("fx_input")
            .selected_text(fx_input_label(effect.input_source))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut effect.input_source, FxInputSource::Mic1, "Mic 1");
                ui.selectable_value(&mut effect.input_source, FxInputSource::Mic2, "Mic 2");
                ui.selectable_value(
                    &mut effect.input_source,
                    FxInputSource::Wireless1,
                    "Wireless 1",
                );
                ui.selectable_value(
                    &mut effect.input_source,
                    FxInputSource::Wireless2,
                    "Wireless 2",
                );
            });
    });

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Effects (toggle each on/off independently)")
            .size(13.0)
            .color(Color32::from_rgb(180, 180, 190)),
    );
    ui.add_space(4.0);

    // ── Reverb ──
    ui.checkbox(&mut effect.reverb.enabled, "Reverb");
    if effect.reverb.enabled {
        ui.indent("reverb_params", |ui| {
            ui.horizontal(|ui| {
                ui.label("Room:");
                egui::ComboBox::from_id_salt("reverb_model")
                    .selected_text(reverb_model_label(effect.reverb.model))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut effect.reverb.model,
                            ReverbModel::SmallRoom,
                            "Small Room",
                        );
                        ui.selectable_value(
                            &mut effect.reverb.model,
                            ReverbModel::MediumRoom,
                            "Medium Room",
                        );
                        ui.selectable_value(
                            &mut effect.reverb.model,
                            ReverbModel::LargeRoom,
                            "Large Room",
                        );
                        ui.selectable_value(
                            &mut effect.reverb.model,
                            ReverbModel::SmallHall,
                            "Small Hall",
                        );
                        ui.selectable_value(
                            &mut effect.reverb.model,
                            ReverbModel::LargeHall,
                            "Large Hall",
                        );
                    });
            });
            ui.horizontal(|ui| {
                ui.label("Level:");
                ui.add(egui::Slider::new(&mut effect.reverb.mix, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("Low Cut:");
                ui.add(egui::Slider::new(&mut effect.reverb.low_cut, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("High Cut:");
                ui.add(egui::Slider::new(&mut effect.reverb.high_cut, 0.0..=1.0));
            });
        });
    }

    // ── Echo ──
    ui.checkbox(&mut effect.echo.enabled, "Echo");
    if effect.echo.enabled {
        ui.indent("echo_params", |ui| {
            ui.horizontal(|ui| {
                ui.label("Level:");
                ui.add(egui::Slider::new(&mut effect.echo.mix, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("Low Cut:");
                ui.add(egui::Slider::new(&mut effect.echo.low_cut, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("High Cut:");
                ui.add(egui::Slider::new(&mut effect.echo.high_cut, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("Delay:");
                ui.add(egui::Slider::new(&mut effect.echo.delay, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                ui.label("Decay:");
                ui.add(egui::Slider::new(&mut effect.echo.decay, 0.0..=1.0));
            });
        });
    }

    // ── Megaphone ──
    ui.checkbox(&mut effect.megaphone.enabled, "Megaphone");
    if effect.megaphone.enabled {
        ui.indent("megaphone_params", |ui| {
            ui.horizontal(|ui| {
                ui.label("Intensity:");
                let mut level = (effect.megaphone.intensity * 9.0).round() as i32;
                if ui.add(egui::Slider::new(&mut level, 0..=9)).changed() {
                    effect.megaphone.intensity = level as f64 / 9.0;
                }
            });
        });
    }

    // ── Robot ──
    ui.checkbox(&mut effect.robot.enabled, "Robot");
    if effect.robot.enabled {
        ui.indent("robot_params", |ui| {
            ui.horizontal(|ui| {
                ui.label("Mix:");
                let mut level = (effect.robot.mix * 3.0).round() as i32;
                if ui.add(egui::Slider::new(&mut level, 0..=2)).changed() {
                    effect.robot.mix = level as f64 / 3.0;
                }
            });
        });
    }

    // ── Voice Disguise ──
    ui.checkbox(&mut effect.voice_disguise.enabled, "Voice Disguise");

    // ── Pitch Shift ──
    ui.checkbox(&mut effect.pitch_shift.enabled, "Pitch Shift");
    if effect.pitch_shift.enabled {
        ui.indent("pitch_params", |ui| {
            ui.horizontal(|ui| {
                ui.label("Semitones:");
                ui.add(
                    egui::Slider::new(&mut effect.pitch_shift.semitones, -12.0..=12.0)
                        .suffix(" st"),
                );
            });
        });
    }
}

fn draw_mixer_config(ui: &mut egui::Ui, mixer: &mut MixerPadConfig) {
    // Mode selector
    ui.horizontal(|ui| {
        ui.label("Mode:");
        egui::ComboBox::from_id_salt("mixer_mode")
            .selected_text(mixer_mode_label(mixer.mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mixer.mode, MixerMode::Censor, "Censor");
                ui.selectable_value(&mut mixer.mode, MixerMode::TrashTalk, "Trash Talk");
                ui.selectable_value(&mut mixer.mode, MixerMode::FadeInOut, "Fade In/Out");
                ui.selectable_value(&mut mixer.mode, MixerMode::BackChannel, "Back Channel");
                ui.selectable_value(&mut mixer.mode, MixerMode::Ducking, "Ducking");
            });
    });

    // Trigger mode (latch/momentary)
    ui.horizontal(|ui| {
        ui.label("Trigger:");
        egui::ComboBox::from_id_salt("mixer_latch_mode")
            .selected_text(latch_mode_label(mixer.latch_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mixer.latch_mode, LatchMode::Latch, "Latch (toggle)");
                ui.selectable_value(
                    &mut mixer.latch_mode,
                    LatchMode::Momentary,
                    "Momentary (hold)",
                );
            });
    });

    ui.add_space(4.0);

    // Mode-specific parameters
    match mixer.mode {
        MixerMode::Censor => {
            ui.horizontal(|ui| {
                ui.label("Beep Tone Trim:");
                ui.add(egui::Slider::new(&mut mixer.beep_gain_db, -20.0..=0.0).suffix(" dB"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.censor_custom, "Custom Censor Sound");
            });
            if mixer.censor_custom {
                ui.horizontal(|ui| {
                    ui.label("File:");
                    ui.text_edit_singleline(&mut mixer.censor_file_path);
                    if ui.button("Browse...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Audio", &["wav", "mp3"])
                            .set_title("Select censor sound file")
                            .pick_file()
                        {
                            mixer.censor_file_path = path.display().to_string();
                        }
                    }
                });
            }
        }
        MixerMode::TrashTalk => {
            ui.label(
                egui::RichText::new("Mutes other channels when pad is active")
                    .size(12.0)
                    .color(Color32::from_rgb(140, 140, 150)),
            );
        }
        MixerMode::FadeInOut => {
            ui.horizontal(|ui| {
                ui.label("Fade In:");
                ui.add(egui::Slider::new(&mut mixer.fade_in_seconds, 0.5..=10.0).suffix(" s"));
            });
            ui.horizontal(|ui| {
                ui.label("Fade Out:");
                ui.add(egui::Slider::new(&mut mixer.fade_out_seconds, 0.5..=10.0).suffix(" s"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.fade_exclude_host, "Exclude Host Output");
            });
        }
        MixerMode::BackChannel => {
            ui.label(
                egui::RichText::new("Route audio to selected channels:")
                    .size(12.0)
                    .color(Color32::from_rgb(180, 180, 190)),
            );
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.back_channel_mic2, "Mic 2");
                ui.checkbox(&mut mixer.back_channel_mic3, "Mic 3");
                ui.checkbox(&mut mixer.back_channel_mic4, "Mic 4");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.back_channel_usb1_comms, "USB 1 Comms");
                ui.checkbox(&mut mixer.back_channel_usb2_main, "USB 2 Main");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.back_channel_bluetooth, "Bluetooth");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut mixer.back_channel_callme1, "CallMe 1");
                ui.checkbox(&mut mixer.back_channel_callme2, "CallMe 2");
                ui.checkbox(&mut mixer.back_channel_callme3, "CallMe 3");
            });
        }
        MixerMode::Ducking => {
            ui.horizontal(|ui| {
                ui.label("Ducking Depth:");
                ui.add(egui::Slider::new(&mut mixer.ducker_depth_db, -12.0..=-6.0).suffix(" dB"));
            });
            ui.label(
                egui::RichText::new("Note: Ducking depth is a global setting")
                    .size(11.0)
                    .color(Color32::from_rgb(140, 140, 150)),
            );
        }
    }
}

fn draw_trigger_config(ui: &mut egui::Ui, trigger: &mut TriggerPadConfig) {
    match &mut trigger.trigger_type {
        TriggerType::MidiNote {
            channel,
            note,
            velocity,
        } => {
            ui.label(
                egui::RichText::new("MIDI Note Trigger")
                    .size(13.0)
                    .color(Color32::from_rgb(180, 180, 190)),
            );
            ui.horizontal(|ui| {
                ui.label("Channel:");
                let mut ch = *channel as i32;
                if ui
                    .add(egui::DragValue::new(&mut ch).range(1..=16))
                    .changed()
                {
                    *channel = ch as u8;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Note:");
                let mut n = *note as i32;
                if ui
                    .add(egui::DragValue::new(&mut n).range(0..=127))
                    .changed()
                {
                    *note = n as u8;
                }
                ui.label(
                    egui::RichText::new(midi_note_name(*note))
                        .size(11.0)
                        .color(Color32::from_rgb(140, 140, 150)),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Velocity:");
                let mut v = *velocity as i32;
                if ui
                    .add(egui::DragValue::new(&mut v).range(0..=127))
                    .changed()
                {
                    *velocity = v as u8;
                }
            });
        }
    }
}

fn draw_color_picker(ui: &mut egui::Ui, assignment: &mut PadAssignment) -> Option<PadColor> {
    let current_color = match assignment {
        PadAssignment::Off => return None,
        PadAssignment::Sound(s) => &mut s.color,
        PadAssignment::Effect(e) => &mut e.color,
        PadAssignment::Mixer(m) => &mut m.color,
        PadAssignment::Trigger(t) => &mut t.color,
    };

    let mut selected = None;
    ui.horizontal(|ui| {
        ui.label("Pad Color:");
        for color in PadColor::ALL {
            let rgb = pad_color_rgb(color);
            let is_selected = *current_color == color;
            let size = if is_selected { 20.0 } else { 16.0 };
            let (response, painter) =
                ui.allocate_painter(Vec2::splat(size + 4.0), egui::Sense::click());
            let center = response.rect.center();
            painter.circle_filled(center, size / 2.0, Color32::from_rgb(rgb.0, rgb.1, rgb.2));
            if is_selected {
                painter.circle_stroke(center, size / 2.0 + 2.0, Stroke::new(1.5, Color32::WHITE));
            }
            if response.clicked() {
                *current_color = color;
                selected = Some(color);
            }
            response.on_hover_text(color.display_name());
        }
    });
    selected
}

// ── Helper functions ─────────────────────────────────────────────────

fn pad_assignment_color(assignment: &PadAssignment) -> Color32 {
    match assignment {
        PadAssignment::Off => Color32::from_rgb(100, 100, 110),
        PadAssignment::Sound(s) => {
            let (r, g, b) = pad_color_rgb(s.color);
            Color32::from_rgb(r, g, b)
        }
        PadAssignment::Effect(e) => {
            let (r, g, b) = pad_color_rgb(e.color);
            Color32::from_rgb(r, g, b)
        }
        PadAssignment::Mixer(m) => {
            let (r, g, b) = pad_color_rgb(m.color);
            Color32::from_rgb(r, g, b)
        }
        PadAssignment::Trigger(t) => {
            let (r, g, b) = pad_color_rgb(t.color);
            Color32::from_rgb(r, g, b)
        }
    }
}

fn pad_short_label(pad: &SoundPadConfig) -> String {
    match &pad.assignment {
        PadAssignment::Off => "OFF".into(),
        PadAssignment::Sound(s) => {
            let filename = s.file_path.rsplit('/').next().unwrap_or(&s.file_path);
            if filename.is_empty() {
                "Sound".into()
            } else {
                truncate_str(filename, 10)
            }
        }
        PadAssignment::Effect(e) => e.active_effects_summary(),
        PadAssignment::Mixer(m) => mixer_mode_label(m.mode).into(),
        PadAssignment::Trigger(_) => "MIDI".into(),
    }
}

fn pad_type_label(assignment: &PadAssignment) -> &'static str {
    match assignment {
        PadAssignment::Off => "",
        PadAssignment::Sound(_) => "SND",
        PadAssignment::Effect(_) => "FX",
        PadAssignment::Mixer(_) => "MIX",
        PadAssignment::Trigger(_) => "TRG",
    }
}

fn pad_color_rgb(color: PadColor) -> (u8, u8, u8) {
    match color {
        PadColor::Red => (200, 60, 60),
        PadColor::Orange => (220, 140, 40),
        PadColor::Amber => (210, 170, 30),
        PadColor::Yellow => (220, 200, 50),
        PadColor::Lime => (140, 200, 50),
        PadColor::Green => (60, 180, 80),
        PadColor::Teal => (40, 180, 160),
        PadColor::Cyan => (40, 180, 220),
        PadColor::Blue => (60, 100, 200),
        PadColor::Purple => (140, 60, 200),
        PadColor::Magenta => (200, 60, 180),
        PadColor::Pink => (200, 80, 130),
    }
}

fn lighten(c: Color32, amount: f32) -> Color32 {
    let r = (c.r() as f32 + (255.0 - c.r() as f32) * amount) as u8;
    let g = (c.g() as f32 + (255.0 - c.g() as f32) * amount) as u8;
    let b = (c.b() as f32 + (255.0 - c.b() as f32) * amount) as u8;
    Color32::from_rgb(r, g, b)
}

fn darken(c: Color32, amount: f32) -> Color32 {
    let r = (c.r() as f32 * (1.0 - amount)) as u8;
    let g = (c.g() as f32 * (1.0 - amount)) as u8;
    let b = (c.b() as f32 * (1.0 - amount)) as u8;
    Color32::from_rgb(r, g, b)
}

fn play_mode_label(mode: PlayMode) -> &'static str {
    match mode {
        PlayMode::OneShot => "One Shot",
        PlayMode::Toggle => "Toggle",
        PlayMode::Hold => "Hold",
    }
}

fn replay_mode_label(mode: ReplayMode) -> &'static str {
    match mode {
        ReplayMode::Replay => "Replay",
        ReplayMode::Continue => "Continue",
    }
}

fn latch_mode_label(mode: LatchMode) -> &'static str {
    match mode {
        LatchMode::Latch => "Latch",
        LatchMode::Momentary => "Momentary",
    }
}

fn mixer_mode_label(mode: MixerMode) -> &'static str {
    match mode {
        MixerMode::Censor => "Censor",
        MixerMode::TrashTalk => "Trash Talk",
        MixerMode::FadeInOut => "Fade In/Out",
        MixerMode::BackChannel => "Back Channel",
        MixerMode::Ducking => "Ducking",
    }
}

fn reverb_model_label(model: ReverbModel) -> &'static str {
    match model {
        ReverbModel::SmallRoom => "Small Room",
        ReverbModel::MediumRoom => "Medium Room",
        ReverbModel::LargeRoom => "Large Room",
        ReverbModel::SmallHall => "Small Hall",
        ReverbModel::LargeHall => "Large Hall",
    }
}

fn fx_input_label(input: FxInputSource) -> &'static str {
    match input {
        FxInputSource::Mic1 => "Mic 1",
        FxInputSource::Mic2 => "Mic 2",
        FxInputSource::Wireless1 => "Wireless 1",
        FxInputSource::Wireless2 => "Wireless 2",
    }
}

fn midi_note_name(note: u8) -> String {
    let names = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i8 - 1;
    let name = names[(note % 12) as usize];
    format!("{}{}", name, octave)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}
