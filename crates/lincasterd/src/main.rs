mod app_mapper;
mod dbus_service;
mod device_discovery;
mod fader_control;
mod graph_manager;
mod pipewire_registry;
mod pw_exec;
mod state;
mod usb_hid;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use lincaster_proto::{BusState, Config, SoundPadConfig, StreamSnapshot, RODECASTER_DUO_PID, RODECASTER_PRO_II_PID};
use tracing::{error, info, warn};

use crate::pipewire_registry::{apply_event, format_status, PipeWireState, PwEvent};
use crate::pw_exec::PwExecManager;

/// Commands from the DBus service to the main daemon loop.
#[derive(Debug)]
pub enum DaemonCommand {
    SetGain(String, f32),
    SetMute(String, bool),
    SetSolo(String, bool),
    SetSoloSafe(String, bool),
    ReloadConfig(String),
    RouteStream(u32, String),
    UnrouteStream(u32),
    RouteToDefault(u32),
    /// Enable/disable manual override (ignores config.json routing rules).
    SetManualOverride(bool),
    /// Connect to (or reconnect) the RØDECaster HID interface.
    HidConnect,
    /// Set the active sound pad bank (0-indexed).
    SetPadBank(u8),
    /// Apply a full pad configuration to a specific bank/position.
    ApplyPadConfig {
        bank: u8,
        position: u8,
        config_json: String,
    },
    /// Clear/reset a pad to Off state.
    ClearPad { bank: u8, position: u8 },
    /// Set a single pad colour on the device (live/immediate feedback).
    SetPadColor { bank: u8, position: u8, color: u32 },
    /// Set a single pad property on the currently-selected pad.
    SetPadProperty { property: String, value_json: String },
    /// Enter (true) or exit (false) transfer/editing mode on the device.
    SetTransferMode(bool),
    /// Assign a sound file to a specific pad (after file has been copied to storage).
    AssignPadFile {
        bank: u8,
        position: u8,
        device_path: String,
        display_name: String,
        color: u32,
    },
    /// Re-read pad state from the device state dump (refreshes shared_pad_configs).
    RefreshPadState,
}

#[derive(Parser)]
#[command(
    name = "lincasterd",
    about = "LinCaster daemon for RØDECaster virtual driver on Linux"
)]
struct Args {
    /// Path to the configuration file (JSON).
    #[arg(short, long, default_value_os_t = default_config_path())]
    config: PathBuf,

    /// Path to the state file for persistence.
    #[arg(long, default_value_os_t = default_state_path())]
    state_file: PathBuf,

    /// Run in one-shot mode: probe device, print status, and exit.
    #[arg(long)]
    status_only: bool,

    /// Increase verbosity (can be repeated: -v, -vv, -vvv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Disable the hidraw drain process on disconnect. Use this when the
    /// kernel boot parameter usbhid.quirks is set for your device:
    ///   Duo:    0x19F7:0x0079:0x0400
    ///   Pro II: 0x19F7:0x0078:0x0400
    #[arg(long)]
    no_drain: bool,
}

fn default_config_path() -> PathBuf {
    config_dirs_path("lincaster", "config.json")
}

fn default_state_path() -> PathBuf {
    dirs_path("lincaster", "state.json")
}

fn config_dirs_path(app: &str, file: &str) -> PathBuf {
    if let Some(config_dir) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(config_dir).join(app).join(file)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config").join(app).join(file)
    } else {
        PathBuf::from(format!("/tmp/{}/{}", app, file))
    }
}

fn dirs_path(app: &str, file: &str) -> PathBuf {
    if let Some(data_dir) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(data_dir).join(app).join(file)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".local/share")
            .join(app)
            .join(file)
    } else {
        PathBuf::from(format!("/tmp/{}/{}", app, file))
    }
}

