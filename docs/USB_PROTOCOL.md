# RodeCaster USB Communications Protocol Documentation

## Overview
This document records the reverse-engineered USB communication between the RodeCaster Windows application and the physical RodeCaster Duo / Pro II devices.


## USB Architecture Overview
- **Vendor ID (VID)**: 0x19F7 (RODE Microphones)
- **Product IDs (PID) Per Onboard Multitrack Configuration**:

   | Product | USB 1 Output | USB 1 Input | PID |
   |---------|--------------|-------------|-----|
   | RodeCaster Duo | Off | Expanded | 0x0079 |
   | RodeCaster Duo | Pre/Post-Fader | Expanded | 0x0095 |
   | RodeCaster Duo | Off | Standard | 0x0050 | 
   | RodeCaster Duo | Pre/Post-Fader | Standard | 0x0093 |
   | RodeCaster Pro II | Off | Expanded | 0x0078 |
   | RodeCaster Pro II | Pre/Post-Fader | Expanded | 0x0094 |
   | RodeCaster Pro II | Off | Standard | 0x0037 |
   | RodeCaster Pro II | Pre/Post-Fader | Standard | 0x0092 |

- **Interfaces**: The device enumerates multiple interfaces on a single USB composite device:

   | Interface | Class | Endpoints | Purpose |
   |-----------|-------|-----------|---------|
   | 0-2 | Audio (UAC) | EP1 OUT/IN (Isochronous) | Stereo chat audio |
   | 3-5 | Audio (UAC) | EP2 OUT/IN (Isochronous) | Multitrack audio streaming |
   | 6-7 | MIDI (Bulk) | EP3 OUT/IN (Bulk) | MIDI-class interface |
   | 8 | Mass Storage | **EP4 OUT/IN (Bulk)** | SCSI over USB — file/sound transfer |
   | 9 | HID | **EP5 OUT/IN (Interrupt)** | **Primary command/control channel** |

- **Windows Driver Complications**: The official Rode Central application does not communicate with the device using standard `WinUSB.sys` or generic HID drivers. Instead, it delegates commands through a proprietary kernel-level driver (`rodecaster.sys`). Because this driver executes its `URB_BULK_OUT` and `URB_INTERRUPT_OUT` requests synchronously below the Windows USBPcap filter altitude, packet sniffers on Windows cannot record the Host-to-Device data payloads.

### Recommended Sniffing Environment (Linux Host)
To bypass the Windows `rodecaster.sys` cloaking and successfully intercept the command protocol:
1. Boot into the target **Linux OS**.
2. Run a clean **Windows Virtual Machine** (e.g., QEMU/KVM, VirtualBox) with the official Rode App installed.
3. Pass the physical RodeCaster Duo USB device through to the Windows VM.
4. Use `wireshark` (via the `usbmon` kernel module) on the **Linux Host** to sniff the raw USB bus.
5. Because `usbmon` operates at the Linux hardware abstraction layer, it will passively capture all `URB_BULK_OUT` commands sent by the Windows VM without requiring any intrusive drivers.

### Capture Files
- `captures/session1_smart_pads_banks.pcapng` — Full capture of: app open → smart pads menu → bank cycling → voice disguise effect selection → close
- `captures/session2_soundpad_sound_and_FX.pcapng` — Full capture of: pad play mode cycling, loop/replay toggles, file export/assign, pad type changes (Sound→FX→Mixer→MIDI→Video→Clear), FX input routing, FX trigger modes, FX parameter sweeps (reverb/echo/megaphone/robot/disguise/pitch shift)
- `captures/session3_FX_input_choice.pcapng` — Capture of: toggling FX input device between Mic 1, Mic 2, Wireless 1, Wireless 2
- `captures/session4_mixer.pcapng` — Capture of: mixer pad (padType=3) options — censor beep tone trim, custom censor file assign/clear, mixer mode cycling (Censor→Trash Talk→Fade In/Out→Back Channel→Ducking), fade in/out seconds, exclude host toggle, back channel routing toggles, ducking depth adjustment
- `captures/session5_MIDI_and_Video.pcapng` — Capture of: MIDI pad (padType=4) — trigger mode toggle (latching/momentary), custom mode enable, Type (CC/Note), Control/Channel/On/Off value sweeps, Send mode cycling; Video pad (padType=6) — Input 1-4 + FTB, Scene A-E, Media A-E, Overlay A-E, Control auto/cut
- `captures/session6_colorchange_and_bankchange.pcapng` — Capture of: pad colour cycling (all 11 colours 0–10), bank switching (Bank 1→Bank 2→Bank 3), pad selection across banks, pad index→bank mapping discovery
- `captures/rodecaster_win_app_sound_assignment.pcapng` — **Pro II capture** of: Windows RØDE Central app assigning sound files to a pad (enter transfer mode, pad select/clear, MP3 and WAV file assign/replace, exit). Captured on usbmon1 with VM passthrough. 41,271 frames, ~67 seconds. **Critical finding:** no section redirect (04-prefix) used.
- `captures/state_dump.bin` — Reassembled binary state dump (186KB) from the initial device→host state transfer

---

## Endpoint Roles

### EP5 HID Interrupt (Primary Command Channel)
- **EP5 OUT (0x05)**: Host → Device commands (set properties, mode changes)
- **EP5 IN (0x85)**: Device → Host responses (state dumps, property change notifications)
- All configuration, state querying, and real-time control use this endpoint.
- Packets are 64 or 256 bytes (HID report size).

### EP4 Mass Storage / SCSI (File Transfer)
- **EP4 OUT (0x04)**: SCSI Command Block Wrappers (CBW) — host sends SCSI commands
- **EP4 IN (0x84)**: SCSI responses / data-in transfers
- Observed SCSI opcodes during capture:
  - `0x00` — TEST UNIT READY
  - `0x03` — REQUEST SENSE
  - `0x1a` — MODE SENSE
  - `0x1b` — START STOP UNIT
  - `0x25` — READ CAPACITY
  - `0x28` — READ(10) — bulk data reads (512B, 4KB, 8KB, 64KB blocks)
  - `0x2a` — WRITE(10) — writing data to device storage
  - `0x35` — SYNCHRONIZE CACHE
- The device exposes internal storage as a SCSI block device for sound file transfer.

---

## HID Message Protocol (EP5)

### Message Framing
All HID messages on EP5 share a common header:

```
Byte 0:     Message Type (u8)
Bytes 1-4:  Payload Length (u32 LE)
Bytes 5+:   Payload data
```

For multi-packet messages (payload > ~251 bytes), subsequent HID reports use:
```
Byte 0:     Message Type (same as first packet, acts as continuation marker)
Bytes 1+:   Continuation payload data
```

### Message Types

| Type | Direction | Name | Description |
|------|-----------|------|-------------|
| `0x01` | Host → Device | **Handshake** | Initial connection request. Payload is mostly zeros. |
| `0x02` | Device → Host | **Device Identification** | 64-byte response containing device name ("DECaster Duo"). |
| `0x03` | Host → Device | **Set Property** | Set a single property value on the device. |
| `0x04` | Device → Host | **State Dump / Notification** | Full device state tree dump (~186KB) on connect, or individual property change notifications. |

---

## Discovery & Initialization

### Sequence
1. **Host sends Handshake** (Type `0x01`):
   ```
   01 4e 00 00 00 00 00 00 00 00 00 00 ... (64 bytes, mostly zeros)
   ```
   - Payload length = 0x4e (78 bytes), content is all zeros.

2. **Device responds with Identification** (Type `0x02`):
   ```
   02 41 52 c3 98 44 45 43 61 73 74 65 72 20 44 75 6f 00 ... (64 bytes)
   ```
   - Contains UTF-8 device name: "RoDECaster Duo" (with some binary prefix bytes).

3. **Host requests State Dump** (Type `0x03`):
   ```
   03 04 00 00 00 ad 10 a7 b0 ... (256 bytes)
   ```
   - Short 4-byte payload with a fixed magic token `AD 10 A7 B0`.
   - **This step is required** — the device does NOT send the state dump
     automatically after the handshake. Without this request, the device
     will never send Type `0x04` state dump reports.

