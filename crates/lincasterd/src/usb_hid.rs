//! USB HID device communication for the RØDECaster Duo / Pro / Pro II.
//!
//! Opens the HID Interrupt endpoint (EP5) and provides send/receive
//! for the binary command protocol. A background reader thread keeps
//! EP5 IN drained so the device can always deliver notifications.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{debug, info, trace, warn};

use lincaster_proto::{RODECASTER_DUO_PID, RODECASTER_PRO_II_PID, RODE_VENDOR_ID};

/// Events emitted by the HID background reader to the daemon main loop.
#[derive(Debug)]
pub enum HidEvent {
    /// The device exited transfer mode (user pressed on-screen button).
    TransferModeExited,
}

/// HID interface number on the RØDECaster Duo.
const HID_INTERFACE: u8 = 9;

/// EP5 OUT endpoint address.
const EP5_OUT: u8 = 0x05;

/// EP5 IN endpoint address.
const EP5_IN: u8 = 0x85;

/// Timeout for USB writes.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Timeout for USB reads.
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Short timeout for background reader (keeps polling responsive to shutdown).
const READER_POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// Delay between sequential HID reports (matching observed ~15–30ms intervals).
const INTER_COMMAND_DELAY: Duration = Duration::from_millis(20);

/// Wrapper for the rusb handle that is shared between the writer (main thread)
/// and the background reader thread. read_interrupt and write_interrupt target
/// different endpoints so concurrent access is safe.
struct SharedHandle {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
}

// Safety: rusb DeviceHandle is Send; we only use it from two threads
// (main for EP5_OUT writes, reader for EP5_IN reads) on separate endpoints.
unsafe impl Send for SharedHandle {}
unsafe impl Sync for SharedHandle {}

/// Thread-safe handle to the USB HID device.
#[derive(Clone)]
pub struct HidDevice {
    handle: Arc<Mutex<Option<Arc<SharedHandle>>>>,
    reader_running: Arc<AtomicBool>,
    reader_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    in_transfer_mode: Arc<AtomicBool>,
    remount_completed: Arc<AtomicBool>,
    event_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<HidEvent>>>>,
}

impl HidDevice {
    /// Create a new HidDevice. Does not connect immediately.
    pub fn new() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
            reader_running: Arc::new(AtomicBool::new(false)),
            reader_handle: Arc::new(Mutex::new(None)),
            in_transfer_mode: Arc::new(AtomicBool::new(false)),
            remount_completed: Arc::new(AtomicBool::new(false)),
            event_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the event sender for HID reader notifications.
    pub fn set_event_tx(&self, tx: std::sync::mpsc::Sender<HidEvent>) {
        *self.event_tx.lock().unwrap() = Some(tx);
    }

    /// Check if the device is currently connected.
    pub fn is_connected(&self) -> bool {
        self.handle.lock().unwrap().is_some()
    }

    /// Attempt to open the RØDECaster Duo HID interface.
    /// Returns Ok(true) if newly connected, Ok(false) if already connected.
    pub fn connect(&self) -> Result<bool> {
        {
            let guard = self.handle.lock().unwrap();
            if guard.is_some() {
                return Ok(false);
            }
        }

        let devices = rusb::devices().context("Failed to enumerate USB devices")?;

        for device in devices.iter() {
            let desc = match device.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };

            if desc.vendor_id() != RODE_VENDOR_ID {
                continue;
            }

            // Accept known RØDECaster product IDs
            if desc.product_id() != RODECASTER_DUO_PID && desc.product_id() != RODECASTER_PRO_II_PID
            {
                continue;
            }

            debug!(
                "Found RØDECaster USB device: {:04X}:{:04X}",
                desc.vendor_id(),
                desc.product_id()
            );

            let handle = device.open().context("Failed to open USB device")?;

            // Detach kernel driver if attached (e.g., usbhid)
            match handle.kernel_driver_active(HID_INTERFACE) {
                Ok(true) => {
                    handle
                        .detach_kernel_driver(HID_INTERFACE)
                        .context("Failed to detach kernel driver from HID interface")?;
                    info!(
                        "Detached kernel HID driver from interface {}",
                        HID_INTERFACE
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    debug!("Could not check kernel driver status: {}", e);
                }
            }

            handle
                .claim_interface(HID_INTERFACE)
                .context("Failed to claim HID interface")?;

            info!(
                "Claimed HID interface {} on RØDECaster device",
                HID_INTERFACE
            );

            let shared = Arc::new(SharedHandle { handle });

            {
                let mut guard = self.handle.lock().unwrap();
                *guard = Some(Arc::clone(&shared));
            }

            return Ok(true);
        }

        bail!("RØDECaster device not found on USB bus");
    }

