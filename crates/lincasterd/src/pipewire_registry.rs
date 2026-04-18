use std::collections::HashMap;
use std::sync::mpsc;

use pipewire as pw;
use pw::registry::GlobalObject;
use pw::types::ObjectType;
use tracing::{info, trace, warn};

/// Represents a PipeWire node discovered via registry enumeration.
#[derive(Debug, Clone)]
pub struct PwNode {
    pub id: u32,
    pub name: String,
    pub media_class: String,
    pub description: String,
    pub nick: String,
    pub props: HashMap<String, String>,
}

/// Represents a PipeWire port discovered via registry enumeration.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PwPort {
    pub id: u32,
    pub node_id: u32,
    pub name: String,
    pub direction: String,
    pub channel: String,
    pub props: HashMap<String, String>,
}

/// Represents a PipeWire link between two ports.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PwLink {
    pub id: u32,
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
}

/// Represents a PipeWire client (application).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PwClient {
    pub id: u32,
    pub name: String,
    pub pid: Option<u32>,
    pub props: HashMap<String, String>,
}

/// Aggregated state of the PipeWire graph.
#[derive(Debug, Default, Clone)]
pub struct PipeWireState {
    pub nodes: HashMap<u32, PwNode>,
    pub ports: HashMap<u32, PwPort>,
    pub links: HashMap<u32, PwLink>,
    pub clients: HashMap<u32, PwClient>,
    pub initial_sync_done: bool,
}

#[allow(dead_code)]
impl PipeWireState {
    /// Get all nodes that are audio sinks.
    pub fn audio_sinks(&self) -> Vec<&PwNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class.contains("Audio/Sink") || n.media_class.contains("Audio/Duplex")
            })
            .collect()
    }

    /// Get all nodes that are audio sources.
    pub fn audio_sources(&self) -> Vec<&PwNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class.contains("Audio/Source") || n.media_class.contains("Audio/Duplex")
            })
            .collect()
    }

    /// Get all nodes matching a name pattern (for finding ALSA device nodes).
    pub fn nodes_matching(&self, pattern: &str) -> Vec<&PwNode> {
        let pattern_lower = pattern.to_lowercase();
        self.nodes
            .values()
            .filter(|n| {
                n.name.to_lowercase().contains(&pattern_lower)
                    || n.description.to_lowercase().contains(&pattern_lower)
                    || n.nick.to_lowercase().contains(&pattern_lower)
            })
            .collect()
    }

    /// Get all ports belonging to a specific node.
    pub fn ports_for_node(&self, node_id: u32) -> Vec<&PwPort> {
        self.ports
            .values()
            .filter(|p| p.node_id == node_id)
            .collect()
    }

    /// Count playback (input direction) ports for a node.
    pub fn playback_channel_count(&self, node_id: u32) -> usize {
        self.ports
            .values()
            .filter(|p| p.node_id == node_id && p.direction == "in")
            .count()
    }

    /// Count capture (output direction) ports for a node.
    pub fn capture_channel_count(&self, node_id: u32) -> usize {
        self.ports
            .values()
            .filter(|p| p.node_id == node_id && p.direction == "out")
            .count()
    }

    /// Get all stream nodes (client audio streams, not device nodes).
    pub fn stream_nodes(&self) -> Vec<&PwNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class.contains("Stream")
                    || n.media_class.contains("Audio/Sink")
                        && n.props
                            .get("node.driver")
                            .map(|v| v == "false")
                            .unwrap_or(false)
            })
            .collect()
    }

    /// Find a node by its name property.
    pub fn find_node_by_name(&self, name: &str) -> Option<&PwNode> {
        self.nodes.values().find(|n| n.name == name)
    }

    /// Find node by ID.
    pub fn find_node(&self, id: u32) -> Option<&PwNode> {
        self.nodes.get(&id)
    }
}

/// Events emitted from the PipeWire thread to the main daemon.
#[derive(Debug)]
pub enum PwEvent {
    /// A new node was added to the graph.
    NodeAdded(PwNode),
    /// A node was removed from the graph.
    NodeRemoved(u32),
    /// A new port was added.
    PortAdded(PwPort),
    /// A port was removed.
    PortRemoved(u32),
    /// A new link was created.
    LinkAdded(PwLink),
    /// A link was removed.
    LinkRemoved(u32),
    /// A new client connected.
    ClientAdded(PwClient),
    /// A client disconnected.
    ClientRemoved(u32),
    /// Initial registry enumeration is complete (core sync done).
    InitialSyncDone,
    /// PipeWire connection error.
    #[allow(dead_code)]
    Error(String),
    /// PipeWire connection was lost (server crashed or restarted).
    Disconnected,
}

