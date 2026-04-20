# LinCaster

A Linux-first, Rust-based "virtual driver" that recreates the core functionality of the Windows Virtual Devices driver workflow for the RØDECaster Duo and Pro II: multiple named software outputs ("System", "Chat", "Game", "Music", "A", "B"), per-application routing to those outputs, fader-like controls (gain/mute/solo) per virtual device, USB HID communication for sound pad configuration, and a GUI for visual control.

Built around **PipeWire** as the primary audio graph and policy integration layer.

## Features

- **6 virtual audio sinks**: System, Chat, Game, Music, Virtual A, Virtual B
- **Per-application routing**: Regex-based stream matching with priority rules; direct `pw-link` port-level linking with WirePlumber correction
- **Fader controls**: Gain, mute, and solo per bus (with solo-safe semantics) — *implemented without capture data and currently untested*
- **USB HID device control**: Sound pad configuration, bank switching, colour setting, transfer mode, sound file import — all via the reverse-engineered EP5 protocol
- **GUI application**: egui-based interface with a routing view (drag-and-drop stream assignment) and a sound pad editor (8-bank grid, import, clear, colour picker)
- **DBus API**: Session bus service for bus control, stream routing, sound pad management, and device status
- **CLI tool**: `lincasterctl` for command-line control (bus state, stream routing, pad import/clear, transfer mode)
- **State persistence**: Fader positions and mute states survive restarts
- **Two modes**: Hardware multitrack (preferred) or software-only fallback

## Prerequisites

```bash
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libasound2-dev libusb-1.0-0-dev libdbus-1-dev libclang-dev pkg-config

# Fedora
sudo dnf install pipewire-devel alsa-lib-devel libusb1-devel dbus-devel clang-devel

# Arch Linux
sudo pacman -S pipewire alsa-lib libusb dbus clang pkgconf
```

