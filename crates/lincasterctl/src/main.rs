use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use dbus::blocking::Connection;

const DBUS_NAME: &str = "com.lincaster.Daemon";
const DBUS_PATH: &str = "/com/lincaster/Daemon";
const DBUS_IFACE: &str = "com.lincaster.Daemon";
const DBUS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "lincasterctl", about = "CLI control for the LinCaster daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Increase verbosity.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// Show daemon and device status.
    Status,

    /// List all virtual busses and their states.
    ListBusses,

    /// Get the state of a specific bus.
    GetBus {
        /// Bus identifier (e.g., "system", "chat", "game").
        bus_id: String,
    },

    /// Set the gain for a bus (0.0 to 1.0).
    SetGain {
        /// Bus identifier.
        bus_id: String,
        /// Gain value (0.0 to 1.0).
        gain: f64,
    },

    /// Mute or unmute a bus.
    Mute {
        /// Bus identifier.
        bus_id: String,
        /// Mute state: "on" or "off".
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },

    /// Solo or unsolo a bus.
    Solo {
        /// Bus identifier.
        bus_id: String,
        /// Solo state: "on" or "off".
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },

    /// Reload the daemon's configuration.
    ReloadConfig {
        /// Path to the new configuration file.
        config_path: PathBuf,
    },

    /// Enter transfer mode and mount device storage read-write.
    TransferMode {
        /// Which storage to mount: pads or recordings.
        #[arg(default_value = "pads")]
        storage: StorageType,
    },

    /// Exit transfer mode (return device to normal operation).
    ExitTransferMode,

    /// Import a sound file to a pad on device storage.
    ImportSound {
        /// Pad number (1-based). Bank 1 pad 1 = 1, bank 2 pad 1 = 9, etc.
        pad: usize,
        /// Path to the sound file (.wav or .mp3).
        file: PathBuf,
        /// Pad colour (0=red, 1=orange, 2=amber, 3=yellow, 4=lime, 5=green,
        /// 6=teal, 7=cyan, 8=blue, 9=purple, 10=magenta, 11=pink).
        #[arg(long, default_value = "0")]
        color: u32,
    },

    /// Clear/reset a pad to Off state.
    ClearPad {
        /// Pad number (1-based). Bank 1 pad 1 = 1, bank 2 pad 1 = 9, etc.
        pad: usize,
    },

    /// Set the colour of a pad on the device (immediate/live feedback).
    SetPadColor {
        /// Pad number (1-based). Bank 1 pad 1 = 1, bank 2 pad 1 = 9, etc.
        pad: usize,
        /// Colour index (0=red, 1=orange, 2=amber, 3=yellow, 4=lime, 5=green,
        /// 6=teal, 7=cyan, 8=blue, 9=purple, 10=magenta, 11=pink).
        color: u32,
    },

    /// Set the active sound pad bank on the device (0-indexed).
    SetPadBank {
        /// Bank number (0-7).
        bank: u8,
    },

    /// Re-read pad state from the device (refreshes daemon's in-memory state).
    RefreshState,

    /// List all active audio streams and their current routing.
    ListStreams,

    /// Route an audio stream to one of the virtual busses.
    RouteStream {
        /// PipeWire node ID of the stream to route.
        node_id: u32,
        /// Target bus identifier (e.g. "chat", "game", "system").
        bus_id: String,
    },

    /// Unroute a stream, returning it to the default audio device.
    UnrouteStream {
        /// PipeWire node ID of the stream to unroute.
        node_id: u32,
    },

    /// Enable or disable manual routing override (suppresses config auto-routing).
    SetManualOverride {
        /// Override state: "on" or "off".
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum StorageType {
    Pads,
    Recordings,
}

fn parse_on_off(s: &str) -> Result<bool, String> {
    match s.to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Ok(true),
        "off" | "false" | "0" | "no" => Ok(false),
        _ => Err(format!("Expected 'on' or 'off', got '{}'", s)),
    }
}