fn init_tracing(verbose: u8) {
    let filter = match verbose {
        0 => "lincasterd=info,lincaster_proto=info",
        1 => "lincasterd=debug,lincaster_proto=debug",
        2.. => "lincasterd=trace,lincaster_proto=trace,pipewire=debug",
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_target(true)
        .with_thread_names(true)
        .init();
}

/// Node name prefix for virtual sinks.
const NODE_PREFIX: &str = "lincaster";

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    info!("lincasterd starting (config={})", args.config.display());

    // ── Load configuration ──────────────────────────────────────────
    let config = load_config(&args.config)?;
    info!(
        "Loaded config: {} busses, {} routes, {} app rules, latency={:?}",
        config.busses.len(),
        config.routes.len(),
        config.app_rules.len(),
        config.latency_mode
    );

    // ── Check prerequisites ─────────────────────────────────────────
    if !PwExecManager::check_pactl_available() {
        warn!("pactl not found. Virtual sink creation/volume control will fail.");
        warn!("Install pipewire-pulse (or pulseaudio-utils) to enable full functionality.");
    }

    if !PwExecManager::check_pw_link_available() {
        warn!("pw-link not found. Hardware route linking will fail.");
        warn!("Install pipewire (pw-link is part of the base package).");
    }

    install_wireplumber_config();

    // ── Device discovery ────────────────────────────────────────────
    let device = device_discovery::probe_device(&config.device)?;
    log_device_info(&device, &config);

    // ── Connect to PipeWire ─────────────────────────────────────────
    info!("Connecting to PipeWire...");
    let mut event_rx = pipewire_registry::start_pipewire_thread();
    let mut pw_state = PipeWireState::default();
    wait_for_initial_sync(&event_rx, &mut pw_state);

    let status = format_status(&pw_state, &config.device.alsa_card_id_hint);
    println!("{}", status);

    if args.status_only {
        info!("Status-only mode; exiting.");
        return Ok(());
    }

    // ── Load persisted state ────────────────────────────────────────
    let bus_states = state::load_state(&args.state_file, &config);

    // ── Shared state for DBus ───────────────────────────────────────
    let shared_states = Arc::new(Mutex::new(bus_states.clone()));
    let shared_config = Arc::new(Mutex::new(config.clone()));
    let shared_streams: Arc<Mutex<Vec<StreamSnapshot>>> = Arc::new(Mutex::new(Vec::new()));
    let shared_device = Arc::new(Mutex::new(device.clone()));
    let pads_per_bank = match device.as_ref().map(|d| d.usb_product_id) {
        Some(RODECASTER_PRO_II_PID) => 8,
        Some(RODECASTER_DUO_PID) | _ => 6,
    };
    let shared_pad_configs: Arc<Mutex<Vec<Vec<SoundPadConfig>>>> = Arc::new(Mutex::new(
        (0..8)
            .map(|_| {
                (0..pads_per_bank)
                    .map(|i| SoundPadConfig {
                        pad_index: i as u8,
                        name: String::new(),
                        assignment: lincaster_proto::PadAssignment::Off,
                    })
                    .collect()
            })
            .collect(),
    ));
    // HID index map: padIdx → absolute SOUNDPADS child index (for HID addressing)
    let shared_hid_index_map: Arc<Mutex<Vec<Option<u8>>>> = Arc::new(Mutex::new(
        (0..8 * pads_per_bank).map(|i| Some(i as u8)).collect(),
    ));
    // Effects slot map: effectsIdx → PADEFFECTS child index (for effects HID addressing)
    let shared_effects_slot_map: Arc<Mutex<std::collections::HashMap<u32, u8>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    // Total children in PADEFFECTS section (for fabricating new effects slots)
    let shared_effects_total_children: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    // ── USB HID device for sound pad control ────────────────────────
    let hid_device = usb_hid::HidDevice::new();
    let (hid_event_tx, hid_event_rx) = mpsc::channel();
    hid_device.set_event_tx(hid_event_tx);
    match hid_device.connect() {
        Ok(true) => match hid_device.perform_handshake(pads_per_bank) {
            Ok(parsed) => {
                let assigned: usize = parsed.banks
                    .iter()
                    .flat_map(|b| b.iter())
                    .filter(|p| !matches!(p.assignment, lincaster_proto::PadAssignment::Off))
                    .count();
                info!(
                    "HID device connected and handshake complete ({} assigned pads)",
                    assigned
                );
                if let Ok(mut lock) = shared_pad_configs.lock() {
                    *lock = parsed.banks;
                }
                if let Ok(mut lock) = shared_hid_index_map.lock() {
                    *lock = parsed.hid_index_map;
                }
                if let Ok(mut lock) = shared_effects_slot_map.lock() {
                    *lock = parsed.effects_slot_map;
                }
                if let Ok(mut lock) = shared_effects_total_children.lock() {
                    *lock = parsed.effects_total_children;
                }
            }
            Err(e) => warn!("HID handshake failed: {:#}", e),
        },
        Ok(false) => {}
        Err(e) => {
            info!("HID device not available (will retry on demand): {}", e);
        }
    }

    // ── Create virtual sinks ────────────────────────────────────────
    let mut exec = PwExecManager::new(NODE_PREFIX);
    exec.cleanup_stale_sinks();
    create_virtual_sinks(&config, &mut exec);

    // Wait briefly for PW to register the new nodes, then discover their IDs
    std::thread::sleep(Duration::from_millis(500));
    drain_pw_events(&event_rx, &mut pw_state);
    register_sink_node_ids(&config, &pw_state, &mut exec);

    // Link virtual sink monitors to hardware device playback channels.
    // After a USB port reset (from a previous daemon exit), the ALSA device
    // re-enumerates and PipeWire may need extra time to discover it.  Retry
    // a few times with backoff before giving up.
    {
        let mut linked = false;
        for attempt in 0..6 {
            if attempt > 0 {
                let wait = Duration::from_secs(1) * attempt;
                info!(
                    "Hardware device not found yet, retrying in {:?} (attempt {}/6)",
                    wait,
                    attempt + 1
                );
                std::thread::sleep(wait);
                drain_pw_events(&event_rx, &mut pw_state);
            }
            // Check if the hardware device is visible
            let hw_nodes: Vec<_> = pw_state
                .nodes_matching(&config.device.alsa_card_id_hint)
                .into_iter()
                .filter(|n| {
                    n.media_class.contains("Audio/Sink") || n.media_class.contains("Audio/Duplex")
                })
                .collect();
            if !hw_nodes.is_empty() {
                if let Err(e) = exec.link_routes_to_hardware(&config, &pw_state) {
                    warn!("Failed to link routes to hardware: {:#}", e);
                }
                linked = true;
                break;
            }
        }
        if !linked {
            warn!(
                "Hardware device '{}' not found after retries; routes will not be linked",
                config.device.alsa_card_id_hint
            );
        }
    }

    // Apply initial volume/mute from persisted state
    apply_all_bus_states(&bus_states, &exec);

    // ── Start per-app mapper ────────────────────────────────────────
    let mut mapper = app_mapper::AppMapper::new(&config);

    // ── Route any streams that were already playing before we started ──
    for node in pw_state.nodes.values() {
        if node.media_class.contains("Stream/Output/Audio") {
            if let Some(target_bus) = mapper.match_stream(node) {
                info!(
                    "Routing pre-existing stream '{}' (node {}) -> bus '{}'",
                    node.name, node.id, target_bus
                );
                if let Err(e) = exec.route_stream(node.id, &target_bus, &pw_state) {
                    warn!("Failed to route pre-existing stream: {:#}", e);
                } else {
                    exec.mark_auto_routed(node.id);
                }
            }
        }
    }

    // ── Start DBus service ──────────────────────────────────────────
    let (daemon_cmd_tx, daemon_cmd_rx) = mpsc::channel();
    let dbus_handle = dbus_service::start_dbus_service(
        shared_config.clone(),
        shared_states.clone(),
        shared_streams.clone(),
        shared_pad_configs.clone(),
        shared_device,
        daemon_cmd_tx,
    );
    match &dbus_handle {
        Ok(_) => info!("DBus service started on session bus"),
        Err(e) => warn!("Failed to start DBus service: {:#}", e),
    }

    // ── Signal handling ─────────────────────────────────────────────
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown_flag = shutdown.clone();
        ctrlc::set_handler(move || {
            info!("Received shutdown signal");
            shutdown_flag.store(true, Ordering::SeqCst);
        })
        .expect("Failed to set Ctrl+C handler");
    }

    info!("lincasterd running. Press Ctrl+C to stop.");

    // ── Main event loop ─────────────────────────────────────────────
    let mut bus_states = bus_states;
    let mut config = config;
    let mut pw_reconnect_backoff = Duration::from_secs(1);

    loop {
        // Check for shutdown
        if shutdown.load(Ordering::SeqCst) {
            info!("Shutdown requested");
            break;
        }

        // Process PipeWire events
        match event_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                pw_reconnect_backoff = Duration::from_secs(1); // reset backoff
                match &event {
                    PwEvent::NodeAdded(ref node) => {
                        // Check if this is one of our virtual sinks
                        if node.name.starts_with(NODE_PREFIX) {
                            for bus in &config.busses {
                                let expected = exec.node_name(&bus.bus_id);
                                if node.name == expected {
                                    exec.register_node_id(&bus.bus_id, node.id);
                                }
                            }
                        }

                        // Auto-route matching streams (unless manual override is on)
                        if node.media_class.contains("Stream/Output/Audio")
                            && !exec.manual_override_enabled()
                        {
                            if let Some(target_bus) = mapper.match_stream(node) {
                                info!(
                                    "Auto-routing stream '{}' (node {}) -> bus '{}'",
                                    node.name, node.id, target_bus
                                );
                                if let Err(e) = exec.route_stream(node.id, &target_bus, &pw_state) {
                                    warn!("Failed to route stream: {:#}", e);
                                } else {
                                    exec.mark_auto_routed(node.id);
                                }
                            }
                        }
                    }
                    PwEvent::NodeRemoved(id) => {
                        exec.remove_stream_route(*id);
                    }
                    PwEvent::PortAdded(ref port) => {
                        // Apply port to state FIRST so link logic can find it
                        let port_node_id = port.node_id;
                        apply_event(&mut pw_state, event);
                        // When a port appears for a stream with a pending route,
                        // complete the pw-link connection.
                        if let Err(e) = exec.try_link_pending_stream(port_node_id, &pw_state) {
                            warn!("Failed to link pending stream: {:#}", e);
                        }
                        continue; // skip the apply_event below (already applied)
                    }
                    PwEvent::LinkAdded(ref link) => {
                        // If WirePlumber creates a link for a stream we routed,
                        // check it goes to the right sink and correct if needed.
                        let link_snapshot = link.clone();
                        apply_event(&mut pw_state, event);
                        if let Err(e) = exec.check_link_target(&link_snapshot, &pw_state) {
                            warn!("Failed to correct link target: {:#}", e);
                        }
                        continue;
                    }
                    PwEvent::Disconnected => {
                        error!("PipeWire disconnected! Attempting reconnection...");
                        apply_event(&mut pw_state, event);

                        // Wait and reconnect
                        info!(
                            "Waiting {:?} before reconnecting to PipeWire",
                            pw_reconnect_backoff
                        );
                        std::thread::sleep(pw_reconnect_backoff);
                        pw_reconnect_backoff =
                            (pw_reconnect_backoff * 2).min(Duration::from_secs(30));

                        // Restart PW monitoring
                        event_rx = pipewire_registry::start_pipewire_thread();
                        pw_state = PipeWireState::default();
                        wait_for_initial_sync(&event_rx, &mut pw_state);

                        // Recreate virtual sinks (old ones are gone)
                        exec = PwExecManager::new(NODE_PREFIX);
                        exec.cleanup_stale_sinks();
                        create_virtual_sinks(&config, &mut exec);
                        std::thread::sleep(Duration::from_millis(500));
                        drain_pw_events(&event_rx, &mut pw_state);
                        register_sink_node_ids(&config, &pw_state, &mut exec);
                        if let Err(e) = exec.link_routes_to_hardware(&config, &pw_state) {
                            warn!("Failed to link routes to hardware: {:#}", e);
                        }
                        apply_all_bus_states(&bus_states, &exec);

                        info!("PipeWire reconnection complete");
                        continue;
                    }
                    _ => {}
                }
                apply_event(&mut pw_state, event);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Normal timeout
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("PipeWire event channel disconnected unexpectedly");
                // Try to reconnect
                info!("Attempting PipeWire reconnection...");
                std::thread::sleep(pw_reconnect_backoff);
                pw_reconnect_backoff = (pw_reconnect_backoff * 2).min(Duration::from_secs(30));
                event_rx = pipewire_registry::start_pipewire_thread();
                pw_state = PipeWireState::default();
                wait_for_initial_sync(&event_rx, &mut pw_state);

                exec = PwExecManager::new(NODE_PREFIX);
                exec.cleanup_stale_sinks();
                create_virtual_sinks(&config, &mut exec);
                std::thread::sleep(Duration::from_millis(500));
                drain_pw_events(&event_rx, &mut pw_state);
                register_sink_node_ids(&config, &pw_state, &mut exec);
                if let Err(e) = exec.link_routes_to_hardware(&config, &pw_state) {
                    warn!("Failed to link routes to hardware: {:#}", e);
                }
                apply_all_bus_states(&bus_states, &exec);
            }
        }

        // Process daemon commands from DBus
        while let Ok(cmd) = daemon_cmd_rx.try_recv() {
            handle_daemon_command(
                cmd,
                &mut bus_states,
                &mut config,
                &mut mapper,
                &mut exec,
                &pw_state,
                &hid_device,
                &shared_pad_configs,
                &shared_hid_index_map,
                &shared_effects_slot_map,
                &shared_effects_total_children,
                pads_per_bank,
            );
            // Sync shared state back to DBus
            if let Ok(mut lock) = shared_states.lock() {
                *lock = bus_states.clone();
            }
            if let Ok(mut lock) = shared_config.lock() {
                *lock = config.clone();
            }
        }

        // Process HID events from the background reader
        while let Ok(event) = hid_event_rx.try_recv() {
            match event {
                usb_hid::HidEvent::TransferModeExited => {
                    info!("Device exited transfer mode (on-screen button); unmounting storage");
                    if let Err(e) = lincaster_proto::storage::unmount_device_storage() {
                        warn!("Failed to unmount device storage: {}", e);
                    }
                }
            }
        }

        // Update stream snapshots for GUI
        let snapshots = build_stream_snapshots(&pw_state, NODE_PREFIX, &exec);
        if let Ok(mut lock) = shared_streams.lock() {
            *lock = snapshots;
        }
    }

    // ── Graceful shutdown ───────────────────────────────────────────
    info!("Saving state to {}...", args.state_file.display());
    if let Err(e) = state::save_state(&args.state_file, &bus_states) {
        error!("Failed to save state: {:#}", e);
    }

    // Destroy virtual sinks FIRST while PipeWire is still stable.
    // The HID disconnect performs a USB port reset which re-enumerates the
    // entire device (including ALSA audio interfaces), disrupting PipeWire.
    // If we destroy sinks after the reset, pactl commands may fail.
    info!("Destroying virtual sinks...");
    exec.destroy_all();

    info!("Disconnecting HID device...");
    hid_device.disconnect(!args.no_drain);

    info!("lincasterd shut down cleanly");
    Ok(())
}

