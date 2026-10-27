# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased (1.0 in progress)

The 1.0 work in [docs/plan-1.0.md](docs/plan-1.0.md) lands on `main` in
phases; nothing is released until 1.0.0. The 0.5 config is not migrated.

### Changed

- **New configuration**: TOML in `/etc/kiss-tnc-bridge/` (`kiss-tnc-bridge.toml`
  plus `conf.d/*.toml`) describing bridges with inline ports, replacing the
  INI file. `/etc/kiss-tnc-bridge.conf` is ignored with a warning.
- The daemon is a KISS switchboard: each bridge connects one TNC port
  (`spp-client`, `serial` or `tcp-client`) to any number of app ports
  (`ble-server`, `tcp-server`). Frames from apps go to the TNC through one serialized
  writer; frames from the TNC are broadcast to every app; apps never hear
  each other.
- The TNC connection no longer blocks BLE handling: connecting runs in its
  own task with exponential backoff, uplink frames are queued while it
  comes up, and a flood from one app backs up to that app instead of
  being dropped.
- The KISS "return" command (`C0 FF C0`) from an app is dropped;
  `app_params = "drop"` also drops KISS parameter commands.
- Each `ble-server` port registers its own GATT application and
  advertisement, named after the bridge's `public_name`.
- systemd: `ConfigurationDirectory=`, `StateDirectory=`, `RuntimeDirectory=`
  and `ProtectSystem=strict`; the lock file moved to `/run/kiss-tnc-bridge/`.
- The daemon handles SIGTERM as well as SIGINT.

### Added

- `tcp-server` ports: KISS over TCP for apps, bound to loopback by default,
  an address, or an interface (`wlan0:8001`), with an `allow` list and
  `max_clients`.
- `[[adapter]]` objects give adapter MACs readable ids; adapter values can
  be an id, a MAC or `hciN`.
- `kiss-tnc-bridge -t` prints the effective bridges with their source files
  and warnings, and checks references and conflicts among enabled bridges.
- Bridges can be disabled and kept for another setup.
- A PR CI workflow (fmt, clippy, tests on Linux and macOS) and an
  end-to-end TCP test with no Bluetooth.
- `[[tnc]]` objects for hardware TNCs, with an `spp` section (Bluetooth
  address and the adapter it's paired on) and/or a `serial` section (device
  path, baud). A TNC can be in only one enabled bridge, over whichever
  transport.
- `spp-client` ports: a TNC over Bluetooth Classic, such as the TH-D74/D75's
  built-in TNC, connected through a BlueZ SPP profile (no channel numbers).
  A TNC that isn't paired is reported as unpaired and checked again every
  few seconds; pair it with `bluetoothctl` (see the README).
- `serial` ports: a TNC on a USB or UART serial device, opened exclusively,
  waiting for the device when it's unplugged and reconnecting when it's back.
- `model = "th-d74" | "th-d75"` puts the radio's TNC in KISS mode on every
  connect, over Bluetooth or USB. Other TNCs use the default,
  `model = "generic"`; an unknown model is an error that lists the models. `exit_kiss_on_disconnect` takes it back out
  when the bridge lets go, restoring APRS mode if that's how it was found.
- `connect` and `idle_timeout` on every TNC port type.
- `tcp-server` ports are announced with DNS-SD (`_kiss-tnc._tcp`) through
  Avahi, scoped to the interface they're bound to (`announce = false` turns
  it off). A name collision on the LAN moves the bridge to "Name (2)" for
  DNS-SD and BLE alike, kept across restarts in
  `/var/lib/kiss-tnc-bridge/state.toml`.
- `kiss-tnc-bridge -t` lists TNCs and warns about TNCs not paired on their
  adapter (when BlueZ is reachable), unstable serial paths such as
  `/dev/ttyACM0`, and serial devices that aren't present.
- A udev rule that keeps ModemManager away from the TH-D75's USB port.
- Dynamic config in `/var/lib/kiss-tnc-bridge/dynamic.toml`, which the
  control API will write (`ktbctl`), alongside the static config in `/etc`.
  Static config can't refer to dynamic objects, and static objects win
  collisions. Enable overrides for static bridges live in `state.toml`.
- Reloading: `SIGHUP` (and `systemctl reload`) re-reads the config and
  changes only what changed. Settings like `allow`, `max_clients`,
  `connect`, `idle_timeout`, `public_name` and `log_level` apply live;
  a changed TNC address or device restarts just that port, with app
  sessions staying connected; sessions a tighter `allow` or lower
  `max_clients` no longer admit are disconnected. An invalid config is
  rejected and the running bridges carry on.
- The control API: a Unix socket (`/run/kiss-tnc-bridge/control.sock`,
  group `kiss-tnc-bridge`) speaking newline-delimited JSON, documented in
  `docs/control-api.md` with a published JSON Schema. `status`; events for
  state changes, activity and frames (with AX.25 headers decoded for
  display); `reload` and `config.check` with the plan they'd apply and
  whether it's disruptive; and commands that create and change dynamic
  bridges, ports, TNCs and adapters, enable, disable and switch bridges
  (static ones through overrides), hold TNC links up or down, kick
  sessions, and list serial devices. Every change can be refused rather
  than disrupt anyone (`non_disruptive`). `ktbctl` comes next.