/// Parse properties from a PipeWire GlobalObject's DictRef into a HashMap.
fn props_to_map(global: &GlobalObject<&pw::spa::utils::dict::DictRef>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(props) = global.props {
        for (key, value) in props.iter() {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

/// Get a string property from a PipeWire GlobalObject, with a default.
fn get_prop(global: &GlobalObject<&pw::spa::utils::dict::DictRef>, key: &str) -> String {
    global
        .props
        .and_then(|p| p.get(key))
        .unwrap_or("")
        .to_string()
}

/// Start the PipeWire connection thread. Returns a receiver for events.
/// The thread will send a Disconnected event if PipeWire goes away,
/// allowing the main thread to restart monitoring.
pub fn start_pipewire_thread() -> mpsc::Receiver<PwEvent> {
    let (event_tx, event_rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("pipewire-main".into())
        .spawn(move || {
            if let Err(e) = run_pipewire_loop(event_tx.clone()) {
                warn!("PipeWire loop exited with error: {:#}", e);
                let _ = event_tx.send(PwEvent::Disconnected);
            } else {
                let _ = event_tx.send(PwEvent::Disconnected);
            }
        })
        .expect("Failed to spawn PipeWire thread");

    event_rx
}

/// Main PipeWire event loop. Runs on a dedicated thread.
fn run_pipewire_loop(event_tx: mpsc::Sender<PwEvent>) -> anyhow::Result<()> {
    pw::init();
    info!("PipeWire initialized, connecting to server...");

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|_| anyhow::anyhow!("Failed to create PipeWire main loop"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|_| anyhow::anyhow!("Failed to create PipeWire context"))?;
    let core = context
        .connect_rc(None)
        .map_err(|_| anyhow::anyhow!("Failed to connect to PipeWire server"))?;
    let registry = core
        .get_registry()
        .map_err(|_| anyhow::anyhow!("Failed to get PipeWire registry"))?;

    info!("Connected to PipeWire server");

    // Set up registry listener for global objects
    let tx = event_tx.clone();
    let tx_remove = event_tx.clone();

    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            handle_global_added(global, &tx);
        })
        .global_remove(move |id| {
            handle_global_removed(id, &tx_remove);
        })
        .register();

    // Request initial sync — when the "done" event fires with our seq,
    // we know all existing globals have been enumerated.
    let pending_seq = core.sync(0).expect("Failed to sync PipeWire core");
    let tx_done = event_tx.clone();
    let tx_err = event_tx.clone();
    let mainloop_quit = mainloop.clone();

    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending_seq {
                info!("PipeWire initial registry sync complete");
                let _ = tx_done.send(PwEvent::InitialSyncDone);
            }
        })
        .error(move |id, seq, res, msg| {
            warn!(
                "PipeWire core error: id={}, seq={}, res={}, msg={}",
                id, seq, res, msg
            );
            // If we get an error on the core object itself, PW may be going down
            if id == pw::core::PW_ID_CORE {
                let _ = tx_err.send(PwEvent::Disconnected);
                mainloop_quit.quit();
            }
        })
        .register();

    // Run the main loop (blocks until quit is called)
    mainloop.run();

    info!("PipeWire main loop exited");
    Ok(())
}