// ── Helper functions ─────────────────────────────────────────────────

const DEFAULT_CONFIG: &str = include_str!("../../../configs/config.json");
const WIREPLUMBER_RENAME_CONF: &str = include_str!("../../../contrib/51-rodecaster-rename.conf");

fn load_config(path: &std::path::Path) -> Result<Config> {
    if path.exists() {
        Config::load_from_file(path)
            .with_context(|| format!("Failed to load config from {}", path.display()))
    } else {
        info!(
            "Config file not found at {}; creating default config",
            path.display()
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        std::fs::write(path, DEFAULT_CONFIG)
            .with_context(|| format!("Failed to write default config to {}", path.display()))?;
        info!("Wrote default config to {}", path.display());
        Config::load_from_file(path)
            .with_context(|| format!("Failed to load config from {}", path.display()))
    }
}

fn install_wireplumber_config() {
    let conf_dir = if let Some(config_dir) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(config_dir).join("wireplumber/wireplumber.conf.d")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/wireplumber/wireplumber.conf.d")
    } else {
        return;
    };

    let conf_path = conf_dir.join("51-rodecaster-rename.conf");
    if conf_path.exists() {
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&conf_dir) {
        warn!(
            "Could not create WirePlumber config dir {}: {}",
            conf_dir.display(),
            e
        );
        return;
    }
    match std::fs::write(&conf_path, WIREPLUMBER_RENAME_CONF) {
        Ok(()) => info!(
            "Installed WirePlumber rename config to {}",
            conf_path.display()
        ),
        Err(e) => warn!(
            "Could not write WirePlumber config to {}: {}",
            conf_path.display(),
            e
        ),
    }
}

