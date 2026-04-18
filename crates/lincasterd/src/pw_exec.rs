//! PipeWire command execution via subprocess calls.
//!
//! This module provides reliable PipeWire graph mutations (creating virtual sinks,
//! setting volume, routing streams) using standard PipeWire and PulseAudio CLI tools.
//! This approach is more robust than native API calls because:
//! - Works across all PipeWire versions
//! - No threading/ownership complexity with PW proxy objects
//! - Nodes created via pactl persist independently of the creating process
//! - Clean cleanup via module unloading

use std::collections::{HashMap, HashSet};
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};

use crate::pipewire_registry::PipeWireState;
use lincaster_proto::Config;

/// Manages PipeWire virtual sinks and routing via subprocess calls.
pub struct PwExecManager {
    /// Map of bus_id -> pactl module ID for cleanup on exit.
    module_ids: HashMap<String, u32>,
    /// Map of bus_id -> PipeWire node ID (discovered via registry).
    node_ids: HashMap<String, u32>,
    /// Map of stream node_id -> target bus_id for deferred port linking.
    pending_stream_routes: HashMap<u32, String>,
    /// Map of stream node_id -> target bus_id for active (linked) routes.
    /// Used to detect and correct WirePlumber re-linking.
    active_stream_routes: HashMap<u32, String>,
    /// Stream node_ids that were auto-routed by config app_rules (not manually).
    auto_routed_streams: HashSet<u32>,
    /// When true, manual routing overrides config and disables link-correction.
    manual_override: bool,
    /// Naming prefix for all nodes.
    node_prefix: String,
}

impl PwExecManager {
    pub fn new(node_prefix: &str) -> Self {
        Self {
            module_ids: HashMap::new(),
            node_ids: HashMap::new(),
            pending_stream_routes: HashMap::new(),
            active_stream_routes: HashMap::new(),
            auto_routed_streams: HashSet::new(),
            manual_override: false,
            node_prefix: node_prefix.to_string(),
        }
    }

    /// Full node name for a bus.
    pub fn node_name(&self, bus_id: &str) -> String {
        format!("{}.{}", self.node_prefix, bus_id)
    }

    /// Check if a stream was auto-routed by a config rule.
    pub fn is_auto_routed(&self, node_id: u32) -> bool {
        self.auto_routed_streams.contains(&node_id)
    }

    /// Enable or disable manual override mode.
    pub fn set_manual_override(&mut self, enabled: bool) {
        info!(
            "Manual routing override: {}",
            if enabled { "ON" } else { "OFF" }
        );
        self.manual_override = enabled;
    }

    pub fn manual_override_enabled(&self) -> bool {
        self.manual_override
    }

    /// Mark a stream as auto-routed by config rules.
    pub fn mark_auto_routed(&mut self, node_id: u32) {
        self.auto_routed_streams.insert(node_id);
    }

    /// Clear auto-routed flag (when user manually reroutes a stream).
    pub fn clear_auto_routed(&mut self, node_id: u32) {
        self.auto_routed_streams.remove(&node_id);
    }