/// Handle a new global object appearing in the PipeWire registry.
fn handle_global_added(
    global: &GlobalObject<&pw::spa::utils::dict::DictRef>,
    tx: &mpsc::Sender<PwEvent>,
) {
    match global.type_ {
        ObjectType::Node => {
            let node = PwNode {
                id: global.id,
                name: get_prop(global, "node.name"),
                media_class: get_prop(global, "media.class"),
                description: get_prop(global, "node.description"),
                nick: get_prop(global, "node.nick"),
                props: props_to_map(global),
            };
            trace!(
                "Node added: id={} name='{}' class='{}'",
                node.id,
                node.name,
                node.media_class
            );
            let _ = tx.send(PwEvent::NodeAdded(node));
        }
        ObjectType::Port => {
            let port = PwPort {
                id: global.id,
                node_id: get_prop(global, "node.id").parse().unwrap_or(0),
                name: get_prop(global, "port.name"),
                direction: get_prop(global, "port.direction"),
                channel: get_prop(global, "audio.channel"),
                props: props_to_map(global),
            };
            trace!(
                "Port added: id={} node={} name='{}' dir='{}'",
                port.id,
                port.node_id,
                port.name,
                port.direction
            );
            let _ = tx.send(PwEvent::PortAdded(port));
        }
        ObjectType::Link => {
            let link = PwLink {
                id: global.id,
                output_node: get_prop(global, "link.output.node").parse().unwrap_or(0),
                output_port: get_prop(global, "link.output.port").parse().unwrap_or(0),
                input_node: get_prop(global, "link.input.node").parse().unwrap_or(0),
                input_port: get_prop(global, "link.input.port").parse().unwrap_or(0),
            };
            trace!(
                "Link added: id={} {}:{} -> {}:{}",
                link.id,
                link.output_node,
                link.output_port,
                link.input_node,
                link.input_port
            );
            let _ = tx.send(PwEvent::LinkAdded(link));
        }
        ObjectType::Client => {
            let client = PwClient {
                id: global.id,
                name: get_prop(global, "application.name"),
                pid: get_prop(global, "application.process.id").parse().ok(),
                props: props_to_map(global),
            };
            trace!("Client added: id={} name='{}'", client.id, client.name);
            let _ = tx.send(PwEvent::ClientAdded(client));
        }
        _ => {
            // Ignore other object types (Device, Module, Factory, etc.)
        }
    }
}

/// Handle a global object being removed from the PipeWire registry.
fn handle_global_removed(id: u32, tx: &mpsc::Sender<PwEvent>) {
    trace!("Global removed: id={}", id);
    // We don't know the type, so send all removal events and let the state handler figure it out.
    let _ = tx.send(PwEvent::NodeRemoved(id));
    let _ = tx.send(PwEvent::PortRemoved(id));
    let _ = tx.send(PwEvent::LinkRemoved(id));
    let _ = tx.send(PwEvent::ClientRemoved(id));
}

/// Apply a PipeWire event to the aggregated state.
pub fn apply_event(state: &mut PipeWireState, event: PwEvent) {
    match event {
        PwEvent::NodeAdded(node) => {
            state.nodes.insert(node.id, node);
        }
        PwEvent::NodeRemoved(id) => {
            state.nodes.remove(&id);
        }
        PwEvent::PortAdded(port) => {
            state.ports.insert(port.id, port);
        }
        PwEvent::PortRemoved(id) => {
            state.ports.remove(&id);
        }
        PwEvent::LinkAdded(link) => {
            state.links.insert(link.id, link);
        }
        PwEvent::LinkRemoved(id) => {
            state.links.remove(&id);
        }
        PwEvent::ClientAdded(client) => {
            state.clients.insert(client.id, client);
        }
        PwEvent::ClientRemoved(id) => {
            state.clients.remove(&id);
        }
        PwEvent::InitialSyncDone => {
            state.initial_sync_done = true;
        }
        PwEvent::Error(msg) => {
            warn!("PipeWire error event: {}", msg);
        }
        PwEvent::Disconnected => {
            warn!("PipeWire disconnected — clearing graph state");
            state.nodes.clear();
            state.ports.clear();
            state.links.clear();
            state.clients.clear();
            state.initial_sync_done = false;
        }
    }
}