fn log_device_info(device: &Option<lincaster_proto::DeviceIdentity>, config: &Config) {
    match device {
        Some(d) => {
            info!("Detected RØDECaster device:");
            info!("  USB: {:04X}:{:04X}", d.usb_vendor_id, d.usb_product_id);
            if let Some(ref name) = d.alsa_card_name {
                info!("  ALSA card: {}", name);
            }
            info!(
                "  Channels: {} playback, {} capture",
                d.playback_channels, d.capture_channels
            );
            if d.is_multitrack() {
                info!("  Multitrack mode: YES (10 out / 20 in)");
            } else {
                warn!("  Multitrack mode: NO (expected 10 out / 20 in)");
                if config.device.require_multitrack {
                    warn!("  Config requires multitrack. Some features will be unavailable.");
                }
            }
        }
        None => {
            warn!("No RØDECaster device detected. Running in software-only mode.");
        }
    }
}

fn wait_for_initial_sync(event_rx: &mpsc::Receiver<PwEvent>, pw_state: &mut PipeWireState) {
    let sync_timeout = Duration::from_secs(5);
    let start = std::time::Instant::now();
    loop {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                let is_sync = matches!(&event, PwEvent::InitialSyncDone);
                apply_event(pw_state, event);
                if is_sync {
                    info!("PipeWire initial sync complete");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() > sync_timeout {
                    warn!("PipeWire initial sync timed out after {:?}", sync_timeout);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn!("PipeWire thread disconnected during initial sync");
                break;
            }
        }
    }
}

