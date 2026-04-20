mod cli_exec;
mod dbus_client;
mod routing_view;
mod sound_pad_view;

use std::sync::mpsc;

use cli_exec::GuiCommand;
use dbus_client::{BusInfo, DaemonUpdate};
use egui::Color32;
use lincaster_proto::{DeviceIdentity, StreamSnapshot, RODECASTER_PRO_II_PID};
use routing_view::DragState;
use sound_pad_view::SoundPadState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Routing,
    SoundPads,
}

struct LinCasterApp {
    // Daemon communication
    update_rx: mpsc::Receiver<DaemonUpdate>,
    command_tx: mpsc::Sender<GuiCommand>,

    // Daemon state
    busses: Vec<BusInfo>,
    streams: Vec<StreamSnapshot>,
    connected: bool,
    device: Option<DeviceIdentity>,

    // UI state
    active_tab: Tab,
    drag_state: Option<DragState>,
    sound_pad_state: SoundPadState,
    pads_initialized: bool,
    manual_override: bool,
}

impl LinCasterApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (update_rx, command_tx) = dbus_client::start_comm_thread();
        Self {
            update_rx,
            command_tx,
            busses: Vec::new(),
            streams: Vec::new(),
            connected: false,
            device: None,
            active_tab: Tab::Routing,
            drag_state: None,
            sound_pad_state: SoundPadState::default(),
            pads_initialized: false,
            manual_override: false,
        }
    }

    fn process_updates(&mut self) {
        while let Ok(update) = self.update_rx.try_recv() {
            match update {
                DaemonUpdate::State {
                    busses,
                    streams,
                    device,
                    pad_configs,
                    current_bank,
                } => {
                    self.busses = busses;
                    self.streams = streams;
                    self.connected = true;

                    // Update pad count if device changed
                    if self.device.as_ref().map(|d| d.usb_product_id)
                        != device.as_ref().map(|d| d.usb_product_id)
                    {
                        let pads = device_pad_count(&device);
                        if pads != self.sound_pad_state.pads_per_bank {
                            self.sound_pad_state = SoundPadState::new(pads);
                            self.pads_initialized = false;
                        }
                    }
                    self.device = device;

                    // Sync pad configs from daemon on every poll cycle.
                    // The daemon is the source of truth — imports, clears, and
                    // on-device edits all update shared_pad_configs, so the GUI
                    // must continuously reflect that state.  Skip the
                    // currently-selected pad so local edits aren't clobbered
                    // before the user applies them.
                    if !pad_configs.is_empty() {
                        let state = &mut self.sound_pad_state;
                        let editing_pad = state.selected_pad;
                        let editing_bank = state.current_bank;
                        for (bank_idx, bank) in pad_configs.iter().enumerate() {
                            if bank_idx < state.banks.len() {
                                for (pad_idx, pad) in bank.iter().enumerate() {
                                    if pad_idx < state.banks[bank_idx].len() {
                                        // Don't overwrite the pad the user is editing
                                        if bank_idx == editing_bank && editing_pad == Some(pad_idx)
                                        {
                                            continue;
                                        }
                                        state.banks[bank_idx][pad_idx] = pad.clone();
                                    }
                                }
                            }
                        }
                        self.pads_initialized = true;
                    }

                    // Sync active bank from device. Only update when the user
                    // has no pad selected (to avoid switching away mid-edit).
                    if let Some(bank) = current_bank {
                        let bank = bank as usize;
                        if self.sound_pad_state.selected_pad.is_none()
                            && bank != self.sound_pad_state.current_bank
                        {
                            self.sound_pad_state.current_bank = bank;
                        }
                    }
                }
                DaemonUpdate::Disconnected => {
                    self.connected = false;
                    self.pads_initialized = false;
                }
            }
        }
    }
}

