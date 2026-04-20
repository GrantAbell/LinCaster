use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use dbus::blocking::Connection;
use dbus_crossroads::{Crossroads, IfaceBuilder};
use lincaster_proto::{BusState, Config, DeviceIdentity, SoundPadConfig, StreamSnapshot};
use tracing::{error, info, warn};

use crate::DaemonCommand;

const DBUS_NAME: &str = "com.lincaster.Daemon";
const DBUS_PATH: &str = "/com/lincaster/Daemon";
const DBUS_IFACE: &str = "com.lincaster.Daemon";

/// Shared state accessible from DBus method handlers.
struct DbusContext {
    config: Arc<Mutex<Config>>,
    bus_states: Arc<Mutex<Vec<BusState>>>,
    streams: Arc<Mutex<Vec<StreamSnapshot>>>,
    pad_configs: Arc<Mutex<Vec<Vec<SoundPadConfig>>>>,
    device: Arc<Mutex<Option<DeviceIdentity>>>,
    current_bank: Arc<Mutex<u8>>,
    cmd_tx: mpsc::Sender<DaemonCommand>,
}

/// Start the DBus service on the session bus in a background thread.
/// The service reads from shared state and sends commands to the main loop.
pub fn start_dbus_service(
    config: Arc<Mutex<Config>>,
    bus_states: Arc<Mutex<Vec<BusState>>>,
    streams: Arc<Mutex<Vec<StreamSnapshot>>>,
    pad_configs: Arc<Mutex<Vec<Vec<SoundPadConfig>>>>,
    device: Arc<Mutex<Option<DeviceIdentity>>>,
    current_bank: Arc<Mutex<u8>>,
    cmd_tx: mpsc::Sender<DaemonCommand>,
) -> Result<thread::JoinHandle<()>> {
    let ctx = Arc::new(DbusContext {
        config,
        bus_states,
        streams,
        pad_configs,
        device,
        current_bank,
        cmd_tx,
    });

    let handle = thread::Builder::new()
        .name("dbus-service".into())
        .spawn(move || {
            if let Err(e) = run_dbus_service(ctx) {
                error!("DBus service error: {:#}", e);
            }
        })?;

    Ok(handle)
}

