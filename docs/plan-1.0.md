# kiss-tnc-bridge 1.0 plan

Status: draft, October 2026

## Goal

Turn `kiss-tnc-bridge` from a one-way "TCP TNC → BLE" bridge into a small
**KISS switchboard**, modeled on the virtual networks in VMware Workstation or
Linux bridges. You define **bridges** (`ktbr0`, `ktbr1`, …). Each bridge
connects one KISS modem (a radio's built-in TNC over Bluetooth Classic SPP, or
a TCP KISS server such as Dire Wolf) to any number of apps, over BLE and over
TCP. Bridges are managed from a CLI and a full-screen TUI, and the management
protocol is designed so a web UI and phone apps can be added later.

The motivating setup:

```
                                 ┌──── ktbr0 "TH-D75 portable" ────┐
  Kenwood TH-D75 ──SPP/RFCOMM──> │ radio                    phones │ ──BLE──> iPhone (aprs.fi, RadioMail)
                                 │                             lan │ ──TCP──> laptop (Pat/Winlink, Xastir, YAAC)
                                 └─────────────────────────────────┘
                                 ┌──── ktbr1 "Graywolf at home" ───┐
  Graywolf ───────────TCP──────> │ tnc                      phones │ ──BLE──> iPad (on a USB dongle)
                                 └─────────────────────────────────┘
```