4. **Device sends full State Dump** (Type `0x04`):
   - Massive multi-packet response (~186KB across ~731 HID reports).
   - Payload length declared as 186,169 bytes (0x2D739).
   - Contains the entire device state tree (see State Tree section below).

---

## Set Property Command (Type `0x03`)

### Format
```
03                          -- Message type
LL LL LL LL                 -- Payload length (u32 LE)
[Address Bytes]             -- Tree navigation path (variable length)
[Property Name]\0           -- Null-terminated ASCII property name
[Value Encoding]            -- Type-prefixed value (see Value Types)
00 00 00 ...                -- Zero-padded to 256 bytes total
```

### Address Bytes
The address bytes navigate the hierarchical device state tree to locate the target section. The format consists of nested pairs and indices that trace a path from the root. The first byte indicates the address _type_ or _purpose_:

- `01` prefix: Standard property set/get (most common)
- `03` prefix: Section announcement/reset marker (used before bulk property resets)
- `04` prefix: Section redirect/prepare (observed on Duo only; **NOT used** on Pro II — see note below)

**Address structure:**
```
[prefix] [level1] [level2] [level3...] [sectionId] [childIndex...]
```

The address hierarchy generally follows: `01 01 XX YY [section bytes] [child index(es)]`

**Known address patterns:**

| Address Bytes | Target Section | Description |
|---------------|---------------|-------------|
| `01 01 01 01 07` | GUI/System | `selectedBank` |
| `01 01 01 01 06` | ENCODER | `encoderColour` |
| `01 01 01 01 0f` | SYSTEM | `transferModeType`, `remountPadStorage` |
| `01 01 02 02 9a 02 00` | PAD (current/default) | Pad properties without explicit index |
| `01 01 02 02 9a 02 01 14` | PAD at index 0x14 (20) | Pad properties for specific pad slot |
| `01 01 02 02 9a 02 01 13` | PAD at index 0x13 (19) | Pad properties for specific pad slot |
| `01 01 02 02 9d 02 01 2f` | EFFECTS_PARAMETERS at index 0x2F (47) | Effects parameters for effects slot (effectsIdx=0) |
| `01 01 02 02 9d 02 01 30` | EFFECTS_PARAMETERS at index 0x30 (48) | Effects parameters for effects slot (effectsIdx=1) |
| `01 01 02 02 9d 02 01 2e` | EFFECTS_PARAMETERS at index 0x2E (46) | Effects parameters for effects slot (effectsIdx=2) |
| `01 01 02 00 01 23` | PADBUTTON physical button | `padButtonPressed` (device→host) |
| `01 01 01 01 10` | NETWORK | `wifiScan*` properties |
| `01 01 02 01 10 01 XX` | WIFISCANRESULT child XX | `wifiScanResultSSID` |

**Section announcement addresses** (prefix `03`):
| Address Bytes | Property Name | Purpose |
|---------------|--------------|---------|
| `03 01 01 02 9a 02 01 14` | `PAD` | Announces pad section reset for pad at index 0x14 |
| `03 01 01 02 9d 02 01 2f` | `EFFECTS_PARAMETERS` | Announces effects parameter initialization for slot 0x2F |
| `03 01 01 02 9d 02 01 30` | `EFFECTS_PARAMETERS` | Announces effects parameter initialization for slot 0x30 |

**Pad index mapping**: The last byte(s) of the pad address identify the specific pad slot. Observed indices: 0x13 (19), 0x14 (20), 0x15 (21). The initial "current pad" address `9a 02 00` is used for simple property changes on the currently-selected pad, while the longer form `9a 02 01 XX` targets a specific pad index.

**Effects slot mapping**: Effects parameter addresses end with the effects slot index. Observed: 0x2E (46), 0x2F (47), 0x30 (48) corresponding to effectsIdx values 0, 1, 2 respectively. Each FX pad is assigned its own effects slot.

### Value Types

| Type Code | After `01` marker | Format | Size | Description |
|-----------|-------------------|--------|------|-------------|
| `01` | `01 01 VV` | u8/bool | 3 bytes | Single byte value. 0x02=true/active, 0x03=false/inactive |
| `02` | `01 02 VV` | enum/u8 | 3 bytes | Enumerated single byte value |
| `03` | `01 03 [STRING\0]` | string | variable | Null-terminated ASCII string |
| `04` | `01 04 LL [STRING\0]` | string (length-prefixed) | variable | Length byte + string + null |
| `05` | `01 05 01 VV VV VV VV` | u32 | 7 bytes | Little-endian 32-bit unsigned integer (sub-marker=0x01) |
| `09` | `01 09 04 VV×8` | f64 | 11 bytes | Little-endian 64-bit IEEE 754 double (sub-marker=0x04) |
| `0b` | `01 0b SS [DATA×SS]` | complex/array | 2+SS bytes | Size byte followed by raw data |
| `NN` | `01 NN 05 [STRING]\0` | length-prefixed string | NN+1 bytes | **String encoding**: sub-marker=0x05, NN = len(string_with_null) + 1. Used by `padName`, `wifiScanResultSSID`, and other variable-length string properties. See padName encoding below. |

**padName String Encoding (sub-marker 0x05):**

The `padName` property (and WiFi SSIDs) use a length-prefixed string encoding where the "type byte" is actually a total-length indicator:
```
01 NN 05 [ASCII string bytes] 00
```
- `01` = value marker (standard)
- `NN` = total bytes following (1 sub-marker + string length + 1 null terminator)
- `05` = string sub-marker (distinguishes this from u32 sub=0x01 and f64 sub=0x04)
- `[string]` = ASCII characters
- `00` = null terminator

**Examples:**
| String | Chars | NN | Full Encoding |
|--------|-------|----|---------------|
| "FTB" | 3 | 0x05 | `01 05 05 46 54 42 00` |
| "Scene A" | 7 | 0x09 | `01 09 05 53 63 65 6e 65 20 41 00` |
| "RCV Auto" | 8 | 0x0a | `01 0a 05 52 43 56 20 41 75 74 6f 00` |
| "RCV Input 1" | 11 | 0x0d | `01 0d 05 52 43 56 20 49 6e 70 75 74 20 31 00` |
| "Overlay A" | 9 | 0x0b | `01 0b 05 4f 76 65 72 6c 61 79 20 41 00` |

This explains previously-unknown type bytes: 0x05, 0x08, 0x09, 0x0a, 0x0b, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x15, 0x16, 0x17, 0x1f, 0x20, 0x29 — all are string length indicators when followed by sub-marker 0x05.

**padName special encodings:**
- `01 02 05` = enum(5) = cleared/empty name
- `01 09 04 00×8` = f64(0.0) = zero/reset name (used after type changes, sub=0x04 indicates true f64)

---

## Known Commands

### Bank Selection
Switch the active sound pad bank (1-indexed, banks 1–8):

```
03 19 00 00 00              -- Type=SetProperty, PayloadLen=25
01 01 01 01 07              -- Address: system property index 7
73 65 6c 65 63 74 65 64     -- "selected"
42 61 6e 6b 00              -- "Bank\0"
01 05 01                    -- Value type: u32
XX 00 00 00                 -- Bank number (01=Bank 1, 02=Bank 2, ... 08=Bank 8)
00 00 00 ...                -- Zero padding to 256 bytes
```

### Transfer Mode (Enter/Exit Smart Pad Editing)
Enter or exit the transfer/editing mode for sound pad configuration:

```
03 1d 00 00 00              -- Type=SetProperty, PayloadLen=29
01 01 01 01 0f              -- Address: system property index 15
74 72 61 6e 73 66 65 72     -- "transfer"
4d 6f 64 65 54 79 70 65 00  -- "ModeType\0"
01 05 01                    -- Value type: u32
XX 00 00 00                 -- 00=Normal mode, 02=Transfer/editing mode
```

### Remount Pad Storage
Triggers re-mount of the device's internal mass storage for file access (used before and after file operations):