fn drain_pw_events(event_rx: &mpsc::Receiver<PwEvent>, pw_state: &mut PipeWireState) {
    while let Ok(event) = event_rx.try_recv() {
        apply_event(pw_state, event);
    }
}

fn create_virtual_sinks(config: &Config, exec: &mut PwExecManager) {
    for bus in &config.busses {
        if bus.direction == lincaster_proto::BusDirection::Capture {
            continue;
        }
        if let Err(e) = exec.create_virtual_sink(&bus.bus_id, &bus.display_name, bus.channels) {
            error!("Failed to create virtual sink '{}': {:#}", bus.bus_id, e);
        }
    }
}

fn register_sink_node_ids(config: &Config, pw_state: &PipeWireState, exec: &mut PwExecManager) {
    for bus in &config.busses {
        let node_name = exec.node_name(&bus.bus_id);
        if let Some(node) = pw_state.find_node_by_name(&node_name) {
            exec.register_node_id(&bus.bus_id, node.id);
        }
    }
}

fn apply_all_bus_states(bus_states: &[BusState], exec: &PwExecManager) {
    let mut states_clone = bus_states.to_vec();
    let fader = fader_control::FaderController::new(&mut states_clone);
    for state in bus_states {
        let solo_muted = fader.is_solo_muted(&state.bus_id);
        if let Err(e) = exec.apply_bus_state(&state.bus_id, state.gain, state.mute, solo_muted) {
            warn!("Failed to apply state for bus '{}': {:#}", state.bus_id, e);
        }
    }
}

