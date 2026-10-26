# kiss-tnc-bridge 1.0 plan

Status: draft, October 2026

## Goal

Turn `kiss-tnc-bridge` from a one-way "TCP TNC → BLE" bridge into a small
**KISS switchboard**, modeled on the virtual networks in VMware Workstation or
Linux bridges. You define **bridges** (`ktbr0`, `ktbr1`, …). Each bridge
connects one KISS TNC (a radio's built-in TNC over Bluetooth Classic SPP or
USB, a standalone TNC, or a software TNC like Dire Wolf over TCP) to any
number of apps, over BLE and over TCP. Bridges are managed from a CLI and a
full-screen TUI, and the management protocol is designed so a web UI and phone
apps can be added later.

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

- **One TNC port per bridge** in 1.0. Bridges with several TNC ports,
  presented as a multi-channel KISS TNC, come after 1.0.
- **No app-to-app echo** in 1.0. Apps only hear what the TNC sends.
- **New TOML config in a new directory, `/etc/kiss-tnc-bridge/`.** 0.5 INI
  configs aren't read or migrated. There are very few users, and the old file
  at `/etc/kiss-tnc-bridge.conf` is left untouched.
- **Static config** (bridges, TNCs, adapters) lives in `/etc`, in the main
  file and in `conf.d/`. Its definitions can't be changed through management
  tools. A static bridge's enabled state can be: `ktbctl enable` / `disable` /
  `switch` store an override in `/var/lib`, so switching setups never
  requires editing `/etc`. **Dynamic config** lives in
  `/var/lib/kiss-tnc-bridge/dynamic.toml` and is owned by the daemon.