fn init_tracing(verbose: u8) {
    let filter = match verbose {
        0 => "lincasterctl=warn",
        1 => "lincasterctl=info",
        2.. => "lincasterctl=debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_target(false)
        .with_level(true)
        .init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Commands::Status => cmd_status()?,
        Commands::ListBusses => cmd_list_busses()?,
        Commands::GetBus { bus_id } => cmd_get_bus(&bus_id)?,
        Commands::SetGain { bus_id, gain } => cmd_set_gain(&bus_id, gain)?,
        Commands::Mute { bus_id, state } => cmd_set_mute(&bus_id, state)?,
        Commands::Solo { bus_id, state } => cmd_set_solo(&bus_id, state)?,
        Commands::ReloadConfig { config_path } => {
            cmd_reload_config(&config_path.display().to_string())?
        }
        Commands::TransferMode { storage } => cmd_transfer_mode(&storage)?,
        Commands::ExitTransferMode => cmd_exit_transfer_mode()?,
        Commands::ImportSound { pad, file, color } => cmd_import_sound(pad, &file, color)?,
        Commands::ClearPad { pad } => cmd_clear_pad(pad)?,
        Commands::SetPadColor { pad, color } => cmd_set_pad_color(pad, color)?,
        Commands::SetPadBank { bank } => cmd_set_pad_bank(bank)?,
        Commands::RefreshState => cmd_refresh_state()?,
        Commands::ListStreams => cmd_list_streams()?,
        Commands::RouteStream { node_id, bus_id } => cmd_route_stream(node_id, &bus_id)?,
        Commands::UnrouteStream { node_id } => cmd_unroute_stream(node_id)?,
        Commands::SetManualOverride { state } => cmd_set_manual_override(state)?,
    }

    Ok(())
}

fn dbus_conn() -> Result<Connection> {
    Connection::new_session().context("Failed to connect to session DBus. Is the daemon running?")
}

fn call_method<R: dbus::arg::ReadAll>(
    conn: &Connection,
    method: &str,
    args: impl dbus::arg::AppendAll,
) -> Result<R> {
    let proxy = conn.with_proxy(DBUS_NAME, DBUS_PATH, DBUS_TIMEOUT);
    let result: R = proxy
        .method_call(DBUS_IFACE, method, args)
        .map_err(|e| anyhow::anyhow!("DBus call '{}' failed: {}", method, e))?;
    Ok(result)
}

fn cmd_status() -> Result<()> {
    let conn = dbus_conn()?;
    let (json,): (String,) = call_method(&conn, "GetStatus", ())?;

    let status: serde_json::Value = serde_json::from_str(&json)?;
    println!("LinCaster Daemon Status");
    println!("=======================");
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

fn cmd_list_busses() -> Result<()> {
    let conn = dbus_conn()?;
    let (json,): (String,) = call_method(&conn, "ListBusses", ())?;

    let busses: Vec<serde_json::Value> = serde_json::from_str(&json)?;
    println!("Virtual Busses ({}):", busses.len());
    println!("{:<12} {:>6} {:>6} {:>6}", "ID", "Gain", "Mute", "Solo");
    println!("{}", "-".repeat(36));
    for bus in &busses {
        println!(
            "{:<12} {:>5.2}  {:>5}  {:>5}",
            bus["bus_id"].as_str().unwrap_or("?"),
            bus["gain"].as_f64().unwrap_or(0.0),
            if bus["mute"].as_bool().unwrap_or(false) {
                "ON"
            } else {
                "off"
            },
            if bus["solo"].as_bool().unwrap_or(false) {
                "ON"
            } else {
                "off"
            },
        );
    }
    Ok(())
}

fn cmd_get_bus(bus_id: &str) -> Result<()> {
    let conn = dbus_conn()?;
    let (json,): (String,) = call_method(&conn, "GetBusState", (bus_id.to_string(),))?;
    let state: serde_json::Value = serde_json::from_str(&json)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

fn cmd_set_gain(bus_id: &str, gain: f64) -> Result<()> {
    if !(0.0..=1.0).contains(&gain) {
        anyhow::bail!("Gain must be between 0.0 and 1.0, got {}", gain);
    }
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetBusGain", (bus_id.to_string(), gain))?;
    println!("Set gain for '{}' to {:.2}", bus_id, gain);
    Ok(())
}

fn cmd_set_mute(bus_id: &str, mute: bool) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetBusMute", (bus_id.to_string(), mute))?;
    println!(
        "{} bus '{}'",
        if mute { "Muted" } else { "Unmuted" },
        bus_id
    );
    Ok(())
}

fn cmd_set_solo(bus_id: &str, solo: bool) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetBusSolo", (bus_id.to_string(), solo))?;
    println!(
        "{} solo for bus '{}'",
        if solo { "Enabled" } else { "Disabled" },
        bus_id
    );
    Ok(())
}

fn cmd_reload_config(config_path: &str) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "ReloadConfig", (config_path.to_string(),))?;
    println!("Config reloaded from '{}'", config_path);
    Ok(())
}