    /// Start the background reader thread that continuously reads EP5 IN.
    /// This prevents the device firmware from hanging when it tries to send
    /// notifications (e.g., after exiting transfer mode).
    fn start_reader(&self, shared: Arc<SharedHandle>) {
        let running = Arc::clone(&self.reader_running);
        running.store(true, Ordering::SeqCst);
        let in_transfer_mode = Arc::clone(&self.in_transfer_mode);
        let remount_completed = Arc::clone(&self.remount_completed);
        let event_tx = Arc::clone(&self.event_tx);

        let jh = std::thread::Builder::new()
            .name("hid-reader".into())
            .spawn(move || {
                debug!("HID background reader started");
                let mut buf = vec![0u8; 256];
                while running.load(Ordering::SeqCst) {
                    match shared
                        .handle
                        .read_interrupt(EP5_IN, &mut buf, READER_POLL_TIMEOUT)
                    {
                        Ok(n) => {
                            if n > 0 {
                                let msg_type = buf[0];
                                trace!(
                                    "HID notification: type=0x{:02X} len={} first_bytes={:02X?}",
                                    msg_type,
                                    n,
                                    &buf[..n.min(32)]
                                );

                                // Detect transferModeType notifications:
                                // Type 0x04, address 01 01 01 01 0f,
                                // property "transferModeType\0"
                                if msg_type == 0x04 && n >= 30 {
                                    if buf[5..10] == [0x01, 0x01, 0x01, 0x01, 0x0f] {
                                        if let Some(prop_end) =
                                            buf[10..n].iter().position(|&b| b == 0)
                                        {
                                            let prop_name = &buf[10..10 + prop_end];
                                            if prop_name == b"transferModeType" {
                                                // Value follows: 01 05 01 XX XX XX XX (u32 LE)
                                                let val_start = 10 + prop_end + 1;
                                                if val_start + 7 <= n
                                                    && buf[val_start] == 0x01
                                                    && buf[val_start + 1] == 0x05
                                                {
                                                    let mode = u32::from_le_bytes([
                                                        buf[val_start + 3],
                                                        buf[val_start + 4],
                                                        buf[val_start + 5],
                                                        buf[val_start + 6],
                                                    ]);
                                                    info!(
                                                        "Device transferModeType changed to {}",
                                                        mode
                                                    );
                                                    if mode == 0
                                                        && in_transfer_mode
                                                            .swap(false, Ordering::SeqCst)
                                                    {
                                                        // Was in transfer mode, device exited
                                                        if let Some(tx) =
                                                            event_tx.lock().unwrap().as_ref()
                                                        {
                                                            let _ = tx
                                                                .send(HidEvent::TransferModeExited);
                                                        }
                                                    }
                                                }
                                            } else if prop_name == b"remountPadStorage" {
                                                // Value: 01 01 03 = bool(false) means remount complete
                                                let val_start = 10 + prop_end + 1;
                                                if val_start + 3 <= n
                                                    && buf[val_start] == 0x01
                                                    && buf[val_start + 1] == 0x01
                                                    && buf[val_start + 2] == 0x03
                                                {
                                                    info!("Device remountPadStorage completed");
                                                    remount_completed.store(true, Ordering::SeqCst);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            // Timeout — normal, just keep polling
                        }
                    }
                }
                debug!("HID background reader stopped");
            })
            .expect("Failed to spawn HID reader thread");

        *self.reader_handle.lock().unwrap() = Some(jh);
    }

    /// Disconnect from the device cleanly.
    ///
    /// After the Type 0x01 handshake the device enters "host session mode"
    /// and sends 256-byte Type 0x04 property-change notifications on EP5 IN
    /// for every physical interaction (pad presses, bank navigation, knob
    /// turns, etc.).
    ///
    /// Pcap analysis of the official RØDE Central Windows app shows it sends
    /// NO close commands — no transferModeType=0, no padActive=false.  It
    /// simply releases the interface and the OS HID driver re-binds, keeping
    /// EP5 IN polled so the device can always deliver notifications.
    ///
    /// On Linux, usbhid only polls EP5 IN when something has the hidraw
    /// device open (unlike Windows where the HID class driver always polls).
    /// After reattaching usbhid we spawn a small background process that
    /// holds the hidraw device open, keeping EP5 IN drained so the firmware
    /// notification FIFO never fills.
    pub fn disconnect(&self, drain: bool) {
        // Step 1: Stop the background reader and join the thread so its
        // Arc<SharedHandle> reference is dropped before we release.
        self.reader_running.store(false, Ordering::SeqCst);
        if let Some(jh) = self.reader_handle.lock().unwrap().take() {
            match jh.join() {
                Ok(()) => debug!("Reader thread joined successfully"),
                Err(_) => warn!("Reader thread panicked during join"),
            }
        }

        self.in_transfer_mode.store(false, Ordering::SeqCst);

        // Step 2: Release the HID interface and reattach the kernel driver.
        {
            let mut guard = self.handle.lock().unwrap();
            if let Some(shared) = guard.take() {
                if let Err(e) = shared.handle.release_interface(HID_INTERFACE) {
                    warn!("Failed to release HID interface: {}", e);
                }
                match shared.handle.attach_kernel_driver(HID_INTERFACE) {
                    Ok(()) => info!(
                        "Reattached kernel HID driver to interface {}",
                        HID_INTERFACE
                    ),
                    Err(e) => warn!("Could not reattach kernel driver: {}", e),
                }
                drop(shared);
            }
        }

        // Step 3: Spawn a background drain process to keep EP5 IN polled.
        // On Linux, usbhid only polls EP5 IN when something has the hidraw
        // device open.  Without this, the firmware notification FIFO fills
        // after ~2 button presses and physical controls freeze.
        //
        // Disabled with --no-drain when using the kernel boot parameter
        // usbhid.quirks=0x19F7:<PID>:0x0400 (HID_QUIRK_ALWAYS_POLL).
        if drain {
            spawn_hidraw_drain();
        }

        info!("Disconnected from RØDECaster HID device");
    }

    /// Send a single HID report (256 bytes for SET_PROPERTY, 64 for handshake).
    pub fn send_report(&self, data: &[u8]) -> Result<()> {
        let guard = self.handle.lock().unwrap();
        let shared = guard.as_ref().context("HID device not connected")?;

        // Log first 40 bytes of the outgoing message for protocol debugging.
        let preview_len = data.len().min(40);
        let hex: String = data[..preview_len]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        info!("EP5 OUT [{}B]: {}", data.len(), hex);

        let written = shared
            .handle
            .write_interrupt(EP5_OUT, data, WRITE_TIMEOUT)
            .context("Failed to write HID report")?;

        if written != data.len() {
            warn!("Short write: sent {}/{} bytes", written, data.len());
        }

        Ok(())
    }

    /// Read a HID report from the device. Returns the data or times out.
    pub fn read_report(&self) -> Result<Vec<u8>> {
        let guard = self.handle.lock().unwrap();
        let shared = guard.as_ref().context("HID device not connected")?;

        let mut buf = vec![0u8; 256];
        let read = shared
            .handle
            .read_interrupt(EP5_IN, &mut buf, READ_TIMEOUT)
            .context("Failed to read HID report")?;

        buf.truncate(read);
        debug!("Read {} bytes from EP5 IN", read);
        Ok(buf)
    }

    /// Send a sequence of HID reports with inter-command delays.
    pub fn send_sequence(&self, reports: &[Vec<u8>]) -> Result<()> {
        for (i, report) in reports.iter().enumerate() {
            self.send_report(report)
                .with_context(|| format!("Failed on report {}/{}", i + 1, reports.len()))?;

            if i < reports.len() - 1 {
                std::thread::sleep(INTER_COMMAND_DELAY);
            }
        }
        Ok(())
    }

    /// Re-read the device state dump and return fresh parsed pad state.
    ///
    /// Stops the background reader, performs the state dump exchange,
    /// then restarts the reader.  Safe to call while the device is
    /// already connected and in any transfer-mode state.
    pub fn refresh_state(
        &self,
        pads_per_bank: usize,
    ) -> Result<lincaster_proto::state_dump::ParsedPadState> {
        // Stop the background reader so it doesn't consume state-dump reports
        self.reader_running.store(false, Ordering::SeqCst);
        if let Some(jh) = self.reader_handle.lock().unwrap().take() {
            let _ = jh.join();
        }

        let result = self.perform_handshake_inner(pads_per_bank);

        // Restart the reader
        if let Some(shared) = self.handle.lock().unwrap().as_ref() {
            self.start_reader(Arc::clone(shared));
        }

        result
    }

    /// Perform the full connection handshake sequence.
    /// Returns parsed pad configurations (8 banks × N pads, 6 for Duo, 8 for Pro II) if the state dump
    /// was successfully received and parsed.
    ///
    /// Also starts the background EP5 IN reader thread after the state dump is
    /// collected, so subsequent device notifications are always drained.
    pub fn perform_handshake(
        &self,
        pads_per_bank: usize,
    ) -> Result<lincaster_proto::state_dump::ParsedPadState> {
        let result = self.perform_handshake_inner(pads_per_bank);

        // Start the background reader now that the state dump has been read.
        if let Some(shared) = self.handle.lock().unwrap().as_ref() {
            self.start_reader(Arc::clone(shared));
        }

        result
    }

    fn perform_handshake_inner(
        &self,
        pads_per_bank: usize,
    ) -> Result<lincaster_proto::state_dump::ParsedPadState> {
        info!("Performing HID handshake...");

        // Send handshake
        let hs = lincaster_proto::hid::handshake();
        self.send_report(&hs)?;

        // Read device identification response (Type 0x02).
        // The device may have pending Type 0x04 property-change notifications
        // queued from before we connected, so we must skip those until we see
        // the 0x02 device identification message.
        let mut got_device_id = false;
        for attempt in 0..20 {
            match self.read_report() {
                Ok(data) => {
                    let msg_type = data.first().copied().unwrap_or(0);
                    if msg_type == 0x02 {
                        // Type 0x02 = Device Identification
                        if let Ok(name) = parse_device_name(&data) {
                            info!("Device identified: {}", name);
                        }
                        got_device_id = true;
                        break;
                    }
                    debug!(
                        "Skipping pre-handshake notification: type=0x{:02X} len={} (attempt {})",
                        msg_type,
                        data.len(),
                        attempt + 1
                    );
                }
                Err(e) => {
                    warn!(
                        "Timeout waiting for device identification (attempt {}): {}",
                        attempt + 1,
                        e
                    );
                    break;
                }
            }
        }
        if !got_device_id {
            warn!("No device identification (0x02) received; continuing anyway");
        }

        // Request the state dump — the device does NOT send it automatically
        // after the handshake. The host must send an explicit request command.
        info!("Sending state dump request...");
        let req = lincaster_proto::hid::request_state_dump();
        self.send_report(&req)?;

        // The device will now send a full state dump (Type 0x04, ~186KB across
        // ~730 reports).  Use read timeout to detect end-of-dump — do NOT stop
        // on non-0x04 messages since the device may interleave notifications.
        //
        // IMPORTANT: The device (especially the Pro II) may send small Type 0x04
        // property-change notifications BEFORE the actual state dump blob.  The
        // dump start is the first 0x04 report whose declared payload_len is
        // large (> 1024 bytes).  Small notifications that arrive before it are
        // skipped so they don't corrupt the reassembled dump.
        info!("Reading initial state dump...");
        let mut state_reports = Vec::new();
        let mut consecutive_timeouts = 0;
        loop {
            match self.read_report() {
                Ok(data) => {
                    consecutive_timeouts = 0;
                    let msg_type = data.first().copied().unwrap_or(0);
                    if msg_type == 0x04 {
                        // If we haven't found the dump start yet, check
                        // the declared payload_len in the header. Individual
                        // notifications fit in a single 256-byte report
                        // (payload < 252), while the state dump is >100KB.
                        if state_reports.is_empty() && data.len() >= 5 {
                            let payload_len =
                                u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
                            if payload_len < 1024 {
                                debug!(
                                    "Skipping pre-dump 0x04 notification (payload_len={})",
                                    payload_len
                                );
                                continue;
                            }
                            info!(
                                "State dump start: payload_len={} first_bytes={:02X?}",
                                payload_len,
                                &data[..data.len().min(16)]
                            );
                        }
                        state_reports.push(data);
                    } else {
                        debug!(
                            "Skipping non-0x04 report during state dump: type=0x{:02X} len={}",
                            msg_type,
                            data.len()
                        );
                    }
                }
                Err(_) => {
                    consecutive_timeouts += 1;
                    if state_reports.is_empty() {
                        // Haven't received any state dump yet — keep waiting
                        // for a few more timeouts in case the device is slow.
                        if consecutive_timeouts >= 5 {
                            warn!(
                                "No state dump reports received after {} timeouts",
                                consecutive_timeouts
                            );
                            break;
                        }
                    } else {
                        // We were receiving data and hit a timeout — dump is done.
                        break;
                    }
                }
            }
        }

        let total_bytes: usize = state_reports.iter().map(|r| r.len()).sum();
        info!(
            "Collected {} state dump reports ({} bytes total)",
            state_reports.len(),
            total_bytes
        );

        // Parse the state dump
        let pad_state = if !state_reports.is_empty() {
            if let Some(payload) =
                lincaster_proto::state_dump::reassemble_state_dump(&state_reports)
            {
                info!("Parsing state dump ({} bytes payload)...", payload.len());

                // Save to disk for debugging
                if let Err(e) = std::fs::write("/tmp/lincaster_state_dump_live.bin", &payload) {
                    warn!("Could not save state dump to /tmp: {}", e);
                } else {
                    info!("Saved live state dump to /tmp/lincaster_state_dump_live.bin");
                }

                let parsed =
                    lincaster_proto::state_dump::parse_pad_configs(&payload, pads_per_bank);
                let assigned: usize = parsed
                    .banks
                    .iter()
                    .flat_map(|bank| bank.iter())
                    .filter(|p| !matches!(p.assignment, lincaster_proto::PadAssignment::Off))
                    .count();
                let mapped: usize = parsed.hid_index_map.iter().filter(|x| x.is_some()).count();
                info!(
                    "Parsed pad state: {} assigned pads across {} banks ({} HID indices mapped, SOUNDPADS: {} total children, {} PAD, {} non-PAD)",
                    assigned,
                    parsed.banks.len(),
                    mapped,
                    parsed.total_children,
                    parsed.num_pad_children,
                    parsed.total_children.saturating_sub(parsed.num_pad_children),
                );
                parsed
            } else {
                warn!("Failed to reassemble state dump; using defaults");
                default_pad_state(pads_per_bank)
            }
        } else {
            warn!("No state dump received; using defaults");
            default_pad_state(pads_per_bank)
        };

        info!("Handshake complete");
        Ok(pad_state)
    }

    /// Enter or exit transfer mode on the device.
    /// Transfer mode (mode=2) tells the RØDECaster a host app is actively
    /// editing sound pads. Normal mode (mode=0) returns the device to
    /// standalone operation.
    pub fn set_transfer_mode(&self, editing: bool) -> Result<()> {
        let mode = if editing { 2u32 } else { 0u32 };
        let cmd = lincaster_proto::hid::set_transfer_mode(mode);
        self.send_report(&cmd)?;
        self.in_transfer_mode.store(editing, Ordering::SeqCst);
        info!(
            "Transfer mode set to {}",
            if editing { "editing" } else { "normal" }
        );
        Ok(())
    }

    /// Assign a sound file to a pad on the device.
    ///
    /// Protocol flow (from Pro II pcap capture analysis —
    /// see captures/sound_assignment_analysis.md):
    ///   1. Full pad clear (announce + all defaults — NO section redirect)
    ///   2. Set padType=1 (Sound) + envelope defaults + auto name + colour
    ///   3. Send remountPadStorage = true
    ///   4. Wait for device confirmation (remountPadStorage = false)
    ///   5. Set padFilePath with full device path
    ///   6. Set padName (overwrites auto placeholder)
    ///
    /// The Windows app does NOT send padActive, padPlayMode, padLoop,
    /// padReplay, or padGain during file assignment — these are already
    /// set to defaults during the clear sequence.
    ///
    /// Uses long form addressing (specific pad by hw_index) since the daemon
    /// controls pads programmatically rather than through the touchscreen UI.
    ///
    /// The file must already be copied to device storage before calling this.
    /// `hw_index` is the SOUNDPADS HID index for the target pad.
    /// `pad_idx` is the logical pad index (bank * pads_per_bank + position).
    /// `device_path` is the full device-internal path (e.g. "/Application/emmc-data/pads/17/sound.mp3").
    /// `display_name` is the human-readable name shown on the device display.
    pub fn assign_pad_file(
        &self,
        hw_index: u8,
        pad_idx: usize,
        device_path: &str,
        display_name: &str,
        color: lincaster_proto::PadColor,
    ) -> Result<()> {
        info!(
            "Assigning pad file: hw_idx=0x{:02X} pad_idx={} path='{}' name='{}' color={:?}",
            hw_index, pad_idx, device_path, display_name, color
        );

        // Ensure device is in transfer mode — it ignores property writes otherwise.
        if !self.in_transfer_mode.load(Ordering::SeqCst) {
            info!("Device not in transfer mode; entering transfer mode for file assignment");
            self.set_transfer_mode(true)?;
            std::thread::sleep(Duration::from_millis(100));
        }

        // Step 1: Full pad clear — section announce + all property defaults.
        // The Windows app does NOT send a section redirect (04-prefix).
        let clear_cmds = lincaster_proto::hid::pad_clear_sequence(hw_index, pad_idx as u8);
        self.send_sequence(&clear_cmds)?;
        std::thread::sleep(Duration::from_millis(50));

        // Step 2: Set padType=1 (Sound) + envelope + auto name + colour.
        // No padActive needed — the Windows app doesn't send it.
        let assign_cmds = lincaster_proto::hid::pad_assign_sound(hw_index, pad_idx as u8, color);
        self.send_sequence(&assign_cmds)?;
        std::thread::sleep(INTER_COMMAND_DELAY);

        // Step 3: Trigger storage remount so device re-scans its FAT32.
        // No padFilePath clear needed for first assignment (already empty from clear).
        self.remount_completed.store(false, Ordering::SeqCst);
        self.send_report(&lincaster_proto::hid::remount_pad_storage())?;

        // Step 4: Wait for device to confirm remount complete (~186ms typical).
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if self.remount_completed.load(Ordering::SeqCst) {
                    info!("Remount confirmation received from device");
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if !self.remount_completed.load(Ordering::SeqCst) {
                warn!("Timed out waiting for remountPadStorage confirmation (5s)");
            }
        }

        // Step 5: Set padFilePath (2ms after ack in capture).
        self.send_report(&lincaster_proto::hid::set_pad_property(
            hw_index,
            "padFilePath",
            &lincaster_proto::hid::encode_string(device_path),
        ))?;
        std::thread::sleep(INTER_COMMAND_DELAY);

        // Step 6: Set padName (overwrites the auto "Sound NN" placeholder).
        if !display_name.is_empty() {
            self.send_report(&lincaster_proto::hid::set_pad_property(
                hw_index,
                "padName",
                &lincaster_proto::hid::encode_string(display_name),
            ))?;
        } else {
            self.send_report(&lincaster_proto::hid::set_pad_property(
                hw_index,
                "padName",
                &lincaster_proto::hid::encode_enum_clear(),
            ))?;
        }
        std::thread::sleep(INTER_COMMAND_DELAY);

        info!("Pad file assignment complete");
        Ok(())
    }

    /// Apply a full pad configuration to the device.
    /// This performs: clear → assign type → set properties.
    /// `hw_index` is the sequential SOUNDPADS index from the state-dump HID index map.
    /// `pad_idx` is the logical pad index (bank * pads_per_bank + position).
    /// `effects_slot` is the PADEFFECTS child index for FX pads (from state dump).
    pub fn apply_pad_config(
        &self,
        hw_index: u8,
        position: u8,
        pad_idx: usize,
        effects_slot: Option<u8>,
        config: &lincaster_proto::SoundPadConfig,
    ) -> Result<()> {
        info!(
            "Applying pad config: pos={} hw_idx=0x{:02X} pad_idx={} effects_slot={:?} type={:?}",
            position, hw_index, pad_idx, effects_slot, config.assignment
        );

        // Ensure device is in transfer mode before making changes.
        if !self.in_transfer_mode.load(Ordering::SeqCst) {
            info!("Device not in transfer mode; entering transfer mode automatically");
            self.set_transfer_mode(true)?;
            std::thread::sleep(Duration::from_millis(100));
        }

        use lincaster_proto::PadAssignment;

        // For Off, delegate to clear_pad which sends the property-reset
        // sequence + remount.  Don't fall through to the assign flow which
        // would redundantly send the same clear sequence as a prefix.
        if matches!(config.assignment, PadAssignment::Off) {
            return self.clear_pad(hw_index, pad_idx);
        }

        // Step 1: Clear/reset the pad (section announce + 47 property defaults).
        // Required before assigning a new type so all properties start clean.
        let clear_cmds = lincaster_proto::hid::pad_clear_sequence(hw_index, pad_idx as u8);
        self.send_sequence(&clear_cmds)?;

        // Brief pause after reset
        std::thread::sleep(Duration::from_millis(50));

        // Step 2: Activate the pad for editing (required for property writes
        // to take effect — the clear sequence sets padActive=false).
        // For FX pads, padActive is sent later (after padType) to match the
        // Windows capture order.
        if !matches!(config.assignment, PadAssignment::Effect(_)) {
            self.send_report(&lincaster_proto::hid::activate_pad(hw_index))?;
            std::thread::sleep(INTER_COMMAND_DELAY);
        }

        // Step 3: Assign new type and set properties
        match &config.assignment {
            PadAssignment::Off => unreachable!(),
            PadAssignment::Sound(sound) => {
                let assign_cmds =
                    lincaster_proto::hid::pad_assign_sound(hw_index, pad_idx as u8, sound.color);
                self.send_sequence(&assign_cmds)?;

                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set sound-specific properties (all long-form addressing)
                let play_mode = match sound.play_mode {
                    lincaster_proto::PlayMode::OneShot => 0,
                    lincaster_proto::PlayMode::Toggle => 1,
                    lincaster_proto::PlayMode::Hold => 2,
                };
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padPlayMode",
                    &lincaster_proto::hid::encode_u32(play_mode),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padLoop",
                    &lincaster_proto::hid::encode_bool(sound.loop_enabled),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                let replay = sound.replay_mode == lincaster_proto::ReplayMode::Replay;
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padReplay",
                    &lincaster_proto::hid::encode_bool(replay),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padGain",
                    &lincaster_proto::hid::encode_f64(sound.gain_db),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set pad name (always send — either name or clear)
                if !config.name.is_empty() {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_string(&config.name),
                    ))?;
                } else {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_enum_clear(),
                    ))?;
                }
                std::thread::sleep(INTER_COMMAND_DELAY);

                // File assignment: if the file_path is a device-internal path
                // (starts with /Application/), the file has already been copied
                // to device storage.  Trigger a storage remount so the device
                // firmware re-scans FAT32, then set padFilePath + padName.
                //
                // Protocol sequence (from Windows capture):
                //   remountPadStorage = true   → wait for ack (~186ms)
                //   padFilePath = device_path  → padName = display_name
                if sound.file_path.starts_with("/Application/") {
                    self.remount_completed.store(false, Ordering::SeqCst);
                    self.send_report(&lincaster_proto::hid::remount_pad_storage())?;

                    // Wait for device confirmation (remountPadStorage = false)
                    {
                        let deadline = std::time::Instant::now() + Duration::from_secs(5);
                        while std::time::Instant::now() < deadline {
                            if self.remount_completed.load(Ordering::SeqCst) {
                                info!("Remount confirmation received from device");
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if !self.remount_completed.load(Ordering::SeqCst) {
                            warn!("Timed out waiting for remountPadStorage confirmation (5s)");
                        }
                    }

                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padFilePath",
                        &lincaster_proto::hid::encode_string(&sound.file_path),
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);

                    // Overwrite the auto-generated padName with actual display name
                    if !config.name.is_empty() {
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padName",
                            &lincaster_proto::hid::encode_string(&config.name),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                    }
                }
            }
            PadAssignment::Effect(effect) => {
                // Protocol order (from captures):
                //   1. Pad clear (done above)
                //   2. Effects section announcement + all effects properties
                //   3. padType = 2, padName, padColourIndex
                //   4. padEffectTriggerMode, padActive
                //   5. padEffectInput

                // Step 2: Effects announcement + properties (MUST come before padType)
                let slot = effects_slot.unwrap_or(hw_index);

                self.send_report(&lincaster_proto::hid::effects_section_announce(slot))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Reverb
                self.send_report(&lincaster_proto::hid::set_reverb_on(
                    slot,
                    effect.reverb.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);
                if effect.reverb.enabled {
                    self.send_report(&lincaster_proto::hid::set_reverb_model(
                        slot,
                        effect.reverb.model.to_wire(),
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_reverb_mix(
                        slot,
                        effect.reverb.mix,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_reverb_low_cut(
                        slot,
                        effect.reverb.low_cut,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_reverb_high_cut(
                        slot,
                        effect.reverb.high_cut,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                }

                // Echo
                self.send_report(&lincaster_proto::hid::set_echo_on(
                    slot,
                    effect.echo.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);
                if effect.echo.enabled {
                    self.send_report(&lincaster_proto::hid::set_echo_mix(slot, effect.echo.mix))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_echo_low_cut(
                        slot,
                        effect.echo.low_cut,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_echo_high_cut(
                        slot,
                        effect.echo.high_cut,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_echo_delay(
                        slot,
                        effect.echo.delay,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                    self.send_report(&lincaster_proto::hid::set_echo_decay(
                        slot,
                        effect.echo.decay,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                }

                // Megaphone (distortion)
                self.send_report(&lincaster_proto::hid::set_distortion_on(
                    slot,
                    effect.megaphone.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);
                if effect.megaphone.enabled {
                    self.send_report(&lincaster_proto::hid::set_distortion_intensity(
                        slot,
                        effect.megaphone.intensity,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                }

                // Robot
                self.send_report(&lincaster_proto::hid::set_robot_on(
                    slot,
                    effect.robot.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);
                if effect.robot.enabled {
                    self.send_report(&lincaster_proto::hid::set_robot_mix(slot, effect.robot.mix))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                }

                // Voice Disguise
                self.send_report(&lincaster_proto::hid::set_voice_disguise_on(
                    slot,
                    effect.voice_disguise.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Pitch Shift
                self.send_report(&lincaster_proto::hid::set_pitch_shift_on(
                    slot,
                    effect.pitch_shift.enabled,
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);
                if effect.pitch_shift.enabled {
                    self.send_report(&lincaster_proto::hid::set_pitch_shift_semitones(
                        slot,
                        effect.pitch_shift.semitones,
                    ))?;
                    std::thread::sleep(INTER_COMMAND_DELAY);
                }

                // Write effectsIdx to link this effects slot to the pad
                self.send_report(&lincaster_proto::hid::set_effects_property(
                    slot,
                    "effectsIdx",
                    &lincaster_proto::hid::encode_u32(pad_idx as u32),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Step 3: Assign pad type (padType=2, padColourIndex)
                let assign_cmds = lincaster_proto::hid::pad_assign_fx(hw_index, effect.color);
                self.send_sequence(&assign_cmds)?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Step 4: Finalize — padEffectTriggerMode, padActive, padEffectInput, padName
                // (matches Windows capture order: these come after padType)
                let trigger_mode = match effect.latch_mode {
                    lincaster_proto::LatchMode::Latch => 1,
                    lincaster_proto::LatchMode::Momentary => 0,
                };
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padEffectTriggerMode",
                    &lincaster_proto::hid::encode_u32(trigger_mode),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                self.send_report(&lincaster_proto::hid::activate_pad(hw_index))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set FX input source
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padEffectInput",
                    &lincaster_proto::hid::encode_u32(effect.input_source.to_wire()),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set pad name
                if !config.name.is_empty() {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_string(&config.name),
                    ))?;
                } else {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_enum_clear(),
                    ))?;
                }
                std::thread::sleep(INTER_COMMAND_DELAY);
            }
            PadAssignment::Mixer(mixer) => {
                let assign_cmds = lincaster_proto::hid::pad_assign_mixer(hw_index, mixer.color);
                self.send_sequence(&assign_cmds)?;

                std::thread::sleep(INTER_COMMAND_DELAY);

                let mode = match mixer.mode {
                    lincaster_proto::MixerMode::Censor => 0,
                    lincaster_proto::MixerMode::TrashTalk => 1,
                    lincaster_proto::MixerMode::FadeInOut => 2,
                    lincaster_proto::MixerMode::BackChannel => 3,
                    lincaster_proto::MixerMode::Ducking => 4,
                };
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padMixerMode",
                    &lincaster_proto::hid::encode_u32(mode),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set mixer trigger mode (latch/momentary)
                // Device wire values: 0 = Momentary (hold), 1 = Latch (toggle).
                let trigger_mode = match mixer.latch_mode {
                    lincaster_proto::LatchMode::Latch => 1,
                    lincaster_proto::LatchMode::Momentary => 0,
                };
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padMixerTriggerMode",
                    &lincaster_proto::hid::encode_u32(trigger_mode),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Mode-specific properties
                match mixer.mode {
                    lincaster_proto::MixerMode::Censor => {
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padMixerCensorCustom",
                            &lincaster_proto::hid::encode_bool(mixer.censor_custom),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padGain",
                            &lincaster_proto::hid::encode_f64(mixer.beep_gain_db),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                    }
                    lincaster_proto::MixerMode::FadeInOut => {
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padMixerFadeInSeconds",
                            &lincaster_proto::hid::encode_f64(mixer.fade_in_seconds),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padMixerFadeOutSeconds",
                            &lincaster_proto::hid::encode_f64(mixer.fade_out_seconds),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padMixerFadeExcludeHost",
                            &lincaster_proto::hid::encode_bool(mixer.fade_exclude_host),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                    }
                    lincaster_proto::MixerMode::BackChannel => {
                        let channels = [
                            ("padMixerBackChannelMic2", mixer.back_channel_mic2),
                            ("padMixerBackChannelMic3", mixer.back_channel_mic3),
                            ("padMixerBackChannelMic4", mixer.back_channel_mic4),
                            (
                                "padMixerBackChannelUsb1Comms",
                                mixer.back_channel_usb1_comms,
                            ),
                            ("padMixerBackChannelUsb2Main", mixer.back_channel_usb2_main),
                            ("padMixerBackChannelBluetooth", mixer.back_channel_bluetooth),
                            ("padMixerBackChannelCallMe1", mixer.back_channel_callme1),
                            ("padMixerBackChannelCallMe2", mixer.back_channel_callme2),
                            ("padMixerBackChannelCallMe3", mixer.back_channel_callme3),
                        ];
                        for (prop, enabled) in &channels {
                            self.send_report(&lincaster_proto::hid::set_pad_property(
                                hw_index,
                                prop,
                                &lincaster_proto::hid::encode_bool(*enabled),
                            ))?;
                            std::thread::sleep(INTER_COMMAND_DELAY);
                        }
                    }
                    lincaster_proto::MixerMode::Ducking => {
                        self.send_report(&lincaster_proto::hid::set_ducker_depth(
                            mixer.ducker_depth_db,
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                    }
                    _ => {}
                }

                // Set pad name (always send — either name or clear)
                if !config.name.is_empty() {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_string(&config.name),
                    ))?;
                } else {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_enum_clear(),
                    ))?;
                }
                std::thread::sleep(INTER_COMMAND_DELAY);
            }
            PadAssignment::Trigger(trigger) => {
                let assign_cmds = lincaster_proto::hid::pad_assign_midi(hw_index, trigger.color);
                self.send_sequence(&assign_cmds)?;

                std::thread::sleep(INTER_COMMAND_DELAY);

                // Enable custom mode and set MIDI parameters
                self.send_report(&lincaster_proto::hid::set_pad_property(
                    hw_index,
                    "padTriggerCustom",
                    &lincaster_proto::hid::encode_bool(true),
                ))?;
                std::thread::sleep(INTER_COMMAND_DELAY);

                match &trigger.trigger_type {
                    lincaster_proto::TriggerType::MidiNote {
                        channel,
                        note,
                        velocity,
                    } => {
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padTriggerType",
                            &lincaster_proto::hid::encode_u32(1),
                        ))?; // Note
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padTriggerChannel",
                            &lincaster_proto::hid::encode_u32(*channel as u32),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padTriggerControl",
                            &lincaster_proto::hid::encode_u32(*note as u32),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padTriggerOn",
                            &lincaster_proto::hid::encode_u32(*velocity as u32),
                        ))?;
                        std::thread::sleep(INTER_COMMAND_DELAY);
                        self.send_report(&lincaster_proto::hid::set_pad_property(
                            hw_index,
                            "padTriggerOff",
                            &lincaster_proto::hid::encode_u32(0),
                        ))?;
                    }
                }
                std::thread::sleep(INTER_COMMAND_DELAY);

                // Set pad name (always send — either name or clear)
                if !config.name.is_empty() {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_string(&config.name),
                    ))?;
                } else {
                    self.send_report(&lincaster_proto::hid::set_pad_property(
                        hw_index,
                        "padName",
                        &lincaster_proto::hid::encode_enum_clear(),
                    ))?;
                }
                std::thread::sleep(INTER_COMMAND_DELAY);
            }
        }

        info!("Pad config applied successfully");
        Ok(())
    }

    /// Clear/reset a pad to Off using the capture-verified 3-command protocol.
    ///
    /// From captures/soundpad_clear_sound.pcapng (Windows RØDE Central app):
    ///   1. Set padFilePath = "" (clear file association)
    ///   2. Section redirect (04-prefix) to the PAD section
    ///   3. remountPadStorage = true (trigger firmware re-scan)
    ///
    /// No file deletion, no transfer mode entry, no 47-property reset needed.
    /// The remount causes the firmware to re-scan storage and see the empty
    /// padFilePath, transitioning the pad to an unassigned state.
    pub fn clear_pad(&self, hw_index: u8, pad_idx: usize) -> Result<()> {
        info!(
            "Clearing pad: hw_idx=0x{:02X} pad_idx={}",
            hw_index, pad_idx
        );

        // Ensure device is in transfer mode before making changes.
        if !self.in_transfer_mode.load(Ordering::SeqCst) {
            info!("Device not in transfer mode; entering transfer mode automatically");
            self.set_transfer_mode(true)?;
            std::thread::sleep(Duration::from_millis(100));
        }

        // Send the simple 3-command clear sequence (matching Windows protocol).
        let clear_cmds = lincaster_proto::hid::pad_clear_simple(hw_index);
        self.remount_completed.store(false, Ordering::SeqCst);
        self.send_sequence(&clear_cmds)?;

        // Wait for device to confirm remount complete.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.remount_completed.load(Ordering::SeqCst) {
                info!("Remount confirmation received from device (clear)");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !self.remount_completed.load(Ordering::SeqCst) {
            warn!("Timed out waiting for remountPadStorage confirmation during clear (5s)");
        }

        info!("Pad cleared successfully");
        Ok(())
    }

    /// Send a single pad colour change to the device (immediate/live feedback).
    pub fn set_pad_color(&self, hw_index: u8, color: lincaster_proto::PadColor) -> Result<()> {
        self.send_report(&lincaster_proto::hid::set_pad_colour_at(hw_index, color))
    }
}

impl Drop for HidDevice {
    fn drop(&mut self) {
        if self.is_connected() {
            info!("HidDevice dropped while connected — running disconnect");
            self.disconnect(true);
        }
    }
}

/// Build default (all-Off) pad state.
fn default_pad_state(pads_per_bank: usize) -> lincaster_proto::state_dump::ParsedPadState {
    let banks = (0..8)
        .map(|_| {
            (0..pads_per_bank)
                .map(|i| lincaster_proto::SoundPadConfig {
                    pad_index: i as u8,
                    name: String::new(),
                    assignment: lincaster_proto::PadAssignment::Off,
                })
                .collect()
        })
        .collect();
    let total = 8 * pads_per_bank;
    // Default identity mapping: padIdx N → HID index N
    let hid_index_map = (0..total).map(|i| Some(i as u8)).collect();
    lincaster_proto::state_dump::ParsedPadState {
        banks,
        hid_index_map,
        effects_slot_map: std::collections::HashMap::new(),
        total_children: total,
        num_pad_children: 0,
        effects_total_children: 0,
    }
}

/// Parse device name from a Type 0x02 identification response.
fn parse_device_name(data: &[u8]) -> Result<String> {
    // Skip type byte (0x02) and look for printable ASCII
    let mut name_bytes = Vec::new();
    for &b in &data[1..] {
        if b >= 0x20 && b < 0x7f {
            name_bytes.push(b);
        } else if !name_bytes.is_empty() && b == 0x00 {
            break;
        }
    }
    String::from_utf8(name_bytes).context("Invalid device name encoding")
}

/// Spawn a detached `cat` process that holds the hidraw device open, keeping
/// usbhid polling EP5 IN so the firmware notification FIFO never fills and
/// freezing physical controls.  The spawned process reads (and discards)
/// data until the device is unplugged, then exits.
fn spawn_hidraw_drain() {
    // Find the hidraw device by matching the HID_ID in sysfs, then exec
    // `cat` to hold it open.  `cat` blocks reading forever; when the device
    // is unplugged it gets an I/O error and exits.
    let script = concat!(
        "sleep 0.3; ",
        "for d in /sys/class/hidraw/hidraw*/device/uevent; do ",
        "  if grep -qE 'HID_ID=0003:000019F7:0000(0079|0078)' \"$d\" 2>/dev/null; then ",
        "    dev=\"/dev/$(basename $(dirname $(dirname \"$d\")))\"; ",
        "    exec cat \"$dev\" > /dev/null 2>&1; ",
        "  fi; ",
        "done"
    );

    match std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            info!(
                "Spawned hidraw drain process (pid {}) to keep EP5 IN polled",
                child.id()
            );
        }
        Err(e) => {
            warn!(
                "Failed to spawn hidraw drain process: {}. \
                 Physical controls may freeze after ~2 presses.",
                e
            );
        }
    }
}
