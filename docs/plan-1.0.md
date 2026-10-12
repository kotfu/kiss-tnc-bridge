# kiss-tnc-bridge 1.0 plan

Status: draft, October 2026

## Goal

Turn `kiss-tnc-bridge` from a one-way "TCP TNC → BLE" bridge into a small
**KISS hub**. Any KISS modem it can reach (a TCP KISS server, or a radio's
built-in TNC over Bluetooth Classic SPP) can be shared with any number of
clients over BLE and over TCP. A companion CLI covers discovery, pairing,
configuration and live status, so nobody has to hand-edit INI files or drive
`bluetoothctl`.

The motivating setup:

```
  Kenwood TH-D75 ──SPP/RFCOMM──┐                 ┌──BLE──> iPhone (aprs.fi, RadioMail)
                               ├─ kiss-tnc-bridge┤
  (Dire Wolf / Graywolf) ──TCP─┘   on Pi or      └──TCP──> laptop (Pat/Winlink, Xastir, YAAC)
                                   Flipper One
```

This replaces the discontinued [B.B. Link](https://github.com/islandmagic/bb-link/)
ESP32 adapter. It also does things B.B. Link can't: several clients at once,
TCP clients, and running on hardware you already carry.

### Targets

| Platform | Arch | Bluetooth | Notes |
|---|---|---|---|
| Raspberry Pi 3/4/5, Zero W, Zero 2 W | arm64 / armhf | Broadcom/Cypress, UART-attached, dual-mode | The primary platform until Flipper One ships |
| Pi 1 / Zero + USB dongle | armhf | RTL8761B and similar, dual-mode | Advertising limits already documented in README |
| Generic x86_64 Linux | amd64 | anything BlueZ supports | Dev/test |
| **Flipper One** | arm64 (Rockchip RK3576) | Bluetooth 5.2 on the MediaTek MT7921AUN combo module | Debian-based Flipper OS. Specs are still preliminary ([CNX](https://www.cnx-software.com/2026/05/21/flipper-one-a-rockchip-rk3576-powered-portable-arm-linux-computer-and-networking-multi-tool/), [The Register](https://www.theregister.com/personal-tech/2026/05/21/flipper-one-wants-to-be-the-linux-multi-tool-in-your-pocket/5244165)) |

### Non-goals for 1.0

- Acting as a BLE *central* to other BLE TNCs (Mobilinkd, etc.). The source
  abstraction leaves room for it; see [After 1.0](#after-10).
- Any non-Linux daemon. The CLI and the TCP↔TCP hub should still build and run
  on macOS for development.
- AX.25 or APRS awareness in the data path. The hub moves KISS frames. Decoding
  is only for display, in `monitor`.

---

## Target architecture

### Concepts

- **TNC (hub)**: one config section. It has exactly one *source* and one or
  more *front-ends*. Hubs are independent, the same as today's per-TNC
  isolation.
- **Source (upstream)**: the thing that actually is a modem. `tcp` (client to
  Dire Wolf/Graywolf/etc.), `rfcomm` (client to a radio's SPP port), and
  optionally `serial` later.
- **Front-end (downstream)**: how clients reach the hub. `ble` (today's GATT
  peripheral) and `tcp` (new KISS TCP server).
- **Client**: one connected BLE device or TCP peer. `ClientId` is
  `Ble(Address) | Tcp(SocketAddr)`.

Routing stays the same as today. Every client's complete frames go to the
source, serialized. Every frame from the source is broadcast to every client
on every front-end of that hub. Frames never pass directly between clients.

```
               ┌──────────────────────── Hub "TH-D75" ─────────────────────────┐
               │                                                                │
  BLE front-end│  per-client reassembly ─┐                ┌── per-client queue ──│──> BLE notify
  TCP front-end│  per-client reassembly ─┼─> KISS policy ─┤                      │
               │                         │   filter       └── per-client queue ──│──> TCP write
               │                         v                         ^            │
               │                 Source state machine ─────────────┘            │
               │          Idle → Connecting → Connected → Backoff → …           │
               └───────────────────────────────┬────────────────────────────────┘
                                               │ AsyncRead + AsyncWrite
                                   TcpStream | bluer::rfcomm::Stream | (serial)
```

### Module layout

```
src/
  lib.rs                 shared by both binaries
  kiss/                  unchanged: frame.rs, buffer.rs
  hub/
    mod.rs               Hub task: source state machine, client registry, routing, counters
    policy.rs            KISS command filtering (see "Multiplexing a radio")
  source/
    mod.rs               trait Source { async fn connect(&self) -> Result<KissStream> }
    stream.rs            KissConnection<S: AsyncRead + AsyncWrite> (generalized from bridge/tcp.rs)
    tcp.rs
    rfcomm.rs            linux only
    radio_init.rs        per-radio init sequences (th-d74, th-d75, none)
  frontend/
    ble.rs               GATT service + advertising + max_clients (from bridge/manager.rs)
    tcp.rs               TcpListener, one task per client
  control/
    protocol.rs          serde request/response/event types, shared with the CLI
    server.rs            Unix socket server
  supervisor.rs          adapter lifecycle + desired-vs-running reconciler (from main.rs)
  agent.rs               pairing agent: normal mode + pairing window
  config.rs
  bin/
    kiss-tnc-bridge.rs   daemon
    kiss-tnc-bridgectl.rs CLI
```

One package with two `[[bin]]` targets and a shared lib. A workspace would be
overkill at this size, and a single package keeps the existing deb/rpm/release
pipeline nearly unchanged.

### The hub never blocks its select loop

Today `BridgeManager::run` awaits `TcpKissConnection::connect` and
`tokio::time::sleep(backoff)` inline in its `select!` arms
([manager.rs](../src/bridge/manager.rs)). That stalls BLE event handling for up
to 30 s. It also leaves two reconnect gaps:

1. If the *first* connect fails when a client arrives, `tcp` stays `None`, the
   read arm is permanently pending, and nothing retries until a new client
   connects. Frames from the connected phone are dropped ("no TCP connection")
   indefinitely.
2. A reconnect after a read error only happens if `clients` (writers) is
   non-empty. A client that has only subscribed to notifications won't trigger
   it.

RFCOMM makes this worse, because a Classic page/connect can take several
seconds. The new source state machine runs `connect()` in a spawned task and
reports back over a channel. Backoff is a `tokio::time::Sleep` stored in the
hub and polled as its own select arm. While `Connecting`, uplink frames are
queued (bounded: 32 frames / 5 s) instead of dropped.

Gap 1 is worth fixing in a 0.5.x patch now, independently of this plan.

### Source lifecycle policy

| Key | Values | Default | Meaning |
|---|---|---|---|
| `connect` | `on_demand`, `always` | `on_demand` | `on_demand` matches today: connect when the first client arrives. `always` keeps the source up even with no clients, which is useful for a TCP front-end that should look "live" |
| `idle_timeout` | duration | `0` (immediate) | With `on_demand`, how long to hold the source after the last client leaves. Keeping an RFCOMM link warm for ~1–2 minutes avoids a slow reconnect when iOS briefly drops BLE |

---

## Configuration (v1 schema)

Backward compatible: every existing 0.5 config keeps working unchanged.
A section with `host`/`port` and no `source` key is a `tcp` source with a
`ble` front-end, and `max_clients` still means BLE clients.

```ini
[global]
log_level = info
# adapter = hci0
# control_socket = /run/kiss-tnc-bridge/control.sock
# include_dir = /etc/kiss-tnc-bridge.d      # managed by the CLI, see below

# 0.5-style section: still valid
[Graywolf TNC]
host = 127.0.0.1
port = 6700
max_clients = 3

# Radio over Bluetooth Classic, shared over BLE and TCP
[TH-D75]
source = rfcomm
address = 00:11:22:33:44:55
# channel = auto           # auto = SDP lookup via BlueZ profile connect
radio = th-d75             # init/teardown profile: th-d74 | th-d75 | none
connect = on_demand
idle_timeout = 90s

ble = yes                  # advertise a BLE KISS service named after the section
max_clients = 2            # BLE client limit (unchanged meaning)

tcp_listen = 8001          # port alone binds 127.0.0.1; use 0.0.0.0:8001 to expose
tcp_max_clients = 4
```

Validation rules:

- Exactly one source per section.
- At least one front-end. `ble = no` with no `tcp_listen` is an error.
- `tcp_listen` ports must be unique across sections.
- `source = tcp` together with `tcp_listen` is allowed, as a plain multiplexer.

**TCP exposure is a licensing issue, not just a security one.** KISS TCP has no
authentication. Anyone who can reach the port can transmit on your radio under
your callsign. A bare port number binds to `127.0.0.1`, and binding to
`0.0.0.0` logs a warning at startup. An optional `tcp_allow = 192.168.4.0/24`
allowlist is cheap to add and worth it on a Flipper One running as a Wi-Fi AP.

### Where CLI-made changes are stored

The daemon (running as root) writes one file per managed TNC into
`include_dir` (default `/etc/kiss-tnc-bridge.d/`). They use the same INI format
and are human-readable, and the CLI never rewrites the hand-edited main file.
That matters because `configparser` drops comments on write. If a section name
appears in both places, the main config wins and a warning is logged.
`include_dir` is configurable in case Flipper OS snapshots make `/etc` the wrong
place (see open questions).

---

## Multiplexing a radio safely

Sharing one radio TNC among several clients needs a few rules that a single
TCP Dire Wolf connection never needed:

- **Drop KISS "return" (`C0 FF C0`) from clients.** On Kenwood radios it takes
  the TNC out of KISS mode, which breaks it for everyone. The hub logs it and
  swallows it. Only the daemon sends it, during teardown, and only if
  `radio_exit_on_disconnect = yes`.
- **KISS parameter commands** (TXDELAY, P, SlotTime, TXtail, FullDuplex, SetHardware) pass through
  by default. A per-hub `client_params = pass | drop` lets an operator pin the
  radio's settings.
- **KISS init.** On connect, `radio = th-d75` sends the sequence that puts the
  TNC in KISS mode. Use B.B. Link's firmware (the `halka/bb-link` fork has
  explicit KISS-mode initialisation) as the behavioral reference for the exact
  CAT commands, timing, and the D74/D75 differences. Re-implement it from the
  documented behavior rather than porting code, until the license is confirmed
  compatible with GPL-3.0.
- **Port nibble.** The KISS port number (high nibble of the command byte) passes
  through untouched. Multi-port radios are out of scope for 1.0.

---

## Bluetooth Classic specifics

- Enable `bluer`'s `rfcomm` feature. Verified that bluer 0.17.4 exposes
  `Session::register_profile`, `Device::connect_profile`, `Device::pair`, and
  `rfcomm::{Profile, Role, Stream}`.
- **Connect through a BlueZ profile, not a hard-coded channel.** Register a
  `Profile { uuid: SPP 0x1101, role: Client, require_authentication: true }`,
  call `device.connect_profile(SPP)`, and take the `Stream` from the
  `ProfileHandle` connect request. BlueZ does the SDP lookup. A `channel = N`
  override is kept for oddball devices.
- After a successful pair, `set_trusted(true)` so reconnects need no
  authorization.
- Watch for BlueZ or PipeWire auto-connecting the radio's audio profiles
  (HFP/HSP). It's harmless on a headless Pi, but on a desktop it can grab the
  radio. Document it, and possibly `Device::disconnect_profile` the audio UUIDs.
- **Both radios on one adapter:** BLE peripheral advertising and BR/EDR
  connections at the same time are normal for dual-mode controllers. Classic
  *inquiry* (scanning) can disturb live links, so `scan` warns when clients are
  connected. Per-TNC `adapter` remains the escape hatch: a USB dongle for BLE,
  built-in for Classic, or the reverse.

### Pairing agent

Today the daemon registers a `NoInputNoOutput` agent so BLE phones pair with
Just Works ([main.rs](../src/main.rs)). BlueZ uses the agent's declared
capability for the whole session, and the radio may need PIN or passkey
interaction. Plan:

1. **Normal mode:** the current `NoInputNoOutput` agent, unchanged.
2. **Pairing window:** when the control API receives `pair <addr>`, the daemon
   unregisters the normal agent and registers a `KeyboardDisplay` agent whose
   callbacks (`request_pin_code`, `display_passkey`, `request_confirmation`, …)
   are forwarded as events to the requesting control client. Then it calls
   `device.pair()`, waits up to 60 s, sets the device trusted, and restores the
   normal agent. A BLE phone pairing during that window could see a numeric
   prompt. Document this as acceptable.
3. If Phase 0 shows the D75 pairs fine under `NoInputNoOutput`, skip step 2
   for 1.0 and keep it as a fallback for other radios.

---

## Control API

The CLI is just one client of this API. Any other UI (a Flipper One on-screen
app, a small web page) is another. **Every action the CLI can take goes through
the API, including pairing prompts.** That rule is what makes the project
"ready for Flipper One" without knowing its app SDK yet.

- **Transport:** Unix stream socket at `/run/kiss-tnc-bridge/control.sock`
  (systemd `RuntimeDirectory=`), mode `0660`, group `kiss-tnc-bridge` (created
  by the deb/rpm postinst).
- **Framing:** newline-delimited JSON. Every message carries `"v": 1`.
  Requests have an `id`. Responses echo it with `ok`/`error`. Events have no
  `id`.
- **Why not D-Bus:** we already link libdbus, but JSON lines can be used from
  Python, shell (`socat`), or whatever Flipper's app runtime turns out to be,
  with zero codegen. The protocol types live in `control/protocol.rs` with
  serde, so the CLI and daemon can't drift.

```jsonc
→ {"v":1,"id":1,"cmd":"status"}
← {"v":1,"id":1,"ok":true,"result":{"tncs":[{
     "name":"TH-D75",
     "source":{"type":"rfcomm","address":"00:11:22:33:44:55","state":"connected","since":"2026-10-11T14:02:11Z"},
     "frontends":[
       {"type":"ble","advertising":false,"clients":["7C:2A:..."]},
       {"type":"tcp","listen":"0.0.0.0:8001","clients":["192.168.4.20:51515"]}],
     "counters":{"frames_up":12,"frames_down":340,"dropped":0}}]}}

→ {"v":1,"id":2,"cmd":"subscribe","topics":["state","pairing","frames"]}
← {"v":1,"event":"pairing.request_passkey","request":7,"device":"00:11:22:33:44:55"}
→ {"v":1,"id":3,"cmd":"pairing.reply","request":7,"passkey":"0000"}
```

| Command | Purpose |
|---|---|
| `status` | Hubs, source states, front-ends, clients, counters, adapters |
| `subscribe` | Event stream: `state`, `pairing`, `scan`, `frames` (len + decoded AX.25 header) |
| `scan.start` / `scan.stop` | Discovery with `transport: classic | le | auto`. Emits `scan.device` events and flags likely radios (SPP UUID, `TH-D7*` names) |
| `pair` / `unpair` | Pairing window as above |
| `tnc.add` / `tnc.update` / `tnc.remove` | Write or delete a drop-in file, then reconcile |
| `tnc.connect` / `tnc.disconnect` | Force the source up or down |
| `client.kick` | Disconnect one client |
| `reload` | Re-read config and reconcile (also on `SIGHUP`) |

### Runtime reconfiguration

The supervisor becomes a **reconciler**. It holds the desired set of hubs (from
config plus drop-ins) and the running set, and on any change it starts, stops,
or restarts only the hubs that differ. To make that cheap:

- **One GATT application per hub**, instead of one per adapter. Adding or
  removing a hub then registers or unregisters only its own application, and
  BLE clients of other hubs on the same adapter aren't disconnected. BlueZ
  allows multiple applications per adapter.
- Adapter add/remove and power on/off handling (today's supervisor loop) stays
  the same, but now feeds the reconciler.

---

## CLI: `kiss-tnc-bridgectl`

Modeled on `systemctl`/`bluetoothctl`. Human output by default, `--json`
everywhere for scripts. It talks only to the control socket and never to BlueZ
directly.

```
kiss-tnc-bridgectl status [NAME]
kiss-tnc-bridgectl setup                       # interactive wizard (below)
kiss-tnc-bridgectl scan [--classic|--le] [--timeout 15s]
kiss-tnc-bridgectl pair ADDR | unpair ADDR
kiss-tnc-bridgectl add NAME --rfcomm ADDR [--radio th-d75] [--tcp-listen PORT] [--no-ble]
kiss-tnc-bridgectl add NAME --tcp HOST:PORT [--tcp-listen PORT]
kiss-tnc-bridgectl remove NAME
kiss-tnc-bridgectl connect NAME | disconnect NAME
kiss-tnc-bridgectl clients NAME | kick NAME CLIENT
kiss-tnc-bridgectl monitor [NAME]              # live frames: W1AW-9>APDR16,WIDE1-1: ...
kiss-tnc-bridgectl reload
```

**`setup` wizard** (the B.B. Link Configurator equivalent):

1. Tell the user to put the radio in Bluetooth pairing mode and turn on KISS
   (with menu paths for the D74 and D75).
2. Scan for Classic devices and list the likely radios first.
3. Pair, relaying any PIN or confirm prompt.
4. Ask for a name (the default is the radio's name), whether to share over BLE
   (yes), and whether to share over TCP (no, or which port, with the exposure
   warning).
5. `tnc.add`, connect, then wait for the first received frame or 30 s of
   silence, and report the result.

Dependencies: `serde`/`serde_json` (shared), `dialoguer` for the wizard. Keep
the daemon's footprint small for the Pi 1; the CLI's dependencies don't load
into the daemon.

---

## Phases

Each phase ends in a release. The jump to **1.0.0** marks the new config
schema, control API v1, and the CLI as stable contracts.

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
- [ ] Read the bb-link source (both forks): KISS init commands, reconnect
      behavior, D74 vs D75 differences, license.
- [ ] Measure RFCOMM connect time and behavior when the radio is off or out of
      range (page timeout).

**Exit:** a short findings section appended to this doc. If pairing or
coexistence is a dead end on Pi hardware, stop here.

### Phase 1: Hub refactor (→ 0.6.0)

Behavior parity, no new user features.

- [ ] Extract `KissConnection<S>` from `TcpKissConnection`.
- [ ] Split `BridgeManager` into `Hub` + `frontend::ble`. Reassembly moves into
      the front-end, so the hub only sees complete frames.
- [ ] Non-blocking source state machine. This fixes both reconnect gaps.
- [ ] One GATT application per hub.
- [ ] Hub unit tests with `tokio::io::duplex` sources and fake front-ends. This
      is the first time the routing logic is testable without BlueZ.
- [ ] Add a PR CI workflow (`cargo test`, `clippy`, `fmt --check`). Today only
      the release workflow exists.

### Phase 2: TCP KISS server front-end (→ 0.6.0, same release)

Small, and fully testable in CI, so it proves the multi-front-end design
before Bluetooth Classic is involved.

- [ ] `frontend::tcp` with `tcp_listen`, `tcp_max_clients`, localhost default,
      and optional `tcp_allow`.
- [ ] End-to-end CI test: fake TNC server → hub → several TCP clients, checking
      broadcast and uplink serialization.
- [ ] Config schema v1 parsing with full 0.5 back-compat tests.

### Phase 3: RFCOMM source (→ 0.7.0)

- [ ] `source::rfcomm` using the profile approach. Pairing is still manual
      (`bluetoothctl`) in this release.
- [ ] `radio_init` for th-d74/th-d75, the KISS policy filter, and
      `idle_timeout`.
- [ ] README: TH-D75 walkthrough, hardware notes, and the TCP exposure warning.
- [ ] Manual hardware test matrix (below).

At this point the core value is shipped: a Pi replaces a B.B. Link and also
shares the radio over TCP.

### Phase 4: Control API + runtime reconfiguration (→ 0.8.0)

- [ ] `control::server`, protocol types, `subscribe` events, counters.
- [ ] Reconciler, `include_dir` drop-ins, `reload`/`SIGHUP`.
- [ ] systemd unit: `RuntimeDirectory=kiss-tnc-bridge`, group creation in
      postinst/rpm scriptlets, and moving the lock file into the runtime dir.
- [ ] Protocol documented in `docs/control-api.md`, with a versioning policy:
      additive changes within v1, breaking changes need v2.

### Phase 5: CLI + pairing (→ 0.9.0)

- [ ] `kiss-tnc-bridgectl` with all commands above, `--json`, and the `setup`
      wizard.
- [ ] Pairing-window agent, unless Phase 0 made it unnecessary.
- [ ] `scan` with radio detection.
- [ ] `monitor` with an AX.25 address-header decode (display only).
- [ ] Package the CLI in deb/rpm/tarball.

### Phase 6: Hardening → 1.0.0

- [ ] Soak test: 24 h on a Pi Zero 2 W with a D75, two BLE clients and one TCP
      client, checking memory, reconnects, and that no frames are lost while
      the source is up.
- [ ] Check the footprint on the Pi 1 (armhf) still fits the README's "~5MB"
      claim.
- [ ] Rewrite the README around the hub model and update the architecture
      section and diagrams.
- [ ] CHANGELOG migration notes. Nothing should be required, but call out the
      new defaults.
- [ ] Flipper One validation, if hardware is available by then. Otherwise
      1.0 ships for Pi and the Flipper work becomes 1.1.

### Phase 7: Flipper One (1.x, gated on hardware and SDK)

- [ ] Verify on Flipper OS: BlueZ is the stack, systemd is present, glibc
      works with our `cross` builds, and which paths survive its profile
      snapshots.
- [ ] Check the MT7921 Bluetooth under load (MediaTek btusb firmware has had
      quirks on Linux): BLE peripheral and RFCOMM at the same time, and
      extended advertising with multiple hubs.
- [ ] Check for conflicts with Flipper OS's own Bluetooth services (a default
      agent, or its own GATT apps).
- [ ] Power: measure the battery cost of `connect = always` against
      `on_demand`, and consider a configurable advertising interval.
- [ ] On-device UI for the 256×144 screen, built on the control API: a status
      page (source state, client count, last heard station) plus a guided
      pairing flow. The technology depends on what Flipper ships for apps.
- [ ] Possibly ship as a Flipper OS package or profile if they have a
      distribution channel.

The Flipper One fits this project unusually well. It has Bluetooth for the
radio and the phone, Ethernet for a laptop running Winlink, Wi-Fi for an AP
serving TCP clients in the field, a battery, and a screen for status. None of
that requires anything beyond this plan's architecture. The
control API is the integration point.

---

## Test matrix (manual, per release from 0.7)

| Host | Radio | BLE clients | TCP clients |
|---|---|---|---|
| Pi 4 (built-in) | TH-D75 | aprs.fi iOS, RadioMail | Pat, Xastir |
| Pi Zero 2 W (built-in) | TH-D75 | aprs.fi iOS | — |
| Pi 1 + RTL8761B | TH-D75 | aprs.fi iOS (1 client) | YAAC |
| x86_64 + dongle | TH-D74 (if available) | aprs.fi iOS | Dire Wolf-style client |
| Flipper One | TH-D75 | aprs.fi iOS | Pat over Ethernet |

Scenarios: cold start with the radio off; radio power-cycled mid-session; radio
walked out of range and back; KISS mode turned off on the radio; adapter
unplugged; a BLE client sends `C0 FF C0`; a client floods the uplink; a slow
TCP client.

---

## Open questions

1. **D75 pairing model** (Phase 0). This decides whether the agent swap is
   needed at all.
2. **Drop-in dir vs. state file** for CLI-managed TNCs. Recommendation: the
   drop-in dir. Revisit if Flipper OS makes `/etc` awkward.
3. **CLI name:** `kiss-tnc-bridgectl` (recommended) or subcommands on the main
   binary (`kiss-tnc-bridge ctl …`).
4. **Serial source** (`source = serial`, e.g. the D74/D75 over USB). It's
   nearly free once sources are just streams (`tokio-serial`). In for 1.0, or
   1.1?
5. **Other SPP KISS devices** besides the D74/D75. The generic `radio = none`
   path should work for any SPP KISS TNC. Collect testers after 0.7.
6. **Flipper OS unknowns:** app SDK, Bluetooth ownership, filesystem layout,
   package format.

## After 1.0

- **BLE central source:** connect *to* BLE KISS TNCs (Mobilinkd TNC4, radios
  that implement the BLE KISS API) and re-share them over TCP or to more BLE
  clients.
- **Web status page** on the control API, for Pis without a terminal at hand.
- **Per-client transmit permissions** (receive-only clients) on top of the
  KISS policy filter.
