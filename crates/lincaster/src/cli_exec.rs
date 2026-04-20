//! Subprocess execution of `lincasterctl` commands on behalf of the GUI.
//!
//! The GUI sends [`GuiCommand`] values through a channel; [`dispatch_command`]
//! translates each variant into the appropriate `lincasterctl` invocation.

use std::path::Path;

use lincaster_proto::{PadAssignment, SoundPadConfig};
use tracing::{debug, info, warn};

// ── Public types ─────────────────────────────────────────────────────────────

/// Commands from the GUI directed at the daemon.
pub enum GuiCommand {
    RouteStream {
        node_id: u32,
        bus_id: String,
    },
    UnrouteStream {
        node_id: u32,
    },
    SetManualOverride {
        enabled: bool,
    },
    SetPadBank {
        bank: u8,
    },
    ApplyPadConfig {
        bank: u8,
        position: u8,
        config_json: String,
    },
    ClearPad {
        bank: u8,
        position: u8,
    },
    /// Send a live pad colour change to the device.
    SetPadColor {
        bank: u8,
        position: u8,
        color: u32,
    },
    /// Enter transfer mode: HidConnect + SetTransferMode(true).
    EnterTransferMode,
    /// Exit transfer mode: unmount storage + SetTransferMode(false).
    ExitTransferMode,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Dispatch a [`GuiCommand`] by running the appropriate `lincasterctl` subcommand.
pub fn dispatch_command(cmd: GuiCommand) {
    match cmd {
        GuiCommand::RouteStream { node_id, bus_id } => {
            run(&["route-stream", &node_id.to_string(), &bus_id]);
        }

        GuiCommand::UnrouteStream { node_id } => {
            run(&["unroute-stream", &node_id.to_string()]);
        }

        GuiCommand::SetManualOverride { enabled } => {
            run(&["set-manual-override", if enabled { "on" } else { "off" }]);
        }

        GuiCommand::SetPadBank { bank } => {
            run(&["set-pad-bank", &bank.to_string()]);
        }

        GuiCommand::ApplyPadConfig {
            bank,
            position,
            config_json,
        } => {
            // Off pad → clear-pad
            let is_off = serde_json::from_str::<SoundPadConfig>(&config_json)
                .map(|c| matches!(c.assignment, PadAssignment::Off))
                .unwrap_or(false);
            if is_off {
                let pad_num = (bank as usize) * 8 + (position as usize) + 1;
                run(&["clear-pad", &pad_num.to_string()]);
                return;
            }

            // Sound pad with a local file → import-sound (handles file copy +
            // ApplyPadConfig with device path internally)
            if let Ok(config) = serde_json::from_str::<SoundPadConfig>(&config_json) {
                if let PadAssignment::Sound(ref sound) = config.assignment {
                    let path = &sound.file_path;
                    if !path.is_empty()
                        && !path.starts_with("pads/")
                        && !path.starts_with("/Application/")
                        && !path.starts_with("/run/media/")
                    {
                        let local_path = Path::new(path);
                        if local_path.is_file() {
                            let pad_num = (bank as usize) * 8 + (position as usize) + 1;
                            let color_idx = sound.color.wire_index();
                            // Errors are logged inside; either way we're done here —
                            // sending the local path to the device would be wrong.
                            if let Err(e) = import_sound(pad_num, local_path, color_idx) {
                                warn!(
                                    "Failed to import sound file: {}. \
                                     Ensure transfer mode is active.",
                                    e
                                );
                            }
                            return;
                        }
                    }
                }
            }

            // All other pad types (FX, Mixer, MIDI, Video, Sound with device
            // path) — delegate to lincasterctl apply-pad-config raw.
            let pad_num = (bank as usize) * 8 + (position as usize) + 1;
            run(&["apply-pad-config", &pad_num.to_string(), "raw", &config_json]);
        }

        GuiCommand::ClearPad { bank, position } => {
            let pad_num = (bank as usize) * 8 + (position as usize) + 1;
            run(&["clear-pad", &pad_num.to_string()]);
        }

        GuiCommand::SetPadColor {
            bank,
            position,
            color,
        } => {
            let pad_num = (bank as usize) * 8 + (position as usize) + 1;
            run(&["set-pad-color", &pad_num.to_string(), &color.to_string()]);
        }

        GuiCommand::EnterTransferMode => {
            run(&["transfer-mode", "pads"]);
        }

        GuiCommand::ExitTransferMode => {
            run(&["exit-transfer-mode"]);
        }
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Resolve the `lincasterctl` binary path.
///
/// Prefers a sibling of the current executable (both live in
/// `target/release/` or `target/debug/`), then falls back to `PATH`.
fn lincasterctl_bin() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("lincasterctl");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("lincasterctl")
}

/// Run `lincasterctl <args>`, logging the exact command at info level.
fn run(args: &[&str]) {
    let bin = lincasterctl_bin();
    info!("exec: lincasterctl {}", args.join(" "));
    match std::process::Command::new(&bin).args(args).output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if !stdout.trim().is_empty() {
                debug!("lincasterctl {}: {}", args[0], stdout.trim());
            }
        }
        Ok(out) => {
            warn!(
                "lincasterctl {} failed: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => warn!("Failed to run lincasterctl: {}", e),
    }
}

/// Run `lincasterctl import-sound <pad> <file> --color <color>` and return
/// the device-internal path parsed from stdout.
fn import_sound(pad_num: usize, file: &Path, color: u32) -> Result<String, String> {
    let bin = lincasterctl_bin();
    info!(
        "exec: lincasterctl import-sound {} {} --color {}",
        pad_num,
        file.display(),
        color
    );
    let output = std::process::Command::new(&bin)
        .args([
            "import-sound",
            &pad_num.to_string(),
            &file.display().to_string(),
            "--color",
            &color.to_string(),
        ])
        .output()
        .map_err(|e| format!("Failed to run lincasterctl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "import-sound failed: {} {}",
            stderr.trim(),
            stdout.trim()
        ));
    }

    // Output format: "Device path: /Application/emmc-data/pads/N/sound.mp3"
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("Device path: ") {
            return Ok(path.trim().to_string());
        }
    }

    // Fallback if output format changes
    Ok(format!("pads/{}/sound.mp3", pad_num))
}