```
03 1a 00 00 00              -- Type=SetProperty, PayloadLen=26
01 01 01 01 0f              -- Address: system property index 0x0f
72 65 6d 6f 75 6e 74 50     -- "remountP"
61 64 53 74 6f 72 61 67     -- "adStorag"
65 00                       -- "e\0"
01 01                       -- Value type: bool
02                          -- 0x02 = true (trigger remount)
```

The device responds with a notification confirming remount completion (`remountPadStorage` = bool 0x03/false).

---

## Sound Pad Protocol

### Pad Type System

Each sound pad has a `padType` property that determines its behavior:

| padType Value | Name | Description |
|---------------|------|-------------|
| `0` | **Sound** | Plays an audio file from device storage |
| `1` | **Sound (assigned)** | Sound pad with an audio file already assigned (has padEnvStop=1.0, padEnvFadeOut=1.0) |
| `2` | **FX** | Voice effect pad — triggers real-time audio effects on a selected input |
| `3` | **Mixer** | Mixer control pad — duck/mute/fade channels with routing |
| `4` | **MIDI** | Sends MIDI messages when triggered (Note or CC) |
| `6` | **Video** | Video/streaming control pad — sends RCV (RODE Central Video) commands for input switching, scenes, media, overlays, and transitions |

### Pad Play Mode

| padPlayMode Value | Name | Description |
|-------------------|------|-------------|
| `0` | **One-shot** | Plays once when pressed, cannot be interrupted |
| `1` | **Toggle** | Press to start, press again to stop |
| `2` | **Hold** | Plays while button is held, stops on release |

### Pad Colour Index

The pad colour is set via `padColourIndex` (u32, range 0–11). The colour wheel in the app cycles through 12 colours. There is no separate "default" or "off" index — when a pad is cleared, the LED state is managed independently of this property.

| padColourIndex Value | Colour |
|---------------------|--------|
| `0` | Red |
| `1` | Orange |
| `2` | Amber |
| `3` | Yellow |
| `4` | Lime |
| `5` | Green |
| `6` | Teal |
| `7` | Cyan |
| `8` | Blue |
| `9` | Purple |
| `10` | Magenta |
| `11` | Pink |

Colour cycling is continuous — the app sends rapid `SET_PROPERTY` commands (~30-70ms intervals) as the user scrolls the colour wheel.

### Complete Pad Properties

All properties are set via `SET_PROPERTY` (type `0x03`) targeting a pad address (e.g., `01 01 02 02 9a 02 01 14`).

#### Core Properties

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padType` | u32 | 0 | Pad type enum (see table above) |
| `padActive` | bool | 0x03 (inactive) | Whether pad is currently active/selected for editing. When a new pad is activated on the same bank, the device sends a notification deactivating the previously-active pad. |
| `padColourIndex` | u32 | 0 | Display colour index (0=Red through 11=Pink, see Pad Colour Index table above) |
| `padName` | varies | empty (encode_string("")) | Pad display name. Uses `01 NN 05 [string] 00` length-prefixed encoding (see Value Types). Clear with `encode_string("")` = `01 02 05 00`. Auto-generated as "Sound NN" (padIdx+1) during type assignment. |
| `padProgress` | f64 | 0.0 | Current playback progress (0.0 to 1.0) |
| `padFilePath` | varies | empty (encode_string("")) | Audio file path on device storage. Uses standard string encoding for paths, `encode_string("")` = `01 02 05 00` for clear |
| `padPlayMode` | u32 | 0 | Play mode: 0=One-shot, 1=Toggle, 2=Hold |
| `padLoop` | bool | 0x03 (off) | Whether audio loops on playback completion |
| `padReplay` | bool | 0x03 (off) | Whether pad replays from start when re-triggered during playback |
| `padIsInternal` | bool | 0x03 (off) | Whether pad uses an internal/built-in sound |
| `padGain` | f64 | -12.0 | Pad output gain in dB |
| `padIdx` | u32 | varies | Pad index within bank (observed: 0, 1, 2) |

#### Audio Envelope Properties

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padEnvStart` | f64 | 0.0 | Playback start point (normalized 0.0-1.0 of file duration) |
| `padEnvFadeIn` | f64 | 0.0 | Fade-in duration (normalized) |
| `padEnvFadeOut` | f64 | 0.0 | Fade-out duration (normalized). Default 1.0 for assigned Sound pads |
| `padEnvStop` | f64 | 0.0 | Playback stop/end point (normalized). Default 1.0 for assigned Sound pads |

#### Mixer Pad Properties (padType=3)

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `padMixerMode` | u32 | 0 | 0–4 | Mixer operating mode (see enum table below) |
| `padMixerTriggerMode` | u32 | 0 | 0–1 | Mixer trigger behavior. Device sends `u32=1` when switching to Fade In/Out mode |
| `padMixerCensorCustom` | bool | 0x03 (off) | 0x02/0x03 | Use custom censor sound file instead of built-in beep tone |
| `padMixerCensorFilePath` | enum=5 | empty | — | Path to custom censor sound file (uses type 0x29 for assign, enum=5 for clear) |
| `padMixerFadeInSeconds` | f64 | 3.0 | 1.3–3.0 observed | Fade-in duration in seconds (continuous, step ~0.1s) |
| `padMixerFadeOutSeconds` | f64 | 3.0 | 0.9–3.0 observed | Fade-out duration in seconds (continuous, step ~0.1s) |
| `padMixerFadeExcludeHost` | bool | 0x03 (off) | 0x02/0x03 | Exclude host output from fade effect |
| `padGain` | f64 | -12.0 | continuous | Beep tone trim level in dB when padMixerMode=0 (Censor). Continuous, step ~0.1 dB. Observed range -13.1 to -10.6 |

**`padMixerMode` Enum Values:**

| Value | Hex | Mode | Description |
|-------|-----|------|-------------|
| 0 | `0x00` | **Censor** | Default. Plays a beep tone (or custom sound) to censor audio. `padGain` controls beep tone trim level. |
| 1 | `0x01` | **Trash Talk** | Mutes other channels when pad is held/active |
| 2 | `0x02` | **Fade In/Out** | Fades audio channels in/out over configurable durations. Uses `padMixerFadeInSeconds`, `padMixerFadeOutSeconds`, `padMixerFadeExcludeHost`. Device sends default values on mode entry. |
| 3 | `0x03` | **Back Channel** | Routes audio to selected back-channel outputs. Uses `padMixerBackChannel*` boolean routing properties. |
| 4 | `0x04` | **Ducking** | Ducks (lowers volume of) other channels. Uses global `duckerDepth` property (see Ducking section below). |

**Custom Censor File Workflow:**

To assign a custom censor sound (replacing the built-in beep tone):
1. `padMixerCensorCustom = bool(0x02)` — enable custom censor mode
2. `remountPadStorage = bool(0x02)` — trigger storage remount for file access
3. `padFilePath = type_0x29(path_string)` — set the audio file path (long string encoding)
4. `padName = array[5](ASCII bytes)` — set the pad display name (e.g., "Music" = `4d75736963`)

To clear a custom censor sound (reverting to built-in beep):
1. `padFilePath = enum(5)` — clear the file path
2. `remountPadStorage = bool(0x02)` — trigger storage remount
3. `padName = type_0x08` — clear the pad name
4. `padMixerCensorCustom = bool(0x03)` — disable custom censor mode (revert to beep)

**Fade In/Out Mode Behavior:**

When switching `padMixerMode` to `u32=2` (Fade In/Out), the device sends three notifications back to the host:
- `padMixerTriggerMode = u32(1)` — trigger mode set to 1
- `padMixerFadeInSeconds = f64(3.0)` — default fade-in duration
- `padMixerFadeOutSeconds = f64(3.0)` — default fade-out duration

The fade seconds parameters are continuous and sent as rapid streams (~15ms intervals) while the user drags the slider control.

#### Mixer Back-Channel Routing (padType=3, padMixerMode=3)