fn handle_daemon_command(
    cmd: DaemonCommand,
    bus_states: &mut Vec<BusState>,
    config: &mut Config,
    mapper: &mut app_mapper::AppMapper,
    exec: &mut PwExecManager,
    pw_state: &PipeWireState,
    hid_device: &usb_hid::HidDevice,
    shared_pad_configs: &Arc<Mutex<Vec<Vec<SoundPadConfig>>>>,
    shared_hid_index_map: &Arc<Mutex<Vec<Option<u8>>>>,
    shared_effects_slot_map: &Arc<Mutex<std::collections::HashMap<u32, u8>>>,
    shared_effects_total_children: &Arc<Mutex<usize>>,
    pads_per_bank: usize,
) {
    match cmd {
        DaemonCommand::SetGain(bus_id, gain) => {
            let solo_muted = {
                let mut fader = fader_control::FaderController::new(bus_states);
                if let Err(e) = fader.set_gain(&bus_id, gain) {
                    warn!("SetGain error: {}", e);
                    return;
                }
                fader.is_solo_muted(&bus_id)
            };
            if let Err(e) = exec.apply_bus_state(&bus_id, gain, false, solo_muted) {
                warn!("Failed to apply gain to PW: {:#}", e);
            }
        }
        DaemonCommand::SetMute(bus_id, mute) => {
            let (gain, solo_muted) = {
                let mut fader = fader_control::FaderController::new(bus_states);
                if let Err(e) = fader.set_mute(&bus_id, mute) {
                    warn!("SetMute error: {}", e);
                    return;
                }
                let gain = fader.get_gain(&bus_id).unwrap_or(1.0);
                let solo_muted = fader.is_solo_muted(&bus_id);
                (gain, solo_muted)
            };
            if let Err(e) = exec.apply_bus_state(&bus_id, gain, mute, solo_muted) {
                warn!("Failed to apply mute to PW: {:#}", e);
            }
        }
        DaemonCommand::SetSolo(bus_id, solo) => {
            {
                let mut fader = fader_control::FaderController::new(bus_states);
                if let Err(e) = fader.set_solo(&bus_id, solo) {
                    warn!("SetSolo error: {}", e);
                    return;
                }
            }
            // Solo affects all busses, so re-apply all states
            apply_all_bus_states(bus_states, exec);
        }
        DaemonCommand::SetSoloSafe(bus_id, safe) => {
            {
                let mut fader = fader_control::FaderController::new(bus_states);
                if let Err(e) = fader.set_solo_safe(&bus_id, safe) {
                    warn!("SetSoloSafe error: {}", e);
                    return;
                }
            }
            apply_all_bus_states(bus_states, exec);
        }
        DaemonCommand::ReloadConfig(path) => {
            match Config::load_from_file(std::path::Path::new(&path)) {
                Ok(new_config) => {
                    info!("Reloaded config from '{}'", path);
                    mapper.reload(&new_config);
                    *config = new_config;
                }
                Err(e) => error!("Failed to reload config: {:#}", e),
            }
        }
        DaemonCommand::RouteStream(node_id, bus_id) => {
            if !exec.manual_override_enabled() && exec.is_auto_routed(node_id) {
                info!(
                    "Ignoring manual route for auto-routed stream {} (enable manual override first)",
                    node_id
                );
                return;
            }
            info!("Routing stream node {} -> bus '{}'", node_id, bus_id);
            exec.clear_auto_routed(node_id);
            if let Err(e) = exec.route_stream(node_id, &bus_id, pw_state) {
                warn!("Failed to route stream: {:#}", e);
            }
        }
        DaemonCommand::UnrouteStream(node_id) => {
            if !exec.manual_override_enabled() && exec.is_auto_routed(node_id) {
                info!(
                    "Ignoring unroute for auto-routed stream {} (enable manual override first)",
                    node_id
                );
                return;
            }
            info!("Unrouting stream node {}", node_id);
            exec.remove_stream_route(node_id);
        }
        DaemonCommand::RouteToDefault(node_id) => {
            if !exec.manual_override_enabled() && exec.is_auto_routed(node_id) {
                info!(
                    "Ignoring route-to-default for auto-routed stream {} (enable manual override first)",
                    node_id
                );
                return;
            }
            info!("Routing stream node {} to default", node_id);
            exec.remove_stream_route(node_id);
        }
        DaemonCommand::SetManualOverride(enabled) => {
            exec.set_manual_override(enabled);
        }
        DaemonCommand::HidConnect => {
            match hid_device.connect() {
                Ok(true) => match hid_device.perform_handshake(pads_per_bank) {
                    Ok(parsed) => {
                        info!("HID device connected via DBus command");
                        if let Ok(mut lock) = shared_pad_configs.lock() {
                            *lock = parsed.banks;
                        }
                        if let Ok(mut lock) = shared_hid_index_map.lock() {
                            *lock = parsed.hid_index_map;
                        }
                        if let Ok(mut lock) = shared_effects_slot_map.lock() {
                            *lock = parsed.effects_slot_map;
                        }
                    }
                    Err(e) => error!("HID handshake failed: {:#}", e),
                },
                Ok(false) => info!("HID device already connected"),
                Err(e) => error!("HID connect failed: {:#}", e),
            }
        }
        DaemonCommand::SetPadBank(bank) => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot set bank");
                return;
            }
            let cmd = lincaster_proto::hid::set_selected_bank(bank);
            if let Err(e) = hid_device.send_report(&cmd) {
                error!("Failed to set pad bank: {:#}", e);
            }
        }
        DaemonCommand::ApplyPadConfig {
            bank,
            position,
            config_json,
        } => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot apply pad config");
                return;
            }
            let pad_idx = (bank as usize) * pads_per_bank + (position as usize);
            let hw_index = shared_hid_index_map
                .lock()
                .ok()
                .and_then(|lock| lock.get(pad_idx).copied().flatten());
            let hw_index = match hw_index {
                Some(idx) => idx,
                None => {
                    error!("No HID index for bank={} position={} (padIdx={})", bank, position, pad_idx);
                    return;
                }
            };
            // Look up the effects slot byte for this pad (effectsIdx == padIdx)
            let effects_slot = shared_effects_slot_map
                .lock()
                .ok()
                .and_then(|lock| lock.get(&(pad_idx as u32)).copied());
            // For new FX pads without an existing PADEFFECTS entry (e.g. after
            // a clear removed it), fabricate a slot at the next available
            // position — same pattern as SOUNDPADS total_children for pads.
            let effects_slot = effects_slot.or_else(|| {
                shared_effects_total_children
                    .lock()
                    .ok()
                    .map(|v| *v as u8)
            });
            match serde_json::from_str::<lincaster_proto::SoundPadConfig>(&config_json) {
                Ok(pad_config) => {
                    if let Err(e) = hid_device.apply_pad_config(hw_index, position, pad_idx, effects_slot, &pad_config) {
                        error!("Failed to apply pad config: {:#}", e);
                    } else if let Ok(mut lock) = shared_pad_configs.lock() {
                        if let Some(bank_pads) = lock.get_mut(bank as usize) {
                            if let Some(slot) = bank_pads.get_mut(position as usize) {
                                *slot = pad_config;
                            }
                        }
                    }
                }
                Err(e) => error!("Invalid pad config JSON: {:#}", e),
            }
        }
        DaemonCommand::ClearPad { bank, position } => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot clear pad");
                return;
            }
            let pad_idx = (bank as usize) * pads_per_bank + (position as usize);

            let hw_index = shared_hid_index_map
                .lock()
                .ok()
                .and_then(|lock| lock.get(pad_idx).copied().flatten());
            match hw_index {
                Some(hw_index) => {
                    if let Err(e) = hid_device.clear_pad(hw_index, pad_idx) {
                        error!("Failed to clear pad: {:#}", e);
                    } else {
                        if let Ok(mut lock) = shared_pad_configs.lock() {
                            if let Some(bank_pads) = lock.get_mut(bank as usize) {
                                if let Some(slot) = bank_pads.get_mut(position as usize) {
                                    *slot = SoundPadConfig {
                                        pad_index: position,
                                        name: String::new(),
                                        assignment: lincaster_proto::PadAssignment::Off,
                                    };
                                }
                            }
                        }
                        // Refresh state dump so hid_index_map stays current.
                        match hid_device.refresh_state(pads_per_bank) {
                            Ok(parsed) => {
                                if let Ok(mut lock) = shared_pad_configs.lock() {
                                    *lock = parsed.banks;
                                }
                                if let Ok(mut lock) = shared_hid_index_map.lock() {
                                    *lock = parsed.hid_index_map;
                                }
                                if let Ok(mut lock) = shared_effects_slot_map.lock() {
                                    *lock = parsed.effects_slot_map;
                                }
                                if let Ok(mut lock) = shared_effects_total_children.lock() {
                                    *lock = parsed.effects_total_children;
                                }
                            }
                            Err(e) => warn!("Post-clear state refresh failed: {:#}", e),
                        }
                    }
                }
                None => error!("No HID index for bank={} position={} (padIdx={})", bank, position, pad_idx),
            }
        }
        DaemonCommand::SetPadColor { bank, position, color } => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot set pad color");
                return;
            }
            let pad_idx = (bank as usize) * pads_per_bank + (position as usize);
            let hw_index = shared_hid_index_map
                .lock()
                .ok()
                .and_then(|lock| lock.get(pad_idx).copied().flatten());
            match hw_index {
                Some(hw_index) => {
                    if let Some(pad_color) = lincaster_proto::PadColor::from_wire_index(color) {
                        if let Err(e) = hid_device.set_pad_color(hw_index, pad_color) {
                            error!("Failed to set pad color: {:#}", e);
                        } else if let Ok(mut lock) = shared_pad_configs.lock() {
                            // Update the stored color so state stays in sync
                            if let Some(bank_pads) = lock.get_mut(bank as usize) {
                                if let Some(slot) = bank_pads.get_mut(position as usize) {
                                    match &mut slot.assignment {
                                        lincaster_proto::PadAssignment::Sound(s) => s.color = pad_color,
                                        lincaster_proto::PadAssignment::Effect(e) => e.color = pad_color,
                                        lincaster_proto::PadAssignment::Mixer(m) => m.color = pad_color,
                                        lincaster_proto::PadAssignment::Trigger(t) => t.color = pad_color,
                                        lincaster_proto::PadAssignment::Off => {}
                                    }
                                }
                            }
                        }
                    } else {
                        error!("Invalid pad color index: {}", color);
                    }
                }
                None => error!("No HID index for bank={} position={} (padIdx={})", bank, position, pad_idx),
            }
        }
        DaemonCommand::SetPadProperty {
            property,
            value_json,
        } => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot set pad property");
                return;
            }
            // Parse value_json to determine encoding
            let value_bytes = match parse_property_value(&value_json) {
                Ok(v) => v,
                Err(e) => {
                    error!("Invalid property value '{}': {}", value_json, e);
                    return;
                }
            };
            let cmd =
                lincaster_proto::hid::set_current_pad_property(&property, &value_bytes);
            if let Err(e) = hid_device.send_report(&cmd) {
                error!("Failed to set pad property: {:#}", e);
            }
        }
        DaemonCommand::SetTransferMode(editing) => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot set transfer mode");
                return;
            }
            if let Err(e) = hid_device.set_transfer_mode(editing) {
                error!("Failed to set transfer mode: {:#}", e);
            }
        }
        DaemonCommand::AssignPadFile { bank, position, device_path, display_name, color } => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot assign pad file");
                return;
            }
            let pad_idx = (bank as usize) * pads_per_bank + (position as usize);
            let hw_index = shared_hid_index_map
                .lock()
                .ok()
                .and_then(|lock| lock.get(pad_idx).copied().flatten());
            let hw_index = match hw_index {
                Some(idx) => idx,
                None => {
                    error!("No HID index for bank={} position={} (padIdx={})", bank, position, pad_idx);
                    return;
                }
            };
            let pad_color = lincaster_proto::PadColor::from_wire_index(color)
                .unwrap_or(lincaster_proto::PadColor::Red);
            if let Err(e) = hid_device.assign_pad_file(hw_index, pad_idx, &device_path, &display_name, pad_color) {
                error!("Failed to assign pad file: {:#}", e);
            } else if let Ok(mut lock) = shared_pad_configs.lock() {
                if let Some(bank_pads) = lock.get_mut(bank as usize) {
                    if let Some(slot) = bank_pads.get_mut(position as usize) {
                        *slot = SoundPadConfig {
                            pad_index: position,
                            name: display_name.clone(),
                            assignment: lincaster_proto::PadAssignment::Sound(lincaster_proto::SoundConfig {
                                file_path: device_path.clone(),
                                play_mode: lincaster_proto::PlayMode::default(),
                                gain_db: -12.0,
                                color: pad_color,
                                loop_enabled: false,
                                replay_mode: lincaster_proto::ReplayMode::default(),
                            }),
                        };
                    }
                }
            }
        }
        DaemonCommand::RefreshPadState => {
            if !hid_device.is_connected() {
                warn!("HID device not connected; cannot refresh pad state");
                return;
            }
            info!("Refreshing pad state from device state dump");
            match hid_device.refresh_state(pads_per_bank) {
                Ok(parsed) => {
                    let assigned: usize = parsed.banks
                        .iter()
                        .flat_map(|b| b.iter())
                        .filter(|p| !matches!(p.assignment, lincaster_proto::PadAssignment::Off))
                        .count();
                    info!("Pad state refreshed ({} assigned pads)", assigned);
                    if let Ok(mut lock) = shared_pad_configs.lock() {
                        *lock = parsed.banks;
                    }
                    if let Ok(mut lock) = shared_hid_index_map.lock() {
                        *lock = parsed.hid_index_map;
                    }
                    if let Ok(mut lock) = shared_effects_slot_map.lock() {
                        *lock = parsed.effects_slot_map;
                    }
                    if let Ok(mut lock) = shared_effects_total_children.lock() {
                        *lock = parsed.effects_total_children;
                    }
                }
                Err(e) => error!("Failed to refresh pad state: {:#}", e),
            }
        }
    }
}