fn cmd_transfer_mode(storage: &StorageType) -> Result<()> {
    match storage {
        StorageType::Recordings => {
            anyhow::bail!("Recordings storage is not yet supported");
        }
        StorageType::Pads => {}
    }

    // Step 1: Ensure the HID device is connected, then enter transfer mode
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "HidConnect", ())?;
    call_method::<()>(&conn, "SetTransferMode", (true,))?;
    println!("Device entering transfer mode...");

    // Step 2: Mount the device storage (waits for block device + mounts via udisksctl)
    let mount = lincaster_proto::storage::mount_device_storage()
        .context("Device storage not found. Is the RØDECaster connected?")?;

    // Step 3: Ensure the mount is writable
    lincaster_proto::storage::ensure_mount_writable(&mount)
        .context("Failed to make device storage writable")?;

    println!("Device storage mounted read-write at: {}", mount.display());
    Ok(())
}

fn cmd_exit_transfer_mode() -> Result<()> {
    // Step 1: Unmount the device storage (like Windows RØDE Central does)
    match lincaster_proto::storage::unmount_device_storage() {
        Ok(()) => println!("Device storage unmounted"),
        Err(e) => eprintln!("Warning: could not unmount storage: {}", e),
    }

    // Step 2: Exit transfer mode on the device
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetTransferMode", (false,))?;
    println!("Device returned to normal mode");
    Ok(())
}

fn cmd_import_sound(pad: usize, file: &std::path::Path, color: u32) -> Result<()> {
    if pad == 0 || pad > 64 {
        anyhow::bail!("Pad number must be 1–64 (8 banks × 8 pads)");
    }
    if color > 11 {
        anyhow::bail!("Color must be 0–11 (red=0, orange=1, ..., pink=11)");
    }

    // Verify the source file exists and has a supported extension
    if !file.is_file() {
        anyhow::bail!("File not found: {}", file.display());
    }
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "wav" && ext != "mp3" {
        anyhow::bail!("Unsupported file format '{}'. Use .wav or .mp3", ext);
    }

    // Refresh the daemon's HID index map so we address the correct pad slot.
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "RefreshPadState", ())?;

    // Find the device storage mount point
    let mount = lincaster_proto::storage::find_device_mount()
        .context("Device storage not found. Run 'transfer-mode' first.")?;

    // Verify mount is writable (indicates transfer mode is active)
    lincaster_proto::storage::ensure_mount_writable(&mount)
        .context("Device storage is read-only. Run 'transfer-mode' first.")?;

    // pad is 1-based, import_sound_file expects 0-based pad_idx
    let pad_idx = pad - 1;
    let rel_path = lincaster_proto::storage::import_sound_file(&mount, pad_idx, file)
        .context("Failed to copy sound file to device")?;

    let device_path = format!("/Application/emmc-data/{}", rel_path);

    // Derive a display name from the source filename (without extension)
    let display_name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    println!(
        "Imported '{}' -> {}/{}",
        file.display(),
        mount.display(),
        rel_path
    );
    println!("Device path: {}", device_path);

    // Do NOT unmount device storage here.  The Windows app keeps storage
    // mounted throughout the transfer session and sends remountPadStorage
    // (via HID) while the host still has the FAT32 mounted.  Unmounting
    // before the HID sequence can confuse the device firmware — it may
    // re-initialise its storage view and miss the newly-written file.
    // The storage will be unmounted later by `exit-transfer-mode`.
    //
    // The file data was already fsynced by import_sound_file(), so the
    // device firmware will see consistent data when it re-scans.

    // Convert 1-based pad number to bank + position (8 pads per bank)
    let bank = ((pad - 1) / 8) as u8;
    let position = ((pad - 1) % 8) as u8;

    // Build a SoundPadConfig with the device path and send via ApplyPadConfig.
    // This routes through the same code path that the GUI uses for all pad
    // configuration, which handles: clear → type → properties → remount →
    // padFilePath → padName in a single sequence.
    let pad_color =
        lincaster_proto::PadColor::from_wire_index(color).unwrap_or(lincaster_proto::PadColor::Red);
    let config = lincaster_proto::SoundPadConfig {
        pad_index: position,
        name: display_name,
        assignment: lincaster_proto::PadAssignment::Sound(lincaster_proto::SoundConfig {
            file_path: device_path,
            play_mode: lincaster_proto::PlayMode::default(),
            gain_db: -12.0,
            color: pad_color,
            loop_enabled: false,
            replay_mode: lincaster_proto::ReplayMode::default(),
        }),
    };
    let config_json = serde_json::to_string(&config)?;
    call_method::<()>(&conn, "ApplyPadConfig", (bank, position, config_json))?;
    println!("File assigned to pad on device");

    // Refresh the daemon's pad state from the device so the
    // HID index map stays current after the mutation.
    call_method::<()>(&conn, "RefreshPadState", ())?;

    Ok(())
}

