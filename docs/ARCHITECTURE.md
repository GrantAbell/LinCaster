# LinCaster Architecture

## Overview

LinCaster is a Linux virtual driver for the RØDECaster Duo / Pro II, implemented in Rust using PipeWire as the audio routing engine. It provides per-application audio routing via virtual sinks, USB HID communication with the device for sound pad configuration, and a GUI for visual control.

## Crate Structure

```
crates/
├── lincaster/         GUI application (egui/eframe)
│   ├── main.rs             Entry point, egui app skeleton
│   ├── dbus_client.rs      DBus polling thread for daemon state
│   ├── routing_view.rs     Audio routing / stream assignment UI
│   └── sound_pad_view.rs   Sound pad bank/grid editor UI
├── lincaster-proto/   Shared types, config models, error types, HID protocol encoding, state dump parser
├── lincasterd/        Daemon binary
│   ├── main.rs             Entry point, daemon orchestration, event loop
│   ├── device_discovery.rs USB/ALSA device detection (rusb + /proc/asound)
│   ├── pipewire_registry.rs PipeWire graph state tracking (native libpipewire)
│   ├── graph_manager.rs    Static multitrack channel map (bus → hw channel pair)
│   ├── pw_exec.rs          PipeWire mutations via CLI tools (pactl, pw-link)
│   ├── app_mapper.rs       Per-app stream routing rules engine
│   ├── fader_control.rs    Gain/mute/solo controller
│   ├── usb_hid.rs          USB HID device communication (EP5, rusb)
│   ├── dbus_service.rs     DBus session bus API
│   └── state.rs            Persistent state (save/load)
└── lincasterctl/      CLI tool (communicates with daemon via DBus)
```

## Threading Model

```
┌────────────────────────────────┐  ┌──────────────────────────┐
│  Main thread (std, blocking)   │  │  PipeWire thread          │
│  ─────────────────             │  │  ────────────────          │
│  • Config loading              │  │  • pw::MainLoop event loop│
│  • Device discovery            │  │  • Registry listener      │
│  • Event processing loop       │  │  • Node/Port/Link/Client  │
│  • DBus command dispatch       │  │    change notifications   │
│  • PW graph mutations via      │  │                            │
│    pactl / pw-link subprocesses│  │  Communicates via:         │
│                                │  │  std::sync::mpsc::channel  │
│  ←── PwEvent ──────────────────│──│  (PwEvent sent to main)    │
│                                │  │                            │
├────────────────────────────────┤  └──────────────────────────┘
│  DBus thread                   │
│  ──────────────                │  ┌──────────────────────────┐
│  • Session bus listener        │  │  HID reader thread        │
│  • Shared state reads          │  │  ────────────────          │
│  • DaemonCommand → main via    │  │  • EP5 IN polling         │
│    mpsc::channel               │  │  • Notification parsing   │
│                                │  │  • HidEvent → main via    │
│                                │  │    mpsc::channel           │
└────────────────────────────────┘  └──────────────────────────┘
```

## Audio Routing

### Hardware Multitrack Mode (preferred)

When the RØDECaster's ALSA Pro Audio profile exposes multichannel endpoints:

```
Application streams
    ↓ (pw-link: direct port-level links)
Virtual Sinks (System, Chat, Game, Music, Virtual A, Virtual B)
    ↓ (pw-link: monitor out → hw playback in, channel-mapped)
Hardware Multitrack Playback (10 channels, "pro-output-1")
    System → ch 0,1
    Game   → ch 2,3
    Music  → ch 4,5
    Virt A → ch 6,7
    Virt B → ch 8,9
    Chat   → separate playback device ("pro-output-0")
```

Virtual sinks are created via `pactl load-module module-null-sink`. Hardware route links are created via `pw-link` (port ID → port ID). When WirePlumber tries to re-route a stream to the default sink, the daemon detects the incorrect link and corrects it.

### Software-Only Mode (fallback)

When the hardware device is not detected, virtual sinks are still created as PipeWire null-audio-sinks but no hardware route links are established.

## Per-App Routing

Streams are matched using PipeWire node properties:
- `application.process.binary`
- `application.name`
- `client.name`
- `application.id` (Flatpak)

Matching uses regex patterns with priority ordering. Routing is actuated via `pw-link`: the daemon disconnects any existing output links from the stream and creates new direct port-level links to the target virtual sink. WirePlumber link-correction detects and reverses any re-routing WirePlumber attempts after the fact.

## USB HID Communication

The daemon communicates with the RØDECaster via USB HID Interrupt endpoint (EP5) using `rusb`:

1. **Handshake** (Host → Device, Type `0x01`) — opens host session
2. **State dump request** (Host → Device, Type `0x03`, magic `AD 10 A7 B0`) — asks the device to send its full state
3. **State dump response** (Device → Host, Type `0x04`) — ~186KB multi-packet response, parsed into pad configurations
4. **Set Property** (Host → Device, Type `0x03`) — modify device state (pad type, colour, bank, transfer mode, etc.)

A background reader thread continuously drains EP5 IN to prevent device firmware FIFO overflow. On disconnect, the kernel `usbhid` driver is reattached and a lightweight `cat /dev/hidrawN > /dev/null` drain process is spawned (unless `--no-drain` is set).

See [USB_PROTOCOL.md](USB_PROTOCOL.md) for the full reverse-engineered protocol reference.

## Control Plane

- **DBus** (`com.lincaster.Daemon`): Session bus service with methods for:
  - Bus control: ListBusses, GetBusState, SetBusGain, SetBusMute, SetBusSolo, SetBusSoloSafe
  - Stream routing: ListStreams, RouteStream, UnrouteStream, RouteToDefault, SetManualOverride
  - Sound pads: GetPadConfigs, HidConnect, SetPadBank, ApplyPadConfig, ClearPad, SetPadColor, SetPadProperty, SetTransferMode, AssignPadFile
  - General: GetStatus, ReloadConfig
- **CLI** (`lincasterctl`): Thin DBus client with subcommands for all daemon methods plus `transfer-mode`, `import-sound`, and `clear-pad`.
- **GUI** (`lincaster`): egui/eframe application with a routing view (drag-and-drop stream assignment) and sound pad editor (bank/grid view, import, clear, colour picker).
- **MIDI/OSC** (future): CC mapping for fader control.