These booleans control which audio channels receive back-channel audio when the mixer pad is triggered in Back Channel mode. Each can be independently toggled on (0x02) or off (0x03):

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padMixerBackChannelMic2` | bool | 0x03 (off) | Route to Microphone 2 |
| `padMixerBackChannelMic3` | bool | 0x03 (off) | Route to Microphone 3 |
| `padMixerBackChannelMic4` | bool | 0x03 (off) | Route to Microphone 4 |
| `padMixerBackChannelUsb1Comms` | bool | 0x03 (off) | Route to USB 1 Communications |
| `padMixerBackChannelUsb2Main` | bool | 0x03 (off) | Route to USB 2 Main |
| `padMixerBackChannelBluetooth` | bool | 0x03 (off) | Route to Bluetooth |
| `padMixerBackChannelCallMe1` | bool | 0x03 (off) | Route to RODE CallMe channel 1 |
| `padMixerBackChannelCallMe2` | bool | 0x03 (off) | Route to RODE CallMe channel 2 |
| `padMixerBackChannelCallMe3` | bool | 0x03 (off) | Route to RODE CallMe channel 3 |

All 9 back-channel routing properties are observed in the pad clear/reset sequence. Session 4 confirmed toggling of: `padMixerBackChannelMic2`, `padMixerBackChannelUsb1Comms`, `padMixerBackChannelUsb2Main`, `padMixerBackChannelBluetooth`, `padMixerBackChannelCallMe1`.

#### Ducking (Global Property — DUCKER Section)

When `padMixerMode = u32(4)` (Ducking), the mixer pad uses the **global** `duckerDepth` property. This is NOT a per-pad property — it lives in the DUCKER section at address `[01 01 01 01 01]` (state tree section index 1).

| Property | Value Type | Address | Default | Range | Description |
|----------|-----------|---------|---------|-------|-------------|
| `duckerDepth` | f64 | `01 01 01 01 01` | -9.0 | -12.0 – -6.0 observed | Ducking depth in dB. How much other channels are reduced when the mixer pad is active. More negative = deeper duck. Continuous, step ~0.1 dB. |

The duckerDepth control sends rapid `SET_PROPERTY` streams (~15ms intervals) as the user drags the slider. The default of -9.0 dB was confirmed as the center value of the observed sweep (went down to -12.0 then back up through -6.1, then returned to -9.0).

**Important**: Because duckerDepth is global, changing it affects ALL mixer pads configured in Ducking mode simultaneously.

#### FX Pad Properties (padType=2)

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padEffectInput` | u32 | 0 | Which input source the FX processes (see FX Input Source table below) |
| `padEffectTriggerMode` | u32 | 0 | 0=Latch (stays active), 1=Momentary (active while held) |

**FX Input Source (`padEffectInput`) values:**

| Value | Hex | Input Source |
|-------|-----|-------------|
| 0 | `0x00` | Mic 1 / Input 1 (default) |
| 1 | `0x01` | Mic 2 / Input 2 |
| 19 | `0x13` | Wireless 1 |
| 20 | `0x14` | Wireless 2 |

Note: The gap between 1 and 19 suggests additional inputs may exist at intermediate values (e.g., USB, Bluetooth, HDMI inputs). The Wireless inputs at 0x13/0x14 are likely indices matching their position in the device's overall input source list.