PipeWire + WirePlumber must be running as the user session's audio server.
You also need [Rust](https://www.rust-lang.org/tools/install) installed (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).

## Quick Start

### 1. Build & Install

```bash
cargo build --release
sudo install -m 755 target/release/lincasterd target/release/lincasterctl target/release/lincaster /usr/local/bin/
```

> **PATH tip:** If you prefer `cargo install --path` over `sudo install`, make sure `~/.cargo/bin` is in your `PATH`:
>
> ```bash
> export PATH="$HOME/.cargo/bin:$PATH"  # add to ~/.bashrc or ~/.zshrc to persist
> ```

### 2. Install udev Rules (required)

```bash
sudo cp contrib/99-rodecaster.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

These rules serve two purposes:

1. **USB device access** — allows non-root processes to open the RØDECaster for HID communication via libusb.
2. **Prevent device freeze** — the hidraw `MODE="0666"` rules are critical. When `lincasterd` exits, the device remains in host-session mode and generates EP5 IN notifications for every physical interaction (button press, dial turn). On Linux, the `usbhid` driver only polls EP5 IN when something has the `/dev/hidrawX` device open. If nothing drains these notifications, the firmware FIFO fills after ~2 button presses and **all physical controls freeze** until the device is power-cycled or USB connection to computer is unplugged.

By default, `lincasterd` spawns a lightweight background drain process (`cat /dev/hidrawN > /dev/null`) on disconnect that holds the hidraw device open and keeps the FIFO drained. The hidraw udev rules give that process (and other userspace HID consumers) permission to open the device.

> **Kernel parameter alternative:** If you prefer not to rely on the drain process (e.g. you want freeze protection even if `lincasterd` crashes unexpectedly), you can add a kernel boot parameter that tells `usbhid` to always poll the endpoint. Use the quirk for your device:
>
> | Model  | Kernel parameter |
> |--------|------------------|
> | Duo    | `usbhid.quirks=0x19F7:0x0079:0x0400` |
> | Pro II | `usbhid.quirks=0x19F7:0x0078:0x0400` |
>
> Add this to your bootloader's kernel command line (e.g. `/etc/kernel/cmdline`, GRUB config, or Limine config). `0x0400` is `HID_QUIRK_ALWAYS_POLL`. With this parameter, the drain process is unnecessary — disable it at runtime:
>
> ```bash
> lincasterd --no-drain
> ```

### 3. Configure (optional)

On first run, `lincasterd` automatically creates `~/.config/lincaster/config.json` with sensible defaults (6 busses, app routing rules for Discord/Zoom/Steam/Spotify, etc.).

It also installs a WirePlumber config to `~/.config/wireplumber/wireplumber.conf.d/51-rodecaster-rename.conf` that gives your RODECaster devices friendly names (e.g. \"RODECaster Duo Main\" instead of cryptic ALSA identifiers). Supports RODECaster Duo and Pro II.

To customise before first run:

```bash
mkdir -p ~/.config/lincaster
cp configs/config.json ~/.config/lincaster/config.json
```

Edit `~/.config/lincaster/config.json` to taste. See [Configuration](#configuration) below for details.

### 4. Run

**Option A — Manually:**

```bash
lincasterd
```

(uses `~/.config/lincaster/config.json` by default — override with `--config /path/to/config.json`)

**Option B — As a systemd user service (recommended):**

```bash
mkdir -p ~/.config/systemd/user
cp contrib/lincasterd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now lincasterd
```

Check it's running:

```bash
systemctl --user status lincasterd
lincasterctl status
```

## Usage

### Daemon

```bash
# Run with your config
lincasterd --config ~/.config/lincaster/config.json

# One-shot status check (probe device and print PipeWire graph info)
lincasterd --status-only

# Or run directly from source without installing (for development)
cargo run -p lincasterd -- --config ./configs/config.json
```

### CLI

```bash
lincasterctl status              # Show daemon status
lincasterctl list-busses         # List all virtual busses
lincasterctl get-bus system      # Get state of a specific bus
lincasterctl set-gain system 0.8 # Set System bus gain to 80%  (untested — no capture data)
lincasterctl mute chat on        # Mute the Chat bus           (untested — no capture data)
lincasterctl solo game on        # Solo the Game bus            (untested — no capture data)
lincasterctl reload-config ./configs/config.json

# Stream routing
lincasterctl list-streams              # List active audio streams and their routing
lincasterctl route-stream 42 game      # Route PipeWire node 42 to the Game bus
lincasterctl unroute-stream 42         # Unroute stream 42 (return to default device)
lincasterctl set-manual-override on    # Disable auto-routing rules (manual mode)
lincasterctl set-manual-override off   # Re-enable auto-routing rules

# Sound pad management
lincasterctl transfer-mode       # Enter transfer mode (mount device storage)
lincasterctl exit-transfer-mode  # Exit transfer mode
lincasterctl import-sound 1 ~/sounds/airhorn.mp3 --color 0  # Import sound to pad 1
lincasterctl clear-pad 1         # Clear pad 1
lincasterctl set-pad-color 1 8   # Set pad 1 colour to blue
lincasterctl set-pad-bank 0      # Switch to bank 1 on device (0-indexed)
lincasterctl refresh-state       # Re-read pad state from device
```

### GUI

```bash
# Run the GUI (requires the daemon to be running)
lincaster

# Or from source
cargo run -p lincaster
```

The GUI has two tabs:
- **Routing** — shows active audio streams and lets you drag them to virtual busses
- **Sound Pads** — 8-bank grid editor for configuring pads (import sounds, set colours, clear)

### DBus

The daemon exposes `com.lincaster.Daemon` on the session bus:

```bash
# List busses
dbus-send --session --print-reply --dest=com.lincaster.Daemon \
  /com/lincaster/Daemon com.lincaster.Daemon.ListBusses

# Set gain
dbus-send --session --print-reply --dest=com.lincaster.Daemon \
  /com/lincaster/Daemon com.lincaster.Daemon.SetBusGain \
  string:"system" double:0.75

# Route a stream to a bus
dbus-send --session --print-reply --dest=com.lincaster.Daemon \
  /com/lincaster/Daemon com.lincaster.Daemon.RouteStream \
  uint32:42 string:"game"

# Get pad configurations
dbus-send --session --print-reply --dest=com.lincaster.Daemon \
  /com/lincaster/Daemon com.lincaster.Daemon.GetPadConfigs
```

## Configuration

See [configs/config.json](configs/config.json) for a complete example. Key sections:

- **device**: USB vendor/product IDs and ALSA card hints for device detection
- **busses**: Virtual bus definitions (name, channels, default gain)
- **routes**: Mapping from busses to hardware channel pairs
- **app_rules**: Per-application routing rules with regex matching
- **latency_mode**: `ultra_low` (64 frames) or `low` (256 frames) — *stored in config and reported in status, but not yet applied to PipeWire*

## Project Structure

```
crates/
├── lincaster/         GUI application (egui/eframe)
├── lincaster-proto/   Shared types, config, HID protocol encoding, state dump parser
├── lincasterd/        Daemon (device discovery, PipeWire graph, routing, USB HID, DBus)
└── lincasterctl/      CLI tool
configs/                Example configuration files
contrib/                udev rules, WirePlumber config, systemd service
docs/                   Architecture and USB protocol documentation
captures/               USB pcap captures used for protocol reverse-engineering
```

## Testing

```bash
cargo test --workspace                        # Run all tests
cargo fmt --all -- --check                    # Format check
cargo clippy --all-targets -- -D warnings     # Lint check
```
## Disclaimer

This project was built using LLM technology. The PRD was generated with ChatGPT's deep research offering, and the source code was built using Claude Opus 4.6. There may be redundant bits in the code base, missing unit tests, security flaws, inconsistencies, or other general 'oopsies'. For this reason, the software should be ran in the user space only (best practice for a tool of this type regardless of AI involvement), and any edge cases you can think of should be tested. If bugs or broken functionality occur, an issue should be opened or a PR submitted. 

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