fn run_dbus_service(ctx: Arc<DbusContext>) -> Result<()> {
    let conn = Connection::new_session()
        .map_err(|e| anyhow::anyhow!("Failed to connect to session bus: {}", e))?;

    conn.request_name(DBUS_NAME, false, true, false)
        .map_err(|e| anyhow::anyhow!("Failed to request DBus name '{}': {}", DBUS_NAME, e))?;

    info!("DBus service registered as '{}'", DBUS_NAME);

    let mut cr = Crossroads::new();
    cr.set_async_support(None);

    let iface_token = cr.register(DBUS_IFACE, |b: &mut IfaceBuilder<Arc<DbusContext>>| {
        // Method: ListBusses() -> JSON string
        b.method(
            "ListBusses",
            (),
            ("busses_json",),
            |_, ctx: &mut Arc<DbusContext>, ()| {
                let lock = ctx.bus_states.lock().unwrap();
                let json =
                    serde_json::to_string_pretty(&*lock).unwrap_or_else(|_| "[]".to_string());
                Ok((json,))
            },
        );

        // Method: GetBusState(bus_id: String) -> JSON string
        b.method(
            "GetBusState",
            ("bus_id",),
            ("state_json",),
            |_, ctx: &mut Arc<DbusContext>, (bus_id,): (String,)| {
                let lock = ctx.bus_states.lock().unwrap();
                match lock.iter().find(|s| s.bus_id == bus_id) {
                    Some(bus_state) => {
                        let json =
                            serde_json::to_string(bus_state).unwrap_or_else(|_| "{}".to_string());
                        Ok((json,))
                    }
                    None => Err(dbus::MethodErr::failed(&format!(
                        "Bus '{}' not found",
                        bus_id
                    ))),
                }
            },
        );

        // Method: SetBusGain(bus_id: String, gain: f64)
        b.method(
            "SetBusGain",
            ("bus_id", "gain"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bus_id, gain): (String, f64)| {
                let gain = gain as f32;
                if !(0.0..=1.0).contains(&gain) {
                    return Err(dbus::MethodErr::failed(&format!(
                        "Gain {} out of range [0.0, 1.0]",
                        gain
                    )));
                }
                info!("DBus: SetBusGain '{}' = {}", bus_id, gain);
                ctx.cmd_tx
                    .send(DaemonCommand::SetGain(bus_id, gain))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetBusMute(bus_id: String, mute: bool)
        b.method(
            "SetBusMute",
            ("bus_id", "mute"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bus_id, mute): (String, bool)| {
                info!("DBus: SetBusMute '{}' = {}", bus_id, mute);
                ctx.cmd_tx
                    .send(DaemonCommand::SetMute(bus_id, mute))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetBusSolo(bus_id: String, solo: bool)
        b.method(
            "SetBusSolo",
            ("bus_id", "solo"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bus_id, solo): (String, bool)| {
                info!("DBus: SetBusSolo '{}' = {}", bus_id, solo);
                ctx.cmd_tx
                    .send(DaemonCommand::SetSolo(bus_id, solo))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetBusSoloSafe(bus_id: String, safe: bool)
        b.method(
            "SetBusSoloSafe",
            ("bus_id", "safe"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bus_id, safe): (String, bool)| {
                info!("DBus: SetBusSoloSafe '{}' = {}", bus_id, safe);
                ctx.cmd_tx
                    .send(DaemonCommand::SetSoloSafe(bus_id, safe))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: GetStatus() -> JSON string
        b.method(
            "GetStatus",
            (),
            ("status_json",),
            |_, ctx: &mut Arc<DbusContext>, ()| {
                let states = ctx.bus_states.lock().unwrap();
                let config = ctx.config.lock().unwrap();
                let device = ctx.device.lock().unwrap();
                let current_bank = *ctx.current_bank.lock().unwrap();
                let status = serde_json::json!({
                    "busses": *states,
                    "latency_mode": config.latency_mode,
                    "app_rules_count": config.app_rules.len(),
                    "device": *device,
                    "current_bank": current_bank,
                });
                Ok((status.to_string(),))
            },
        );

        // Method: ReloadConfig(path: String)
        b.method(
            "ReloadConfig",
            ("config_path",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (config_path,): (String,)| {
                info!("DBus: ReloadConfig from '{}'", config_path);
                ctx.cmd_tx
                    .send(DaemonCommand::ReloadConfig(config_path))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: ListStreams() -> JSON string
        b.method(
            "ListStreams",
            (),
            ("streams_json",),
            |_, ctx: &mut Arc<DbusContext>, ()| {
                let lock = ctx.streams.lock().unwrap();
                let json =
                    serde_json::to_string_pretty(&*lock).unwrap_or_else(|_| "[]".to_string());
                Ok((json,))
            },
        );

        // Method: GetPadConfigs() -> JSON string (8 banks × N pads)
        b.method(
            "GetPadConfigs",
            (),
            ("pad_configs_json",),
            |_, ctx: &mut Arc<DbusContext>, ()| {
                let lock = ctx.pad_configs.lock().unwrap();
                let json = serde_json::to_string(&*lock).unwrap_or_else(|_| "[]".to_string());
                Ok((json,))
            },
        );

        // Method: RouteStream(node_id: u32, bus_id: String)
        b.method(
            "RouteStream",
            ("node_id", "bus_id"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (node_id, bus_id): (u32, String)| {
                info!("DBus: RouteStream node {} -> bus '{}'", node_id, bus_id);
                ctx.cmd_tx
                    .send(DaemonCommand::RouteStream(node_id, bus_id))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: UnrouteStream(node_id: u32)
        b.method(
            "UnrouteStream",
            ("node_id",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (node_id,): (u32,)| {
                info!("DBus: UnrouteStream node {}", node_id);
                ctx.cmd_tx
                    .send(DaemonCommand::UnrouteStream(node_id))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: RouteToDefault(node_id: u32)
        b.method(
            "RouteToDefault",
            ("node_id",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (node_id,): (u32,)| {
                info!("DBus: RouteToDefault node {}", node_id);
                ctx.cmd_tx
                    .send(DaemonCommand::RouteToDefault(node_id))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // ── Sound Pad methods ───────────────────────────────────────

        // Method: HidConnect() — connect to the RØDECaster HID interface
        b.method("HidConnect", (), (), |_, ctx: &mut Arc<DbusContext>, ()| {
            info!("DBus: HidConnect");
            ctx.cmd_tx
                .send(DaemonCommand::HidConnect)
                .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
            Ok(())
        });

        // Method: SetPadBank(bank: u8)
        b.method(
            "SetPadBank",
            ("bank",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bank,): (u8,)| {
                info!("DBus: SetPadBank({})", bank);
                ctx.cmd_tx
                    .send(DaemonCommand::SetPadBank(bank))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: ApplyPadConfig(bank: u8, position: u8, config_json: String)
        b.method(
            "ApplyPadConfig",
            ("bank", "position", "config_json"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bank, position, config_json): (u8, u8, String)| {
                info!("DBus: ApplyPadConfig(bank={}, pos={})", bank, position);
                ctx.cmd_tx
                    .send(DaemonCommand::ApplyPadConfig {
                        bank,
                        position,
                        config_json,
                    })
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: ClearPad(bank: u8, position: u8)
        b.method(
            "ClearPad",
            ("bank", "position"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bank, position): (u8, u8)| {
                info!("DBus: ClearPad(bank={}, pos={})", bank, position);
                ctx.cmd_tx
                    .send(DaemonCommand::ClearPad { bank, position })
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetPadColor(bank: u8, position: u8, color: u32)
        b.method(
            "SetPadColor",
            ("bank", "position", "color"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (bank, position, color): (u8, u8, u32)| {
                info!(
                    "DBus: SetPadColor(bank={}, pos={}, color={})",
                    bank, position, color
                );
                ctx.cmd_tx
                    .send(DaemonCommand::SetPadColor {
                        bank,
                        position,
                        color,
                    })
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetManualOverride(enabled: bool)
        b.method(
            "SetManualOverride",
            ("enabled",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (enabled,): (bool,)| {
                info!("DBus: SetManualOverride({})", enabled);
                ctx.cmd_tx
                    .send(DaemonCommand::SetManualOverride(enabled))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetPadProperty(property: String, value: String)
        b.method(
            "SetPadProperty",
            ("property", "value"),
            (),
            |_, ctx: &mut Arc<DbusContext>, (property, value): (String, String)| {
                info!("DBus: SetPadProperty({}, {})", property, value);
                ctx.cmd_tx
                    .send(DaemonCommand::SetPadProperty {
                        property,
                        value_json: value,
                    })
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: SetTransferMode(editing: bool)
        b.method(
            "SetTransferMode",
            ("editing",),
            (),
            |_, ctx: &mut Arc<DbusContext>, (editing,): (bool,)| {
                info!("DBus: SetTransferMode({})", editing);
                ctx.cmd_tx
                    .send(DaemonCommand::SetTransferMode(editing))
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );

        // Method: AssignPadFile(bank: u8, position: u8, device_path: String, display_name: String, color: u32)
        b.method(
                "AssignPadFile",
                ("bank", "position", "device_path", "display_name", "color"),
                (),
                |_,
                 ctx: &mut Arc<DbusContext>,
                 (bank, position, device_path, display_name, color): (
                    u8,
                    u8,
                    String,
                    String,
                    u32,
                )| {
                    info!(
                        "DBus: AssignPadFile(bank={}, pos={}, path='{}', name='{}', color={})",
                        bank, position, device_path, display_name, color
                    );
                    ctx.cmd_tx
                        .send(DaemonCommand::AssignPadFile {
                            bank,
                            position,
                            device_path,
                            display_name,
                            color,
                        })
                        .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                    Ok(())
                },
            );

        // Method: RefreshPadState() — re-read pad configs from device state dump
        b.method(
            "RefreshPadState",
            (),
            (),
            |_, ctx: &mut Arc<DbusContext>, ()| {
                info!("DBus: RefreshPadState");
                ctx.cmd_tx
                    .send(DaemonCommand::RefreshPadState)
                    .map_err(|e| dbus::MethodErr::failed(&e.to_string()))?;
                Ok(())
            },
        );
    });

    cr.insert(DBUS_PATH, &[iface_token], ctx);

    info!("DBus service listening at {} {}", DBUS_NAME, DBUS_PATH);

    // Process DBus messages in a loop.
    // Use read_write + pop_message instead of conn.process(), which would
    // consume messages before Crossroads can handle them.
    loop {
        if conn
            .channel()
            .read_write(Some(std::time::Duration::from_millis(1000)))
            .is_err()
        {
            warn!("DBus connection closed");
            break Ok(());
        }
        while let Some(msg) = conn.channel().pop_message() {
            cr.handle_message(msg, &conn).ok();
        }
    }
}
