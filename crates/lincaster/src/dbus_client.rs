use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dbus::blocking::Connection;
use lincaster_proto::{BusState, DeviceIdentity, PadAssignment, SoundPadConfig, StreamSnapshot};
use tracing::{debug, warn};

const DBUS_NAME: &str = "com.lincaster.Daemon";
const DBUS_PATH: &str = "/com/lincaster/Daemon";
const DBUS_IFACE: &str = "com.lincaster.Daemon";
const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Find the lincasterctl binary.
///
/// Looks for it next to the current executable first (both live in
/// target/release/ or target/debug/), then falls back to PATH.
fn lincasterctl_bin() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("lincasterctl");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("lincasterctl")
}

/// Updates sent from the DBus communication thread to the GUI.
pub enum DaemonUpdate {
    /// Fresh state from the daemon.
    State {
        busses: Vec<BusInfo>,
        streams: Vec<StreamSnapshot>,
        device: Option<DeviceIdentity>,
        pad_configs: Vec<Vec<SoundPadConfig>>,
    },
    /// Daemon is not reachable.
    Disconnected,
}

/// Bus information combining config display name + runtime state.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BusInfo {
    pub bus_id: String,
    pub display_name: String,
    pub gain: f32,
    pub mute: bool,
    pub solo: bool,
}

/// Commands from the GUI to the daemon.
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

