use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dbus::blocking::Connection;
use lincaster_proto::{BusState, DeviceIdentity, PadAssignment, SoundPadConfig, StreamSnapshot};
use tracing::{debug, warn};

use crate::cli_exec::{dispatch_command, GuiCommand};

const DBUS_NAME: &str = "com.lincaster.Daemon";
const DBUS_PATH: &str = "/com/lincaster/Daemon";
const DBUS_IFACE: &str = "com.lincaster.Daemon";
const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

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
                dispatch_command(cmd);
            }

            // If we just sent a command, wait briefly then re-poll immediately
            // for faster UI feedback.
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