/// Print a summary of the PipeWire state to the log and return a formatted string.
pub fn format_status(state: &PipeWireState, device_hint: &str) -> String {
    let mut out = String::new();

    // Find device nodes matching the hint
    let device_nodes = state.nodes_matching(device_hint);

    out.push_str("PipeWire Graph Summary\n");
    out.push_str("======================\n");
    out.push_str(&format!("Total nodes: {}\n", state.nodes.len()));
    out.push_str(&format!("Total ports: {}\n", state.ports.len()));
    out.push_str(&format!("Total links: {}\n", state.links.len()));
    out.push_str(&format!("Total clients: {}\n\n", state.clients.len()));

    if device_nodes.is_empty() {
        out.push_str(&format!("No nodes matching '{}' found.\n", device_hint));
    } else {
        out.push_str(&format!("Device Nodes matching '{}':\n", device_hint));
        for node in &device_nodes {
            let playback_ports = state.playback_channel_count(node.id);
            let capture_ports = state.capture_channel_count(node.id);
            out.push_str(&format!(
                "  [{}] '{}' ({})\n    class: {}\n    playback ports: {}, capture ports: {}\n",
                node.id,
                node.name,
                node.description,
                node.media_class,
                playback_ports,
                capture_ports
            ));
        }
    }

    out.push_str("\nAudio Sinks:\n");
    for sink in state.audio_sinks() {
        out.push_str(&format!(
            "  [{}] '{}' - {}\n",
            sink.id, sink.name, sink.description
        ));
    }

    out.push_str("\nAudio Sources:\n");
    for source in state.audio_sources() {
        out.push_str(&format!(
            "  [{}] '{}' - {}\n",
            source.id, source.name, source.description
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_node(id: u32, name: &str, media_class: &str) -> PwNode {
        PwNode {
            id,
            name: name.to_string(),
            media_class: media_class.to_string(),
            description: name.to_string(),
            nick: String::new(),
            props: HashMap::new(),
        }
    }

    fn make_test_port(id: u32, node_id: u32, dir: &str) -> PwPort {
        PwPort {
            id,
            node_id,
            name: format!("port_{}", id),
            direction: dir.to_string(),
            channel: "FL".to_string(),
            props: HashMap::new(),
        }
    }

    #[test]
    fn test_apply_node_add_remove() {
        let mut state = PipeWireState::default();
        let node = make_test_node(42, "test.sink", "Audio/Sink");

        apply_event(&mut state, PwEvent::NodeAdded(node));
        assert_eq!(state.nodes.len(), 1);
        assert!(state.find_node_by_name("test.sink").is_some());

        apply_event(&mut state, PwEvent::NodeRemoved(42));
        assert!(state.nodes.is_empty());
    }

    #[test]
    fn test_apply_port_add_remove() {
        let mut state = PipeWireState::default();
        let port = make_test_port(10, 42, "in");

        apply_event(&mut state, PwEvent::PortAdded(port));
        assert_eq!(state.ports.len(), 1);
        assert_eq!(state.playback_channel_count(42), 1);

        apply_event(&mut state, PwEvent::PortRemoved(10));
        assert!(state.ports.is_empty());
    }

    #[test]
    fn test_apply_link_add_remove() {
        let mut state = PipeWireState::default();
        let link = PwLink {
            id: 100,
            output_node: 1,
            output_port: 2,
            input_node: 3,
            input_port: 4,
        };

        apply_event(&mut state, PwEvent::LinkAdded(link));
        assert_eq!(state.links.len(), 1);

        apply_event(&mut state, PwEvent::LinkRemoved(100));
        assert!(state.links.is_empty());
    }

    #[test]
    fn test_apply_client_add_remove() {
        let mut state = PipeWireState::default();
        let client = PwClient {
            id: 5,
            name: "firefox".to_string(),
            pid: Some(1234),
            props: HashMap::new(),
        };

        apply_event(&mut state, PwEvent::ClientAdded(client));
        assert_eq!(state.clients.len(), 1);

        apply_event(&mut state, PwEvent::ClientRemoved(5));
        assert!(state.clients.is_empty());
    }

    #[test]
    fn test_initial_sync_done() {
        let mut state = PipeWireState::default();
        assert!(!state.initial_sync_done);

        apply_event(&mut state, PwEvent::InitialSyncDone);
        assert!(state.initial_sync_done);
    }

    #[test]
    fn test_disconnected_clears_all_state() {
        let mut state = PipeWireState::default();

        // Populate state
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(1, "sink", "Audio/Sink")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(2, "source", "Audio/Source")),
        );
        apply_event(&mut state, PwEvent::PortAdded(make_test_port(10, 1, "in")));
        apply_event(
            &mut state,
            PwEvent::LinkAdded(PwLink {
                id: 100,
                output_node: 1,
                output_port: 10,
                input_node: 2,
                input_port: 20,
            }),
        );
        apply_event(
            &mut state,
            PwEvent::ClientAdded(PwClient {
                id: 5,
                name: "test".into(),
                pid: None,
                props: HashMap::new(),
            }),
        );
        apply_event(&mut state, PwEvent::InitialSyncDone);

        assert!(!state.nodes.is_empty());
        assert!(!state.ports.is_empty());
        assert!(!state.links.is_empty());
        assert!(!state.clients.is_empty());
        assert!(state.initial_sync_done);

        // Disconnect clears everything
        apply_event(&mut state, PwEvent::Disconnected);

        assert!(state.nodes.is_empty());
        assert!(state.ports.is_empty());
        assert!(state.links.is_empty());
        assert!(state.clients.is_empty());
        assert!(!state.initial_sync_done);
    }

    #[test]
    fn test_disconnected_then_repopulate() {
        let mut state = PipeWireState::default();

        // First connection
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(1, "old_sink", "Audio/Sink")),
        );
        apply_event(&mut state, PwEvent::InitialSyncDone);
        assert_eq!(state.nodes.len(), 1);

        // Disconnect
        apply_event(&mut state, PwEvent::Disconnected);
        assert!(state.nodes.is_empty());

        // Reconnect with different graph
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(10, "new_sink", "Audio/Sink")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(11, "new_src", "Audio/Source")),
        );
        apply_event(&mut state, PwEvent::InitialSyncDone);

        assert_eq!(state.nodes.len(), 2);
        assert!(state.find_node_by_name("new_sink").is_some());
        assert!(state.find_node_by_name("old_sink").is_none());
    }

    #[test]
    fn test_find_node_by_name() {
        let mut state = PipeWireState::default();
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(1, "lincaster.system", "Audio/Sink")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(2, "lincaster.chat", "Audio/Sink")),
        );

        assert_eq!(state.find_node_by_name("lincaster.system").unwrap().id, 1);
        assert_eq!(state.find_node_by_name("lincaster.chat").unwrap().id, 2);
        assert!(state.find_node_by_name("lincaster.missing").is_none());
    }

    #[test]
    fn test_audio_sinks_and_sources() {
        let mut state = PipeWireState::default();
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(1, "sink1", "Audio/Sink")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(2, "source1", "Audio/Source")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(3, "duplex", "Audio/Duplex")),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(4, "stream", "Stream/Output/Audio")),
        );

        assert_eq!(state.audio_sinks().len(), 2); // sink1 + duplex
        assert_eq!(state.audio_sources().len(), 2); // source1 + duplex
    }

    #[test]
    fn test_channel_counts() {
        let mut state = PipeWireState::default();
        // Add 2 playback (in) ports and 1 capture (out) port for node 42
        apply_event(
            &mut state,
            PwEvent::PortAdded(PwPort {
                id: 1,
                node_id: 42,
                name: "p1".into(),
                direction: "in".into(),
                channel: "FL".into(),
                props: HashMap::new(),
            }),
        );
        apply_event(
            &mut state,
            PwEvent::PortAdded(PwPort {
                id: 2,
                node_id: 42,
                name: "p2".into(),
                direction: "in".into(),
                channel: "FR".into(),
                props: HashMap::new(),
            }),
        );
        apply_event(
            &mut state,
            PwEvent::PortAdded(PwPort {
                id: 3,
                node_id: 42,
                name: "p3".into(),
                direction: "out".into(),
                channel: "FL".into(),
                props: HashMap::new(),
            }),
        );

        assert_eq!(state.playback_channel_count(42), 2);
        assert_eq!(state.capture_channel_count(42), 1);
        assert_eq!(state.playback_channel_count(999), 0); // no such node
    }

    #[test]
    fn test_nodes_matching() {
        let mut state = PipeWireState::default();
        apply_event(
            &mut state,
            PwEvent::NodeAdded(PwNode {
                id: 1,
                name: "alsa_output.RODECaster_Duo".into(),
                media_class: "Audio/Sink".into(),
                description: "RODECaster Duo Multitrack".into(),
                nick: "RDC".into(),
                props: HashMap::new(),
            }),
        );
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(2, "other_device", "Audio/Sink")),
        );

        let matches = state.nodes_matching("RODECaster");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, 1);

        let matches = state.nodes_matching("rdc"); // case-insensitive via nick
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_format_status_handles_empty() {
        let state = PipeWireState::default();
        let status = format_status(&state, "RODECaster");
        assert!(status.contains("Total nodes: 0"));
        assert!(status.contains("No nodes matching"));
    }

    #[test]
    fn test_global_remove_sends_all_types() {
        // When a global is removed, we don't know its type, so we send
        // removal events for all types. The apply_event handler should
        // handle this gracefully (removing from whichever map has the ID).
        let mut state = PipeWireState::default();
        apply_event(
            &mut state,
            PwEvent::NodeAdded(make_test_node(42, "test", "Audio/Sink")),
        );

        // Simulate global_remove behavior: send all removal types
        apply_event(&mut state, PwEvent::NodeRemoved(42));
        apply_event(&mut state, PwEvent::PortRemoved(42));
        apply_event(&mut state, PwEvent::LinkRemoved(42));
        apply_event(&mut state, PwEvent::ClientRemoved(42));

        assert!(state.nodes.is_empty());
    }
}