/// Start the background DBus communication thread.
/// Returns channels for receiving updates and sending commands.
pub fn start_comm_thread() -> (mpsc::Receiver<DaemonUpdate>, mpsc::Sender<GuiCommand>) {
    let (update_tx, update_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();

    thread::Builder::new()
        .name("dbus-gui-comm".into())
        .spawn(move || {
            comm_loop(update_tx, cmd_rx);
        })
        .expect("Failed to spawn DBus comm thread");

    (update_rx, cmd_tx)
}

fn comm_loop(update_tx: mpsc::Sender<DaemonUpdate>, cmd_rx: mpsc::Receiver<GuiCommand>) {
    loop {
        let conn = match Connection::new_session() {
            Ok(c) => c,
            Err(e) => {
                warn!("DBus connect failed: {}", e);
                let _ = update_tx.send(DaemonUpdate::Disconnected);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        let proxy = conn.with_proxy(DBUS_NAME, DBUS_PATH, DBUS_TIMEOUT);

        loop {
            // Process GUI commands
            let mut had_command = false;
            while let Ok(cmd) = cmd_rx.try_recv() {
                had_command = true;
                match cmd {
                    GuiCommand::RouteStream { node_id, bus_id } => {
                        debug!("Routing stream {} -> '{}' via lincasterctl", node_id, bus_id);
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["route-stream", &node_id.to_string(), &bus_id])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                debug!("route-stream: {}", String::from_utf8_lossy(&out.stdout).trim());
                            }
                            Ok(out) => {
                                warn!("route-stream failed: {}", String::from_utf8_lossy(&out.stderr).trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::UnrouteStream { node_id } => {
                        debug!("Unrouting stream {} via lincasterctl", node_id);
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["unroute-stream", &node_id.to_string()])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                debug!("unroute-stream: {}", String::from_utf8_lossy(&out.stdout).trim());
                            }
                            Ok(out) => {
                                warn!("unroute-stream failed: {}", String::from_utf8_lossy(&out.stderr).trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::SetManualOverride { enabled } => {
                        debug!("SetManualOverride({}) via lincasterctl", enabled);
                        let bin = lincasterctl_bin();
                        let state = if enabled { "on" } else { "off" };
                        match std::process::Command::new(&bin)
                            .args(["set-manual-override", state])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                debug!("set-manual-override: {}", String::from_utf8_lossy(&out.stdout).trim());
                            }
                            Ok(out) => {
                                warn!("set-manual-override failed: {}", String::from_utf8_lossy(&out.stderr).trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::SetPadBank { bank } => {
                        debug!("Setting pad bank {} via lincasterctl", bank);
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["set-pad-bank", &bank.to_string()])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                debug!("set-pad-bank: {}", stdout.trim());
                            }
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                warn!("set-pad-bank failed: {}", stderr.trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::ApplyPadConfig {
                        bank,
                        position,
                        config_json,
                    } => {
                        debug!("Sending ApplyPadConfig(bank={}, pos={})", bank, position);

                        // Check if this is an Off config — if so, use the
                        // simple 3-command clear protocol (ClearPad) instead
                        // of the full ApplyPadConfig flow.
                        let is_off = serde_json::from_str::<SoundPadConfig>(&config_json)
                            .map(|c| matches!(c.assignment, PadAssignment::Off))
                            .unwrap_or(false);
                        if is_off {
                            debug!(
                                "Off type detected; using ClearPad for bank={} pos={}",
                                bank, position
                            );
                            let _: Result<(), _> =
                                proxy.method_call(DBUS_IFACE, "ClearPad", (bank, position));
                            continue;
                        }

                        // If this is a Sound pad with a local file, import it via
                        // lincasterctl which handles: file copy + fsync, then
                        // sends ApplyPadConfig with the device path to the daemon
                        // for the full HID setup (clear, padType, color, playback
                        // props, remountPadStorage, padFilePath, padName).
                        //
                        // On success we skip sending ApplyPadConfig from here
                        // because lincasterctl already sent it with the correct
                        // device-internal path.
                        //
                        // On failure we also skip — sending the local file path
                        // to the device is meaningless and leaves the pad broken.
                        let mut imported = false;
                        if let Ok(config) = serde_json::from_str::<SoundPadConfig>(&config_json) {
                            if let PadAssignment::Sound(ref sound) = config.assignment {
                                let path = &sound.file_path;
                                // Local paths start with / but device paths start with "pads/"
                                // Also skip paths on the device mount (/run/media/)
                                if !path.is_empty()
                                    && !path.starts_with("pads/")
                                    && !path.starts_with("/Application/")
                                    && !path.starts_with("/run/media/")
                                {
                                    let local_path = std::path::Path::new(path);
                                    if local_path.is_file() {
                                        // pads_per_bank is 8 for Pro II — pad number is 1-based
                                        let pad_num = (bank as usize) * 8 + (position as usize) + 1;
                                        let color_idx = sound.color.wire_index();
                                        debug!(
                                            "Importing sound file '{}' to pad {} color={}",
                                            path, pad_num, color_idx
                                        );
                                        match import_sound_via_cli(pad_num, local_path, color_idx) {
                                            Ok(_) => {
                                                // lincasterctl already sent ApplyPadConfig
                                                // to the daemon with the device path.
                                                imported = true;
                                            }
                                            Err(e) => {
                                                warn!("Failed to import sound file: {}. Ensure transfer mode is active.", e);
                                                imported = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if !imported {
                            let _: Result<(), _> = proxy.method_call(
                                DBUS_IFACE,
                                "ApplyPadConfig",
                                (bank, position, &*config_json),
                            );
                        }
                    }
                    GuiCommand::ClearPad { bank, position } => {
                        debug!(
                            "Clearing pad via DBus ClearPad(bank={}, pos={})",
                            bank, position
                        );
                        let _: Result<(), _> =
                            proxy.method_call(DBUS_IFACE, "ClearPad", (bank, position));
                    }
                    GuiCommand::SetPadColor {
                        bank,
                        position,
                        color,
                    } => {
                        let pad_num = (bank as usize) * 8 + (position as usize) + 1;
                        debug!(
                            "Setting pad {} colour to {} via lincasterctl",
                            pad_num, color
                        );
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["set-pad-color", &pad_num.to_string(), &color.to_string()])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                debug!("set-pad-color: {}", stdout.trim());
                            }
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                warn!("set-pad-color failed: {}", stderr.trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::EnterTransferMode => {
                        debug!("Entering transfer mode via lincasterctl");
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["transfer-mode", "pads"])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                debug!("transfer-mode: {}", stdout.trim());
                            }
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                warn!("transfer-mode failed: {} {}", stderr.trim(), stdout.trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                    GuiCommand::ExitTransferMode => {
                        debug!("Exiting transfer mode via lincasterctl");
                        let bin = lincasterctl_bin();
                        match std::process::Command::new(&bin)
                            .args(["exit-transfer-mode"])
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                debug!("exit-transfer-mode: {}", stdout.trim());
                            }
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                warn!("exit-transfer-mode failed: {}", stderr.trim());
                            }
                            Err(e) => warn!("Failed to run lincasterctl: {}", e),
                        }
                    }
                }
            }

            // If we just sent a command, wait briefly then re-poll immediately
            // for faster UI feedback
            if had_command {
                thread::sleep(Duration::from_millis(50));
            }

            // Poll daemon state
            let busses_result: Result<(String,), _> =
                proxy.method_call(DBUS_IFACE, "ListBusses", ());
            let streams_result: Result<(String,), _> =
                proxy.method_call(DBUS_IFACE, "ListStreams", ());
            let status_result: Result<(String,), _> =
                proxy.method_call(DBUS_IFACE, "GetStatus", ());
            let pads_result: Result<(String,), _> =
                proxy.method_call(DBUS_IFACE, "GetPadConfigs", ());

            match (busses_result, streams_result) {
                (Ok((busses_json,)), Ok((streams_json,))) => {
                    let bus_states: Vec<BusState> =
                        serde_json::from_str(&busses_json).unwrap_or_default();
                    let streams: Vec<StreamSnapshot> =
                        serde_json::from_str(&streams_json).unwrap_or_default();

                    let device: Option<DeviceIdentity> = status_result
                        .ok()
                        .and_then(|(json,)| serde_json::from_str::<serde_json::Value>(&json).ok())
                        .and_then(|v| serde_json::from_value(v["device"].clone()).ok());

                    let pad_configs: Vec<Vec<SoundPadConfig>> = match pads_result {
                        Ok((json,)) => match serde_json::from_str(&json) {
                            Ok(configs) => {
                                let configs: Vec<Vec<SoundPadConfig>> = configs;
                                let total: usize = configs.iter().map(|b| b.len()).sum();
                                let assigned: usize = configs
                                    .iter()
                                    .flat_map(|b| b.iter())
                                    .filter(|p| !matches!(p.assignment, PadAssignment::Off))
                                    .count();
                                if assigned > 0 || total > 0 {
                                    debug!(
                                        "GetPadConfigs: {} banks, {} pads total, {} assigned",
                                        configs.len(),
                                        total,
                                        assigned
                                    );
                                }
                                configs
                            }
                            Err(e) => {
                                warn!("Failed to deserialize pad configs: {}", e);
                                warn!(
                                    "Raw pad JSON (first 500 chars): {}",
                                    &json[..json.len().min(500)]
                                );
                                Vec::new()
                            }
                        },
                        Err(e) => {
                            warn!("GetPadConfigs D-Bus call failed: {}", e);
                            Vec::new()
                        }
                    };

                    let busses = bus_states
                        .into_iter()
                        .map(|s| BusInfo {
                            display_name: format_bus_display_name(&s.bus_id),
                            bus_id: s.bus_id,
                            gain: s.gain,
                            mute: s.mute,
                            solo: s.solo,
                        })
                        .collect();

                    if update_tx
                        .send(DaemonUpdate::State {
                            busses,
                            streams,
                            device,
                            pad_configs,
                        })
                        .is_err()
                    {
                        return; // GUI closed
                    }
                }
                _ => {
                    let _ = update_tx.send(DaemonUpdate::Disconnected);
                    break; // reconnect
                }
            }

            thread::sleep(POLL_INTERVAL);
        }

        thread::sleep(Duration::from_secs(2));
    }
}

fn format_bus_display_name(bus_id: &str) -> String {
    match bus_id {
        "system" => "System".into(),
        "chat" => "Chat".into(),
        "game" => "Game".into(),
        "music" => "Music".into(),
        "a" => "Virtual A".into(),
        "b" => "Virtual B".into(),
        other => other.to_string(),
    }
}

/// Import a sound file to a pad using the lincasterctl CLI.
///
/// Runs `lincasterctl import-sound <pad> <file>` and parses the output
/// to get the device-relative path.
fn import_sound_via_cli(
    pad_num: usize,
    file: &std::path::Path,
    color: u32,
) -> Result<String, String> {
    let bin = lincasterctl_bin();
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

    // Parse the output to extract the device path
    // Output format: "Device path: /Application/emmc-data/pads/N/sound.mp3"
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("Device path: ") {
            return Ok(path.trim().to_string());
        }
    }

    // Fallback: construct path from pad number
    Ok(format!("pads/{}/sound.mp3", pad_num))
}