#### SIP/Phone Properties (padType for SIP)

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padSIPPhoneBookEntry` | u32 | 0 | Phone book entry index |
| `padSIPCallSlot` | u32 | 0 | Call slot assignment |
| `padSIPFlashState` | u32 | 0 | SIP flash state |
| `padSIPQdLock` | bool | 0x03 (off) | Quick-dial lock |

#### MIDI Trigger Properties (padType=4)

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `padTriggerMode` | u32 | 1 | 0–1 | 0=Latching (stays active after press), 1=Momentary (active only while held). Default in reset=1. |
| `padTriggerSend` | u32 | 2 | 0–2 | When to send MIDI message: 0=Press only, 1=Release only, 2=Press + Release |
| `padTriggerType` | u32 | 0 | 0–1 | MIDI message type: 0=CC (Control Change), 1=Note |
| `padTriggerCustom` | bool | 0x03 (off) | 0x02/0x03 | Enable custom MIDI settings (0x02=Custom, 0x03=Default preset). Must enable to edit Type/Control/Channel/On/Off. |
| `padTriggerControl` | u32 | padIdx | 0–127 | When padTriggerType=0 (CC): CC control number. When padTriggerType=1 (Note): MIDI note number. Defaults to the pad's `padIdx` value (e.g., 16 for padIdx=16). UI scrollwheel increments by 2. |
| `padTriggerChannel` | u32 | 1 | 1–16 | MIDI channel. UI scrollwheel increments by 2. |
| `padTriggerOn` | u32 | 127 (0x7F) | 0–127 | MIDI on value — note velocity (Note) or CC value on press (CC). UI scrollwheel decrements by 2. |
| `padTriggerOff` | u32 | 0 | 0–127 | MIDI off value — note-off velocity (Note) or CC value on release (CC). UI scrollwheel increments by 2. |

**MIDI Custom Settings Workflow:**

The pad starts in Default mode (`padTriggerCustom = bool(0x03)`). To customize MIDI settings:
1. `padTriggerCustom = bool(0x02)` — switch to Custom mode
2. Set `padTriggerType` (0=CC, 1=Note)
3. Adjust `padTriggerControl`, `padTriggerChannel`, `padTriggerOn`, `padTriggerOff` as desired
4. Set `padTriggerSend` (0=Press, 1=Release, 2=Press+Release)

The latching/momentary toggle (`padTriggerMode`) can be changed independently of custom mode.

#### Video Pad Properties (padType=6)

Video pads send RODE Central Video (RCV) commands for controlling streaming/production software. The pad uses `padRCVSyncPadType` as the primary control property, with each value mapping to a specific video action.

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `padRCVSyncPadType` | u32 | 0 | RCV video action index (see enum table below) |
| `padName` | string | varies | Auto-set display name based on the selected action (e.g., "RCV Input 1", "Scene A", "Overlay A") |

**`padRCVSyncPadType` Enum Values:**

| Value | Hex | Tab | Action | padName |
|-------|-----|-----|--------|---------|
| 0 | `0x00` | Input | Input 1 (default) | "RCV Input 1" |
| 1 | `0x01` | Input | Input 2 | "RCV Input 2" |
| 2 | `0x02` | Input | Input 3 | "RCV Input 3" |
| 3 | `0x03` | Input | Input 4 | "RCV Input 4" |
| 6 | `0x06` | Media | Media A | (name set) |
| 7 | `0x07` | Media | Media B | (name set) |
| 8 | `0x08` | Media | Media C | (name set) |
| 9 | `0x09` | Media | Media D | (name set) |
| 10 | `0x0A` | Media | Media E | (name set) |
| 13 | `0x0D` | Scene | Scene A | "Scene A" |
| 14 | `0x0E` | Scene | Scene B | "Scene B" |
| 15 | `0x0F` | Scene | Scene C | "Scene C" |
| 16 | `0x10` | Scene | Scene D | "Scene D" |
| 17 | `0x11` | Scene | Scene E | "Scene E" |
| 20 | `0x14` | Overlay | Overlay A | "Overlay A" |
| 21 | `0x15` | Overlay | Overlay B | "Overlay B" |
| 22 | `0x16` | Overlay | Overlay C | "Overlay C" |
| 23 | `0x17` | Overlay | Overlay D | "Overlay D" |
| 24 | `0x18` | Overlay | Overlay E | "Overlay E" |
| 27 | `0x1B` | Input | FTB (Fade To Black / rectangle icon) | "FTB" |
| 28 | `0x1C` | Control | Auto (transition) | "RCV Auto" |
| 29 | `0x1D` | Control | Cut (transition) | "RCV Cut" |

**Index grouping**: Input=0–3, Media=6–10, Scene=13–17, Overlay=20–24, FTB=27, Control=28–29. Gaps at 4–5, 11–12, 18–19, 25–26 may be reserved for future actions.

**Tab behavior**: Each selection sends a `padRCVSyncPadType` command paired with a `padName` update. The Auto (28) and Cut (29) controls are mutually exclusive — enabling one disables the other.

**Video Pad Creation Sequence:**
1. Full pad clear/reset (standard 47-property sequence)
2. `padType = u32(6)` — set to Video type
3. `padName` = string ("RCV Input 1" by default)
4. Selected action configured via `padRCVSyncPadType`

### Pad Addressing

Sound pads use two address formats:

**Short form (current/default pad):**
```
01 01 02 02 9a 02 00
```
Used for simple property changes on whatever pad is currently selected in the app. Seen for initial operations like `padPlayMode`, `padLoop`, `padReplay`, `padFilePath`, `padColourIndex` changes.

**Long form (specific pad index):**
```
01 01 02 02 9a 02 01 XX
```
Where `XX` is the pad index byte. Used for pad clear/reset sequences, type assignments, file assignments, and all bulk operations. **The Windows app uses long form for all pad setup commands.**

**Pad Index to Bank/Position Mapping:**

Each bank has pad indices grouped contiguously. The RodeCaster Duo has 6 pads per bank × 8 banks = 48 pad slots. The RodeCaster Pro II has 8 pads per bank × 8 banks = 64 pad slots.

**RodeCaster Duo (6 pads/bank):**

| Bank | selectedBank value | Pad Indices (hex) | Pad Indices (decimal) |
|------|-------------------|-------------------|----------------------|
| Bank 1 | 0 | 0x13 – 0x18 | 19 – 24 |
| Bank 2 | 1 | 0x05 – 0x0A | 5 – 10 |
| Bank 3 | 2 | 0x08 – 0x0D (estimated) | 8 – 13 |

**RodeCaster Pro II (8 pads/bank):**

| Bank | selectedBank value | Pad Indices (hex) | Pad Indices (decimal) |
|------|-------------------|-------------------|----------------------|
| Bank 3 | 2 | 0x18 (partial, hw_index=24) | Pad 1 at padIdx=16 |

> **Note**: Bank 1 indices on the Duo (0x13–0x18) are NOT contiguous with Bank 2 (0x05–0x07). The index scheme is non-linear. Full mapping of all pad slots requires testing all banks on each device model.

**Bank Selection:**

`selectedBank` is 0-indexed: value 0 = Bank 1, value 1 = Bank 2, ... value 7 = Bank 8.

**Pad Selection Behavior:**

When a pad is selected (tapped) on the device or in the app:
1. Host sends `padActive = bool(0x02)` to the newly-selected pad address
2. Device sends a notification `padActive = bool(0x03)` for the previously-active pad (if any) on the same bank
3. If applicable, host sends `padActive = bool(0x03)` to explicitly deactivate the old pad

The `padIdx` property within each pad identifies its position within the current bank (observed values: 0, 1, 2 for the 3 left-side pads).

### Pad Clear/Reset Protocol

To clear or reset a pad, the app sends a carefully ordered burst of ~44–48 commands in rapid succession (~113ms total on Pro II). This was observed every time the pad type changed.

> **Note on section redirect:** Early Duo captures showed a `04`-prefixed "section redirect" before the announce. The Pro II Windows app capture (`rodecaster_win_app_sound_assignment.pcapng`) does **NOT** send any section redirect — the sequence starts directly with the section announce. Code should omit the redirect for compatibility.

**Step 1: Section announcement** (prefix `03`)
```
03 0e 00 00 00              -- Type=SetProperty, PayloadLen=14
03 01 01 02 9a 02 01 18    -- Address with 03-prefix (section announce), LONG form
50 41 44 00                 -- "PAD\0"
00 00                       -- No value (announcement only)
```

**Step 2: All properties set to defaults** (in this exact order):
```
padColourIndex     = u32(0)
padActive          = bool(0x03)
padLoop            = bool(0x03)
padReplay          = bool(0x03)
padType            = u32(0)        ← type set to Sound/default
padName            = encode_string("")  ← cleared (01 02 05 00)
padProgress        = f64(0.0)
padFilePath        = encode_string("")  ← cleared (01 02 05 00)
padPlayMode        = u32(0)
padEnvStart        = f64(0.0)
padEnvFadeIn       = f64(0.0)
padEnvFadeOut      = f64(0.0)
padEnvStop         = f64(0.0)
padMixerMode       = u32(0)
padMixerTriggerMode = u32(0)
padMixerCensorCustom = bool(0x03)
padMixerCensorFilePath = encode_string("")
padMixerFadeInSeconds = f64(0.0)
padMixerFadeOutSeconds = f64(0.0)
padMixerFadeExcludeHost = bool(0x03)
padMixerBackChannelMic2      = bool(0x03)
padMixerBackChannelMic3      = bool(0x03)
padMixerBackChannelMic4      = bool(0x03)
padMixerBackChannelUsb1Comms = bool(0x03)
padMixerBackChannelUsb2Main  = bool(0x03)
padMixerBackChannelBluetooth = bool(0x03)
padMixerBackChannelCallMe1   = bool(0x03)
padMixerBackChannelCallMe2   = bool(0x03)
padMixerBackChannelCallMe3   = bool(0x03)
padRCVSyncPadType        = u32(0)
padEffectInput           = u32(0)
padEffectTriggerMode     = u32(0)
padSIPPhoneBookEntry     = u32(0)
padSIPCallSlot           = u32(0)
padSIPFlashState         = u32(0)
padSIPQdLock             = bool(0x03)
padTriggerMode           = u32(1)     ← note: default is 1, not 0
padTriggerSend           = u32(2)     ← note: default is 2, not 0
padTriggerType           = u32(0)
padTriggerCustom         = bool(0x03)
padTriggerControl        = u32(N)     ← defaults to padIdx value (e.g. 16 for padIdx=16)
padTriggerChannel        = u32(1)     ← note: default is 1
padTriggerOn             = u32(127)   ← full velocity
padTriggerOff            = u32(0)
padIsInternal            = bool(0x03)
padGain                  = f64(-12.0) ← -12 dB default
padIdx                   = u32(N)     ← varies per pad position
```

**Step 3: Apply new type** (if changing type, sent immediately after reset):
```
padType        = u32(NEW_TYPE)
padName        = (type-specific name encoding)
padColourIndex = u32(NEW_COLOUR)
```

**Observed type change sequences after reset:**

Setting to Sound with file (type 1):
```
padType         = u32(1)
padEnvStop      = f64(1.0)          ← full file duration
padEnvFadeOut   = f64(1.0)          ← full fade-out
padName         = string("Sound NN") ← auto-generated from padIdx+1
padColourIndex  = u32(4)            ← pad colour
```

Setting to FX (type 2):
```
padEffectTriggerMode = u32(1)       ← set to momentary
padActive            = bool(0x02)   ← activate
padType              = u32(2)       ← FX
padName              = f64(0.0)     ← name encoding
padColourIndex       = u32(1)       ← FX colour
```

---

## Effects Parameters Protocol

### Effects Slot Initialization

When an FX pad is created, the app first announces the effects parameter section and then sets all effect parameters to defaults:

**Section announcement:**
```
03 1d 00 00 00              -- Type=SetProperty, PayloadLen=29
03 01 01 02 9d 02 01 2f    -- Address: 03-prefix, effects slot 0x2F
45 46 46 45 43 54 53 5f     -- "EFFECTS_"
50 41 52 41 4d 45 54 45     -- "PARAMETE"
52 53 00                    -- "RS\0"
00 00                       -- Section announcement (no value)
```

**Then all effect properties are set.** The effects slot index (last address byte) maps to the `effectsIdx` value assigned to the FX pad:

| Address suffix | effectsIdx | Description |
|---------------|------------|-------------|
| `0x2E` (46) | 2 | Third effects slot |
| `0x2F` (47) | 0 | First effects slot |
| `0x30` (48) | 1 | Second effects slot |

### Complete Effects Properties

All effect properties target an effects slot address, e.g., `01 01 02 02 9d 02 01 30` for slot 0x30:

#### Reverb

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `reverbOn` | bool | 0x03 (off) | 0x02/0x03 | Reverb enable/disable |
| `reverbMix` | f64 | 0.5 | 0.0 – 1.0 | Reverb wet/dry mix (continuous) |
| `reverbLowCut` | f64 | 0.666146 | 0.0 – 1.0 | Low-cut filter frequency (continuous, higher = more cut) |
| `reverbHighCut` | f64 | 0.333325 | 0.0 – 1.0 | High-cut filter frequency (continuous, higher = more cut) |
| `reverbModel` | f64 | 0.6 | 0.0 – 0.8 | Room model/size. **Discrete steps**: 0.0, 0.2, 0.4, 0.6, 0.8 (5 room types) |

#### Echo

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `echoOn` | bool | 0x03 (off) | 0x02/0x03 | Echo enable/disable |
| `echoMix` | f64 | 0.5 | 0.0 – 1.0 | Echo wet/dry mix (continuous) |
| `echoLowCut` | f64 | 0.5 | 0.0 – 1.0 | Low-cut filter frequency (continuous) |
| `echoHighCut` | f64 | 0.5 | 0.0 – 1.0 | High-cut filter frequency (continuous) |
| `echoDelay` | f64 | 0.165 | 0.0 – 1.0 | Delay time (continuous) |
| `echoDecay` | f64 | 0.5 | 0.0 – 1.0 | Feedback/decay amount (continuous) |

#### Pitch Shift

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `pitchShiftOn` | bool | 0x03 (off) | 0x02/0x03 | Pitch shift enable/disable |
| `pitchShiftSemitones` | f64 | 7.0 | -12.0 – 12.0 | Pitch shift in semitones. Fine control sends fractional values (e.g., 7.1, -11.4), which snap to integers on release. Coarse control jumps by whole semitones |

#### Distortion / Megaphone

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `distortionOn` | bool | 0x03 (off) | 0x02/0x03 | Distortion/megaphone enable/disable |
| `distortionIntensity` | f64 | 0.7 | 0.0 – 1.0 | Intensity level. **Discrete steps**: 0.0, 0.111111, 0.222222, 0.333333, 0.444444, 0.555556, 0.666667, 0.777778, 0.888889, 1.0 (10 levels, step = 1/9) |

#### Robot Voice

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `robotOn` | bool | 0x03 (off) | 0x02/0x03 | Robot voice enable/disable |
| `robotMix` | f64 | 0.0 | 0.0 – 0.666667 | Robot voice mix. **Discrete steps**: 0.0, 0.333333, 0.666667 (3 levels, step = 1/3) |
| `robotLevel` | f64 | 0.0 | 0.0+ | Robot voice level (continuous, observed at 0.0 only) |

#### Voice Disguise

| Property | Value Type | Default | Range | Description |
|----------|-----------|---------|-------|-------------|
| `voiceDisguiseOn` | bool | 0x03 (off) | 0x02/0x03 | Voice disguise enable/disable (no additional parameters) |

#### Effects Slot Index

| Property | Value Type | Default | Description |
|----------|-----------|---------|-------------|
| `effectsIdx` | u32 | varies | Index identifying which effects slot this is (0, 1, 2 observed) |

### FX Pad Creation Sequence

When the user assigns a pad as FX type, the full sequence is:

1. **Pad reset** (full 47-property clear sequence, see Pad Clear/Reset Protocol)
2. **Effects section announcement**: `03`-prefixed address with prop="EFFECTS_PARAMETERS"
3. **All effects properties set to defaults** (reverb, echo, pitch shift, distortion, robot, voice disguise, effectsIdx)
4. **Pad type change**: `padType = u32(2)`
5. **Pad name set** (type-specific encoding)
6. **Pad colour set**: `padColourIndex = u32(1)` (FX colour)
7. **FX trigger mode set**: `padEffectTriggerMode = u32(1)` (momentary by default)
8. **Pad activated**: `padActive = bool(0x02)`

### Effect Enable/Disable Toggle Behavior

Each effect has an `*On` property. During the session, these were toggled on (0x02) and then off (0x03) for testing:

```
reverbOn=2 → reverbOn=3          (enable then disable reverb)
echoOn=2 → echoOn=3              (enable then disable echo)
distortionOn=2 → distortionOn=3  (enable then disable distortion)
robotOn=2 → robotOn=3            (enable then disable robot)
voiceDisguiseOn=2 → voiceDisguiseOn=3  (enable then disable disguise)
pitchShiftOn=2 → pitchShiftOn=3  (enable then disable pitch shift)
```

### Effect Parameter Sweep Behavior

The app sends rapid streams of `SET_PROPERTY` commands as the user drags sliders:
- **Continuous parameters** (reverbMix, echoDelay, etc.): Values change in small increments (0.004-0.1 steps), sent approximately every 15-30ms
- **Discrete parameters** (reverbModel, distortionIntensity, robotMix): Values jump between fixed steps
- **Pitch shift**: Coarse control sends integer changes; fine control sends fractional values that the device rounds/snaps

---

## File Operations

### Assigning a Sound File to a Pad

The complete protocol for assigning a sound file includes the pad setup (clear + type assignment) followed by file assignment. Observed in the Pro II capture (`rodecaster_win_app_sound_assignment.pcapng`).

**Phase 1: Pad Setup** (done once when selecting/creating the pad)

This is the standard Pad Clear/Reset Protocol (section announce + 42 property clears) followed by type assignment:
```
padType         = u32(1)                     -- Sound pad type
padEnvStop      = f64(1.0)                   -- Full playback range
padEnvFadeOut   = f64(1.0)                   -- Full fade range
padName         = string("Sound NN")         -- Auto-generated placeholder (padIdx+1)
padColourIndex  = u32(color)                 -- Pad colour
```

**Phase 2a: First File Assignment** (pad just cleared, padFilePath already empty)

```
1. [File copy via EP4 mass storage SCSI bulk transfers]
2. remountPadStorage = bool(0x02)            -- Trigger storage remount
3. [Wait for remountPadStorage=false ack]     -- ~186ms typical
4. padFilePath = string(path)                 -- e.g. "/Application/emmc-data/pads/17/sound.mp3"
5. padName = string(display_name)             -- Overwrites auto placeholder
```

**Phase 2b: Replacement File** (pad already has a file assigned)

```
1. padFilePath = encode_string("")            -- Clear existing path (01 02 05 00)
2. [File copy via EP4 mass storage SCSI bulk transfers]
3. remountPadStorage = bool(0x02)            -- Trigger storage remount
4. [Wait for remountPadStorage=false ack]     -- ~186ms typical
5. padFilePath = string(path)                 -- New file path
6. padName = string(display_name)             -- New display name
```

**File path convention:** `/Application/emmc-data/pads/{padIdx+1}/sound.{ext}` — directory number is `padIdx + 1`, filename is always `sound.mp3` or `sound.wav`.

**Important timing notes:**
- The `padFilePath` clear and `remountPadStorage` can overlap with the EP4 file copy timing-wise (the app sends them while bulk transfer may still be in progress).
- `padFilePath` and `padName` are set 2-3ms after the remount ack.
- No `padActive=false` is needed for file operations or on session exit.

### File Path Encoding

The `padFilePath` property uses `encode_string()` for both setting and clearing:
- **Set**: `01 NN 05 [path_bytes] 00` — standard length-prefixed string encoding
- **Clear**: `01 02 05 00` — `encode_string("")` (empty string)

**Note**: Previous documentation mentioned type `0x29` for long file paths. The Pro II capture shows standard string encoding is used. The type byte varies with string length (it's always `len+2`).

### Exporting / Overwriting Sound Files

When a user exports or overwrites a sound file (via "Choose File" or drag-and-drop):
1. `remountPadStorage = bool(0x02)` — triggers storage remount
2. File transfer occurs via EP4 Mass Storage (SCSI WRITE commands)
3. Device sends `remountPadStorage = bool(0x03)` notification when complete
4. `padActive = bool(0x03)` notification confirms pad deactivation after file ops

---

## Device Notifications (Type `0x04`)

After the initial state dump, the device sends Type `0x04` messages for individual property changes. These are pushed asynchronously whenever device state changes.

### Notification Format
```
04 LL LL LL LL              -- Type=Notification, PayloadLen
[Address Bytes]              -- Same format as Set Property addresses
[Property Name]\0            -- Null-terminated property name
[Value Encoding]             -- Same value encoding as Set Property
```

### Observed Notification Types

#### Physical Button Press/Release
```
04 1a 00 00 00              -- PayloadLen=26
01 01 02 00 01 23           -- Address: PADBUTTON at index 0x23
70 61 64 42 75 74 74 6f     -- "padButto"
6e 50 72 65 73 73 65 64 00  -- "nPressed\0"
01 01                       -- Value type: bool
02                          -- 0x02 = pressed
```
Followed by:
```
...same address...           -- padButtonPressed
01 01 03                    -- 0x03 = released
```

Press events are typically ~100-300ms apart (press → release). The address `01 01 02 00 01 23` identifies the specific physical button (index 0x23 = pad button 35, relative to PADBUTTON array).

#### Pad Active State Changes
```
04 15 00 00 00              -- PayloadLen=21
01 01 02 02 9a 02 01 14    -- Address: pad at index 0x14
70 61 64 41 63 74 69 76 65 00  -- "padActive\0"
01 01 03                    -- bool = 0x03 (deactivated)
```
Sent when a pad is deactivated (e.g., after file operations complete, or when another pad takes focus).

#### Storage Remount Confirmation
```
04 1a 00 00 00              -- PayloadLen=26
01 01 01 01 0f              -- Address: system property 0x0f
72 65 6d 6f 75 6e 74 50     -- "remountP"
61 64 53 74 6f 72 61 67     -- "adStorag"
65 00                       -- "e\0"
01 01 03                    -- bool = 0x03 (remount complete)
```

#### WiFi Scan Results (Periodic)
The device periodically broadcasts WiFi scan results:
```
04 XX 00 00 00              -- PayloadLen varies
01 01 01 01 10              -- Address: NETWORK
77 69 66 69 53 63 61 6e     -- "wifiScan"
NN 00                       -- "N\0" where N = scan index (3-10 observed)
01 TYPE [data]              -- SSID data (various string types: 0x0e, 0x12, 0x13, 0x15, 0x16, 0x17, 0x18, 0x1f, 0x20)
```

Individual scan results also sent per-child:
```
addr=[01 01 02 01 10 01 XX]   -- WIFISCANRESULT child at index XX (02-09 observed)
prop="wifiScanResultSSID"     -- SSID string for that specific scan entry
```

WiFi scan notifications appear every ~60-120 seconds in the background. The various string type codes (0x0e, 0x11, 0x12, 0x13, 0x15, 0x16, 0x1f, 0x20, etc.) likely encode SSID strings with different lengths or character encodings — the exact type byte appears to be the string length.

#### Mixer Mode Change Notifications
When the host sets `padMixerMode = u32(2)` (Fade In/Out), the device responds with three notifications confirming the mode and default parameter values:

```
padMixerTriggerMode   = u32(1)      -- addr=[010102029a020114], trigger mode set
padMixerFadeInSeconds  = f64(3.0)   -- addr=[010102029a020114], default fade-in
padMixerFadeOutSeconds = f64(3.0)   -- addr=[010102029a020114], default fade-out
```

These notifications are sent at the same pad address as the originating `padMixerMode` set command. They arrive within ~2ms of each other.

#### Effects Section Redirect
```
04 08 00 00 00              -- PayloadLen=8
04 01 01 02 9d 02 01 09    -- Address: effects redirect (04-prefix, index 0x09)
```
Observed once (device→host), purpose unclear — may be an effects section preparation notification.

---

## Device State Tree (Type `0x04`)

### Overview
On connection, the device sends a complete state dump as a single Type `0x04` message (~186KB). This contains a hierarchical tree of all device sections and properties.

### Tree Binary Format

**Root node:**
```
02                          -- Root type marker
[RootName]\0                -- "Rodecaster\0"
00                          -- 0 direct properties on root
02                          -- Child type marker (type-2 children follow)
9f                          -- Child count (159 top-level sections)
```

**Top-level sections** (prefixed with `02`):
```
02                          -- Section type prefix
[SECTIONNAME]\0             -- Uppercase null-terminated name
01                          -- Properties marker
NN                          -- Property count
[properties...]             -- NN property entries
[optional children block]   -- Recursive child sections
```

**Properties:**
```
[propName]\0                -- Lowercase camelCase null-terminated name
01                          -- Property value marker
TT                          -- Type byte (01=bool, 05=u32, 09=f64, etc.)
[type-specific bytes]       -- Sub-type indicator + value bytes
```

**Child sections** within a parent:
```
01                          -- Children marker
CC                          -- Child count
[child sections...]         -- CC sections, separated by 0x00 bytes
```

### Decoded Tree Structure (Partial)
```
Rodecaster (root, 159 top-level sections)
├── [0] PHYSICALINTERFACE (2 props, 89 children)
│   ├── props: onOffSw=u32(0), hwScreenBrightness=u32(250)
│   ├── [0-3]   POT × 4         (potMin, potMax, potLevel)
│   ├── [4-12]  FADER × 9       (faderMin, faderMax, faderLevel)
│   ├── [13-34] METER × 22      (meterStereo, meterPeakL, meterLevelL, meterPeakR, meterLevelR)
│   ├── [35-82] PADBUTTON × 48  (padButtonPressed, padButtonPreview)
│   └── [83-88] SOLOMUTEBUTTON × 6 (soloPressed, mutePressed)
├── [1] DUCKER (duckerDepth=f64, range -12.0 to -6.0, default -9.0 dB — global ducking depth for all Mixer pads in Ducking mode)
├── [2] RECBUTTON (recButtonPressed=u32)
├── [3] RECORDER (recordState, recordTimeMs, requestRecordState, requestDropMarker, ...)
├── [4] PLAYER (playerState, playerJumpToSample, playerFilePath, playerSpeed, ...)
├── [~5] EMERGENCYMUTE (emergencyMuteActive=bool)
├── [~6] ENCODER (encoderColour=u32, encoderSignal=array, encoderPressed=bool)
├── [~7] GUI / System Settings
│   ├── lang=string("en")
│   ├── broadcastMeters=bool
│   ├── selectedBank=u32 (0-indexed internally, 1-indexed in commands)
│   ├── inactiveButtonsBrightness=u32(64)
│   ├── padActiveEdit=u32
│   ├── screenTouched=bool
│   ├── metering=u32
│   ├── autoBrightness=bool
│   ├── screenDimAfterSeconds=u32
│   └── screenBrightness=u32(250)
├── [~8] APP (appOutputDevice, appMonitorMix, appRecording, appCompression)
├── [~9] HEADPHONE × 2 (headphoneType=u32, headphoneColour=hex_color)
├── [~10] OUTPUT (outputMonAutoMute, outputMonMute, outputMonFixed, ...)
│   ├── outputMultiBypass, outputPrefader
│   ├── recordingCompressionQuality, recordingMultitrackMode
│   └── recordingProcessing
├── [~11] SYSTEM
│   ├── systemMidiControl, systemIR (firmware version)
│   ├── systemName=string("RoDECaster Duo")
│   ├── appUpdateAvailable, osUpdateAvailable, updateDownloaded
│   ├── systemDateTimezone, systemDateTimeDaylightSavings
│   ├── systemBetaMode, systemHapticSetting
│   ├── systemRecButtonSetting
│   ├── transferModeType=u32 (0=normal, 2=transfer)
│   └── disableAllHeadphoneOutputs, disableAllPhysicalButtons
├── [~12] STORAGE × 2
│   ├── storageVolumeState=string("5277638656|5268275200|0|1|1")
│   ├── storageVolumeCapacity, storageVolumeFree
│   ├── storageVolumeInserted, storageVolumeMounted
│   ├── storageVolumeFormatted, storageVolumeTransfer
│   └── storageVolumeEject
├── [~13] NETWORK
│   ├── ipAddress, subnetMask, gateway
│   ├── wifiIpAddress, wifiSSID, wifiPSK, wifiScan
│   ├── wiredConnected, cellUSBFound, cellEnabled
│   ├── btDoScan, btDoPair, btPairedNumber1-5, btScan1-10
│   └── WIFISCANRESULT × multiple (wifiScanResultSSID)
├── [~14] STREAMERXMIXPRESET × multiple
│   └── streamerXPresetName=string("Presentation", "Video Call", "Custom 1-5")
├── [~15+] CHANNEL × multiple (one per audio channel)
│   ├── channelInputSource, channelListenSource
│   ├── channelTalkbackEnable, channelOutputMute
│   ├── channelWirelessMute, channelCueEnable
│   ├── channelBypassProcessing, channelAdvancedProcessing
│   ├── channelDepth=f64, channelSparkle=f64, channelPunch=f64
│   ├── channelPanR=f64, channelPanL=f64, channelPan=f64, channelPanOn=bool
│   ├── channelCurrentFxPreset=u32
│   ├── aphexBBTune, aphexBBDrive, aphexAEMix, aphexAETune, aphexOn
│   ├── eqHighQ, eqHighGain, eqHighBell, eqHighOn
│   ├── eqMidQ, eqMidGain, eqMidBell, eqMidOn
│   ├── eqLowQ, eqLowGain, eqLowShelf, eqLowOn, eqOn
│   ├── hpfSlope, hpfFrequency, hpfHigherOn, hpfLowerOn, hpfOn
│   ├── noiseGateHysteresis, noiseGateRange, noiseGateRelease
│   ├── noiseGateHold, noiseGateAttack, noiseGateThreshold, noiseGateOn
│   ├── deesserFrequency, deesserGain, deesserRelease, deesserAttack
│   ├── deesserRatio, deesserThreshold, deesserOn
│   ├── compressorGain, compressorRelease, compressorAttack
│   ├── compressorRatio, compressorThreshold, compressorOn
│   ├── Voice FX: robotMix, robotOn, distortionIntensity, distortionOn
│   └── pitchShiftSemitones, pitchShiftOn
├── PAD × multiple (one per pad slot)
│   └── All pad properties listed in Complete Pad Properties section above
└── EFFECTS_PARAMETERS × multiple (one per effects slot)
    └── All effects properties listed in Effects Parameters Protocol section above