This replaces the discontinued [B.B. Link](https://github.com/islandmagic/bb-link/)
ESP32 adapter. It also does things B.B. Link can't: several clients at once,
TCP clients, several setups that can be switched between, and running on
hardware you already carry.

### Targets

| Platform | Arch | Bluetooth | Notes |
|---|---|---|---|
| Raspberry Pi 3/4/5, Zero W, Zero 2 W | arm64 / armhf | Broadcom/Cypress, UART-attached, dual-mode | The primary platform until Flipper One ships |
| Pi 1 / Zero + USB dongle | armhf | RTL8761B and similar, dual-mode | Advertising limits already documented in README |
| Generic x86_64 Linux | amd64 | anything BlueZ supports | Dev/test |
| **Flipper One** | arm64 (Rockchip RK3576) | Bluetooth 5.2, reportedly a MediaTek MT7921AUN combo module (dual-mode, USB-attached); USB ports available for a dongle | Debian-based Flipper OS. Specs are still preliminary ([CNX](https://www.cnx-software.com/2026/05/21/flipper-one-a-rockchip-rk3576-powered-portable-arm-linux-computer-and-networking-multi-tool/), [The Register](https://www.theregister.com/personal-tech/2026/05/21/flipper-one-wants-to-be-the-linux-multi-tool-in-your-pocket/5244165)) |

### Decisions so far

- **One modem per bridge** in 1.0. Bridges with several modems, presented as a
  multi-port KISS TNC, come after 1.0.
- **No app-to-app echo** in 1.0. Apps only hear what the modem sends.
- **New TOML config in a new directory, `/etc/kiss-tnc-bridge/`.** 0.5 INI
  configs aren't read or migrated. There are very few users, and the old file
  at `/etc/kiss-tnc-bridge.conf` is left untouched.
- **Static bridges** live in `/etc` and can't be changed through management
  tools. **Dynamic bridges** live in `/var/lib/kiss-tnc-bridge/bridges.toml`
  and are owned by the daemon.
- **Bridges have an id (`ktbr0`) and a human-friendly name, and can be
  disabled.** You can keep a bridge for each physical setup and enable the one
  that matches what you have with you.
- **CLI and TUI in 1.0.** The web UI, LAN management and BLE management run in
  a separate unprivileged gateway process, after 1.0. The protocol is
  designed for all of them now.
- **Names:** the daemon stays `kiss-tnc-bridge`. The CLI/TUI is `ktbctl`, the
  gateway `ktb-gateway`, and bridge ids default to `ktbrN`.

### Non-goals for 1.0

- More than one modem per bridge, and any modem-to-modem forwarding
  (digipeating or cross-band gating).
- Acting as a BLE *central* to BLE TNCs (Mobilinkd TNC4, etc.). The port model
  leaves room for it; see [After 1.0](#after-10).
- Any non-Linux daemon. `ktbctl` and TCP-only bridges should still build and
  run on macOS for development.
- AX.25 or APRS awareness in the data path. Bridges move KISS frames. Decoding
  is only for display, in `monitor` and the TUI.
- Migrating 0.5 configs.

---

## Bridge model

### Concepts

- **Bridge**: an independent switchboard. It has an **id** (`ktbr0`) that is
  unique, short, stable, and used in commands and logs, and a **name**
  ("TH-D75 portable") that is human-friendly and shown in UIs. Names must also
  be unique so `ktbctl` can accept either. A bridge is `enabled` or disabled,
  and a disabled bridge holds no resources.
- **Port**: one attachment point on a bridge, with a port id unique within its
  bridge, written `ktbr0/radio`. Every connection type is a port type.
- **Session**: one live connection on a port. A connecting port has at most
  one session. A listening port has one per connected client
  (`ktbr0/phones/7C:2A:…`, `ktbr0/lan/192.168.4.20:51515`).
- **Role**: each port is either **modem**-side (a frame sent to it means
  "transmit this on the air") or **app**-side (an app that uses the radio).
  Users almost never set this; it defaults from the port type.

### Port types

| Type | Default role | Connects to / accepts | 1.0? |
|---|---|---|---|
| `rfcomm-connect` | modem | A radio's SPP port (TH-D74/D75, other SPP KISS TNCs) | yes |
| `tcp-connect` | modem | A TCP KISS server: Dire Wolf, Graywolf, Soundmodem, another kiss-tnc-bridge | yes |
| `ble-peripheral` | app | Phones and tablets via the [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md) (today's behavior) | yes |
| `tcp-listen` | app | Desktop apps connecting in: Pat, Xastir, YAAC | yes |
| `rfcomm-listen` | app | Android apps that support SPP TNCs (e.g. APRSdroid) | stretch for 1.0, otherwise 1.1 |
| `ble-central` | modem | BLE KISS TNCs | after 1.0 |
| `serial` | modem | USB/serial KISS TNCs, possibly the D74/D75 over USB | open question |

A `role = "modem" | "app"` override exists for odd cases, such as a
`tcp-connect` to an app that runs its own KISS server.

### Forwarding rules

KISS isn't Ethernet. Flooding every frame to every port would make one radio
re-transmit everything another radio hears, so forwarding follows roles:

- Every complete frame from an **app** session goes to the **modem** port,
  serialized so frames never interleave.
- Every frame from the **modem** is broadcast to every app session on every
  app port.
- **App → app** never happens in 1.0. One app's transmitted frames aren't
  copied to other app sessions, matching 0.5. An opt-in echo can be added
  later if apps turn out to need it.
- **Modem → modem** can't happen in 1.0: validation allows exactly one
  modem-role port per bridge.

This is today's behavior generalized. A bridge with one modem and one BLE port
acts exactly like 0.5.

### Inside a bridge

```
              ┌──────────────────────────── Bridge ktbr0 ─────────────────────────────┐
              │                                                                        │
 app ports    │ per-session reassembly ─┐                     ┌── per-session queue ───│──> BLE notify
 (ble, tcp-   │ per-session reassembly ─┼─> KISS policy ──┐   ├── per-session queue ───│──> TCP write
  listen)     │                         │   filter        │   │                        │
              │                         │                 v   │                        │
              │                         │          Modem port state machine            │
              │                         │   Unpaired · Idle → Connecting → Connected   │
              │                         │                 → Backoff → …                │
              └─────────────────────────┴──────────────────────┬───────────────────────┘
                                                               │ AsyncRead + AsyncWrite
                                        TcpStream | bluer::rfcomm::Stream | (serial)
```

Each bridge task also keeps counters and emits **activity events** (port,
direction, bytes), coalesced to at most about 10 per second per port. The TUI
uses these to flash activity indicators.

### The bridge never blocks its select loop

Today `BridgeManager::run` awaits `TcpKissConnection::connect` and
`tokio::time::sleep(backoff)` inline in its `select!` arms
([manager.rs](../src/bridge/manager.rs)). That stalls BLE event handling for up
to 30 s, and it leaves two reconnect gaps
([kotfu/kiss-tnc-bridge#2](https://github.com/kotfu/kiss-tnc-bridge/issues/2)).
RFCOMM makes this worse, because a Classic page/connect can take several
seconds.

The modem port state machine runs `connect()` in a spawned task and reports
back over a channel. Backoff is a `tokio::time::Sleep` stored in the bridge and
polled as its own select arm. While `Connecting`, uplink frames are queued
(bounded: 32 frames / 5 s) instead of dropped.

`Unpaired` is a separate state for an `rfcomm-connect` port whose radio has no
pairing on the port's adapter. The bridge reports it and waits; it doesn't
retry in a loop. Pairing (below) moves the port on to `Connecting`.

### Modem connection policy

| Key | Values | Default | Meaning |
|---|---|---|---|
| `connect` | `on_demand`, `always` | `on_demand` | `on_demand` connects the modem when the first app session arrives, as today. `always` keeps it up with no sessions, which is useful for a `tcp-listen` port that should look "live" |
| `idle_timeout` | duration | `0s` | With `on_demand`, how long to hold the modem after the last session leaves. Keeping an RFCOMM link warm for ~1–2 minutes avoids a slow reconnect when iOS briefly drops BLE |

---

## Configuration

### Files

| Path | Owner | Contents |
|---|---|---|
| `/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml` | admin; daemon only reads | Global settings. Can also define static bridges |
| `/etc/kiss-tnc-bridge/bridges.d/*.toml` | admin; daemon only reads | More static bridges, conventionally one bridge per file. Loaded in name order |
| `/var/lib/kiss-tnc-bridge/bridges.toml` | daemon | Dynamic bridges, created and changed through the control API |
| `/var/lib/kiss-tnc-bridge/state.toml` | daemon | Runtime state that persists across restarts, such as enable/disable overrides for static bridges (below) |
| `/var/lib/kiss-tnc-bridge/secrets/` (0700) | daemon | Management tokens and phone credentials, for the gateway in 1.x |
| `/run/kiss-tnc-bridge/` | daemon | Control socket, lock file |
| `/var/lib/bluetooth/` | BlueZ | Pairing keys. We never write these directly |

Paths come from systemd's `ConfigurationDirectory=`, `StateDirectory=` and
`RuntimeDirectory=` (via `$CONFIGURATION_DIRECTORY` etc.), with command-line
overrides. That lets the state directory point at a persistent partition on
read-only or snapshotted root filesystems: Flipper OS profile snapshots, or a
Pi using an overlay root filesystem to protect its SD card.

Because the daemon never writes `/etc`, the unit can run with
`ProtectSystem=strict`, which makes the whole filesystem read-only to the
daemon except its state and runtime directories. The package ships
`kiss-tnc-bridge.toml` as a normal config file with only `[global]` and
commented examples. Nothing rewrites it, so upgrades never prompt about a
modified config file.

If `/etc/kiss-tnc-bridge.conf` (0.5) exists, the daemon logs one warning at
startup saying it's ignored and pointing to the docs. That's all; there's no
migration code.

### Example

```toml
# /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml

[global]
log_level = "info"
adapter = "00:1A:7D:DA:71:13"     # default for Bluetooth ports; name (hci0) or MAC

[control]
socket = "/run/kiss-tnc-bridge/control.sock"
group = "kiss-tnc-bridge"

[[bridge]]
id = "ktbr0"
name = "TH-D75 portable"
enabled = true

  [[bridge.port]]
  id = "radio"
  type = "rfcomm-connect"
  address = "00:11:22:33:44:55"
  radio = "th-d75"                # init/teardown profile: th-d74 | th-d75 | none
  connect = "on_demand"
  idle_timeout = "90s"
  # channel = 2                   # default: SDP lookup via BlueZ profile connect

  [[bridge.port]]
  id = "phones"
  type = "ble-peripheral"
  advertised_name = "TH-D75"      # default: the bridge name
  adapter = "hci1"                # USB dongle for phones; built-in chip talks to the radio
  max_clients = 2

  [[bridge.port]]
  id = "lan"
  type = "tcp-listen"
  listen = "127.0.0.1:8001"
  max_clients = 4
```

```toml
# /etc/kiss-tnc-bridge/bridges.d/home.toml

[[bridge]]
id = "ktbr1"
name = "Graywolf at home"
enabled = false

  [[bridge.port]]
  id = "tnc"
  type = "tcp-connect"
  host = "127.0.0.1"
  port = 6700

  [[bridge.port]]
  id = "phones"
  type = "ble-peripheral"
  max_clients = 3
```

Dynamic bridges in `/var/lib/kiss-tnc-bridge/bridges.toml` use the same
`[[bridge]]` schema.

### Identifiers

- **Bridge ids** use lowercase letters, digits and `-`, at most 15 characters
  (the same limits as Linux interface names). The API assigns `ktbrN` (the
  lowest free N) when a dynamic bridge is created without an id. Static
  bridges must set `id` explicitly; `ktbrN` is the convention but not
  required.
- **Port ids** use the same character rules and must be unique within a
  bridge. If omitted, they default to the type plus an index
  (`ble-peripheral0`).
- **Adapters** can be named by `hciN` or by MAC address. `hciN` numbers depend
  on the order adapters appear, so plugging in a dongle or a change in USB
  enumeration can swap them. BlueZ stores pairings by adapter MAC, so a bridge
  pinned to `hci1` could land on the wrong chip with no pairing. The docs
  recommend MACs, and `ktbctl setup` writes MACs.

### Enable, disable, and switching setups

The point of disabling is to keep several bridges defined, one per physical
setup, and enable whichever matches the hardware you have with you. Several
bridges can use the same radio, port number, or advertised name, as long as
**only one of them is enabled at a time**.

- For **dynamic bridges**, `enabled` is part of the definition and is changed
  through the API.
- For **static bridges**, the `enabled` value in `/etc` is the default.
  `ktbctl enable` / `disable` store an override in
  `/var/lib/kiss-tnc-bridge/state.toml`. The definition itself still can't be
  changed through the API; enablement is runtime state, the same way
  `systemctl enable` works on unit files it doesn't own. `ktbctl status` shows
  when an override is in effect, and `ktbctl enable --reset ktbr1` clears it.
- `ktbctl switch ktbr1` enables `ktbr1` and disables every enabled bridge
  whose resources conflict with it, in one step. Plain `enable` refuses on a
  conflict and names the bridge in the way.

### Validation (`kiss-tnc-bridge -t`)

The existing `-t/--test-config` flag (like `nginx -t`) validates the main file,
`bridges.d/`, and the dynamic `bridges.toml` together. It prints the effective
set of bridges, with source file and enabled state, and exits 0 if valid.

Errors:

- TOML syntax, unknown keys, bad values.
- Duplicate bridge ids or names within the static set (main file +
  `bridges.d/`).
- Duplicate port ids within a bridge.
- A bridge without exactly one modem-role port, or without at least one
  app-role port. This also applies to disabled bridges, so a bridge is known
  valid before it's enabled.
- Among **enabled** bridges only, resource conflicts:
  - the same radio address on two `rfcomm-connect` ports (a radio accepts one
    SPP connection),
  - the same `tcp-listen` address and port,
  - the same advertised BLE name on the same adapter.

Warnings:

- An `rfcomm-connect` radio that isn't paired on its adapter, e.g.
  "ktbr0/radio: 00:11:22:33:44:55 not paired on adapter 00:1A:7D:DA:71:13
  (run `ktbctl pair ktbr0/radio`)".
- A `tcp-listen` bound to `0.0.0.0` (see TCP exposure below).
- No bridge is enabled.

**Static vs. dynamic collisions:** if a dynamic bridge has the same id or name
as a static one, the static bridge wins and the dynamic one is disabled (not
deleted) with a warning. The API refuses to create a dynamic bridge whose id or
name is taken by a static one.

`SIGHUP` or `ktbctl reload` re-reads everything and reconciles (below). An
invalid config on reload is rejected as a whole, and the running bridges keep
going.

### Dynamic bridges: writing and hand-editing

- The daemon writes `bridges.toml` with `toml_edit`, so comments and formatting
  survive. Writes are atomic: write to a temp file, fsync, rename. A power cut
  on a battery-powered device can't leave a half-written file.
- Before every write, the daemon compares the file's hash with what it last
  wrote. If they differ, someone edited it by hand while the daemon was
  running. The daemon refuses to overwrite it and logs "bridges.toml changed
  on disk, run `ktbctl reload`" rather than discarding the edit.
- **`ktbctl edit`** is the documented way to hand-edit, like `visudo` or
  `virsh net-edit`. It opens a copy in `$EDITOR`, validates it through the API
  on save (and reopens the editor on errors), then applies it.
- `ktbctl edit ktbr2` edits a single dynamic bridge. On a static bridge it
  refuses and prints the file that defines it.

### TCP exposure

**This is a licensing issue, not just a security one.** KISS TCP has no
authentication. Anyone who can reach a `tcp-listen` port can transmit on your
radio under your callsign. `listen` with only a port number binds to
`127.0.0.1`, and binding to `0.0.0.0` is a `-t` warning and logged at startup.
An optional `allow = ["192.168.4.0/24"]` allowlist on `tcp-listen` is cheap
and worth it on a Flipper One running as a Wi-Fi AP.

---

## Multiplexing a radio safely

Sharing one radio TNC among several apps needs rules that a single TCP Dire
Wolf connection never needed. They're applied in the bridge's KISS policy
filter on app-to-modem traffic:

- **Drop KISS "return" (`C0 FF C0`) from apps.** On Kenwood radios it takes the
  TNC out of KISS mode, which breaks it for everyone. The bridge logs it and
  swallows it. Only the daemon sends it, during teardown, and only if the
  port's `exit_kiss_on_disconnect = true`.
- **KISS parameter commands** (TXDELAY, P, SlotTime, TXtail, FullDuplex,
  SetHardware) pass through by default. A per-bridge
  `app_params = "pass" | "drop"` lets an operator pin the radio's settings.
- **KISS init.** On connect, `radio = "th-d75"` sends the sequence that puts
  the TNC in KISS mode. Use B.B. Link's firmware (the `halka/bb-link` fork has
  explicit KISS-mode initialisation) as the behavioral reference for the exact
  CAT commands, timing, and the D74/D75 differences. Re-implement it from the
  documented behavior rather than porting code, until the license is confirmed
  compatible with GPL-3.0.
- **Port nibble.** The KISS port number (high nibble of the command byte)
  passes through untouched in 1.0. It becomes the routing key when bridges get
  several modems.

---

## Bluetooth specifics

- Enable `bluer`'s `rfcomm` feature. Verified that bluer 0.17.4 exposes
  `Session::register_profile`, `Device::connect_profile`, `Device::pair`, and
  `rfcomm::{Profile, Role, Stream}`.
- **Connect through a BlueZ profile, not a hard-coded channel.** Register a
  `Profile { uuid: SPP 0x1101, role: Client, require_authentication: true }`,
  call `device.connect_profile(SPP)` on the device under the port's adapter,
  and take the `Stream` from the `ProfileHandle` connect request. BlueZ does
  the SDP lookup. A `channel` override is kept for oddball devices.
- **Pairings belong to an adapter.** The radio has to be paired with the
  adapter named on its `rfcomm-connect` port. Moving the port to another
  adapter means pairing again. `ktbctl pair ktbr0/radio` reads the address and
  adapter from the port.
- **Pairing never changes the configuration**, so it works on static bridges
  too. It's an operation on BlueZ. Pre-built Pi or Flipper images can't
  include pairings, because the keys belong to one specific adapter and radio.
  Pairing is always a first-boot step, which `ktbctl setup` walks through.
- After a successful pair, `set_trusted(true)` so reconnects need no
  authorization.
- Watch for BlueZ or PipeWire auto-connecting the radio's audio profiles
  (HFP/HSP). It's harmless on a headless Pi, but on a desktop it can grab the
  radio. Document it, and possibly `Device::disconnect_profile` the audio UUIDs.
- **Classic and BLE on one adapter:** BLE peripheral advertising and BR/EDR
  connections at the same time are normal for dual-mode controllers. Classic
  *inquiry* (scanning) can disturb live links, so `scan` warns when sessions
  are connected. If one controller can't handle both roles reliably, put the
  ports on different adapters: every Bluetooth port has its own `adapter`
  setting, such as the built-in chip for the radio and a USB dongle for phones.
- **One GATT application per `ble-peripheral` port**, rather than one per
  adapter as today. Enabling, disabling or changing one bridge then registers
  or unregisters only its own application, and doesn't disconnect phones on
  other bridges sharing the adapter. BlueZ allows multiple applications per
  adapter.

### Pairing agent

Today the daemon registers a `NoInputNoOutput` agent so BLE phones pair with
Just Works ([main.rs](../src/main.rs)). BlueZ uses the agent's declared
capability for the whole session, and the radio may need PIN or passkey
interaction. Plan:

1. **Normal mode:** the current `NoInputNoOutput` agent, unchanged.
2. **Pairing window:** when the control API receives `pair`, the daemon
   unregisters the normal agent and registers a `KeyboardDisplay` agent whose
   callbacks (`request_pin_code`, `display_passkey`, `request_confirmation`, …)
   are forwarded as events to the requesting control client. Then it calls
   `device.pair()`, waits up to 60 s, sets the device trusted, and restores the
   normal agent. A BLE phone pairing during that window could see a numeric
   prompt. Document this as acceptable.
3. If Phase 0 shows the D75 pairs fine under `NoInputNoOutput`, skip step 2 for
   1.0 and keep it as a fallback for other radios.

---

## Management

### One management core, many front-ends

The daemon owns one object model: **bridges → ports → sessions**. Each object
has a state and counters, plus a `source` field (`static: <file>` or
`dynamic`) and an `editable` flag. On top of that sit a set of commands and an
event stream. Every UI is a client of that model over some transport:

```
                          ┌── Unix socket ──────── ktbctl (CLI)        ┐
  kiss-tnc-bridge ────────┤                        ktbctl tui (TUI)     ├ 1.0
  (root; bridges, ports,  │                                             ┘
   sessions, events)      └── Unix socket ── ktb-gateway ──┬── HTTP + WebSocket ── web UI       ┐
                               (unprivileged)              ├── TCP + mDNS ──────── LAN phone app ├ 1.x
                                                           └── BLE mgmt GATT ───── phone app,   │
                                                                                   no network   ┘
```

**The daemon only speaks the local Unix socket.** It runs as root and holds
the radios, and it has to stay small enough for a Pi 1. Everything reachable
from the network or over BLE management lives in `ktb-gateway`, a separate
unprivileged process. It talks to the daemon over the same socket protocol as
`ktbctl` and does its own authentication for remote users. Any process can
register its own GATT service with BlueZ, so the BLE management service
doesn't have to live in the daemon. A headless Pi Zero 2 W installs the
gateway; a Pi 1 that only needs the CLI doesn't.

### Control protocol

- **Transport (1.0):** Unix stream socket at
  `/run/kiss-tnc-bridge/control.sock`, mode `0660`, group `kiss-tnc-bridge`
  (created by the deb/rpm scripts).
- **Framing:** newline-delimited JSON. Every message carries `"v": 1`.
  Requests have an `id`, responses echo it with `ok`/`error`, and events have
  no `id`.
- **Types** live in `control/protocol.rs` with serde, shared by the daemon,
  `ktbctl` and `ktb-gateway`, so they can't drift. A **JSON Schema** generated
  from them (`schemars`) is published so Swift and Kotlin apps can generate
  matching types.
- **Same messages over every transport.** Over WebSocket each frame is one
  message. Over BLE management, a write characteristic and a notify
  characteristic carry length-prefixed messages split into MTU-sized chunks.
- **Versioning:** additive changes stay in v1; breaking changes need v2.
  Documented in `docs/control-api.md`.

```jsonc
→ {"v":1,"id":1,"cmd":"status"}
← {"v":1,"id":1,"ok":true,"result":{"bridges":[{
     "id":"ktbr0","name":"TH-D75 portable","enabled":true,
     "source":"static:/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml","editable":false,
     "ports":[
       {"id":"radio","type":"rfcomm-connect","role":"modem","state":"connected",
        "since":"2026-10-12T14:02:11Z","address":"00:11:22:33:44:55"},
       {"id":"phones","type":"ble-peripheral","role":"app","advertising":false,
        "sessions":[{"id":"7C:2A:…","since":"2026-10-12T14:02:09Z"}]},
       {"id":"lan","type":"tcp-listen","role":"app","listen":"127.0.0.1:8001","sessions":[]}],
     "counters":{"frames_up":12,"frames_down":340,"dropped":0}}]}}

→ {"v":1,"id":2,"cmd":"subscribe","topics":["state","activity","pairing"]}
← {"v":1,"event":"activity","port":"ktbr0/radio","dir":"rx","bytes":73}
← {"v":1,"event":"pairing.request_passkey","request":7,"device":"00:11:22:33:44:55"}
→ {"v":1,"id":3,"cmd":"pairing.reply","request":7,"passkey":"0000"}
```

| Command | Purpose |
|---|---|
| `status` | Bridges, ports, sessions, states, counters, adapters |
| `subscribe` | Event topics: `state`, `activity` (coalesced, ≤10/s per port), `frames` (full frames with decoded AX.25 header), `pairing`, `scan` |
| `bridge.create` / `bridge.update` / `bridge.delete` | Dynamic bridges only |
| `bridge.enable` / `bridge.disable` / `bridge.switch` | Any bridge; on static bridges this sets an override in `state.toml` |
| `port.add` / `port.update` / `port.remove` | Dynamic bridges only |
| `port.connect` / `port.disconnect` | Force a modem port up or down |
| `session.kick` | Disconnect one session |
| `scan.start` / `scan.stop` | Discovery with `transport: classic \| le \| auto`. Emits `scan.device` events and flags likely radios (SPP UUID, `TH-D7*` names) |
| `pair` / `unpair` | Pairing window, by port (`ktbr0/radio`) or by address + adapter |
| `config.validate` | Validate a candidate config without applying it (used by `ktbctl edit`) |
| `reload` | Re-read config and reconcile (also on `SIGHUP`) |

### Reconciler

The adapter supervisor becomes a **reconciler**. It holds the desired set of
bridges (static + dynamic + enable overrides) and the running set, and on any
change it starts, stops or restarts only the bridges and ports that differ.

**Adapters are tracked per port, not per bridge.** Today the supervisor groups
TNCs by adapter and tears down everything on an adapter when it goes away. In
the new model, losing a `ble-peripheral` port's adapter stops only that port,
and the bridge's modem and TCP sessions keep running. Losing the modem port's
adapter puts the modem into `Backoff` while app sessions stay connected. When
the adapter returns, only the affected ports restart. Adapter hot-plug and
power on/off detection (today's supervisor loop) stays as it is and feeds the
reconciler.

### `ktbctl`: CLI

Modeled on `ip link` and `systemctl`. Human output by default, `--json`
everywhere for scripts. It talks only to the control socket, never to BlueZ
directly.

```
ktbctl status [BRIDGE]
ktbctl setup                                   # interactive wizard (below)
ktbctl bridge add [ID] --name NAME             # dynamic; id defaults to next free ktbrN
ktbctl bridge rm BRIDGE
ktbctl bridge rename BRIDGE NAME
ktbctl enable BRIDGE | disable BRIDGE | switch BRIDGE
ktbctl port add BRIDGE TYPE [--id ID] [type options]
    e.g. ktbctl port add ktbr0 rfcomm-connect --address 00:11:… --radio th-d75
         ktbctl port add ktbr0 ble-peripheral --advertised-name "TH-D75" --adapter 00:1A:…
         ktbctl port add ktbr0 tcp-listen --listen 127.0.0.1:8001
ktbctl port rm BRIDGE/PORT
ktbctl connect BRIDGE/PORT | disconnect BRIDGE/PORT
ktbctl kick BRIDGE/PORT/SESSION
ktbctl scan [--classic|--le] [--timeout 15s]
ktbctl pair BRIDGE/PORT | pair ADDR --adapter ADAPTER
ktbctl unpair BRIDGE/PORT | unpair ADDR --adapter ADAPTER
ktbctl edit [BRIDGE]                           # dynamic bridges only
ktbctl monitor [BRIDGE]                        # live frames: W1AW-9>APDR16,WIDE1-1: ...
ktbctl reload
ktbctl tui                                     # also: ktbctl with no arguments
```

Bridges can be referred to by id or by name.

**`setup` wizard** (the B.B. Link Configurator equivalent):

1. Tell the user to put the radio in Bluetooth pairing mode and turn on KISS
   (with menu paths for the D74 and D75).
2. Pick adapters (listing them by name, MAC, and whether they're built-in or
   USB), then scan for Classic devices, listing likely radios first.
3. Pair, relaying any PIN or confirm prompt.
4. Ask for a bridge name (the default is the radio's name), whether to share
   over BLE (yes) and on which adapter, and whether to share over TCP (no, or
   which port, with the exposure warning).
5. Create a dynamic bridge with adapter MACs. If another enabled bridge
   conflicts with it, offer `switch`.
6. Connect, then wait for the first received frame or 30 s of silence, and
   report the result.

### `ktbctl tui`: full-screen TUI

Built with `ratatui`, in the same binary as the CLI (the same relationship as
`nmtui` and `nmcli`).

```
┌ kiss-tnc-bridge ─ pi-zero ─────────────────────────────────────────────────┐
│ ktbr0  TH-D75 portable                                         ● enabled  │
│   radio   rfcomm-connect  00:11:22:33:44:55  connected 2h14m   ■ rx ■ tx  │
│   phones  ble-peripheral  hci1 "TH-D75"      1/2 sessions      ■ rx ■ tx  │
│             └ 7C:2A:…  iPhone                                  ■ rx ■ tx  │
│   lan     tcp-listen      127.0.0.1:8001     0/4 sessions                  │
│ ktbr1  Graywolf at home                                        ○ disabled │
├────────────────────────────────────────────────────────────────────────────┤
│ 14:22:07 rx ktbr0/radio  W1AW-9>APDR16,WIDE1-1:!4237.14N/07120.83W>        │
│ 14:22:09 tx ktbr0/phones KD2ABC>APRS,WIDE1-1::W1AW-9   :hello{12           │
├────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ select  e enable  d disable  s switch  p pair  a add  m monitor  q quit │
└────────────────────────────────────────────────────────────────────────────┘
```

- A tree of bridges → ports → sessions, with state, adapter, and uptime.
- Activity squares per port and per session that flash on rx and tx in
  different colors, driven by `activity` events.
- A live frame pane at the bottom, from `frames` events.
- Actions: enable, disable, switch, connect, disconnect, kick, pair (with the
  pairing prompts in a dialog), and add or remove dynamic bridges and ports.
  Static bridges are shown read-only, labeled with their source file.
- It has to work over SSH and at 80×24, because the main use is a headless
  Pi.

### `ktb-gateway` (1.x)

Translates the control protocol to remote transports, each a cargo feature
and each off unless configured:

- **HTTP + WebSocket:** serves a web UI as embedded static files, plus the
  protocol over WebSocket.
- **LAN TCP + mDNS:** the protocol over TCP for phone apps on the LAN,
  advertised through Avahi over D-Bus. Avahi ships with Raspberry Pi OS, and
  running a second mDNS responder alongside it gets messy. It could also
  advertise `tcp-listen` KISS ports; first check whether any APRS apps
  already browse for a service type.
- **BLE management GATT:** a separate 128-bit service UUID (not the KISS
  service), carrying the protocol as described above.

**Security matters more here than for most gadgets: this controls a
transmitter under your callsign.**

- The network transports require a token, generated with `ktbctl` and stored
  in `secrets/`. They bind to LAN addresses only by default, with optional
  TLS (certificates in `/etc/kiss-tnc-bridge/`).
- BLE management can't rely on Bluetooth pairing for security. The daemon's
  Just Works agent gives no protection against an attacker in the middle, and
  a headless Pi has no screen to show a passkey. Authentication happens inside
  the app protocol instead: a setup code like HomeKit's (created with
  `ktbctl gateway code`, or printed in the log), which the phone presents once
  in exchange for a stored credential.
- **Management window:** BLE management only accepts *new* phones for N
  minutes after boot or after `ktbctl gateway open`. This also solves first
  setup on a headless Pi with no network configured: boot it, open the phone
  app within 10 minutes, enter the code.
- The gateway throttles `activity` and `frames` events over BLE.

---

## Code layout

```
src/
  lib.rs                   shared by all binaries
  kiss/                    unchanged: frame.rs, buffer.rs
  bridge/
    mod.rs                 bridge task: modem state machine, session registry, routing,
                           counters, activity events
    policy.rs              KISS command filtering
  port/
    mod.rs                 port types, roles, session ids
    stream.rs              KissConnection<S: AsyncRead + AsyncWrite> (from bridge/tcp.rs)
    tcp_connect.rs
    tcp_listen.rs
    rfcomm_connect.rs      linux only
    ble_peripheral.rs      GATT service + advertising (from bridge/manager.rs), linux only
    radio_init.rs          th-d74, th-d75, none
  config/
    model.rs               serde types
    load.rs                main file + bridges.d + dynamic file + overrides → effective set
    validate.rs            shared by -t, reload, and config.validate
    store.rs               dynamic bridges.toml: toml_edit, atomic writes, hash check
  control/
    protocol.rs            serde types + JSON Schema export
    server.rs              Unix socket server
  supervisor.rs            adapter lifecycle (by name or MAC) + reconciler
  agent.rs                 pairing agent: normal mode + pairing window
  ctl/                     ktbctl: cli.rs, setup.rs, edit.rs, tui/
  bin/
    kiss-tnc-bridge.rs
    ktbctl.rs
    ktb-gateway.rs         1.x, behind a feature
```

One package with several `[[bin]]` targets and a shared lib. A workspace would
be overkill at this size, and a single package keeps the existing deb/rpm/
release pipeline nearly unchanged. The CLI's and TUI's dependencies
(`ratatui`, `dialoguer`) never load into the daemon.

---

## Phases

Each phase ends in a release. 0.6 introduces the new config and stops reading
0.5 configs. **1.0.0** marks the config schema, control protocol v1, and
`ktbctl` as stable.

### Phase 0: Spikes (before committing)

Use throwaway code on a branch to answer the questions that could sink the
plan:

- [ ] TH-D75 pairs with BlueZ (`bluetoothctl`) on a Pi 4, and on an RTL8761B
      dongle. Record exactly which agent callbacks fire (PIN? passkey? Just
      Works?).
- [ ] A bluer `register_profile` + `connect_profile(SPP)` program connects and
      reads KISS frames from the radio.
- [ ] Same controller: run today's BLE GATT bridge and the RFCOMM link
      concurrently for 1 hour with aprs.fi connected. Watch for drops.
- [ ] Repeat with the radio and BLE on different adapters (built-in +
      RTL8761B dongle), to confirm that fallback works.
- [ ] Read the bb-link source (both forks): KISS init commands, reconnect
      behavior, D74 vs D75 differences, license.
- [ ] Measure RFCOMM connect time and behavior when the radio is off or out of
      range (page timeout).

**Exit:** a short findings section appended to this doc. If pairing or
coexistence is a dead end on Pi hardware, stop here.

### Phase 1: Bridge core + new config (→ 0.6.0)

- [ ] Extract `KissConnection<S>` from `TcpKissConnection`.
- [ ] Bridge task with port roles, forwarding rules, KISS policy filter, and
      the non-blocking modem state machine. This fixes
      [kotfu/kiss-tnc-bridge#2](https://github.com/kotfu/kiss-tnc-bridge/issues/2)
      if it hasn't already been patched in 0.5.x.
- [ ] Port types `ble-peripheral` (moved out of `BridgeManager`, one GATT app
      per port), `tcp-connect`, and `tcp-listen` (with the localhost default
      and `allow`).
- [ ] TOML config: main file + `bridges.d/`, ids and names, `enabled`,
      adapters by name or MAC, `-t` validation with conflict checks among
      enabled bridges, and the 0.5 config warning.
- [ ] systemd unit: `ConfigurationDirectory=`, `StateDirectory=`,
      `RuntimeDirectory=`, `ProtectSystem=strict`, and moving the lock file to
      the runtime dir. Package the new example config.
- [ ] Bridge unit tests with `tokio::io::duplex` modems and fake app ports.
      This is the first time routing is testable without BlueZ.
- [ ] End-to-end CI test with no Bluetooth: fake TNC server → `tcp-connect` →
      bridge → `tcp-listen` → several clients, checking broadcast, uplink
      serialization, and `C0 FF C0` filtering.
- [ ] Add a PR CI workflow (`cargo test`, `clippy`, `fmt --check`). Today only
      the release workflow exists.

### Phase 2: RFCOMM modem port (→ 0.7.0)

- [ ] `rfcomm-connect` using the profile approach, with the `Unpaired` state.
      Pairing is still manual (`bluetoothctl`) in this release.
- [ ] `radio_init` for th-d74/th-d75, `idle_timeout`, `connect = "always"`.
- [ ] Per-port adapter tracking in the supervisor.
- [ ] `-t` warning for unpaired radios.
- [ ] README: TH-D75 walkthrough, hardware notes, and the TCP exposure warning.
- [ ] Manual hardware test matrix (below).

At this point the core value is shipped: a Pi replaces a B.B. Link and also
shares the radio over TCP, configured by hand.

### Phase 3: Control API, dynamic bridges, reconciler (→ 0.8.0)

- [ ] `control::server`, protocol types, JSON Schema export, `subscribe` with
      `state`, `activity` and `frames` events, and counters.
- [ ] Reconciler: start, stop or restart only what changed; `reload`/`SIGHUP`.
- [ ] Dynamic `bridges.toml` store (toml_edit, atomic writes, hash check) and
      `state.toml` enable overrides.
- [ ] `bridge.*`, `port.*` and `config.validate` commands, with static
      bridges read-only.
- [ ] `docs/control-api.md`.

### Phase 4: `ktbctl` CLI, pairing, TUI (→ 0.9.0)

- [ ] All CLI commands above, plus `--json`.
- [ ] `ktbctl edit` with validate-and-reopen.
- [ ] Pairing-window agent (unless Phase 0 made it unnecessary), `scan` with
      radio detection, and the `setup` wizard.
- [ ] `ktbctl tui`: tree view, activity squares, frame pane, actions, pairing
      dialog, works at 80×24 over SSH.
- [ ] `monitor` with an AX.25 address-header decode (display only), shared
      with the TUI's frame pane.
- [ ] Package `ktbctl` in deb/rpm/tarball.

### Phase 5: Hardening → 1.0.0

- [ ] Soak test: 24 h on a Pi Zero 2 W with a D75, two BLE clients and one TCP
      client, checking memory, reconnects, and that no frames are lost while
      the modem is up.
- [ ] Check the daemon's footprint on a Pi 1 (armhf) still fits the README's
      "~5MB" claim.
- [ ] Stretch: `rfcomm-listen` for Android SPP apps.
- [ ] Rewrite the README around bridges and ports, with new architecture
      diagrams and a "coming from 0.5" section showing the equivalent TOML.
- [ ] Flipper One validation, if hardware is available by then. Otherwise 1.0
      ships for Pi and the Flipper work moves to 1.x.

### Phase 6: `ktb-gateway` (1.x)

- [ ] Gateway process and packaging: unprivileged user, member of the
      `kiss-tnc-bridge` group.
- [ ] Token auth, `secrets/`, LAN-only defaults, optional TLS.
- [ ] HTTP + WebSocket transport and a web UI.
- [ ] LAN TCP transport with mDNS through Avahi.
- [ ] BLE management GATT service, setup codes, and the management window.
- [ ] Publish the protocol schema for phone app developers. The phone apps
      themselves are separate projects.

### Phase 7: Flipper One (1.x, gated on hardware and SDK)

- [ ] Verify on Flipper OS: BlueZ is the stack, systemd is present, glibc
      works with our `cross` builds, and which paths survive its profile
      snapshots (pointing `StateDirectory` elsewhere if needed).
- [ ] Check the MT7921 Bluetooth under load (MediaTek btusb firmware has had
      quirks on Linux): BLE peripheral and RFCOMM at the same time, and
      extended advertising with several `ble-peripheral` ports. If it falls
      short, the fallback is a USB Bluetooth dongle in one of the Flipper
      One's USB ports, with the radio and BLE ports on different adapters.
      Test that setup too, and document a recommended dongle.
- [ ] Check for conflicts with Flipper OS's own Bluetooth services (a default
      agent, or its own GATT apps).
- [ ] Power: measure the battery cost of `connect = "always"` against
      `on_demand`, and consider a configurable advertising interval.
- [ ] On-device UI for the 256×144 screen, built on the control protocol: the
      enabled bridge, modem state, session count, last heard station, a
      bridge switcher, and a guided pairing flow. The technology depends on
      what Flipper ships for apps.
- [ ] Possibly ship as a Flipper OS package or profile if they have a
      distribution channel.

The Flipper One fits this project unusually well. It has Bluetooth for the
radio and the phone, USB for a second Bluetooth adapter, Ethernet for a laptop
running Winlink, Wi-Fi for an AP serving TCP clients in the field, a battery,
and a screen for status. None of that needs anything beyond this plan's
architecture. The control protocol is the integration point.

---

## Test matrix (manual, per release from 0.7)

| Host | Radio | BLE clients | TCP clients |
|---|---|---|---|
| Pi 4 (built-in) | TH-D75 | aprs.fi iOS, RadioMail | Pat, Xastir |
| Pi Zero 2 W (built-in) | TH-D75 | aprs.fi iOS | — |
| Pi 1 + RTL8761B | TH-D75 | aprs.fi iOS (1 client) | YAAC |
| Pi 4: built-in for radio, RTL8761B dongle for BLE | TH-D75 | aprs.fi iOS | — |
| x86_64 + dongle | TH-D74 (if available) | aprs.fi iOS | Dire Wolf-style client |
| Flipper One | TH-D75 | aprs.fi iOS | Pat over Ethernet |

Scenarios: cold start with the radio off; radio power-cycled mid-session; radio
walked out of range and back; KISS mode turned off on the radio; adapter
unplugged; adapters re-enumerated so `hci0`/`hci1` swap (MAC-pinned config must
survive); `switch` between two bridges sharing the same radio; a BLE client
sends `C0 FF C0`; a client floods the uplink; a slow TCP client; a hand edit to
`bridges.toml` while the daemon runs.

---

## Open questions

1. **D75 pairing model** (Phase 0). This decides whether the pairing-window
   agent is needed at all.
2. **Enable overrides on static bridges:** this plan lets `ktbctl enable` /
   `disable` / `switch` override a static bridge's `enabled` value through
   `state.toml`, so switching setups doesn't require editing `/etc`. The
   alternative is strictly read-only static bridges. Confirm.
3. **Serial modem port** (`serial`, e.g. the D74/D75 over USB). It's nearly
   free once ports are just streams (`tokio-serial`). In 1.0, or 1.1?
4. **Other SPP KISS devices** besides the D74/D75. `radio = "none"` should
   work for any SPP KISS TNC. Collect testers after 0.7.
5. **Flipper OS unknowns:** app SDK, Bluetooth ownership, filesystem layout,
   package format.
6. **mDNS service type** for KISS TCP ports: is there existing practice among
   APRS apps?

## After 1.0

- **Several modems per bridge**, presented to apps as a multi-port KISS TNC.
  The bridge rewrites the KISS port number in both directions: the D75 is
  port 0 and Dire Wolf port 1, so apps that support KISS ports can pick a
  radio.
- **`ble-central` port:** connect *to* BLE KISS TNCs (Mobilinkd TNC4, radios
  that implement the BLE KISS API) and share them like any other modem.
- **Per-session transmit permissions** (receive-only apps) on top of the KISS
  policy filter.
- **App-to-app echo:** an opt-in per-bridge setting that copies each app's
  transmitted frames to the other app sessions, if apps turn out to need it.