fn cmd_clear_pad(pad: usize) -> Result<()> {
    if pad == 0 || pad > 64 {
        anyhow::bail!("Pad number must be 1–64 (8 banks × 8 pads)");
    }

    let bank = ((pad - 1) / 8) as u8;
    let position = ((pad - 1) % 8) as u8;

    let conn = dbus_conn()?;

    // Step 1: Connect and refresh state so we have the correct HID index map.
    call_method::<()>(&conn, "HidConnect", ())?;
    call_method::<()>(&conn, "RefreshPadState", ())?;

    // Step 2: Send the ClearPad command via the daemon.
    // The daemon uses the capture-verified 3-command protocol:
    //   1. Clear padFilePath
    //   2. Section redirect (04-prefix)
    //   3. remountPadStorage
    // No file deletion or storage mount needed — this matches exactly
    // what the Windows RØDE Central app sends.
    call_method::<()>(&conn, "ClearPad", (bank, position))?;
    println!(
        "Cleared pad {} (bank={}, pos={})",
        pad,
        bank + 1,
        position + 1
    );

    Ok(())
}

fn cmd_set_pad_color(pad: usize, color: u32) -> Result<()> {
    if pad == 0 || pad > 64 {
        anyhow::bail!("Pad number must be 1–64 (8 banks × 8 pads)");
    }
    if color > 11 {
        anyhow::bail!("Color must be 0–11 (red=0, orange=1, ..., pink=11)");
    }

    let bank = ((pad - 1) / 8) as u8;
    let position = ((pad - 1) % 8) as u8;

    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetPadColor", (bank, position, color))?;

    let name = lincaster_proto::PadColor::from_wire_index(color)
        .map(|c| c.display_name())
        .unwrap_or("Unknown");
    println!("Set pad {} colour to {} ({})", pad, name, color);
    Ok(())
}

fn cmd_set_pad_bank(bank: u8) -> Result<()> {
    if bank > 7 {
        anyhow::bail!("Bank must be 0–7");
    }
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetPadBank", (bank,))?;
    println!("Set active pad bank to {}", bank);
    Ok(())
}

fn cmd_refresh_state() -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "RefreshPadState", ())?;
    println!("Pad state refresh requested");
    Ok(())
}

fn cmd_list_streams() -> Result<()> {
    let conn = dbus_conn()?;
    let (json,): (String,) = call_method(&conn, "ListStreams", ())?;

    let streams: Vec<serde_json::Value> = serde_json::from_str(&json)?;
    if streams.is_empty() {
        println!("No active audio streams.");
        return Ok(());
    }
    println!("Active streams ({}):", streams.len());
    println!(
        "{:>10}  {:<32}  {:<14}  FLAGS",
        "NODE_ID", "NAME", "ROUTED_TO"
    );
    println!("{}", "-".repeat(72));
    for s in &streams {
        let node_id = s["node_id"].as_u64().unwrap_or(0);
        let name = s["display_name"].as_str().unwrap_or("?");
        let target = s["target_bus_id"].as_str().unwrap_or("(default)");
        let mut flags = Vec::new();
        if s["auto_routed"].as_bool().unwrap_or(false) {
            flags.push("auto");
        }
        println!(
            "{:>10}  {:<32}  {:<14}  {}",
            node_id,
            name,
            target,
            flags.join(",")
        );
    }
    Ok(())
}

fn cmd_route_stream(node_id: u32, bus_id: &str) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "RouteStream", (node_id, bus_id.to_string()))?;
    println!("Routed stream {} to bus '{}'", node_id, bus_id);
    Ok(())
}

fn cmd_unroute_stream(node_id: u32) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "RouteToDefault", (node_id,))?;
    println!("Unrouted stream {} (returned to default)", node_id);
    Ok(())
}

fn cmd_set_manual_override(state: bool) -> Result<()> {
    let conn = dbus_conn()?;
    call_method::<()>(&conn, "SetManualOverride", (state,))?;
    println!(
        "Manual override {}",
        if state { "enabled" } else { "disabled" }
    );
    Ok(())
}