```

### Key Observations
- **48 PADBUTTON instances** = 6 pads × 8 banks = all sound pad buttons across all banks.
- **22 METERs** covers all input/output channels (stereo pairs + mono).
- **9 FADERs** matches the physical fader count on the RodeCaster Duo.
- **4 POTs** matches physical rotary controls.
- **6 SOLOMUTEBUTTONs** for per-channel solo/mute.
- Bool values use `0x02` for true/active and `0x03` for false/inactive (not standard 0/1).
- **PAD sections** are stored separately from the PADBUTTON physical buttons — PADBUTTON tracks the physical press state, while PAD stores the configuration/content.
- **EFFECTS_PARAMETERS sections** are separate from PAD sections and linked via `effectsIdx`.

---

## Future Investigation Needed
- [ ] Map all pad addresses to physical pad positions across all 8 banks (need to test all 48 pads) — **PARTIAL** (session 6): Bank 1 = 0x13–0x15, Bank 2 = 0x05–0x07, Bank 3 = 0x08–0x0A. Non-linear mapping, remaining 5 banks untested.
- [x] ~~Decode `padEffectInput` values~~ — **DONE**: 0=Mic1, 1=Mic2, 19=Wireless1, 20=Wireless2. Gap 2-18 likely maps to other inputs (USB, Bluetooth, etc.)
- [x] ~~Decode `padMixerMode` enum~~ — **DONE** (session 4): 0=Censor, 1=Trash Talk, 2=Fade In/Out, 3=Back Channel, 4=Ducking
- [x] ~~Verify `padMixerFadeInSeconds` / `padMixerFadeOutSeconds` ranges~~ — **DONE** (session 4): both f64, default 3.0s, step ~0.1s
- [x] ~~Verify `padMixerFadeExcludeHost` toggle behavior~~ — **DONE** (session 4): standard bool toggle 0x02/0x03
- [x] ~~Verify back channel routing properties~~ — **DONE** (session 4): 5 of 9 channels confirmed toggled (Mic2, Usb1Comms, Usb2Main, Bluetooth, CallMe1)
- [x] ~~Decode `duckerDepth` property~~ — **DONE** (session 4): global at addr=[0101010101], f64, range -12.0 to -6.0 dB, step ~0.1 dB
- [x] ~~Document custom censor file assign/clear flow~~ — **DONE** (session 4): padMixerCensorCustom toggle + remountPadStorage + padFilePath/padName
- [ ] Decode remaining `padEffectInput` values in the 2-18 gap (USB, Bluetooth, HDMI inputs?)
- [x] ~~Decode extended value types: 0x08, 0x0a, 0x0d, 0x10 (various padName encodings)~~ — **DONE** (session 5): These are string length indicators (NN byte) in the `01 NN 05 [string] 00` encoding. 0x0d = 11-char string, 0x0a = 8-char string, etc.
- [ ] Decode value type 0x29 (long string for padFilePath / padMixerCensorFilePath)
- [x] ~~Map MIDI pad protocol in detail (padTriggerType values, padTriggerSend modes)~~ — **DONE** (session 5): padTriggerType 0=CC, 1=Note; padTriggerSend 0=Press, 1=Release, 2=Press+Release; padTriggerMode 0=Latching, 1=Momentary; padTriggerCustom enables editing; padTriggerControl/Channel/On/Off verified with ranges
- [x] ~~Map Video pad protocol (padType=6)~~ — **DONE** (session 5): padRCVSyncPadType enum fully decoded — Input 0-3, Media 6-10, Scene 13-17, Overlay 20-24, FTB 27, Control 28-29
- [ ] Investigate Trash Talk mode behavior (padMixerMode=1) — what properties does it use beyond the mode flag?
- [ ] Determine minimum/maximum bounds for padMixerFadeInSeconds and padMixerFadeOutSeconds (1.3 and 0.9 were observed lows but may not be absolute minimums)
- [ ] Test SIP/phone pad properties (padSIPPhoneBookEntry, padSIPCallSlot, etc.)
- [x] ~~Decode the string type bytes for wifi SSIDs (0x0e, 0x12, 0x13, 0x15, 0x16, 0x17, 0x18, 0x1f)~~ — **DONE** (session 5): Same `01 NN 05 [string] 00` encoding. NN = string length indicator.
- [x] ~~Decode `padName` encoding variants (type 0x10, 0x0a, 0x0d, f64=0.0, enum=5)~~ — **DONE** (session 5): String via `01 NN 05 [string] 00`, clear via `01 02 05` (enum=5), zero/reset via `01 09 04 00×8` (f64=0.0)
- [ ] Decode `padFilePath` full encoding (type 0x29 for long paths) — extract actual path string
- [ ] Fully decode `padType=5` if it exists (gap between MIDI=4 and Video=6)
- [x] ~~Capture and decode Mixer pad (type 3) in-use behavior~~ — **DONE** (session 4): All 5 mixer modes, back-channel routing, ducking, fade seconds, exclude host, custom censor file workflow
- [ ] Capture sound file upload/download via EP4 Mass Storage (SCSI WRITE/READ operations)
- [ ] Identify the EP3 MIDI endpoint's role (if any) in the control protocol
- [ ] Decode the `0x0b` (complex/array) value type fully (used by `encoderSignal`, `playerJumpToSample`)
- [ ] Capture and decode firmware update protocol
- [ ] Test sending Set Property commands directly from Linux (bypassing Windows app) to validate protocol understanding
- [ ] Determine the effects slot address assignment algorithm (how 0x2E, 0x2F, 0x30 map to effectsIdx 0, 1, 2)
- [x] ~~Investigate `padRCVSyncPadType` purpose (RCV = RODE Central Video?)~~ — **DONE** (session 5): Confirmed as RODE Central Video action index. Full enum decoded with 22 actions across Input/Scene/Media/Overlay/Control tabs.
- [x] ~~Decode padName string encoding~~ — **DONE** (session 5): `01 NN 05 [string] 00` where NN = len(string_with_null) + 1. Sub-marker 0x05 distinguishes strings from numeric types. Applies to padName, wifiScanResultSSID, and other variable-length strings.