- **Ports are defined inline in bridges.** Hardware with a real identity is
  defined once at the top level and referenced: **`[[tnc]]`** (a hardware
  KISS TNC, such as the one built into a TH-D75, with how to reach it over
  Bluetooth and/or USB serial) and **`[[adapter]]`** (a readable id for a
  Bluetooth adapter's MAC). TCP ports have no hardware identity and stay
  entirely inline.
- **The object is a TNC, not a radio.** "Radio" would imply receiving audio
  that still needs demodulating, and this project will never include a
  software modem. Everything on that side of a bridge is a TNC: a hardware
  one (`[[tnc]]`), or a software one reached over TCP (`tcp-client`).
- **Port roles are `tnc` and `app`** (not "modem"), for the same reason.
- **Serial TNCs are in 1.0**, including the TH-D75's TNC over USB.
- **`tcp-server` ports are announced with DNS-SD in 1.0**, following the
  [TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md)
  (`_kiss-tnc._tcp`), so apps like aprs.fi on iOS can pick a bridge from a
  list. A bridge has one public name, used for both BLE and DNS-SD, as the
  spec requires.
- **Port type names use the transport users know:** `tcp-`, `spp-` and `ble-`
  prefixes with `-client`/`-server`, plus `serial`. No RFCOMM or BLE
  central/peripheral jargon in anything users type (see
  [Port types](#port-types)).
- **Bridges have an id (`ktbr0`) and a human-friendly name, and can be
  disabled.** You can keep a bridge for each physical setup and enable the one
  that matches what you have with you.
- **CLI and TUI in 1.0.** The web UI, LAN management and BLE management run in
  a separate unprivileged gateway process, after 1.0. The protocol is
  designed for all of them now.
- **Names:** the daemon stays `kiss-tnc-bridge`. The CLI/TUI is `ktbctl`, the
  gateway `ktb-gateway`, and bridge ids default to `ktbrN`. Port types are
  `<transport>-client` / `<transport>-server` (see
  [Port types](#port-types)), and the attachment points stay "ports" (see
  [Naming conventions](#naming-conventions)).

### Non-goals for 1.0

- More than one TNC port per bridge, and any TNC-to-TNC forwarding
  (digipeating or cross-band gating).
- `ble-client` ports, connecting out to BLE TNCs (BTech UV-PRO, Vero VR-N76,
  Mobilinkd TNC4, etc.). The port model leaves room for them; see
  [After 1.0](#after-10).
- Supporting the daemon on anything but Linux. It should still build and run
  TCP-only bridges on macOS for development, and `ktbctl` should build there
  too, but only Linux is supported.
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
- **Port**: one attachment point on a bridge, defined inline in the bridge,
  with a port id unique within its bridge, written `ktbr0/radio`. Every
  connection type is a port type. Ports carry the settings that vary per
  setup (idle timeout, max clients, listen address), so the same TNC can be
  used differently by different bridges.
- **Session**: one live connection on a port. A client port has at most one
  session. A server port has one per connected client
  (`ktbr0/phones/7C:2A:…`, `ktbr0/lan/192.168.4.20:51515`).
- **TNC**: a top-level object for a hardware KISS TNC. It can be built into a
  radio, like the Kenwood TH-D74/D75 (SPP or USB) or the ICOM ID-5200 (USB in
  1.0; its Bluetooth TNC is probably BLE), or standalone, like a NinoTNC,
  OpenTNC or Mobilinkd. Radios that expose their TNC only over BLE, such as
  the BTech UV-PRO and Vero VR-N76, need a `ble-client` port, which comes
  after 1.0. It has
  an id, name and model (which selects the KISS init sequence), plus a
  section for each way to reach it: `spp` (Bluetooth address, and the
  adapter it's paired on) and/or `serial` (device path). `spp-client` and `serial`
  ports reference a TNC by id. The Bluetooth pairing belongs to the TNC, so a
  TNC is `paired` or `unpaired` regardless of which bridges use it. See
  [TNCs](#tncs).
- **Adapter**: a top-level object that gives a Bluetooth adapter's MAC a
  readable id (`builtin`, `dongle`). Anywhere an adapter is accepted, the
  value can be an adapter id, a MAC, or an `hciN` name.
- **Role**: each port is either a **TNC** port (a frame sent to it means
  "transmit this on the air") or an **app** port (where apps that use the TNC
  connect). Users almost never set this; it defaults from the port type.
  Role and transport are independent: a `tcp-client` is normally a TNC
  client, a `tcp-server` an app server, and a `serial` port a TNC serial
  device, but the `role` override can change that.

### Naming conventions

"Port" has three meanings in this space. The docs and config keep them apart:

- **Port** (or "bridge port") always means our attachment point.
- **TCP port numbers** never get a bare `port` key. TCP endpoints are written
  `host:port` in `address` and `listen` keys, and the docs say "TCP port
  number".
- The KISS command byte's high nibble, which the KISS spec calls a "port", is
  a **KISS channel** in our docs and in the post-1.0 multi-TNC spec. Dire
  Wolf uses "channel" for the same thing. The KISS spec's term only appears
  when quoting it.

### Port types

Types are named for what the daemon does: a `-client` port connects out, and
a `-server` port accepts connections. `serial` has no direction, so it's just
`serial`. `tcp-client` connects to a KISS TCP
server like Dire Wolf's; `tcp-server` runs one.

The prefix is the transport as users meet it in radio manuals and app
settings, not the protocol layer underneath, and it matches the `[[tnc]]`
section names:

| Transport | Port types | TNC section |
|---|---|---|
| TCP | `tcp-client`, `tcp-server` | none (TCP is always inline) |
| Bluetooth Classic, Serial Port Profile | `spp-client`, `spp-server` | `[tnc.spp]` |
| Bluetooth Low Energy | `ble-client`, `ble-server` | `[tnc.ble]` (after 1.0) |
| USB or UART serial | `serial` | `[tnc.serial]` |

- **SPP, not RFCOMM.** RFCOMM is the protocol layer underneath; SPP is what
  radio spec sheets, B.B. Link's docs, and Android apps call it.
- **BLE client/server, not central/peripheral.** In general BLE these are
  separate ideas: central/peripheral is about who connects, GATT
  client/server about who holds the data, and they can be mixed. The [BLE
  KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
  fixes both: the TNC side is a peripheral *and* a GATT server, and the app
  is a central *and* a GATT client. So `ble-server` (today's behavior)
  accepts connections and serves the KISS characteristics, and `ble-client`
  connects out to a BLE TNC, exactly like their TCP counterparts.
- **Section names are per transport, not "bluetooth".** "Bluetooth" would
  cover both Classic and LE, and BLE TNCs get their own section after 1.0.

| Type | Default role | Connects to / accepts | 1.0? |
|---|---|---|---|
| `spp-client` | tnc | A TNC's Bluetooth SPP link (TH-D74/D75, other SPP KISS TNCs), via a `[[tnc]]` with an `spp` section | yes |
| `serial` | tnc | A TNC's USB or UART serial link (TH-D75 or ID-5200 over USB, NinoTNC, OpenTNC, TNC-Pi), via a `[[tnc]]` with a `serial` section | yes |
| `tcp-client` | tnc | A KISS TCP server: Dire Wolf, Graywolf, Soundmodem, another kiss-tnc-bridge's `tcp-server` | yes |
| `ble-server` | app | Phones and tablets via the [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md) (today's behavior) | yes |
| `tcp-server` | app | Apps connecting in over the network: Pat, Xastir, YAAC, and aprs.fi on iOS over Wi-Fi. Announced with DNS-SD | yes |
| `spp-server` | app | Android apps that support SPP TNCs (e.g. APRSdroid) | stretch for 1.0, otherwise 1.1 |
| `ble-client` | tnc | BLE KISS TNCs: BTech UV-PRO, Vero VR-N76, Mobilinkd TNC4, probably the ID-5200's Bluetooth TNC | after 1.0 |

A `role = "tnc" | "app"` override exists for odd cases, such as a
`tcp-client` to an app that runs its own KISS server.

### Forwarding rules

KISS isn't Ethernet. Flooding every frame to every port would make one radio
re-transmit everything another radio hears, so forwarding follows roles:

- Every complete frame from an **app** session goes to the **TNC** port,
  serialized so frames never interleave.
- Every frame from the **TNC** is broadcast to every app session on every
  app port.
- **App → app** never happens in 1.0. One app's transmitted frames aren't
  copied to other app sessions, matching 0.5. An opt-in echo can be added
  later if apps turn out to need it.
- **TNC → TNC** can't happen in 1.0: validation allows exactly one TNC port
  per bridge.

This is today's behavior generalized. A bridge with one TNC port and one BLE port
acts exactly like 0.5.

### Inside a bridge

```
              ┌──────────────────────────── Bridge ktbr0 ─────────────────────────────┐
              │                                                                        │
 app ports    │ per-session reassembly ─┐                     ┌── per-session queue ───│──> BLE notify
 (ble, tcp-   │ per-session reassembly ─┼─> KISS policy ──┐   ├── per-session queue ───│──> TCP write
  server)     │                         │   filter        │   │                        │
              │                         │                 v   │                        │
              │                         │           TNC port state machine             │
              │                         │   Unpaired · Waiting for device ·            │
              │                         │   Idle → Connecting → Connected → Backoff    │
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

The TNC port state machine runs `connect()` in a spawned task and reports
back over a channel. Backoff is a `tokio::time::Sleep` stored in the bridge and
polled as its own select arm. While `Connecting`, uplink frames are queued
(bounded: 32 frames / 5 s) instead of dropped.

`Unpaired` is a separate state for an `spp-client` port whose TNC isn't
paired on its adapter. The bridge reports it and waits; it doesn't retry in a
loop. Pairing the TNC (`ktbctl pair d75`) moves the port on to `Connecting`.
`Waiting for device` is the equivalent for a `serial` port whose device path
doesn't exist (the cable is unplugged); see [Serial details](#serial-details).

### TNC connection policy

| Key | Values | Default | Meaning |
|---|---|---|---|
| `connect` | `on_demand`, `always` | `on_demand` | `on_demand` connects the TNC when the first app session arrives, as today. `always` keeps it up with no sessions, which is useful for a `tcp-server` port that should look "live" |
| `idle_timeout` | duration | `0s` | With `on_demand`, how long to hold the TNC connection after the last session leaves. Keeping an RFCOMM link warm for ~1–2 minutes avoids a slow reconnect when iOS briefly drops BLE |

---

## Configuration

### Files

| Path | Owner | Contents |
|---|---|---|
| `/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml` | admin; daemon only reads | Global settings. Can also define static adapters, TNCs and bridges |
| `/etc/kiss-tnc-bridge/conf.d/*.toml` | admin; daemon only reads | More static adapters, TNCs and bridges, in any mix. Conventionally one bridge per file. Loaded in name order |
| `/var/lib/kiss-tnc-bridge/dynamic.toml` | daemon | Dynamic adapters, TNCs and bridges, created and changed through the control API (e.g. by `ktbctl setup`) |
| `/var/lib/kiss-tnc-bridge/state.toml` | daemon | Runtime state that persists across restarts: enable/disable overrides for static bridges, and public names adjusted after a DNS-SD collision |
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

### Minimal example

The B.B. Link equivalent: a TH-D75 over Bluetooth, shared to phones over BLE
on a Pi 4's built-in adapter.

```toml
# /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml

[[tnc]]
id = "d75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55" }

[[bridge]]
id = "ktbr0"
name = "TH-D75"

  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"

  [[bridge.port]]
  type = "ble-server"
```

- `spp = { ... }` is a TOML inline table, the same as a `[tnc.spp]` section.
- Everything else comes from [Defaults](#defaults): the bridge is enabled,
  both Bluetooth roles use BlueZ's default adapter (the built-in chip on a Pi
  4 with nothing plugged in), the BLE name is "TH-D75", and the port ids are
  `spp-client0` and `ble-server0`. There's no `tcp-server`, so nothing is
  announced with DNS-SD.
- `model = "th-d75"` could be left out too, but then the radio only works if
  KISS mode was turned on from its menu.
- The D75 still has to be paired once (`ktbctl pair d75`, or `bluetoothctl`
  until Phase 4 adds pairing to `ktbctl`). Until then `-t` warns and the port reports `Unpaired`.

### Full example

```toml
# /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml

[global]
log_level = "info"
adapter = "builtin"                # default for Bluetooth ports and TNCs

[control]
group = "kiss-tnc-bridge"          # who may use ktbctl

[[adapter]]
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[adapter]]
id = "dongle"
mac = "00:1A:7D:DA:71:13"

[[tnc]]
id = "d75"
name = "TH-D75"
model = "th-d75"                   # what it gets beyond plain KISS: generic (default) | th-d74 | th-d75

  [tnc.spp]
  address = "00:11:22:33:44:55"
  adapter = "builtin"              # the adapter it's paired on
  # channel = 2                    # default: SDP lookup via BlueZ profile connect

  [tnc.serial]
  path = "/dev/serial/by-id/usb-JVC_KENWOOD_TH-D75_XXXXXXXX-if00"
  # baud = 9600                    # mostly ignored over USB; matters for real UARTs

# Field: the D75 over Bluetooth, shared to phones and a laptop on the Pi's Wi-Fi AP
[[bridge]]
id = "ktbr0"
name = "TH-D75 portable"
enabled = true
# public_name = "TH-D75 on pi-zero"  # BLE + DNS-SD name; default: the bridge name

  [[bridge.port]]
  id = "radio"
  type = "spp-client"
  tnc = "d75"
  idle_timeout = "90s"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
  adapter = "dongle"               # phones on the dongle, radio on the built-in chip
  max_clients = 2

  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "wlan0:8001"            # only on the Wi-Fi AP interface
  max_clients = 4
  # announce = true                # DNS-SD; default true unless bound to loopback
```

```toml
# /etc/kiss-tnc-bridge/conf.d/home.toml

# Home: Graywolf on the same Pi, shared to phones (the 0.5 use case)
[[bridge]]
id = "ktbr1"
name = "Graywolf at home"
enabled = false

  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"
  address = "127.0.0.1:6700"
  connect = "always"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
  adapter = "builtin"
  max_clients = 3

  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "8001"                  # TCP port number alone binds 127.0.0.1

# Desk: the same D75 over its USB cable. Can't be enabled together with
# ktbr0, because both use TNC d75.
[[bridge]]
id = "ktbr3"
name = "TH-D75 USB"
enabled = false

  [[bridge.port]]
  id = "radio"
  type = "serial"
  tnc = "d75"
  connect = "always"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
  adapter = "builtin"

  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "8001"

# Chaining: this Pi's BLE in front of another Pi's ktbr0 "lan" port
[[bridge]]
id = "ktbr2"
name = "Shack relay"
enabled = false

  [[bridge.port]]
  id = "upstream"
  type = "tcp-client"
  address = "192.168.4.1:8001"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
  adapter = "builtin"
```

Dynamic config in `/var/lib/kiss-tnc-bridge/dynamic.toml` uses the same
`[[adapter]]`, `[[tnc]]` and `[[bridge]]` schema.

A standalone serial TNC is a `[[tnc]]` with only a `serial` section:

```toml
[[tnc]]
id = "nino"
name = "NinoTNC"
model = "generic"                  # the default; shown for clarity

  [tnc.serial]
  path = "/dev/serial/by-id/usb-VENDOR_PRODUCT_SERIAL-if00"   # whatever udev names it
  baud = 57600
```

TCP ports are always inline. A `tcp-client` is just a `host:port`, and a
`tcp-server` is just a listening socket. Neither has a hardware identity or
state that outlives a bridge, so repeating `address = "127.0.0.1:6700"` in two
bridges costs nothing.

### Defaults

| Object | Key | Default |
|---|---|---|
| `[global]` | (whole section) | Optional |
| | `log_level` | `info` |
| | `adapter` | BlueZ's default adapter (see the adapter fallback below) |
| `[control]` | `group` | `kiss-tnc-bridge` |
| `[[bridge]]` | `id` | Required for static bridges. The API assigns the lowest free `ktbrN` for dynamic ones |
| | `name` | Required. It's what phones see, through `public_name` |
| | `public_name` | The bridge `name` |
| | `enabled` | `true`. A bridge written into a file is meant to run; disabling is the deliberate exception |
| | `app_params` | `pass` |
| `[[bridge.port]]`, all | `id` | Type plus an index within the bridge: `spp-client0`, `ble-server0` |
| | `role` | From the type: `tnc` for `spp-client`, `serial`, `tcp-client`, `ble-client`; `app` for `ble-server`, `tcp-server`, `spp-server` |
| TNC ports | `connect` | `on_demand` |
| | `idle_timeout` | `0s` |
| `spp-client`, `serial` | `exit_kiss_on_disconnect` | `false` |
| `ble-server` | `adapter` | See the adapter fallback below |
| | `max_clients` | `1`, as in 0.5. Many Bluetooth chips (e.g. the RTL8761B) can't serve more |
| `tcp-server` | `listen` | Required. A TCP port number alone binds `127.0.0.1` |
| | `max_clients` | Unset: no limit. Set it (≥ 1) to cap TCP sessions |
| | `allow` | Unset: any peer that can reach the socket |
| | `announce` | `true`, except `false` for loopback bindings |
| `[[tnc]]` | `name` | The TNC `id` |
| | `model` | `generic`: nothing beyond KISS; for TNCs that are always in KISS mode |
| `[tnc.spp]` | `adapter` | See the adapter fallback below |
| | `channel` | Found by SDP lookup |
| `[tnc.serial]` | `baud` | `9600` |

**Adapter fallback.** Wherever an adapter is needed (`ble-server` ports and
`[tnc.spp]`), the daemon uses the first of:

1. the object's own `adapter`,
2. `[global] adapter`,
3. BlueZ's default adapter: the first one present, which is 0.5's behavior.

The third step keeps single-adapter setups to zero adapter config, but which
adapter is "first" isn't guaranteed once a USB dongle is added. A TNC could
then land on an adapter it isn't paired with. That fails clearly (`-t` warns,
the port reports `Unpaired`), and the docs and `ktbctl setup` steer
multi-adapter setups toward named `[[adapter]]` objects.

### Identifiers and references

- **Bridge ids** use lowercase letters, digits and `-`, at most 15 characters
  (the same limits as Linux interface names). The API assigns `ktbrN` (the
  lowest free N) when a dynamic bridge is created without an id. Static
  bridges must set `id` explicitly; `ktbrN` is the convention but not
  required.
- **Port ids** use the same character rules and must be unique within a
  bridge. If omitted, they default to the type plus an index
  (`ble-server0`).
- **TNC and adapter ids** use the same character rules and are global:
  unique across static and dynamic config, and in separate namespaces from
  each other and from bridges.
- **Adapter values** can be an adapter id, a MAC, or an `hciN` name. `hciN`
  numbers depend on the order adapters appear, so plugging in a dongle or a
  change in USB enumeration can swap them. BlueZ stores pairings by adapter
  MAC, so a port pinned to `hci1` could land on the wrong chip with no
  pairing. The docs recommend adapter ids (or MACs), and `ktbctl setup`
  creates adapter objects.
- **Reference rules:**
  - A static bridge or TNC can only reference static TNCs and adapters,
    so `/etc` is self-contained. An image builder's config then can't depend
    on runtime state, and validating `/etc` alone is meaningful.
  - Dynamic objects can reference static or dynamic ones. A common setup
    would have the admin define the TNC and adapters in `/etc` and build
    bridges around them with `ktbctl`.
  - The API refuses to delete a TNC or adapter that's referenced, and lists
    what references it. Renaming one updates dynamic references, and is
    refused if anything static references it.

### Enable, disable, and switching setups

The point of disabling is to keep several bridges defined, one per physical
setup, and enable whichever matches the hardware you have with you. Several
bridges can use the same TNC, TCP listen socket, public name, or BLE
adapter, as long as **only one of them is enabled at a time**.

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
`conf.d/`, and the dynamic `dynamic.toml` together. It prints the effective
set of bridges, with source file and enabled state, and exits 0 if valid.

Errors:

- TOML syntax, unknown keys, bad values.
- Duplicate ids within the static set (main file + `conf.d/`): bridge ids,
  bridge names, TNC ids, adapter ids.
- Duplicate port ids within a bridge.
- A reference that doesn't resolve (a port's `tnc` or `adapter`, a TNC's
  `spp.adapter`), or a static object referencing a dynamic one.
- A port whose TNC lacks the section it needs: `spp-client` needs `spp`, and
  `serial` needs `serial`.
- Two TNCs with the same Bluetooth address on the same adapter, two TNCs with
  the same serial path, or two adapter objects with the same MAC. These are
  checked whether or not anything is enabled, because they're duplicate
  definitions of the same hardware.
- An **enabled** bridge without exactly one TNC port, or without at least
  one app port. A disabled bridge may be incomplete, so it can be built up
  a port at a time; `status` and `-t` say what it still needs, and enabling
  it fails until it's complete.
- Among **enabled** bridges only, resource conflicts:
  - the same TNC on two ports, whatever the transport. A TNC accepts one SPP
    connection, a serial device should be opened once, and as far as we
    know the D74/D75 only puts its TNC on one interface at a time (Phase 0
    checks this),
  - the same `tcp-server` listen socket. An interface binding (`wlan0:8001`)
    conflicts with an address binding on the same TCP port number if the
    address is on that interface, or is `0.0.0.0`,
  - the same public name on two bridges (it's the BLE advertised name and
    the DNS-SD instance name, so two enabled bridges sharing one would be
    indistinguishable to apps),
  - two `ble-server` ports on the same adapter (see
    [Bluetooth specifics](#bluetooth-specifics) for why).

  `tcp-client` targets never conflict: Dire Wolf and Graywolf accept several
  KISS clients, so two enabled bridges pointing at the same TNC is
  legitimate. A TNC that only accepts one client just shows up as a connect
  failure in that bridge's status.

Warnings:

- A TNC used by an enabled `spp-client` port that isn't paired on its
  adapter, e.g. "tnc d75 (00:11:22:33:44:55) not paired on adapter builtin
  (B8:27:EB:12:34:56); run `ktbctl pair d75`".
- A TNC serial path that isn't under `/dev/serial/by-id/` or
  `/dev/serial/by-path/` (e.g. `/dev/ttyACM0`, which can change between
  boots), or that doesn't currently exist, for TNCs used by an enabled
  `serial` port.
- A `tcp-server` bound to `0.0.0.0` without an `allow` list (see
  [TCP server binding](#tcp-server-binding)).
- A `tcp-server` bound to an interface that doesn't currently exist.
- `announce = true` on a `tcp-server` bound to loopback (nothing on the LAN
  could connect to what's announced).
- A public name longer than about 29 bytes of UTF-8 on a bridge with a
  `ble-server` port. The BLE advertisement may truncate it, and the spec
  wants BLE and DNS-SD names to match.
- No bridge is enabled.

**Static vs. dynamic collisions:** if a dynamic bridge has the same id or name
as a static one, the static bridge wins and the dynamic one is disabled (not
deleted) with a warning. A dynamic TNC or adapter that collides with a
static one is ignored with a warning, and dynamic bridges referencing it then
resolve to the static one. The API refuses to create a dynamic object whose id
or name is taken by a static one.

`SIGHUP` or `ktbctl reload` re-reads everything and reconciles (see
[Reload semantics](#reload-semantics)). An invalid config on reload is
rejected as a whole, and the running bridges keep going.

**`-t` and `ktbctl check` answer different questions:**

| Command | Needs the daemon | Answers |
|---|---|---|
| `kiss-tnc-bridge -t` | No | Is the config on disk valid? For package scripts, CI and image builds, when the daemon often isn't running |
| `ktbctl check` | Yes | Is it valid, as the running daemon sees it? And what would a reload do? |

**Errors are config-only for both:** syntax, values, references, conflicts.
They never depend on the host, so a config that passes `-t` in CI passes on
the device. **Warnings include host checks** (TNC paired, serial device
present, interface up), done on a best-effort basis: `-t` asks BlueZ and
looks at `/dev` and the network interfaces when it can, and skips those
checks with a note when it can't (for example in a CI container with no
BlueZ). `ktbctl check` always has them, from the daemon's live view.

`ktbctl check` validates first and shows the same errors `-t` would. If the
config is valid, it prints the reload plan: each bridge and port as unchanged,
updated in place, restarted, started or stopped, plus any sessions that would
be disconnected. Exit codes: 0 valid, 1 invalid, 2 couldn't check (e.g. the
daemon isn't running). There's deliberately no exit code for "valid, with
changes pending": an unchanged reload is a no-op, so scripts can simply run
`ktbctl reload`. Nor is there one for "valid, but disruptive", which would
trip scripts that only want to know whether the config is valid. Instead,
`ktbctl check --json` includes a top-level `"disruptive": true | false` with
the reasons, and scripts that need to avoid disruption use
`ktbctl reload --non-disruptive` (see
[Reload semantics](#reload-semantics)).

### Dynamic config: writing and hand-editing

- The daemon writes `dynamic.toml` with `toml_edit`, so comments and formatting
  survive. Writes are atomic: write to a temp file, fsync, rename. A power cut
  on a battery-powered device can't leave a half-written file.
- Before every write, the daemon compares the file's hash with what it last
  wrote. If they differ, someone edited it by hand while the daemon was
  running. The daemon refuses to overwrite it and logs "dynamic.toml changed
  on disk, run `ktbctl reload`" rather than discarding the edit.
- **`ktbctl edit`** is the documented way to hand-edit, like `visudo` or
  `virsh net-edit`. It fetches the TOML with `config.get` and opens it in
  `$EDITOR`. On save it runs `config.check` on the edited copy (reopening the
  editor on errors), shows the reload plan, asks for confirmation if the plan
  is disruptive, then writes it with `config.put`. If someone else changed the
  object in the meantime, the revision check fails and `edit` says so instead
  of overwriting their change. You always see what an edit is about to do
  before it happens.
- `ktbctl edit ktbr2` edits a single dynamic bridge, and `ktbctl edit tnc
  d75` a single dynamic TNC. On a static object it refuses and prints the
  file that defines it.

### TCP server binding

`listen` on a `tcp-server` accepts:

| Form | Example | Meaning |
|---|---|---|
| TCP port number only | `"8001"` | `127.0.0.1:8001`. The safe default |
| Address and TCP port number | `"192.168.4.1:8001"`, `"[::1]:8001"`, `"0.0.0.0:8001"` | Bind that address |
| Network interface and TCP port number | `"wlan0:8001"` | Bind on that interface only, whatever its addresses are |

**Interface binding** is the usual intent on a Pi or Flipper One acting as a
Wi-Fi AP: "only on the AP interface", even if its IP changes between setups.
The daemon binds to the wildcard address with `SO_BINDTODEVICE`, which it can
do because it runs as root. If the interface doesn't exist yet (the AP isn't
up), the port reports `waiting for interface` and retries every 5 s. Watching
netlink for interface events instead is an easy later improvement. An
interface name that parses as an IP address is always treated as an address.

**This is a licensing issue, not just a security one.** KISS TCP has no
authentication. Anyone who can reach a `tcp-server` can transmit on your radio
under your callsign. So:

- The default is `127.0.0.1`.
- Binding to `0.0.0.0` (or `[::]`) without an `allow` list is a `-t` warning
  and is logged at startup.
- An optional `allow = ["192.168.4.0/24"]` allowlist on `tcp-server` rejects
  other peers at accept time. It works with any binding form.

### DNS-SD announcement

`tcp-server` ports are announced on the LAN following the
[TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md)
(Heikki Hannikainen, OH7LZB, the same author as the BLE KISS API spec). Dire
Wolf already announces this way, and aprs.fi on iOS browses for it, so a
phone on the Pi's or Flipper One's Wi-Fi can pick a bridge from a list instead
of typing an IP address and TCP port number.

- **Service type:** `_kiss-tnc._tcp` in `local.`, pointing at the
  `tcp-server`'s TCP port number.
- **Instance name: the bridge's public name.** `public_name` is a bridge-level
  key that defaults to the bridge `name`. The spec says a device with a BLE
  KISS interface MUST use the same name for BLE and DNS-SD, so the same value
  is the BLE advertised name for every `ble-server` port on the bridge.
  That's why the name lives on the bridge rather than on each port. It's
  UTF-8, and non-ASCII names are fine; the spec requires clients to handle
  them.
- **Naming style:** the spec recommends friendly names without serial numbers
  or MAC addresses, and suggests including the host name ("Dire Wolf on
  raspberrypi"). Bridge names already fit; the docs suggest something like
  "TH-D75 on pi-zero" for setups where several Pis are on one LAN. BLE
  advertising limits names to roughly 29 bytes, so `-t` warns on longer
  public names for bridges with BLE.
- **Name collisions.** If another device on the LAN already uses the name,
  Avahi reports a collision. Following the spec, the daemon appends or
  increments a number ("TH-D75 portable (2)"), stores the adjusted name in
  `state.toml` so it stays stable across restarts, and uses the adjusted name
  for BLE too, keeping the two matched. `status` and the TUI show when a name
  has been adjusted, and `ktbctl bridge public-name BRIDGE --retry` drops the
  adjustment to retry the configured name. Retrying works on static bridges
  too, since the adjustment is runtime state, not part of the definition.
- **Which interfaces.** Per `tcp-server` port:
  - `announce` defaults to `true`, except for loopback bindings, where it
    defaults to `false`; forcing it on there is a `-t` warning.
  - An interface binding (`wlan0:8001`) is announced only on that interface,
    using Avahi's per-interface publishing.
  - A specific address is announced on the interface that holds it.
  - `0.0.0.0` / `[::]` is announced on all interfaces.
- **TXT record:** no `pn` record in 1.0. The spec says clients assume a
  single-port TNC when it's absent, which matches one TNC port per bridge.
  When bridges get several TNC ports after 1.0, `pn` lists one name per KISS
  channel (the TNC names).
- **Lifecycle:** the announcement exists while the bridge is enabled and the
  `tcp-server` port is listening. It's withdrawn when the bridge is
  disabled, during `switch`, and while an interface-bound port is waiting for
  its interface. Whether the TNC is currently connected doesn't matter: with
  `connect = "on_demand"`, the TNC only connects once an app does.
- **How:** through Avahi's D-Bus API (`EntryGroupNew`, `AddService`, `Commit`)
  using the `dbus` crate we already depend on through `bluer`. Avahi ships
  with Raspberry Pi OS and is likely on Flipper OS. If Avahi isn't running,
  the daemon logs it once and carries on unannounced. Running our own mDNS
  responder next to Avahi would fight over UDP port 5353, so we don't.
- **Exposure:** announcing makes an open KISS port easy to find, which is the
  point, but it also means the [binding rules](#tcp-server-binding) matter
  more. Announcing doesn't change the defaults: a `tcp-server` bound to
  `127.0.0.1` isn't reachable or announced.
- **Debugging:** `avahi-browse -r _kiss-tnc._tcp` on Linux or
  `dns-sd -B _kiss-tnc._tcp` on a Mac shows what's announced.

---

## TNCs

A `[[tnc]]` describes one hardware KISS TNC and every way to reach it. The
TH-D75's TNC is one device reachable over Bluetooth or over USB, so it's one
object with two sections, not two objects. That keeps the conflict rule
simple: **a TNC can be in at most one enabled bridge, over whichever
transport.** Two bridges can't grab the D75 over Bluetooth and over USB at the
same time without validation catching it.

| Key | Meaning |
|---|---|
| `id`, `name` | As for other objects |
| `model` | What the TNC gets beyond plain KISS (init and teardown commands, checks that need it to answer): `generic` (the default, for TNCs that are always in KISS mode), `th-d74`, `th-d75`. Unknown values are an error that lists the models; one table in the code (`MODELS`) feeds that error, the docs and `ktbctl` |
| `[tnc.spp]` | `address`, `adapter` (the adapter it's paired on), optional `channel`. Needed by `spp-client` |
| `[tnc.serial]` | `path`, optional `baud` (default 9600). Needed by `serial` |

### Serial details

- **Stable paths.** udev creates `/dev/serial/by-id/…` symlinks from the USB
  device's vendor, product and serial number. These survive reboots and
  re-plugging, unlike `/dev/ttyACM0`, which shuffles the same way `hciN`
  does. `/dev/serial/by-path/…` (by USB port position) is the fallback for
  devices without a serial number. `-t` warns about anything else.
  `ktbctl setup` lists by-id paths and offers to create the TNC from one.
- **Line settings.** 8N1, no flow control. `baud` is mostly ignored by USB
  CDC devices like the D75 but matters for real UARTs (TNC-Pi, NinoTNC on its
  UART).
- **Exclusive open.** The port sets `TIOCEXCL` after opening, so no other
  process can open the device while the bridge holds it.
- **Hot-plug.** When the device disappears (cable unplugged, radio off), reads
  fail and the port goes to `Waiting for device`. In 1.0 it checks whether
  the path exists every 2 s and reconnects when it reappears. That avoids
  adding libudev as a C dependency in our cross builds. Event-driven
  detection through udev can come later.
- **ModemManager.** On Ubuntu and some other desktops, ModemManager probes
  newly plugged USB serial devices by sending them AT commands. That's a
  known problem with radios and TNCs. Raspberry Pi OS doesn't run it by
  default. The package ships a udev rule that sets `ID_MM_DEVICE_IGNORE` for
  known Kenwood USB IDs, and the docs show how to add other TNCs.
- **Permissions.** The daemon runs as root, so device access needs nothing
  extra today. If the unit later restricts devices with `DevicePolicy=`, it
  needs `DeviceAllow=char-ttyACM` and `char-ttyUSB`.
- **KISS init** uses the same `model` sequence as over Bluetooth, unless
  Phase 0 shows the D75 needs something different over USB.

---

## Multiplexing a TNC safely

Sharing one radio TNC among several apps needs rules that a single TCP Dire
Wolf connection never needed. They're applied in the bridge's KISS policy
filter on app-to-TNC traffic:

- **Drop KISS "return" (`C0 FF C0`) from apps.** On Kenwood radios it takes the
  TNC out of KISS mode, which breaks it for everyone. The bridge logs it and
  swallows it. Only the daemon sends it, during teardown, and only if the
  `spp-client` or `serial` port sets `exit_kiss_on_disconnect = true`.
- **KISS parameter commands** (TXDELAY, P, SlotTime, TXtail, FullDuplex,
  SetHardware) pass through by default. A per-bridge
  `app_params = "pass" | "drop"` lets an operator pin the radio's settings.
- **KISS init.** On connect, a TNC with `model = "th-d75"` gets the sequence
  that puts it in KISS mode. Use B.B. Link's firmware (the `halka/bb-link` fork has
  explicit KISS-mode initialisation) as the behavioral reference for the exact
  CAT commands, timing, and the D74/D75 differences. Re-implement it from the
  documented behavior rather than porting code, until the license is confirmed
  compatible with GPL-3.0.
- **KISS channel.** The KISS channel (the command byte's high nibble, which
  the KISS spec calls a port) passes through untouched in 1.0. It becomes the
  routing key when bridges get several TNC ports.

---

## Bluetooth specifics

- Enable `bluer`'s `rfcomm` feature. Verified that bluer 0.17.4 exposes
  `Session::register_profile`, `Device::connect_profile`, `Device::pair`, and
  `rfcomm::{Profile, Role, Stream}`.
- **Connect through a BlueZ profile, not a hard-coded channel.** Register a
  `Profile { uuid: SPP 0x1101, role: Client, require_authentication: true }`,
  call `device.connect_profile(SPP)` on the device under the TNC's adapter,
  and take the `Stream` from the `ProfileHandle` connect request. BlueZ does
  the SDP lookup. A `channel` override is kept for oddball devices.
- **Pairings belong to a TNC and an adapter.** BlueZ stores each pairing
  under one adapter, which is why `[tnc.spp]` names the adapter it's
  paired on. Changing a TNC's adapter means pairing again. `ktbctl pair d75`
  reads the address and adapter from the TNC.
- **Pairing never changes the configuration**, so it works on static TNCs
  too. It's an operation on BlueZ. Pre-built Pi or Flipper images can't
  include pairings, because the keys belong to one specific adapter and TNC.
  Pairing is always a first-boot step, which `ktbctl setup` walks through.
- After a successful pair, `set_trusted(true)` so reconnects need no
  authorization.
- Watch for BlueZ or PipeWire auto-connecting the radio's audio profiles
  (HFP/HSP). It's harmless on a headless Pi, but on a desktop it can grab the
  radio. Document it, and possibly `Device::disconnect_profile` the audio UUIDs.
- **Classic and BLE on one adapter:** BLE peripheral advertising and BR/EDR
  connections at the same time are normal for dual-mode controllers. Classic
  *inquiry* (scanning) can disturb live links, so `scan` warns when sessions
  are connected. If one controller can't handle both roles reliably, use
  different adapters: the TNC's `spp.adapter` for the Classic link and each
  `ble-server` port's `adapter` for phones, such as the built-in chip for
  the radio and a USB dongle for phones.
- **One GATT application per `ble-server` port**, rather than one per
  adapter as today. Enabling, disabling or changing one bridge then registers
  or unregisters only its own application.
- **At most one enabled `ble-server` per adapter.** BlueZ serves every
  registered GATT application to every phone connected to an adapter, so a
  phone that connected to bridge A's advertisement would also see bridge B's
  KISS service, with the same UUID, and an app could attach to the wrong one.
  iOS also caches one name per Bluetooth address, so two names from one
  adapter would confuse it (the README's existing caveat). Validation makes
  this an error among enabled bridges. Disabled bridges can share an adapter
  freely, so switching setups still works, and two simultaneous BLE bridges
  just need two adapters.
- **Adapter alias.** 0.5 sets the adapter's alias to the first TNC's name, and
  that alias is what a radio sees while pairing and what phones show in their
  Bluetooth settings. 1.0 keeps the spirit: an adapter used by an enabled
  `ble-server` gets that bridge's effective public name as its alias, updated
  when the public name changes. Adapters with no enabled `ble-server` keep
  their system alias, which the daemon leaves alone.

### Pairing agent

Phones don't pair with the bridge (see the Phase 3 notes), so the daemon
normally registers no agent. Radios do: the D75 needs SSP numeric
comparison (Phase 0). As built in Phase 4:

- **Pairing window:** `bluetooth.pair` registers an agent with every
  callback (so BlueZ sees `KeyboardDisplay`) on the daemon's own D-Bus
  connection, only for that pairing. BlueZ gives a `Pair` call to the
  caller's agent before any default (`agent_get(sender)` in 5.82), so the
  agent never needs to be the default. Its callbacks become `pairing`
  events to the requesting connection, answered with
  `bluetooth.pair_reply`. The daemon calls `device.pair()`, waits up to
  90 s, trusts the device, and drops the agent. bluer's own agent API does
  this; the hand-rolled D-Bus agent of Phase 2 (git 662d2e2) existed only
  because phones needed `NoInputNoOutput`.

---

## Management

### One management core, many front-ends

The daemon owns one object model: **bridges → ports → sessions**, plus the
top-level **TNCs** and **adapters** they reference. Adapters are listed
whether or not an `[[adapter]]` object names them, since the TUI and setup
wizard need to show the hardware that's actually present. Each object has a
state and counters, plus a `source` field (`static: <file>` or `dynamic`) and
an `editable` flag. On top of that sit a set of commands and an
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

**For management, the daemon only speaks the local Unix socket.** (Its KISS
`tcp-server` ports and DNS-SD announcements are data-plane features, not
management.) It runs as root and holds
the TNCs, and it has to stay small enough for a Pi 1. Everything reachable
from the network or over BLE management lives in `ktb-gateway`, a separate
unprivileged process. It talks to the daemon over the same socket protocol as
`ktbctl` and does its own authentication for remote users. Any process can
register its own GATT service with BlueZ, so the BLE management service
doesn't have to live in the daemon. A headless Pi Zero 2 W installs the
gateway; a Pi 1 that only needs the CLI doesn't.

### Control protocol

- **Transport (1.0):** Unix stream socket at
  `/run/kiss-tnc-bridge/control.sock`, mode `0660`, group from
  `[control] group` (default `kiss-tnc-bridge`, created by the deb/rpm
  scripts). The path is fixed, under the systemd `RuntimeDirectory=`, rather
  than configurable in `/etc`, because `ktbctl` doesn't read `/etc` and needs
  to find it. For unusual setups (a development daemon, tests), the daemon
  takes a `--socket` option, and `ktbctl` takes `--socket` or the
  `KTBCTL_SOCKET` environment variable.
- **Framing:** newline-delimited JSON. Every message carries `"v": 1`.
  Requests have an `id`, responses echo it with `ok`/`error`, and events have
  no `id`.
- **Errors** carry a stable machine-readable `code` (e.g.
  `tnc_in_use`, `reference_not_found`, `static_object_readonly`), a
  human-readable `message` that says what to do next, and the config path or
  object involved. `ktbctl` shows the message; scripts, the gateway and any
  future LLM tooling match on the code.
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
← {"v":1,"id":1,"ok":true,"result":{
     "adapters":[
       {"id":"builtin","mac":"B8:27:EB:12:34:56","hci":"hci0","present":true,"powered":true},
       {"id":"dongle","mac":"00:1A:7D:DA:71:13","hci":"hci1","present":true,"powered":true}],
     "tncs":[
       {"id":"d75","name":"TH-D75","model":"th-d75",
        "spp":{"address":"00:11:22:33:44:55","adapter":"builtin","paired":true},
        "serial":{"path":"/dev/serial/by-id/usb-JVC_KENWOOD_TH-D75_XXXXXXXX-if00","present":false},
        "in_use_by":"ktbr0","source":"static:/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml"}],
     "bridges":[{
       "id":"ktbr0","name":"TH-D75 portable","enabled":true,
       "public_name":{"configured":"TH-D75 portable","effective":"TH-D75 portable"},
       "source":"static:/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml","editable":false,
       "ports":[
         {"id":"radio","type":"spp-client","role":"tnc","tnc":"d75",
          "state":"connected","since":"2026-10-12T14:02:11Z"},
         {"id":"phones","type":"ble-server","role":"app","adapter":"dongle","advertising":false,
          "sessions":[{"id":"7C:2A:…","since":"2026-10-12T14:02:09Z"}]},
         {"id":"lan","type":"tcp-server","role":"app","listen":"wlan0:8001","state":"listening",
          "announced":true,"sessions":[]}],
       "counters":{"frames_up":12,"frames_down":340,"dropped":0}}]}}

→ {"v":1,"id":2,"cmd":"subscribe","topics":["state","activity","pairing"]}
← {"v":1,"event":"activity","port":"ktbr0/radio","dir":"rx","bytes":73}
← {"v":1,"event":"pairing.request_passkey","request":7,"tnc":"d75"}
→ {"v":1,"id":3,"cmd":"pairing.reply","request":7,"passkey":"0000"}
```

| Command | Purpose |
|---|---|
| `status` | Adapters, TNCs, bridges, ports, sessions, states, counters |
| `subscribe` | Event topics: `state`, `activity` (coalesced, ≤10/s per port), `frames` (full frames with decoded AX.25 header), `pairing`, `scan` |
| `bridge.create` / `bridge.update` / `bridge.delete` / `bridge.clone` | Dynamic bridges only. `clone` copies any bridge, static or dynamic, into a new dynamic one |
| `bridge.enable` / `bridge.disable` / `bridge.switch` | Any bridge; on static bridges this sets an override in `state.toml` |
| `bridge.retry_public_name` | Any bridge; drops a DNS-SD collision adjustment from `state.toml` |
| `port.add` / `port.update` / `port.remove` | Ports on dynamic bridges only |
| `tnc.create` / `tnc.update` / `tnc.delete` | Dynamic TNCs only; delete is refused while referenced |
| `adapter.create` / `adapter.update` / `adapter.delete` | Dynamic adapter objects only; delete is refused while referenced |
| `port.connect` / `port.disconnect` | Force a TNC port up or down |
| `session.kick` | Disconnect one session |
| `scan.start` / `scan.stop` | Discovery with `transport: spp \| ble \| auto`. Emits `scan.device` events and flags likely TNCs (SPP UUID, `TH-D7*` names) |
| `serial.list` | Serial devices present, by stable path, with USB vendor/product names, flagging known TNCs |
| `pair` / `unpair` | Pairing window, by TNC id (`d75`) or by address + adapter |
| `config.check` | Validate the on-disk config, or a candidate passed in (as `ktbctl edit` does), and return the errors or the reload plan with its `disruptive` flag and reasons, without applying anything |
| `config.get` | The TOML for one dynamic object, or all of `dynamic.toml`, plus a revision hash. `ktbctl` users only have socket access, so this is how `edit` (and later `export`) reads config |
| `config.put` | Replace that TOML if the revision still matches (otherwise `revision_conflict`), going through the same check, reconcile and consent rules as any change |
| `reload` | Re-read config and reconcile. Returns the change report. With `non_disruptive: true`, refuses with `reload_would_disrupt` and applies nothing if the plan is disruptive (`ktbctl` sets this unless it has confirmation). `SIGHUP` is equivalent to `reload` without the flag |

### Reconciler

The adapter supervisor becomes a **reconciler**. It holds the desired set of
bridges (static + dynamic + enable overrides) and the running set, and on any
change it starts, stops or restarts only the bridges and ports that differ.

**Adapters are tracked per port, not per bridge.** Today the supervisor groups
TNCs by adapter and tears down everything on an adapter when it goes away. In
the new model, losing a `ble-server` port's adapter stops only that port,
and the bridge's TNC port and TCP sessions keep running. Losing a TNC's
adapter puts the `spp-client` port into `Backoff` while app sessions stay
connected. When the adapter returns, only the affected ports restart.
Adapter hot-plug and power on/off detection (today's supervisor loop) stays as
it is and feeds the reconciler.

### Reload semantics

A reload (`ktbctl reload`, `SIGHUP`, or `systemctl reload`) never touches
anything that hasn't changed, so it's safe to run at any time: from a
config-management run every 30 minutes, or out of habit. Whether a
*disruptive* change gets applied depends on how the reload was invoked
(rules 9 and 10). The rules:

1. **An unchanged config is a no-op.** Nothing is touched: no TCP sockets
   closed, no serial devices reopened, no BLE re-registration. The daemon logs
   "no changes".
2. **The comparison uses the resolved config, not the file text.** It
   happens after defaults are filled in and references resolved. Reformatting,
   editing comments, moving a bridge from the main file into `conf.d/`,
   renaming an `[[adapter]]` id while references still resolve to the same
   MAC, or setting `[global] adapter` to the adapter already in use by default
   change nothing.
3. **Ports are compared individually.** A port's resolved settings include
   whatever it references, so editing the D75's `[tnc.spp]` is a change to
   every port that uses TNC `d75`. A changed port is handled on its own; its
   sibling ports and their sessions are untouched.
4. **Changes are applied live where possible, and restart the port only when
   they change the link itself:**

   | Change | Effect |
   |---|---|
   | `log_level` | Applied live |
   | TNC `name`; bridge `name` when `public_name` is set | Display only |
   | `public_name` (or bridge `name` when it's the default) | BLE advertisement restarts with the new name, the adapter alias follows, and DNS-SD re-announces. Connected phones and TCP sessions stay connected, because the GATT service doesn't change. Any collision adjustment in `state.toml` is cleared |
   | `allow` | Applied live. **Existing sessions that are no longer allowed are disconnected**, because it's a security setting |
   | `max_clients` | Applied live. If it drops below the current session count, **the newest sessions are disconnected** until the port is within the limit |
   | `announce` | DNS-SD only |
   | `connect`, `idle_timeout` | Applied live. `on_demand` → `always` connects now; `always` → `on_demand` follows `idle_timeout` |
   | `app_params` | Applied from the next frame |
   | Anything that identifies the link: TNC `spp.address`, `spp.adapter`, `channel`, `serial.path`, `baud`; `tcp-client` `address`; `tcp-server` `listen`; `ble-server` `adapter`; port `type` or `role` | That port restarts |
   | TNC `model` | The TNC port reconnects, so the new KISS init sequence runs |
   | A port id or bridge id | Treated as removing one object and adding another |
   | `enabled` | The whole bridge starts or stops |

5. **While a TNC port restarts, app sessions stay connected.** The TNC port
   goes back to `Connecting` and uplink frames queue, the same as for any
   reconnect.
6. **Stops happen before starts.** Shared resources (a TNC, a listen socket, a
   public name) are released before another bridge claims them, so "disable
   `ktbr0`, enable `ktbr3`" works in a single reload. `switch` relies on the
   same ordering.
7. **Reloads and API changes are the same operation.** `ktbctl port add`,
   `tnc update` and the rest reconcile just the objects they touched, under
   the same rules. Enable overrides and collision adjustments in `state.toml`
   survive a reload, except where a rule above clears them.
8. **The response says what happened.** Each bridge and port is reported as
   unchanged, updated in place, restarted, started or stopped, with any
   disconnected sessions listed. `ktbctl check` prints the same plan without
   applying anything.
9. **`ktbctl reload` never applies a disruptive plan without consent.**
   Like `terraform apply` or `apt-get`, it refuses when confirmation is
   needed and nobody can give it:

   | Invocation | Plan not disruptive | Plan disruptive |
   |---|---|---|
   | `ktbctl reload` in a terminal | Applies | Shows the plan and asks (e.g. "This will disconnect 2 sessions. Continue?") |
   | `ktbctl reload` without a terminal | Applies | **Refuses**: applies nothing, prints the plan, exits 3 |
   | `ktbctl reload --yes` | Applies | Applies |
   | `ktbctl reload --non-disruptive` | Applies | Refuses and exits 3, even in a terminal, without asking |

   So scripts, cron jobs and config-management runs are safe by default: they
   can't disconnect someone mid-QSO unless their author wrote `--yes` on
   purpose. `--non-disruptive` is the explicit form of that default, and in a
   terminal it means "don't ask, just refuse".

   **The same rule applies to every `ktbctl` command that changes config or
   state** (`enable`, `disable`, `switch`, `bridge rm`, `port add`/`rm`,
   `tnc` and `adapter` changes, `edit`): they all go through the same
   reconcile, and if the resulting plan is disruptive, they ask, refuse, or
   honor `--yes` / `--non-disruptive` exactly as `reload` does. The
   exceptions are commands whose whole purpose is to disrupt, `kick` and
   `disconnect`, which act immediately.
10. **Refusals are all-or-nothing and race-free.** The daemon computes the
    plan and applies it in one step, so there's no gap in which a phone could
    connect between a check and a reload. A refused reload applies
    **nothing**: a partial reload would leave a half-applied config that's
    hard to reason about. Exit codes: 0 applied (or nothing to do), 1
    invalid config, 2 couldn't reach the daemon, 3 refused as disruptive. In
    the API, `reload` takes `non_disruptive: true` and refuses with error
    code `reload_would_disrupt`. `ktbctl` sets it whenever it can't get
    confirmation, then retries without it if the user says yes. The plan it
    shows is the one the daemon returned with the refusal.
    **`SIGHUP` and `systemctl reload` are the exception: they always apply
    everything.** A signal has no way to report a refusal, and admins expect
    `systemctl reload` to apply the config. The docs present `ktbctl reload`
    as the careful path and `systemctl reload` as the forceful one.
11. **Restarting the daemon is different.** `systemctl restart` drops
    everything, by definition. `systemctl reload` (the unit's `ExecReload=`
    sends `SIGHUP`) only touches what changed, but applies disruptive changes
    without asking. `ktbctl reload` is the one that refuses or asks. The docs
    recommend `ktbctl reload`, then `systemctl reload`, and `restart` only
    when needed, such as after an upgrade.

Disconnecting a BLE session means disconnecting the device at the link layer,
as 0.5 already does for rejected clients. A disconnected phone that tries to
reconnect is refused while the port is at its limit, and advertising stops
while the port is full, so it won't keep seeing the bridge.

**What counts as disruptive.** The same definition drives `ktbctl reload`'s
confirmation prompt, `--non-disruptive`, and the `disruptive` field from
`ktbctl check`:

| Disruptive | Not disruptive |
|---|---|
| Any session disconnected: `allow` tightened, `max_clients` lowered below the session count, an app port removed or restarted, a bridge stopped | Starting bridges or ports |
| Any TNC port restart (a link-identity change or a `model` change). App sessions stay connected, but traffic stops while the link reconnects and the TNC re-runs KISS init | Live updates: `log_level`, `connect`, `idle_timeout`, `announce`, `app_params`, raising `max_clients`, loosening `allow` |
| | A `public_name` change: the BLE advertisement and DNS-SD announcement restart, but connected phones and TCP sessions stay connected |
| | Display-only changes such as TNC `name` |

Stopping a bridge, or restarting a TNC port, counts as disruptive only while
something is in use: its TNC link is up, or apps are connected. Restarting
or stopping things nobody is using isn't, so `switch` between idle bridges
needs no confirmation. Any disconnected session is always disruptive.

### `ktbctl`: CLI

Modeled on `ip link` and `systemctl`. Human output by default, `--json`
everywhere for scripts. It talks only to the control socket, never to BlueZ
directly.

```
ktbctl status [BRIDGE]
ktbctl setup                                   # interactive wizard (below)

ktbctl adapter list                            # hardware present + adapter objects
ktbctl adapter add ID --mac MAC | adapter rm ID
ktbctl tnc list
ktbctl tnc add ID --model th-d75 [--name NAME]
        [--spp-address ADDR --adapter ADAPTER] [--serial PATH [--baud N]]
ktbctl tnc rm TNC
ktbctl scan [--spp|--ble] [--adapter ADAPTER] [--timeout 15s]
ktbctl serial list                             # stable paths, flags known TNCs
ktbctl pair TNC | unpair TNC

ktbctl bridge add [ID] --name NAME             # dynamic; id defaults to next free ktbrN
ktbctl bridge clone BRIDGE [NEW_ID] --name NAME
ktbctl bridge rm BRIDGE
ktbctl bridge name BRIDGE NAME                 # dynamic bridges only
ktbctl bridge public-name BRIDGE               # show configured and effective public name
ktbctl bridge public-name BRIDGE NAME          # dynamic bridges only
ktbctl bridge public-name BRIDGE --default     # dynamic: fall back to the bridge name
ktbctl bridge public-name BRIDGE --retry       # any bridge: drop a DNS-SD collision adjustment
ktbctl enable BRIDGE | disable BRIDGE | switch BRIDGE
ktbctl port add BRIDGE TYPE [--id ID] [type options]
    e.g. ktbctl port add ktbr0 spp-client --tnc d75 --idle-timeout 90s
         ktbctl port add ktbr3 serial --tnc d75
         ktbctl port add ktbr0 ble-server --adapter dongle --max-clients 2
         ktbctl port add ktbr0 tcp-server --listen wlan0:8001
         ktbctl port add ktbr1 tcp-client --address 127.0.0.1:6700
ktbctl port rm BRIDGE/PORT
ktbctl connect BRIDGE/PORT | disconnect BRIDGE/PORT
ktbctl kick BRIDGE/PORT/SESSION

ktbctl edit [BRIDGE | tnc TNC | adapter ADAPTER]   # dynamic objects only
ktbctl monitor [BRIDGE]                        # live frames: W1AW-9>APDR16,WIDE1-1: ...
ktbctl check                                   # validate + show what a reload would do
ktbctl reload [--yes | --non-disruptive]       # confirms disruptive changes in a terminal
ktbctl tui                                     # also: ktbctl with no arguments
```

Bridges and TNCs can be referred to by id or by name.

**`setup` wizard** (the B.B. Link Configurator equivalent):

1. Ask how the TNC is connected: Bluetooth or USB/serial.
2. **Bluetooth:**
   1. Tell the user to put the radio in Bluetooth pairing mode and turn on
      KISS (with menu paths for the D74 and D75).
   2. Pick the adapter for the TNC from the adapters present (listed by
      `hciN`, MAC, and whether they're built-in or USB). Create an
      `[[adapter]]` object for it if none exists, offering `builtin` or
      `dongle` as the id.
   3. Scan for Classic devices on that adapter, listing likely TNCs first.
   4. Pair, relaying any PIN or confirm prompt.
3. **USB/serial:** tell the user to connect the cable and turn on KISS (with
   the D75's USB menu settings), then list serial devices by stable path,
   with known TNCs first. Pick one and confirm the baud rate for non-USB
   devices.
4. Create a dynamic `[[tnc]]`, or add the new section to an existing one if
   the same TNC already exists (e.g. adding USB to a D75 already set up for
   Bluetooth).
5. Ask for a bridge name (the default is the TNC's name), whether to share
   over BLE (yes) and on which adapter, and whether to share over TCP (no, or
   which interface or address and TCP port number, with the exposure
   warning).
6. Create a dynamic bridge. If another enabled bridge conflicts with it, offer
   `switch`.
7. Connect, then wait for the first received frame or 30 s of silence, and
   report the result.

If the TNC already exists with the needed section (and is paired, for
Bluetooth), the wizard skips straight to step 5.

### `ktbctl tui`: full-screen TUI

Built with `ratatui`, in the same binary as the CLI (the same relationship as
`nmtui` and `nmcli`).

```
┌ kiss-tnc-bridge ─ pi-zero ─────────────────────────────────────────────────┐
│ ktbr0  TH-D75 portable                                         ● enabled   │
│   radio   spp-client      d75 (TH-D75)       connected 2h14m   ■ rx ■ tx   │
│   phones  ble-server      dongle "TH-D75"    1/2 sessions      ■ rx ■ tx   │
│             └ 7C:2A:…  iPhone                                  ■ rx ■ tx   │
│   lan     tcp-server      wlan0:8001         0/4 sessions                  │
│ ktbr1  Graywolf at home                                        ○ disabled  │
│ ktbr2  Shack relay                                             ○ disabled  │
├────────────────────────────────────────────────────────────────────────────┤
│ tncs     d75 TH-D75  spp: paired on builtin  serial: not present           │
│ adapters builtin hci0 B8:27:EB:… on · dongle hci1 00:1A:7D:… on            │
├────────────────────────────────────────────────────────────────────────────┤
│ 14:22:07 rx ktbr0/radio  W1AW-9>APDR16,WIDE1-1:!4237.14N/07120.83W>        │
│ 14:22:09 tx ktbr0/phones KD2ABC>APRS,WIDE1-1::W1AW-9   :hello{12           │
├────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ select  e enable  d disable  s switch  p pair  a add  m monitor  q quit │
└────────────────────────────────────────────────────────────────────────────┘
```

- A tree of bridges → ports → sessions, with state, adapter, and uptime, and
  a strip showing TNCs (Bluetooth paired or not, serial device present or
  not) and adapters (present, powered).
- Activity squares per port and per session that flash on rx and tx in
  different colors, driven by `activity` events.
- A live frame pane at the bottom, from `frames` events.
- Actions: enable, disable, switch, connect, disconnect, kick, pair (with the
  pairing prompts in a dialog), and add or remove dynamic bridges, ports,
  TNCs and adapters. Static objects are shown read-only, labeled with their
  source file.
- It has to work over SSH and at 80×24, because the main use is a headless
  Pi.

### `ktb-gateway` (1.x)

Translates the control protocol to remote transports, each a cargo feature
and each off unless configured:

- **HTTP + WebSocket:** serves a web UI as embedded static files, plus the
  protocol over WebSocket.
- **LAN TCP + mDNS:** the protocol over TCP for phone apps on the LAN,
  announced through Avahi the same way the daemon announces `tcp-server`
  ports, but under its own service type (e.g. `_ktb-mgmt._tcp`). It must
  never use `_kiss-tnc._tcp`, or KISS apps would try to speak KISS to it.
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
    mod.rs                 bridge task: TNC port state machine, session registry, routing,
                           counters, activity events
    policy.rs              KISS command filtering
  port/
    mod.rs                 port types, roles, session ids
    stream.rs              KissConnection<S: AsyncRead + AsyncWrite> (from bridge/tcp.rs)
    tcp_client.rs
    tcp_server.rs
    spp_client.rs          RFCOMM via a BlueZ SPP profile, linux only
    serial.rs              tokio-serial, TIOCEXCL, path polling
    ble_server.rs          GATT peripheral + advertising (from bridge/manager.rs), linux only
    tnc_init.rs            KISS init by TNC model: th-d74, th-d75, none
    listen.rs              tcp-server binding: address, interface (SO_BINDTODEVICE), allow
  announce.rs              DNS-SD via Avahi D-Bus: entry groups, collisions, per-interface
  config/
    model.rs               serde types: global, adapters, TNCs, bridges, ports
    load.rs                main file + conf.d + dynamic.toml + overrides → effective set
    resolve.rs             adapter values (id, MAC, hciN) and TNC references
    validate.rs            shared by -t, reload, and config.check
    store.rs               dynamic.toml: toml_edit, atomic writes, hash check
    state.rs               state.toml: enable overrides, collision-adjusted names
  control/
    protocol.rs            serde types + JSON Schema export
    server.rs              Unix socket server
  supervisor.rs            adapter lifecycle (by name or MAC) + reconciler
  agent.rs                 pairing window for radios (ktbctl pair)
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

Each phase ends in a milestone on `main`, not a release. **1.0.0** is the
next release, and it marks the config schema, control protocol v1, and
`ktbctl` as stable.

### No releases until 1.0

Once Phase 1 work starts landing on `main`, there can't be another GitHub
release until 1.0 is done:

- Phase 1 replaces the 0.5 INI config with the new TOML layout, so anything
  released from `main` in between would break every existing install without
  yet offering the features that make the switch worthwhile.
- The schema, control protocol and CLI keep changing until Phase 5. A release
  in the middle would turn a half-finished design into something users depend
  on.
- The release workflow builds and publishes any pushed `v*` tag, so the rule
  is simple: **no version tags on `main` until `v1.0.0`.** The version in
  `Cargo.toml` stays at the last 0.5 release until `cargo release major`
  bumps it to 1.0.0.

If 0.5 users need a fix in the meantime (for example
[kotfu/kiss-tnc-bridge#2](https://github.com/kotfu/kiss-tnc-bridge/issues/2)),
it goes on a `0.5.x` maintenance branch cut from the `v0.5.1` tag, released
from there as 0.5.2 and so on, and merged or re-applied to `main` if it still
applies. Testers who want to try 1.0 work early build from `main` (see
`CONTRIBUTING.md`).

### Phase 0: Spikes (before committing)

Use throwaway code on a branch to answer the questions that could sink the
plan:

- [x] TH-D75 pairs with BlueZ (`bluetoothctl`) on a Pi 4, and on an RTL8761B
      dongle. Record exactly which agent callbacks fire (PIN? passkey? Just
      Works?).
- [x] A bluer `register_profile` + `connect_profile(SPP)` program connects and
      reads KISS frames from the radio.
- [x] Same controller: run today's BLE GATT bridge and the RFCOMM link
      concurrently for 1 hour with aprs.fi connected. Watch for drops.
- [x] Repeat with the radio and BLE on different adapters (built-in +
      RTL8761B dongle), to confirm that fallback works.
- [x] Read the bb-link source (both forks): KISS init commands, reconnect
      behavior, D74 vs D75 differences, license.
- [x] Measure RFCOMM connect time and behavior when the radio is off or out of
      range (page timeout).
- [x] Plug the D75 into a Pi over USB: what `lsusb` and `/dev/serial/by-id/`
      show (it may expose more than one interface), which interface carries
      the TNC, and the radio's USB menu settings for KISS.
- [x] Whether KISS over USB uses the same init commands as over Bluetooth.
- [x] Whether the D75 allows its TNC on Bluetooth and USB at the same time,
      or only one. This confirms or relaxes the one-bridge-per-TNC rule.

**Exit:** a short findings section appended to this doc. If pairing or
coexistence is a dead end on Pi hardware, stop here.
Findings: [Phase 0 findings](#phase-0-findings).

### Phase 1: Bridge core + new config

- [x] Extract `KissConnection<S>` from `TcpKissConnection`.
- [x] Bridge task with port roles, forwarding rules, KISS policy filter, and
      the non-blocking TNC port state machine. This fixes
      [kotfu/kiss-tnc-bridge#2](https://github.com/kotfu/kiss-tnc-bridge/issues/2)
      if it hasn't already been fixed on the `0.5.x` branch.
- [x] Port types `ble-server` (moved out of `BridgeManager`, one GATT app
      per port, advertising the bridge's `public_name`, setting the adapter
      alias), `tcp-client`, and `tcp-server` (localhost default, address and
      interface binding, `allow`).
- [x] TOML config: main file + `conf.d/`, bridges with ids, names and
      `enabled`, inline ports, `[[adapter]]` objects and adapter values (id,
      MAC, `hciN`), `public_name`, `[control] group`, `-t` validation with
      reference checks and conflict checks among enabled bridges (including
      one `ble-server` per adapter), and the 0.5 config warning.
- [x] systemd unit: `ConfigurationDirectory=`, `StateDirectory=`,
      `RuntimeDirectory=`, `ProtectSystem=strict`, and moving the lock file to
      the runtime dir. Package the new example config. No `ExecReload=` yet:
      `SIGHUP` would terminate the daemon until Phase 3 implements reload.
- [x] Bridge unit tests with `tokio::io::duplex` TNCs and fake app ports.
      This is the first time routing is testable without BlueZ.
- [x] End-to-end CI test with no Bluetooth: fake TNC server → `tcp-client` →
      bridge → `tcp-server` → several clients, checking broadcast, uplink
      serialization, and `C0 FF C0` filtering.
- [x] Add a PR CI workflow (`cargo test`, `clippy`, `fmt --check`). Today only
      the release workflow exists.

Done October 13, 2026 on the `phase1-bridge-core` branch. Verified on the
dev Pi with a synthetic KISS TNC behind a `tcp-client` port: a `tcp-server`
port broadcasting to two clients with uplink and KISS-return filtering, and a
`ble-server` port on the RTL8761B dongle with aprs.fi on an iPad receiving
packets and sending beacons through the serialized uplink.

### Phase 2: TNCs over Bluetooth and serial

- [x] `[[tnc]]` objects with `spp` and `serial` sections, and the
      one-enabled-bridge-per-TNC rule.
- [x] `spp-client` using the profile approach, with the `Unpaired` state.
      Pairing is still manual (`bluetoothctl`) in this release.
- [x] `serial` port: `tokio-serial`, `TIOCEXCL`, `Waiting for device` with
      path polling, stable-path warnings.
- [x] Packaged udev rule telling ModemManager to ignore known Kenwood devices.
- [x] `tnc_init` for th-d74/th-d75 models, `idle_timeout`,
      `connect = "always"`.
- [x] Per-port adapter tracking in the supervisor.
- [x] `-t` warnings for unpaired TNCs and missing serial devices.
- [x] `state.toml` store (atomic writes), first used for DNS-SD collision
      names.
- [x] DNS-SD announcement of `tcp-server` ports through Avahi: per-interface
      announcing, and collision renaming persisted in `state.toml` and
      applied to BLE and the adapter alias too. Checked with `avahi-browse`
      and aprs.fi on iOS.
- [x] README: TH-D75 walkthroughs for Bluetooth and USB, hardware notes, and
      the TCP exposure warning.
- [ ] Manual hardware test matrix (below). Pi 4 rows done; the rest
      deferred for lack of hardware (see below).

Code done October 13, 2026 on the `phase2-tncs` branch. Checked on the dev
Pi without the radio: the unit tests natively (including a pty round trip
for `serial`), `-t` against the live BlueZ (the D75 paired on hci0, not on
hci1), and DNS-SD with `avahi-browse`: a local name collision moved the
bridge to "… (2)", saved in `state.toml` and kept across a restart, and an
interface binding announced only on that interface.

First hardware round, October 13, 2026, on the dev Pi 4 with the TH-D75,
aprs.fi on an iPad over BLE (the RTL8761B dongle) and on an iPhone over
TCP (`wlan0:8001` with an `allow` list):

- **SPP** (radio on the built-in chip): connected on the first app in
  2.7 s; KISS init took 85 ms from APRS mode. Packets reached both apps,
  and a beacon from each keyed the radio. aprs.fi found the bridge by
  DNS-SD on its own. A radio power-cycle backed off (1–30 s, each attempt
  failing cleanly at the 5 s page timeout), reconnected, and re-ran init
  with the apps still attached; the radio comes back up in APRS mode, so
  init on every connect is needed. `idle_timeout = "90s"` closed the link
  90 s after the last app left.
- **USB**: waited for the device, then connected with `connect = "always"`
  and no apps. Packets and beacons on both apps; a second open of the port
  failed with "Device or resource busy". After a cable replug the D75
  answers nothing on its serial port until the port is reopened; the
  bridge gives up after two silent probes and reconnects on the next
  attempt, 6 s after the replug.
- `-t` rejects the D75 in two enabled bridges, and two `ble-server` ports
  on one adapter.
- Bugs found and fixed: the daemon ignored SIGTERM once its stderr's reader
  was gone (the signal task panicked logging through tracing-subscriber's
  `eprintln!`); tokio-serial's flush is a blocking `tcdrain()`, which hung
  KISS init on the deaf radio with no timeout able to fire; and closing a
  port with unsent output blocked for the tty's 30 s closing wait.
- **Stopping the daemon**: apps over TCP notice at once, even after
  SIGKILL. Over BLE, BlueZ keeps a phone's link up when the daemon's GATT
  service goes away, so aprs.fi stayed "Connected" to nothing. A stopping
  `ble-server` port now drops its phones' links (aprs.fi shows "Searching"
  within a second or two and reconnects about a second after a restart).
  After a SIGKILL the restarted daemon drops the links of the phones the
  old one served, listed in the runtime directory (the unit now has
  `RuntimeDirectoryPreserve=restart`), and the iPad reconnected on its own.
- **Phone pairing never worked, in 0.5 either.** A Bluetooth trace showed
  BlueZ reading a protected attribute on the iPad, the iPad starting a
  Just Works pairing, and bluetoothd asking the default agent's
  `RequestAuthorization`, which bluer's callback-less agent rejects. So
  phones prompted on every connection, stalled 10–15 s while retrying,
  and showed up under a new private address each time. The daemon now
  registers its own `NoInputNoOutput` agent over D-Bus (bluer would make an
  authorization callback `DisplayYesNo`), authorizing LE devices with
  random addresses only. The iPad bonded with one prompt and reconnects
  without one, across daemon restarts.

- **Scenarios**: a cold start with the radio off retried at the 5 s page
  timeout with backoff and connected when it was turned on. `C0 FF C0` from
  a TCP app was dropped and the radio stayed in KISS. With
  `exit_kiss_on_disconnect = true` the radio went from APRS to KISS at
  connect and back to APRS at shutdown. A TNC not paired on its adapter
  logged one warning and never paged the radio.
- **Bonded phones keep their link** after the app lets go, so a session
  now also ends when the phone unsubscribes; otherwise the port stayed at
  `max_clients` with its advertisement withdrawn.
- **KISS (983) on the wrong interface**: the radio accepts `TN 2` on any
  interface but goes KISS only on the one menu 983 names; the Bluetooth
  link stayed a command port and carried nothing for 52 minutes. Init now
  probes once more after `TN 2` and fails the connect with a message naming
  menu 983 if the radio still answers.

Deferred for lack of hardware: the matrix rows for a Pi Zero 2 W (out of
stock), a Pi 1, x86_64 with a TH-D74, a standalone serial TNC, Pat, and a
Pi as the Wi-Fi AP. Phase 5's soak test picks up the Pi Zero 2 W when one
can be had. Notes from the work:

- `model` resolves to one TH-D7x profile; `th-d74` and `th-d75` are
  aliases, as Phase 0 suggested. The value for TNCs that need nothing
  special is `generic` (the default), not `none`, which read like "no TNC".
- Init keeps any KISS frames that arrive while it probes and hands them to
  the bridge, rather than dropping them.
- Teardown (`exit_kiss_on_disconnect`) sends `C0 FF C0`, then `TN 1,<band>`
  if the TNC was in APRS mode when init found it, since leaving KISS turns
  the TNC off. It runs only when the bridge lets go on purpose (idle,
  shutdown), not when the link is lost; shutdown waits for it.
- The daemon registers one SPP client profile and routes BlueZ's connect
  requests to the waiting port by device address.
- An `spp-client` port powers its adapter on at its first connect if it
  needs to, and after that treats a powered-off adapter as deliberate and
  backs off, the same restraint the `ble-server` supervisor shows.
- serialport opens exclusively by default (`TIOCEXCL` plus `flock`), and it
  reports `EBUSY` as "no device"; the port reports that as "in use by
  another program" instead of waiting for a device that's there.
- The udev rule matches only the TH-D75 (`2166:9023`), the one USB id
  confirmed so far.
- Release workflow: the tarball step still copied the 0.5
  `kiss-tnc-bridge.example.conf`; fixed to the `.toml` and the udev rule.

At this point the core value is shipped: a Pi replaces a B.B. Link, shares
the D75 over BLE and TCP by Bluetooth or USB cable, and works with standalone
serial TNCs, all configured by hand.

### Phase 3: Control API, dynamic config, reconciler

- [x] `control::server`, protocol types, JSON Schema export, `subscribe` with
      `state`, `activity` and `frames` events, and counters.
- [x] Reconciler implementing the [reload semantics](#reload-semantics):
      resolved-config diff per port, live updates, port restarts, stop-before-start
      ordering, session kicks for `allow` and `max_clients`, the change report
      with its `disruptive` classification, and `non_disruptive` reloads.
      Unit tests for each row of the table, including "no change means no
      reconnect", and for the disruptive/not-disruptive split.
- [x] `SIGHUP` handler, and `ExecReload=` in the systemd unit.
- [x] Dynamic `dynamic.toml` store (toml_edit, atomic writes, hash check), and
      enable overrides added to `state.toml`.
- [x] `bridge.*`, `port.*`, `tnc.*`, `adapter.*`, `serial.list`,
      `config.check`, `config.get` and `config.put` commands, with static
      objects read-only, reference-integrity checks, revision checks, and
      `bridge.clone`.
- [x] `docs/control-api.md`.

Dynamic config and the reconciler landed first, on the `phase3-control`
branch, October 16, 2026; the control socket and its commands follow in a
second PR. Notes:

- The supervisor became the running daemon: it changes running bridges
  only by applying a plan from `reconcile::plan`, in three passes (stops,
  live updates, starts). Startup is a plan from nothing running, and
  Bluetooth is set up the first time a port needs it, so a reload can add
  the first BLE or SPP port.
- `exit_kiss_on_disconnect` applies live: init always works out how to
  leave KISS mode, and the bridge sends it only if the setting is on when
  it lets go.
- A reload logs only config warnings it hasn't logged before, and a
  refused TCP client is logged once a minute per address (aprs.fi retries
  every 5 s).
- Checked on the dev Pi with the D75 over SPP, an iPad over BLE and an
  iPhone over TCP, reloading with `SIGHUP`: a `public_name` change
  re-advertised and re-announced with nobody disconnected; removing the
  `ble-server` port dropped the iPad and adding it back let it reconnect
  without a prompt; two TNC restarts (`model` changes) kept both apps
  connected; tightening `allow` kicked the iPhone and refused its retries;
  an invalid config was rejected whole.

The control socket and its commands followed on `phase3-control-api`.
Notes:

- In a request, `id` is the request's own; objects are named by their
  kind (`bridge`, `port`, `tnc`, `adapter`, `session`, `object`) and new
  definitions go under `definition`. The first draft flattened an object
  `id` next to the request's, and they collided.
- At first a bridge had to be complete even when disabled, so
  `bridge.create` needed the whole definition, ports included. Phase 4
  dropped that: a disabled bridge may be incomplete, new bridges start
  disabled, and `bridge.enable` checks completeness, so `ktbctl` can add a
  bridge, add its ports, then enable it.
- Every change edits a copy of `dynamic.toml`, rebuilds from the static
  files as last read plus the copy, validates, plans, honours
  `non_disruptive`, then saves and applies, so changes touch only what
  they edit, and a refusal leaves nothing half done.
- `port.connect`/`disconnect` hold the TNC link up or down until the
  other is sent.
- The JSON Schema is generated by a test behind the `schema` feature
  (`docs/control-api.schema.json`), which CI checks is current; the daemon
  binary doesn't carry `schemars`.

The hardware check of the control API turned up BLE problems, all fixed on
the same branch:

- **Phones no longer pair.** Kicking a bonded iPad left it "connected" and
  deaf: BlueZ keeps a bonded device's CCCD after the link drops, so its
  resubscribe never reached us (no `AcquireNotify`). iOS also holds a
  bonded accessory's link after the app lets go, and a phone that
  subscribed before its first pairing got no notifications at all (BlueZ
  files the CCC state under the private address, then looks it up by the
  identity address). All of it came from bonding, and bonding came from
  bluetoothd itself: it reads a connecting phone's Battery Level, iOS
  answers Insufficient Authentication, and BlueZ asks for security. The
  bridge needs none, so the Phase 2 pairing agent is gone (git 662d2e2 has
  it) and the README tells people to set `ReverseServiceDiscovery = false`
  (or `[GATT] Client = false`, or run bluetoothd with `-P battery`); `-t`
  and the daemon warn when none is set. Phones then connect with no
  prompt.
- **Two phones at once.** A connection ends the advertising set and the
  kernel re-enables it only when that phone disconnects (Linux 6.18,
  `hci_le_ext_adv_term_evt`), so a second phone could never find the
  bridge. 0.5.1 seemed to serve two because failed pairings dropped each
  phone every ~30 s and they took turns. The port now registers a new
  advertisement when a phone connects below `max_clients`; the RTL8761B
  runs it while connected, and two phones were served at once.
- **Idle links.** iOS keeps the link after the app disconnects, bonded or
  not, which keeps the controller from advertising. The port watches the
  links of phones it has served and drops one that's connected with no
  session (checked every 10 s).
- **Open: BlueZ ends another phone's subscription.** When a phone's CCC
  state is discarded (every disconnect of an unbonded phone),
  `clear_ccc_state` calls `ccc_write_cb(NULL)`, and `match_client_att`
  matches any client for a NULL ATT, so BlueZ closes some other phone's
  notify fd. With two phones, one leaving cuts the other off until its
  app reconnects (the idle-link drop gets it back within ~10 s). Jerry
  Wu's fix had been posted in August and dropped unreviewed; it fixed
  this on the dev Pi (patched 5.82 bluetoothd, two phones, kicks and
  Bluetooth off), and we resent it with a `Tested-by`:
  <https://lore.kernel.org/linux-bluetooth/20261026032544.49563-1-jared@kotfu.net/T/#u>.
  No workaround in the daemon; the README lists it under Known bugs.

### Phase 4: `ktbctl` CLI, pairing, TUI

Three PRs: the CLI (`phase4-ktbctl-cli`); pairing, `scan` and `setup`; then
the TUI.

- [x] Disabled bridges may be incomplete, and new bridges start disabled, so
      `ktbctl bridge add` takes just a name: add the bridge, add its ports,
      enable it. `status` lists what an incomplete bridge still needs.
- [x] The CLI commands above (all but `scan`, `pair`, `unpair` and `setup`,
      which come with pairing), plus `--json`. `ktbctl` with no command
      shows `status` until the TUI exists.
- [x] `ktbctl tnc models`, through a new `tnc.models` control command
      serving the daemon's `MODELS` table.
- [x] `ktbctl check`; `ktbctl edit` with `config.get`/`config.put`, the
      daemon's check on save, and reopening the editor on errors; the
      consent rule (confirm in a terminal, refuse without one, `--yes`,
      `--non-disruptive`) on `reload` and every other command that changes
      config. `connect`, `disconnect` and `kick` exist to disrupt and act at
      once.
- [x] `monitor`, with the AX.25 header decode the daemon already sends;
      it waits and resubscribes when the daemon restarts.
- [x] `ktbctl` in the deb, rpm and tarball, and in `scripts/pi-deploy.sh`.
- [x] Pairing for radios: `bluetooth.scan`, `bluetooth.pair` (an agent
      registered only for the pairing, on the daemon's own D-Bus connection,
      which BlueZ uses for that `Pair` call; steps relayed to the requesting
      connection as `pairing` events, answered with `bluetooth.pair_reply`)
      and `bluetooth.unpair`; `ktbctl scan`, `pair`, `unpair`. For this a
      connection may have several requests in flight. Tested on the Pi:
      unpairing and re-pairing the D75, which reconnected on its own.
- [ ] The `setup` wizard.
- [ ] Revisit `ktbctl status` output: on the Pi it worked but was hard to
      follow at first. Specific changes held until there's more use of it.
- [ ] `ktbctl tui`: tree view, activity squares, frame pane, actions, pairing
      dialog, works at 80×24 over SSH; it reconnects when the daemon
      restarts, as `monitor` does.

### Phase 5: Hardening → release 1.0.0

- [ ] Soak test: 24 h on a Pi Zero 2 W with a D75, two BLE clients and one TCP
      client, checking memory, reconnects, and that no frames are lost while
      the TNC is up. Repeat over USB.
- [ ] Check the daemon's footprint on a Pi 1 (armhf) still fits the README's
      "~5MB" claim.
- [ ] Stretch: `spp-server` for Android SPP apps.
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
      quirks on Linux): BLE peripheral and RFCOMM at the same time. If it falls
      short, the fallback is a USB Bluetooth dongle in one of the Flipper
      One's USB ports, with the radio and BLE ports on different adapters.
      Test that setup too, and document a recommended dongle.
- [ ] Check for conflicts with Flipper OS's own Bluetooth services (a default
      agent, or its own GATT apps).
- [ ] Power: measure the battery cost of `connect = "always"` against
      `on_demand`, and consider a configurable advertising interval.
- [ ] On-device UI for the 256×144 screen, built on the control protocol: the
      enabled bridge, TNC state, session count, last heard station, a
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

## Test matrix (manual, at each milestone from Phase 2, and before 1.0)

| Host | TNC | BLE clients | TCP clients |
|---|---|---|---|
| Pi 4 (built-in) | TH-D75 | aprs.fi iOS, RadioMail | Pat, Xastir |
| Pi Zero 2 W (built-in) | TH-D75 | aprs.fi iOS | — |
| Pi 4 | TH-D75 over USB | aprs.fi iOS | Pat |
| Pi 4 | NinoTNC or TNC-Pi over serial (if available) | aprs.fi iOS | Xastir |
| Pi 1 + RTL8761B | TH-D75 | aprs.fi iOS (1 client) | YAAC |
| Pi 4: built-in for radio, RTL8761B dongle for BLE | TH-D75 | aprs.fi iOS | — |
| x86_64 + dongle | TH-D74 (if available) | aprs.fi iOS | Dire Wolf-style client |
| Flipper One | TH-D75 | aprs.fi iOS | Pat over Ethernet |
| Pi 4 as Wi-Fi AP | TH-D75 | — | aprs.fi iOS over Wi-Fi, found by DNS-SD |

Scenarios: cold start with the radio off; radio power-cycled mid-session; radio
walked out of range and back; KISS mode turned off on the radio; adapter
unplugged; adapters re-enumerated so `hci0`/`hci1` swap (MAC-pinned config must
survive); `switch` between two bridges sharing the same radio; a
`tcp-server` bound to `wlan0` while the AP goes down and comes back; USB cable
unplugged and replugged mid-session; `switch` between the D75's Bluetooth and
USB bridges; a BLE client sends `C0 FF C0`; a client floods the uplink; a slow TCP client; a hand edit to
`dynamic.toml` while the daemon runs; two Pis on one LAN with the same public
name (collision renaming, and BLE following it); Dire Wolf and a bridge both
announcing on the same Pi.

---

## Open questions

1. **D75 pairing model** (Phase 0). This decides whether the pairing-window
   agent is needed at all.
2. **Other SPP KISS devices** besides the D74/D75. A TNC with
   `model = "generic"` should work for any SPP KISS TNC. Collect testers once
   Phase 2 lands on `main`.
3. **ICOM ID-5200:** confirm its USB TNC works as a plain `serial` KISS TNC,
   whether it needs a KISS init `model`, and whether its Bluetooth TNC is BLE
   (which would make it a `ble-client` TNC after 1.0).
4. **Flipper OS unknowns:** app SDK, Bluetooth ownership, filesystem layout,
   package format, and whether Avahi is present.

## After 1.0

- **Several TNC ports per bridge**, presented to apps as a multi-channel KISS
  TNC.
  The bridge rewrites the KISS channel in both directions: the D75 is
  channel 0 and Dire Wolf channel 1, so apps that support KISS channels can
  pick a radio.
- **`ble-client` port:** connect *to* BLE KISS TNCs and share them like any
  other TNC. That covers radios whose TNC is only on BLE, like the BTech
  UV-PRO and Vero VR-N76, probably the ICOM ID-5200's Bluetooth TNC, and
  standalone ones like the Mobilinkd TNC4. They would get a `[tnc.ble]`
  section. Worth checking early whether these radios follow the
  [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
  or their own GATT layouts, which would need per-model profiles, as
  `model` already does for KISS init.
- **Per-session transmit permissions** (receive-only apps) on top of the KISS
  policy filter.
- **`pty` port** (app role): creates a pseudo-terminal, symlinked at
  `/run/kiss-tnc-bridge/<bridge>.pty`, so apps that only speak serial KISS
  can use a bridge. That includes `kissattach`, which would put the Linux
  kernel's own AX.25 networking stack on a bridge.
- **udev-driven serial hot-plug** in place of path polling.
- **DNS-SD browsing for `tcp-client`:** `address = "dns-sd:Dire Wolf on
  raspberrypi"` resolves the instance name at each connect, so a TNC host
  changing its DHCP address doesn't break the bridge. `ktbctl setup` and the
  TUI would offer a list of discovered `_kiss-tnc._tcp` services when adding a
  `tcp-client`.
- **`pn` TXT record** listing one name per KISS channel, once bridges can have
  several TNC ports.
- **`ktbctl export`:** print a bridge, plus the TNCs and adapters it
  references, as TOML ready to drop into `conf.d/`, built on `config.get`. For example,
  `ktbctl export ktbr2 > /etc/kiss-tnc-bridge/conf.d/field.toml`, then delete
  the dynamic copy. This "promotes" a setup that was built and tuned with
  `ktbctl` into permanent static config, or into a Pi or Flipper image. It
  works because `dynamic.toml` and `conf.d/` share one schema. Objects that
  are already static are left out, or included with `--all`.
- **`ktbctl reload --when-idle`:** the daemon holds a pending reload and
  applies it once the affected ports have no sessions. For unattended setups,
  a config-management push that lands mid-QSO then takes effect after the
  operator disconnects, rather than failing (`--non-disruptive`) or
  interrupting them. It needs pending-state tracking, and `status`, `check`
  and the TUI showing that a reload is waiting and on which ports. A second
  reload replaces the pending one.
- **App-to-app echo:** an opt-in per-bridge setting that copies each app's
  transmitted frames to the other app sessions, if apps turn out to need it.
- **An LLM skill for configuring and troubleshooting.** A packaged skill
  (instructions, the config reference, worked examples) that lets an AI
  assistant set up and diagnose bridges from plain requests: "share my D75
  with my phone", "switch to the home setup", "why can't aprs.fi see the
  bridge?". Design points:
  - **Act through `ktbctl --json`, never by writing files.** Every change
    goes through the control API, so it gets the same validation, reference
    checks and static/dynamic rules as a human. `config.check` gives the
    model a propose → check → fix loop, with errors it can read and a plan
    it can explain to the user before applying.
  - **Make it small enough for on-device models.** Small models are weak at
    free-form TOML but good at filling in structured output. The JSON Schema
    already published for the control protocol can drive constrained
    generation: Apple's on-device Foundation Models framework on macOS has
    guided generation and tool calling, and llama.cpp grammars work for a
    small model on a Pi 5 or the Flipper One (whose RK3576 also has an NPU).
    Exposing a handful of task-level tools (list TNCs, create bridge, add
    port, switch, pair, status) rather than the whole protocol keeps the
    surface small enough for a 1–3B model.
  - **Where the model runs vs. where the daemon runs.** On a Pi or Flipper
    One, the model can call `ktbctl` locally. A model on a Mac or phone
    reaches the device through `ktbctl` over SSH, or through `ktb-gateway`.
    An MCP server wrapping the control protocol would be another way to
    package the same tools for desktop assistants.
  - **Guardrails for a transmitter.** The skill has to confirm with the user
    before anything that widens who can transmit: binding a `tcp-server`
    beyond loopback, removing an `allow` list, or turning on gateway access.
    It also needs to explain the steps a model can't do: putting the radio in
    pairing mode, turning on KISS, plugging in cables.
  - **Diagnostics are the bigger win.** Most questions will be "why isn't it
    working?". A structured `ktbctl doctor` would help humans as much as
    models, and could come first. Where `check` is about the config, `doctor`
    is about the running system: adapter present and powered, TNC paired,
    serial device present, interface up, DNS-SD announced, recent errors.
  - **What 1.0 should get right so this is easy later:** complete and stable
    `--json` output, stable error codes with actionable messages, and the
    published schema. All three are already in the plan.

---

## Phase 0 findings

Done October 12–13, 2026 on a Raspberry Pi 4 (Debian 13, BlueZ 5.82) with
the built-in Cypress adapter (`hci0`) and a TP-Link UB500 (RTL8761B) dongle
(`hci1`), against a Kenwood TH-D75. The spike code and the full notes are on
the `phase0-spikes` branch under `spikes/`. Nothing is a dead end; the plan
stands, with the adjustments listed at the end.

| Question | Answer |
|---|---|
| D75 pairs with BlueZ on the Pi 4 and on the RTL8761B? Which callbacks? | Yes on both (the built-in chip was paired before Phase 0; the dongle pairing was recorded). SSP **numeric comparison**: BlueZ calls `request_confirmation` once with the six-digit passkey the radio shows, our side confirms at once, and the operator presses OK on the radio within 30 s. No PIN, no passkey entry. **Just Works is refused** (`Authentication Failed` in 5 s with a `NoInputNoOutput` agent), so the pairing-window agent is required |
| bluer `register_profile` + `connect_profile(SPP)` reads KISS? | Yes. BlueZ does the SDP lookup and hands over the RFCOMM fd; the link is authenticated and encrypted. No channel number needed |
| RFCOMM connect time; radio off or out of range | 4.8 s cold, 0.6–1.5 s warm, 38 ms with the ACL link still up. Radio unreachable: a fixed 5.13 s page timeout per attempt, no hang |
| BLE GATT + RFCOMM on one controller for an hour with aprs.fi | Clean: 61 min with an iPad on aprs.fi attached, radio link up 65 min without a drop, 398 frames forwarded, no daemon errors or BLE disconnects, daemon RSS 5.3 MB. Receive-only traffic |
| Radio on the built-in chip, BLE on the dongle | Works. The radio link on hci0 ran 1 h 51 min across both coexistence runs without a drop; with the daemon on the dongle, an iPad and an iPhone were attached at once, 0 errors. Stopped after a few minutes by choice |
| bb-link source: init, reconnect, D74 vs D75, license | All three forks GPL-3.0. KISS init is `BT` (silent ⇒ already in KISS ⇒ `C0 FF C0`), `TN` to read mode and band, `TN 2,<band>`; teardown restores the previous mode. Reconnect every 15 s. **No D74/D75 difference in the code** |
| D75 over USB | `2166:9023`, one CDC-ACM port plus USB audio; `/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00`. The radio reports no USB serial number |
| KISS over USB uses the same init? | Yes, byte for byte; replies under 1 ms over USB, ~6 ms over RFCOMM |
| TNC on Bluetooth and USB at once? | No. `Menu > Configuration > Interface > KISS (983)` selects one. Confirms the one-enabled-bridge-per-TNC rule |

Adjustments to the plan:

- **`model`:** one `th-d7x` profile, with `th-d74` and `th-d75` accepted as
  aliases. Init runs on every TNC connect, as halka's fork does, since the
  radio comes up in whatever mode it was left in. `TN` after a KISS exit
  reports mode 0, so the mode to restore is remembered from the `BT` probe,
  not read back.
- **Init reader:** frames arrive while probing with `BT`, so the reply parser
  has to strip KISS frames from the stream rather than read lines.
- **Pairing flow:** needs `request_confirmation` only (DisplayYesNo is
  enough). The daemon should show the passkey it confirmed, tell the operator
  to dismiss any stale prompt and re-enter Pairing Mode first, and treat
  `Authentication Failed` after about 30 s as "OK wasn't pressed in time".
- **Advertising while connected:** the Pi 4's built-in controller stops
  advertising while one BLE central is connected, even with the advertisement
  registered, so a second phone can't discover the bridge there.
  The RTL8761B dongle does keep advertising with one central attached and
  served two phones at once, so `max_clients > 1` works on a dongle and the
  recommended layout for more than one phone is radio on the built-in chip,
  phones on a dongle. `status`/`doctor` should report whether the
  advertisement is active, not just registered. BlueZ puts
  the local name in the scan response, which is where the 29-byte
  `public_name` limit comes from; confirmed from the dongle.
- **Radio-side reconnects:** after a teardown the D75 was unreachable for
  about a minute, then connected on its own (the ACL showed as incoming). The
  retry loop recovered without help; nothing to add beyond the state machine
  already planned.
- **rfkill:** adapters can be soft-blocked (`PowerState: off-blocked`), and
  `Powered = true` then fails with a bare `org.bluez.Error.Failed`. The daemon
  runs as root and should clear the block itself; `-t` and `status` should
  name it.
- **Serial paths:** by-id names are per model for the D75, so two of them on
  one host need by-path. `-t` accepts both; the udev rule matches
  `2166:9023`.
- **State machine:** a connect is one D-Bus call that blocks up to about 5 s,
  and every long await needs a single cancellation point (the spike learned
  this the hard way with SIGINT).