    /// Remove any stale virtual sinks left from a previous run.
    /// Scans `pactl list modules short` for module-null-sink entries whose
    /// arguments contain our node prefix and unloads them.
    pub fn cleanup_stale_sinks(&self) {
        let output = match Command::new("pactl")
            .args(["list", "modules", "short"])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                warn!("Failed to list pactl modules for cleanup: {}", e);
                return;
            }
        };
        if !output.status.success() {
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let prefix = &self.node_prefix;
        for line in stdout.lines() {
            // Format: "42\tmodule-null-sink\tsink_name=lincaster.system ..."
            if !line.contains("module-null-sink") || !line.contains(prefix) {
                continue;
            }
            let module_id = match line
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok())
            {
                Some(id) => id,
                None => continue,
            };
            info!("Removing stale virtual sink (module_id={})", module_id);
            let _ = Command::new("pactl")
                .args(["unload-module", &module_id.to_string()])
                .output();
        }
    }

    /// Create a virtual null-audio-sink via pactl.
    /// Returns the pactl module ID on success.
    pub fn create_virtual_sink(
        &mut self,
        bus_id: &str,
        description: &str,
        channels: u32,
    ) -> Result<u32> {
        let node_name = self.node_name(bus_id);

        // Check if already created
        if self.module_ids.contains_key(bus_id) {
            debug!("Virtual sink '{}' already exists, skipping", node_name);
            return Ok(self.module_ids[bus_id]);
        }

        info!(
            "Creating virtual sink: {} ({}) channels={}",
            node_name, description, channels
        );

        let sink_props = format!(
            "device.description=\"{}\" node.name=\"{}\" media.class=Audio/Sink monitor.channel-volumes=true",
            description, node_name
        );

        let output = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={}", node_name),
                &format!("sink_properties={}", sink_props),
                &format!("channels={}", channels),
                "rate=48000",
            ])
            .output()
            .context("Failed to execute pactl. Is pipewire-pulse running?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "pactl load-module failed for '{}': {}",
                node_name,
                stderr.trim()
            );
        }

        let module_id: u32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .context("Failed to parse pactl module ID")?;

        info!(
            "Created virtual sink '{}' (module_id={})",
            node_name, module_id
        );
        self.module_ids.insert(bus_id.to_string(), module_id);
        Ok(module_id)
    }

    /// Register a PipeWire node ID for a bus (discovered via registry events).
    pub fn register_node_id(&mut self, bus_id: &str, node_id: u32) {
        debug!("Registered node ID {} for bus '{}'", node_id, bus_id);
        self.node_ids.insert(bus_id.to_string(), node_id);
    }

    /// Get the PW node ID for a bus.
    #[allow(dead_code)]
    pub fn get_node_id(&self, bus_id: &str) -> Option<u32> {
        self.node_ids.get(bus_id).copied()
    }

    /// Set volume on a virtual sink. Volume is 0.0 to 1.0.
    pub fn set_volume(&self, bus_id: &str, volume: f32) -> Result<()> {
        let node_name = self.node_name(bus_id);
        let vol_pct = format!("{}%", (volume * 100.0).round() as u32);

        debug!("Setting volume for '{}' to {}", node_name, vol_pct);

        let output = Command::new("pactl")
            .args(["set-sink-volume", &node_name, &vol_pct])
            .output()
            .with_context(|| format!("Failed to set volume for '{}'", node_name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "pactl set-sink-volume failed for '{}': {}",
                node_name,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Set mute state on a virtual sink.
    pub fn set_mute(&self, bus_id: &str, mute: bool) -> Result<()> {
        let node_name = self.node_name(bus_id);
        let mute_str = if mute { "1" } else { "0" };

        debug!("Setting mute for '{}' to {}", node_name, mute);

        let output = Command::new("pactl")
            .args(["set-sink-mute", &node_name, mute_str])
            .output()
            .with_context(|| format!("Failed to set mute for '{}'", node_name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "pactl set-sink-mute failed for '{}': {}",
                node_name,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Route a stream node to a target virtual sink by linking output ports
    /// directly via pw-link. Existing links from the stream to other sinks are
    /// removed first, and WirePlumber link-correction re-applies our routing
    /// if WirePlumber tries to override it.
    ///
    /// If the stream's ports aren't available yet, the route is stored as pending
    /// and will be applied when `try_link_pending_stream` is called on PortAdded.
    pub fn route_stream(
        &mut self,
        stream_node_id: u32,
        target_bus_id: &str,
        pw_state: &PipeWireState,
    ) -> Result<()> {
        let target_name = self.node_name(target_bus_id);

        info!(
            "Routing stream node {} -> sink '{}'",
            stream_node_id, target_name
        );

        // Always record for deferred linking when ports appear
        self.pending_stream_routes
            .insert(stream_node_id, target_bus_id.to_string());

        // Attempt immediate linking if ports are already present
        self.link_stream_to_sink(stream_node_id, target_bus_id, pw_state)
    }

    /// Attempt to link a stream node's output ports to a virtual sink's input ports.
    /// Returns Ok even if ports aren't available yet (deferred to PortAdded).
    fn link_stream_to_sink(
        &mut self,
        stream_node_id: u32,
        target_bus_id: &str,
        pw_state: &PipeWireState,
    ) -> Result<()> {
        let sink_node_name = self.node_name(target_bus_id);
        let sink_node = match pw_state.find_node_by_name(&sink_node_name) {
            Some(n) => n,
            None => {
                debug!(
                    "Sink '{}' not yet in graph, deferring stream link",
                    sink_node_name
                );
                return Ok(());
            }
        };

        // Stream output ports
        let mut stream_ports: Vec<_> = pw_state
            .ports_for_node(stream_node_id)
            .into_iter()
            .filter(|p| p.direction == "out")
            .collect();
        stream_ports.sort_by_key(|p| port_channel_index(&p.name));

        if stream_ports.is_empty() {
            debug!(
                "Stream node {} has no output ports yet, deferring",
                stream_node_id
            );
            return Ok(());
        }

        // Sink input (playback) ports
        let mut sink_ports: Vec<_> = pw_state
            .ports_for_node(sink_node.id)
            .into_iter()
            .filter(|p| p.direction == "in")
            .collect();
        sink_ports.sort_by_key(|p| port_channel_index(&p.name));

        if sink_ports.is_empty() {
            debug!(
                "Sink '{}' has no input ports yet, deferring",
                sink_node_name
            );
            return Ok(());
        }

        // Disconnect existing output links from the stream (WirePlumber's default routing)
        let stream_port_ids: Vec<u32> = stream_ports.iter().map(|p| p.id).collect();
        let sink_port_ids: Vec<u32> = sink_ports.iter().map(|p| p.id).collect();
        let existing_links: Vec<_> = pw_state
            .links
            .values()
            .filter(|l| {
                stream_port_ids.contains(&l.output_port) && !sink_port_ids.contains(&l.input_port)
            })
            .collect();
        for link in &existing_links {
            debug!(
                "Removing existing link {} (port {} -> port {})",
                link.id, link.output_port, link.input_port
            );
            if let Err(e) = self.destroy_link(link.output_port, link.input_port) {
                warn!("Failed to remove old link {}: {:#}", link.id, e);
            }
        }

        // Link matching ports (typically FL->FL, FR->FR)
        let pairs = stream_ports.len().min(sink_ports.len());
        for i in 0..pairs {
            info!(
                "Linking stream port {} ({}) -> sink port {} ({})",
                stream_ports[i].id, stream_ports[i].name, sink_ports[i].id, sink_ports[i].name
            );
            if let Err(e) = self.create_link(stream_ports[i].id, sink_ports[i].id) {
                warn!(
                    "Failed to link stream port {} -> sink port {}: {:#}",
                    stream_ports[i].id, sink_ports[i].id, e
                );
            }
        }

        // Route applied — move from pending to active
        info!(
            "Stream node {} linked to sink '{}' ({} port pairs, {} old links removed)",
            stream_node_id,
            sink_node_name,
            pairs,
            existing_links.len()
        );
        self.active_stream_routes
            .insert(stream_node_id, target_bus_id.to_string());
        self.pending_stream_routes.remove(&stream_node_id);
        Ok(())
    }

    /// Called when a new port appears. If it belongs to a stream with a pending
    /// route, attempt to complete the port-level linking.
    pub fn try_link_pending_stream(
        &mut self,
        port_node_id: u32,
        pw_state: &PipeWireState,
    ) -> Result<()> {
        let target_bus_id = match self.pending_stream_routes.get(&port_node_id) {
            Some(bus_id) => bus_id.clone(),
            None => return Ok(()),
        };

        // link_stream_to_sink will move pending→active if ports are ready
        self.link_stream_to_sink(port_node_id, &target_bus_id, pw_state)
    }

    /// Clean up tracking state when a stream node is removed.
    pub fn remove_stream_route(&mut self, node_id: u32) {
        self.pending_stream_routes.remove(&node_id);
        self.active_stream_routes.remove(&node_id);
        self.auto_routed_streams.remove(&node_id);
    }

    /// Called when a new link appears. If WirePlumber created a link from a
    /// stream we've already routed to a different sink, destroy that link
    /// and re-apply our routing. Skipped when manual override is enabled.
    pub fn check_link_target(
        &mut self,
        link: &crate::pipewire_registry::PwLink,
        pw_state: &PipeWireState,
    ) -> Result<()> {
        if self.manual_override {
            return Ok(());
        }
        // Which node owns the output port?
        let output_node_id = match pw_state.ports.get(&link.output_port) {
            Some(port) => port.node_id,
            None => return Ok(()),
        };

        let target_bus_id = match self.active_stream_routes.get(&output_node_id) {
            Some(bus_id) => bus_id.clone(),
            None => return Ok(()),
        };

        // Check if this link goes to our target sink — if so, it's fine
        let target_node_id = self.node_ids.get(&target_bus_id).copied();
        let input_node_id = pw_state.ports.get(&link.input_port).map(|p| p.node_id);
        if target_node_id.is_some() && target_node_id == input_node_id {
            return Ok(());
        }

        // WirePlumber re-linked to the wrong sink — remove and re-apply
        info!(
            "Detected wrong-target link for stream node {} (port {} -> port {}), correcting",
            output_node_id, link.output_port, link.input_port
        );
        self.destroy_link(link.output_port, link.input_port)?;
        self.link_stream_to_sink(output_node_id, &target_bus_id, pw_state)
    }

    /// Destroy a single virtual sink by bus_id.
    pub fn destroy_sink(&mut self, bus_id: &str) -> Result<()> {
        if let Some(module_id) = self.module_ids.remove(bus_id) {
            info!(
                "Destroying virtual sink '{}' (module_id={})",
                bus_id, module_id
            );
            let output = Command::new("pactl")
                .args(["unload-module", &module_id.to_string()])
                .output();

            match output {
                Ok(o) if !o.status.success() => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    warn!("Failed to unload module {}: {}", module_id, stderr.trim());
                }
                Err(e) => warn!("Failed to run pactl unload-module: {}", e),
                _ => {}
            }
        }
        self.node_ids.remove(bus_id);
        Ok(())
    }

    /// Destroy all virtual sinks created by this manager. Called on shutdown.
    pub fn destroy_all(&mut self) {
        info!(
            "Destroying all virtual sinks ({} modules)",
            self.module_ids.len()
        );
        let bus_ids: Vec<String> = self.module_ids.keys().cloned().collect();
        for bus_id in bus_ids {
            if let Err(e) = self.destroy_sink(&bus_id) {
                error!("Error destroying sink '{}': {:#}", bus_id, e);
            }
        }
    }

    /// Check if pactl is available.
    pub fn check_pactl_available() -> bool {
        Command::new("pactl")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if pw-link is available.
    pub fn check_pw_link_available() -> bool {
        Command::new("pw-link")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Apply effective volume (considering gain, mute, and solo state).
    pub fn apply_bus_state(
        &self,
        bus_id: &str,
        gain: f32,
        mute: bool,
        effective_muted: bool,
    ) -> Result<()> {
        // Set the actual volume
        self.set_volume(bus_id, gain)?;
        // Mute if either explicitly muted or effectively muted by solo
        self.set_mute(bus_id, mute || effective_muted)?;
        Ok(())
    }

    /// Create a PipeWire link between two ports by ID using pw-link.
    pub fn create_link(&self, output_port_id: u32, input_port_id: u32) -> Result<()> {
        debug!(
            "Creating pw-link: port {} -> port {}",
            output_port_id, input_port_id
        );

        let output = Command::new("pw-link")
            .args([&output_port_id.to_string(), &input_port_id.to_string()])
            .output()
            .context("Failed to execute pw-link. Is pipewire installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "File exists" means the link already exists — not an error
            if stderr.contains("File exists") {
                debug!(
                    "Link {} -> {} already exists, skipping",
                    output_port_id, input_port_id
                );
                return Ok(());
            }
            anyhow::bail!(
                "pw-link failed ({} -> {}): {}",
                output_port_id,
                input_port_id,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Remove a PipeWire link between two ports using pw-link -d.
    pub fn destroy_link(&self, output_port_id: u32, input_port_id: u32) -> Result<()> {
        debug!(
            "Destroying pw-link: port {} -> port {}",
            output_port_id, input_port_id
        );

        let output = Command::new("pw-link")
            .args([
                "-d",
                &output_port_id.to_string(),
                &input_port_id.to_string(),
            ])
            .output()
            .context("Failed to execute pw-link -d")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "pw-link -d failed ({} -> {}): {}",
                output_port_id,
                input_port_id,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Link virtual sink monitor ports to hardware device playback ports
    /// according to the route configuration.
    ///
    /// For each route with a channel_map, this finds the virtual sink's monitor
    /// output ports (direction "out") and the hardware device's playback input
    /// ports (direction "in"), then creates pw-link connections mapping
    /// FL->left_ch, FR->right_ch.
    pub fn link_routes_to_hardware(&self, config: &Config, pw_state: &PipeWireState) -> Result<()> {
        // Find all hardware ALSA sink nodes matching the device hint.
        // The RØDECaster in Pro Audio mode exposes multiple sink nodes:
        //   - "Main" (pro-*-1) with 10 playback channels (multitrack)
        //   - "Chat" (pro-*-0) with 2 playback channels (stereo chat)
        let hw_sinks: Vec<_> = pw_state
            .nodes_matching(&config.device.alsa_card_id_hint)
            .into_iter()
            .filter(|n| {
                n.media_class.contains("Audio/Sink") || n.media_class.contains("Audio/Duplex")
            })
            .collect();

        // Filter to only primary Pro Audio devices. The secondary (control)
        // device contains "R__DECaster" in its ALSA name, while the primary
        // Pro Audio device contains "RODECaster" or "RDCaster".
        let hw_sinks: Vec<_> = hw_sinks
            .into_iter()
            .filter(|n| !n.name.contains("R__DECaster"))
            .collect();

        // Identify Main and Chat by ALSA node name suffix:
        //   - Main ends with "pro-output-1" (10 playback channels, multitrack)
        //   - Chat ends with "pro-output-0" (2 playback channels, stereo chat)
        let hw_main = hw_sinks
            .iter()
            .find(|n| n.name.contains("pro-output-1") || n.name.contains("pro-input-1"));
        let hw_chat = hw_sinks
            .iter()
            .find(|n| n.name.contains("pro-output-0") || n.name.contains("pro-input-0"));

        let hw_main = match hw_main {
            Some(n) => {
                info!(
                    "Found Main hardware device '{}' (node {}) for multitrack routing",
                    n.name, n.id
                );
                n
            }
            None => {
                warn!(
                    "Hardware device matching '{}' not found in PipeWire graph; skipping route links",
                    config.device.alsa_card_id_hint
                );
                return Ok(());
            }
        };
        if let Some(n) = hw_chat {
            info!(
                "Found Chat hardware device '{}' (node {}) for chat routing",
                n.name, n.id
            );
        }

        // Collect the Main device's input (playback) ports, sorted by channel index
        let mut hw_input_ports: Vec<_> = pw_state
            .ports_for_node(hw_main.id)
            .into_iter()
            .filter(|p| p.direction == "in")
            .collect();
        hw_input_ports.sort_by_key(|p| port_channel_index(&p.name));

        info!(
            "Main hardware device has {} playback ports",
            hw_input_ports.len()
        );

        for route in &config.routes {
            let sink_node_name = self.node_name(&route.from_bus_id);
            let sink_node = match pw_state.find_node_by_name(&sink_node_name) {
                Some(n) => n,
                None => {
                    warn!(
                        "Virtual sink '{}' not found in PW graph, skipping route",
                        sink_node_name
                    );
                    continue;
                }
            };

            // Get the sink's monitor output ports (direction "out")
            let mut monitor_ports: Vec<_> = pw_state
                .ports_for_node(sink_node.id)
                .into_iter()
                .filter(|p| p.direction == "out")
                .collect();
            monitor_ports.sort_by_key(|p| port_channel_index(&p.name));

            if monitor_ports.len() < 2 {
                warn!(
                    "Virtual sink '{}' has {} monitor ports, need at least 2",
                    sink_node_name,
                    monitor_ports.len()
                );
                continue;
            }

            if let Some(channel_map) = &route.channel_map {
                // Multitrack route: link monitor to specific channels on the Main device
                let left_hw_port = hw_input_ports
                    .iter()
                    .find(|p| port_channel_index(&p.name) == channel_map.left);
                let right_hw_port = hw_input_ports
                    .iter()
                    .find(|p| port_channel_index(&p.name) == channel_map.right);

                match (left_hw_port, right_hw_port) {
                    (Some(lp), Some(rp)) => {
                        info!(
                            "Linking {} monitor -> hw channels [{}, {}] (ports {} -> {}, {} -> {})",
                            route.from_bus_id,
                            channel_map.left,
                            channel_map.right,
                            monitor_ports[0].id,
                            lp.id,
                            monitor_ports[1].id,
                            rp.id
                        );
                        if let Err(e) = self.create_link(monitor_ports[0].id, lp.id) {
                            warn!(
                                "Failed to link {} FL -> hw ch{}: {:#}",
                                route.from_bus_id, channel_map.left, e
                            );
                        }
                        if let Err(e) = self.create_link(monitor_ports[1].id, rp.id) {
                            warn!(
                                "Failed to link {} FR -> hw ch{}: {:#}",
                                route.from_bus_id, channel_map.right, e
                            );
                        }
                    }
                    _ => {
                        warn!(
                            "Hardware ports for channels [{}, {}] not found for route '{}'",
                            channel_map.left, channel_map.right, route.from_bus_id
                        );
                    }
                }
            } else if route.to_target.contains("chat_playback") {
                // Chat route: link monitor to the separate Chat hardware device
                match hw_chat {
                    Some(chat_node) => {
                        let mut chat_input_ports: Vec<_> = pw_state
                            .ports_for_node(chat_node.id)
                            .into_iter()
                            .filter(|p| p.direction == "in")
                            .collect();
                        chat_input_ports.sort_by_key(|p| port_channel_index(&p.name));

                        if chat_input_ports.len() < 2 {
                            warn!(
                                "Chat hardware device has {} playback ports, need at least 2",
                                chat_input_ports.len()
                            );
                            continue;
                        }

                        info!(
                            "Linking {} monitor -> Chat device '{}' (ports {} -> {}, {} -> {})",
                            route.from_bus_id,
                            chat_node.name,
                            monitor_ports[0].id,
                            chat_input_ports[0].id,
                            monitor_ports[1].id,
                            chat_input_ports[1].id
                        );
                        if let Err(e) =
                            self.create_link(monitor_ports[0].id, chat_input_ports[0].id)
                        {
                            warn!(
                                "Failed to link {} FL -> Chat FL: {:#}",
                                route.from_bus_id, e
                            );
                        }
                        if let Err(e) =
                            self.create_link(monitor_ports[1].id, chat_input_ports[1].id)
                        {
                            warn!(
                                "Failed to link {} FR -> Chat FR: {:#}",
                                route.from_bus_id, e
                            );
                        }
                    }
                    None => {
                        warn!(
                            "Chat hardware device not found for route '{}'; \
                             only one ALSA sink node for '{}'",
                            route.from_bus_id, config.device.alsa_card_id_hint
                        );
                    }
                }
            } else {
                debug!(
                    "Route '{}' has no channel_map and unknown target '{}', skipping",
                    route.from_bus_id, route.to_target
                );
            }
        }

        Ok(())
    }
}

/// Parse a numeric channel index from a PipeWire port name.
///
/// Port names follow patterns like:
/// - `playback_AUX0` → 0, `playback_AUX5` → 5
/// - `monitor_FL` → 0, `monitor_FR` → 1
/// - `playback_FL` → 0, `playback_FR` → 1
fn port_channel_index(name: &str) -> u32 {
    // Try to extract AUXn suffix first
    if let Some(rest) = name.rsplit('_').next() {
        if let Some(stripped) = rest.strip_prefix("AUX") {
            if let Ok(n) = stripped.parse::<u32>() {
                return n;
            }
        }
    }
    // Fall back to FL=0, FR=1
    if name.ends_with("_FL") {
        return 0;
    }
    if name.ends_with("_FR") {
        return 1;
    }
    u32::MAX
}

impl Drop for PwExecManager {
    fn drop(&mut self) {
        self.destroy_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_name() {
        let mgr = PwExecManager::new("lincaster");
        assert_eq!(mgr.node_name("system"), "lincaster.system");
        assert_eq!(mgr.node_name("chat"), "lincaster.chat");
    }

    #[test]
    fn test_node_name_custom_prefix() {
        let mgr = PwExecManager::new("myapp");
        assert_eq!(mgr.node_name("main"), "myapp.main");
    }

    #[test]
    fn test_register_node_id() {
        let mut mgr = PwExecManager::new("lincaster");
        mgr.register_node_id("system", 42);
        assert_eq!(mgr.get_node_id("system"), Some(42));
        assert_eq!(mgr.get_node_id("chat"), None);
    }

    #[test]
    fn test_register_node_id_update() {
        let mut mgr = PwExecManager::new("lincaster");
        mgr.register_node_id("system", 42);
        mgr.register_node_id("system", 100); // Update
        assert_eq!(mgr.get_node_id("system"), Some(100));
    }

    #[test]
    fn test_destroy_unknown_sink() {
        let mut mgr = PwExecManager::new("lincaster");
        // Should not panic when destroying a sink that doesn't exist
        assert!(mgr.destroy_sink("nonexistent").is_ok());
    }

    #[test]
    fn test_destroy_clears_node_id() {
        let mut mgr = PwExecManager::new("lincaster");
        mgr.register_node_id("system", 42);
        let _ = mgr.destroy_sink("system");
        assert_eq!(mgr.get_node_id("system"), None);
    }

    #[test]
    fn test_create_skips_duplicate() {
        let mut mgr = PwExecManager::new("lincaster");
        // Simulate a previously created sink by inserting into module_ids
        mgr.module_ids.insert("system".to_string(), 999);

        // create_virtual_sink should skip and return the existing module ID
        let result = mgr.create_virtual_sink("system", "System", 2);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 999);
    }

    #[test]
    fn test_destroy_all_clears_everything() {
        let mut mgr = PwExecManager::new("lincaster");
        mgr.module_ids.insert("system".to_string(), 100);
        mgr.module_ids.insert("chat".to_string(), 101);
        mgr.node_ids.insert("system".to_string(), 42);
        mgr.node_ids.insert("chat".to_string(), 43);

        mgr.destroy_all();

        assert!(mgr.module_ids.is_empty());
        assert!(mgr.node_ids.is_empty());
    }

    #[test]
    fn test_check_pactl_available_runs() {
        // This test just verifies the function doesn't panic.
        // On CI without pactl, it returns false; with pactl, true.
        let _result = PwExecManager::check_pactl_available();
    }

    #[test]
    fn test_port_channel_index() {
        assert_eq!(port_channel_index("playback_AUX0"), 0);
        assert_eq!(port_channel_index("playback_AUX4"), 4);
        assert_eq!(port_channel_index("playback_AUX5"), 5);
        assert_eq!(port_channel_index("playback_AUX9"), 9);
        assert_eq!(port_channel_index("monitor_FL"), 0);
        assert_eq!(port_channel_index("monitor_FR"), 1);
        assert_eq!(port_channel_index("playback_FL"), 0);
        assert_eq!(port_channel_index("playback_FR"), 1);
    }
}