- A disabled bridge may be incomplete (no TNC port, or no app port yet),
  so a bridge can be built up a port at a time and enabled when it's
  complete. Bridges created through the control API start disabled, and
  `status` and `-t` show what an incomplete bridge still needs.
- `ktbctl`, the command-line client for the control socket: `status`;
  listing adapters, TNCs, serial devices and TNC models; adding and
  removing adapters, TNCs, bridges and ports; `enable`, `disable`,
  `switch`, `connect`, `disconnect` and `kick`; `edit` in `$EDITOR`;
  `check`, `reload` and `monitor` (live decoded frames). Changes that would
  disconnect someone are confirmed in a terminal and refused without one
  unless given `--yes`. Every command takes `--json`. Packaged alongside
  the daemon.
- Radios can be paired through the daemon: `ktbctl scan` finds them
  (known radios first, with their model), `ktbctl pair` pairs and trusts
  one by TNC or address, relaying the passkey to confirm, and `ktbctl
  unpair` forgets it. A port waiting for its radio connects a few seconds
  after it's paired. In the control API: `bluetooth.scan`,
  `bluetooth.pair` with `pairing` events and `bluetooth.pair_reply`, and
  `bluetooth.unpair`; a connection can now have several requests in
  flight.
- `ktbctl setup`: a step-by-step wizard for a TNC (over Bluetooth, with
  scanning and pairing, USB/serial, or a KISS TCP server on the network)
  and a bridge sharing it over BLE, TCP or both, ending with a check that
  the TNC connects and is heard. Radio-specific instructions come from the
  daemon's model table, which now lists the radios it knows (served by
  `tnc.models`).
- `port.release` (`ktbctl release BRIDGE/PORT`) hands a TNC link held by
  `connect` or `disconnect` back to its connect policy; `status` shows a
  held link.

### Fixed

- Phones no longer pair with the bridge. BlueZ asks every phone that
  connects to pair (it reads the phone's battery level), and the
  pairing failed, so phones prompted, stalled and reconnected over and
  over. The bridge never needed pairing; one BlueZ setting stops BlueZ
  asking (`ReverseServiceDiscovery = false`, see "Phones and pairing" in
  the README), and `-t` and the daemon warn when it isn't set.
- When the daemon stops, phones connected over BLE are disconnected, so
  apps like aprs.fi notice instead of staying "connected" to nothing.
  After a crash, the restarted daemon disconnects the phones the old one
  left connected, so they reconnect to it.
- A phone whose app has disconnected no longer hides the bridge from other
  phones. iOS keeps the link open, and most controllers can't advertise
  while anything is connected, so the daemon now drops the link of a
  phone it has served that stays connected without a session (checked
  every 10 s).
- A second phone can find the bridge while one is connected, on
  controllers that can advertise while connected (such as the RTL8761B):
  the daemon registers a new advertisement each time a phone connects,
  since the kernel brings the old one back only when that phone leaves.

## 0.5.1 (30 May 2026)

### Fixed

- logic bug in the just works pairing code


## 0.5.0 (30 May 2026)

### Fixed

- Add a `NoInputNoOutput` pairing agent and make the adapter pairable so BLE
  pairing uses the "Just Works" model. Previously, when BlueZ probed a connected
  client's protected characteristics it would trigger a numeric-comparison
  pairing, prompting for a pairing code that the headless daemon could not
  confirm — causing some clients to fail to connect.


## 0.4.5 (26 May 2026)

### Changed

- only allow a single instance of `kiss-tnc-bridge` to run, running multiple
  instances concurrently, especially fron the same config file can cause
  race conditions and malformed data sent to the TCP TNC.
- the default `max_clients` is now 1, since not all bluetooth chips support
  multiple simultaneous connects on the same BLE GATT advertisement


## 0.4.4 (25 May 2026)

### Improved

- more improvements for disappearing bluetooth adapters


## 0.4.3 (25 May 2026)

### Improved

- better resiliance to bluetooth adapters being powered off or disappearing


## 0.4.2 (25 May 2026)

### Fixed

- connection counting bug


## 0.4.1 (25 May 2026)

### Improved

- log output to stderr instead of stdout
- added more trace logging to debug multiplexing with multiple BLE clients


## 0.4.0 (25 May 2026)

### New

- per TNC override of the bluetooth adapter to use

### Improved

- bluetooth adapter detection and error handling


## 0.3.1 (24 May 2026)

### Improved

- minor packaging enhancements


## 0.3.0 (24 May 2026)

### Fixed

- builds cleanly on x86_64, aarch64, and on ancient 32 bit armv6


## 0.2.0 (24 May 2026)

Initial release with the following features:

- Bridges a TCP KISS TNC to a Bluetooth Low Energy device
- Multiple TCP servers
- Multiple bluetooth clients per TCP server
- Runs on Linux: debs and rpms for amd64 and arm64, debs for armhf (32 bit armv6) so
  it will run on the original Raspberry Pi, generic tarballs