/// Parse a JSON-ish value string into HID-encoded bytes.
/// Supports: "bool:true", "bool:false", "u32:123", "f64:1.5", "string:hello", "clear"
fn parse_property_value(value: &str) -> Result<Vec<u8>> {
    if value == "clear" {
        return Ok(lincaster_proto::hid::val_enum_clear());
    }
    if let Some(rest) = value.strip_prefix("bool:") {
        let b: bool = rest.parse().context("Invalid bool")?;
        return Ok(lincaster_proto::hid::val_bool(b));
    }
    if let Some(rest) = value.strip_prefix("u32:") {
        let n: u32 = rest.parse().context("Invalid u32")?;
        return Ok(lincaster_proto::hid::val_u32(n));
    }
    if let Some(rest) = value.strip_prefix("f64:") {
        let f: f64 = rest.parse().context("Invalid f64")?;
        return Ok(lincaster_proto::hid::val_f64(f));
    }
    if let Some(rest) = value.strip_prefix("string:") {
        return Ok(lincaster_proto::hid::val_string(rest));
    }
    bail!("Unknown value format: '{}'. Use bool:, u32:, f64:, string:, or clear", value)
}

/// Build a snapshot of all active audio output streams and their current routing.
fn build_stream_snapshots(
    pw_state: &PipeWireState,
    node_prefix: &str,
    exec: &PwExecManager,
) -> Vec<StreamSnapshot> {
    let mut snapshots = Vec::new();

    for node in pw_state.nodes.values() {
        if !node.media_class.contains("Stream/Output/Audio") {
            continue;
        }

        let output_port_ids: Vec<u32> = pw_state
            .ports
            .values()
            .filter(|p| p.node_id == node.id && p.direction == "out")
            .map(|p| p.id)
            .collect();

        let mut target_sink_name = None;
        for link in pw_state.links.values() {
            if output_port_ids.contains(&link.output_port) {
                if let Some(input_port) = pw_state.ports.get(&link.input_port) {
                    if let Some(target_node) = pw_state.nodes.get(&input_port.node_id) {
                        target_sink_name = Some(target_node.name.clone());
                        break;
                    }
                }
            }
        }

        let target_bus_id = target_sink_name.as_ref().and_then(|name| {
            name.strip_prefix(&format!("{}.", node_prefix))
                .map(|s| s.to_string())
        });

        let display_name = node
            .props
            .get("application.name")
            .filter(|s| !s.is_empty())
            .or_else(|| node.props.get("node.description").filter(|s| !s.is_empty()))
            .cloned()
            .unwrap_or_else(|| node.name.clone());

        snapshots.push(StreamSnapshot {
            node_id: node.id,
            display_name,
            app_name: node
                .props
                .get("application.name")
                .cloned()
                .unwrap_or_default(),
            target_bus_id,
            target_sink_name,
            auto_routed: exec.is_auto_routed(node.id),
        });
    }

    snapshots.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    snapshots
}