impl eframe::App for LinCasterApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Exit transfer mode on app close
        let _ = self.command_tx.send(cli_exec::GuiCommand::ExitTransferMode);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_updates();

        // Request periodic repaints for live updates
        ctx.request_repaint_after(std::time::Duration::from_millis(200));

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new("LinCaster")
                        .color(Color32::from_rgb(200, 200, 210))
                        .size(18.0),
                );

                ui.add_space(24.0);

                let routing_selected = self.active_tab == Tab::Routing;
                if ui.selectable_label(routing_selected, "Routing").clicked() {
                    if self.active_tab == Tab::SoundPads {
                        let _ = self.command_tx.send(GuiCommand::ExitTransferMode);
                    }
                    self.active_tab = Tab::Routing;
                }

                let pads_selected = self.active_tab == Tab::SoundPads;
                if ui.selectable_label(pads_selected, "Sound Pads").clicked() {
                    if self.active_tab != Tab::SoundPads {
                        let _ = self.command_tx.send(GuiCommand::EnterTransferMode);
                        self.pads_initialized = false;
                    }
                    self.active_tab = Tab::SoundPads;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status_color = if self.connected {
                        Color32::from_rgb(80, 200, 100)
                    } else {
                        Color32::from_rgb(200, 80, 80)
                    };
                    let status_text = if self.connected {
                        "Daemon Connected"
                    } else {
                        "Daemon Disconnected"
                    };
                    ui.label(
                        egui::RichText::new(status_text)
                            .color(status_color)
                            .size(12.0),
                    );
                    ui.painter().circle_filled(
                        egui::pos2(ui.min_rect().min.x - 10.0, ui.min_rect().center().y),
                        4.0,
                        status_color,
                    );

                    ui.add_space(16.0);

                    // Device status banner
                    let (dev_text, dev_color) = device_banner_info(&self.device, self.connected);
                    ui.label(egui::RichText::new(dev_text).color(dev_color).size(12.0));
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.connected {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("Waiting for lincasterd daemon...")
                            .size(16.0)
                            .color(Color32::from_rgb(160, 160, 170)),
                    );
                });
                return;
            }

            match self.active_tab {
                Tab::Routing => {
                    let action = routing_view::draw_routing_view(
                        ui,
                        &self.streams,
                        &self.busses,
                        &mut self.drag_state,
                        &mut self.manual_override,
                    );

                    if let Some(routing_view::RoutingAction::Route(node_id, target)) = &action {
                        match target {
                            Some(bus_id) => {
                                let _ = self.command_tx.send(GuiCommand::RouteStream {
                                    node_id: *node_id,
                                    bus_id: bus_id.clone(),
                                });
                            }
                            None => {
                                let _ = self
                                    .command_tx
                                    .send(GuiCommand::UnrouteStream { node_id: *node_id });
                            }
                        }
                    }
                    if let Some(routing_view::RoutingAction::SetManualOverride(enabled)) = &action {
                        let _ = self
                            .command_tx
                            .send(GuiCommand::SetManualOverride { enabled: *enabled });
                    }
                }
                Tab::SoundPads => {
                    for pad_action in
                        sound_pad_view::draw_sound_pad_view(ui, &mut self.sound_pad_state)
                    {
                        match pad_action {
                            sound_pad_view::PadAction::ApplyConfig {
                                bank,
                                position,
                                config,
                            } => {
                                if let Ok(json) = serde_json::to_string(&config) {
                                    let _ = self.command_tx.send(GuiCommand::ApplyPadConfig {
                                        bank,
                                        position,
                                        config_json: json,
                                    });
                                }
                            }
                            sound_pad_view::PadAction::ClearPad { bank, position } => {
                                let _ = self
                                    .command_tx
                                    .send(GuiCommand::ClearPad { bank, position });
                            }
                            sound_pad_view::PadAction::SetBank(bank) => {
                                let _ = self.command_tx.send(GuiCommand::SetPadBank { bank });
                            }
                            sound_pad_view::PadAction::NeedTransferMode => {
                                let _ = self.command_tx.send(GuiCommand::EnterTransferMode);
                            }
                            sound_pad_view::PadAction::SetPadColor {
                                bank,
                                position,
                                color,
                            } => {
                                let _ = self.command_tx.send(GuiCommand::SetPadColor {
                                    bank,
                                    position,
                                    color: color.wire_index(),
                                });
                            }
                        }
                    }
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lincaster=info")),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 500.0])
            .with_min_inner_size([520.0, 350.0])
            .with_title("LinCaster"),
        ..Default::default()
    };

    eframe::run_native(
        "LinCaster",
        options,
        Box::new(|cc| Ok(Box::new(LinCasterApp::new(cc)))),
    )
}

/// Determine device banner text and color from device identity.
fn device_banner_info(device: &Option<DeviceIdentity>, connected: bool) -> (&'static str, Color32) {
    if !connected {
        return ("", Color32::TRANSPARENT);
    }
    match device {
        Some(d) if d.is_multitrack() => (
            "Device Connected (Multitrack)",
            Color32::from_rgb(80, 200, 100),
        ),
        Some(_) => (
            "Device Connected (Limited)",
            Color32::from_rgb(220, 180, 60),
        ),
        None => ("No Device Detected", Color32::from_rgb(200, 130, 60)),
    }
}

/// Get the number of SMART pads based on device model.
/// Duo = 6, Pro II = 8, default = 6.
fn device_pad_count(device: &Option<DeviceIdentity>) -> usize {
    match device.as_ref().map(|d| d.usb_product_id) {
        Some(RODECASTER_PRO_II_PID) => 8,
        _ => 6,
    }
}
