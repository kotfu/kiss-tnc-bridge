# kiss-tnc-bridge architecture

How the daemon works inside, for anyone reading or changing the code. For
installing, configuring and using it, see the [README](../README.md).

`kiss-tnc-bridge` is an asynchronous daemon written in Rust on the
[Tokio](https://tokio.rs/) runtime. It talks to the Linux Bluetooth stack
(BlueZ) and to Avahi over D-Bus, using the
[`bluer`](https://crates.io/crates/bluer) crate for Bluetooth. `ktbctl` is a
separate program that talks to the daemon over a Unix socket.

```
  /etc/kiss-tnc-bridge/*.toml ─┐
  /var/lib/.../dynamic.toml ───┼─> reconciler ──> bridge ktbr0 (task) ── ports
  /var/lib/.../state.toml ─────┘     │    ^       bridge ktbr1 (task) ── ports
                                     │    │
       BlueZ adapters come and go ───┘    │
                                          │
  ktbctl, ktbctl tui ── control.sock ─────┘  (requests, and events: state,
                                              activity, frames)
```

## The daemon and the reconciler

At startup the daemon takes an exclusive lock in its runtime directory
(`/run/kiss-tnc-bridge/`) and exits if another instance holds it. It loads
the static and dynamic config and the enable overrides, validates them as
one, and hands the result to the **reconciler**.

The reconciler holds the desired set of bridges and the running set. On a
reload, a `ktbctl` change, or an adapter appearing or disappearing, it
works out what differs and starts, stops or restarts only those bridges and
ports. Changing one bridge doesn't touch the others, and changing an app
port doesn't drop the TNC link.

Adapters are tracked per port. If a `ble-server` port's adapter goes away (a
dongle unplugged, or powered off with `rfkill`), only that port stops, and
the bridge's TNC link and TCP apps carry on. If a TNC's adapter goes away,
its `spp-client` port backs off and retries while apps stay connected. When
the adapter comes back, the affected ports start again.

## Inside a bridge

Each enabled bridge is one task with one `select!` loop. It owns the TNC
port's state machine, the sessions on its app ports, and its counters.

```
              ┌───────────────────────────── bridge ktbr0 ─────────────────────────────┐
              │                                                                         │
 app ports    │  per-session reassembly ─┐                      ┌─ per-session queue ───│─> BLE notify
 (ble-server, │  per-session reassembly ─┼─> KISS policy ──┐    ├─ per-session queue ───│─> TCP write
  tcp-server) │                          │   filter        │    │                       │
              │                          │                 v    │                       │
              │                          │          TNC port state machine              │
              │                          │   Unpaired · Waiting for device ·            │
              │                          │   Idle → Connecting → Connected → Backoff    │
              └──────────────────────────┴───────────────────────┬──────────────────────┘
                                                                 │
                       TCP socket | Bluetooth RFCOMM | BLE GATT client | serial device
```

- **Apps to the TNC:** each session reassembles its own KISS frames, so
  pieces from different apps never mix, even when a frame arrives over
  several BLE writes or TCP segments. Complete frames from every session go
  into one queue for the bridge (256 frames), in the order they arrive. The
  bridge takes them one at a time, passes them through the KISS policy
  filter (which drops "return", and KISS parameter commands with
  `bridge.app_params = "drop"`), and hands them to a single writer (another 256
  frames) that writes each frame to the TNC whole before starting the next.
  So two apps transmitting at the same moment get their frames sent back to
  back, never interleaved. When the TNC is slower than the apps, the
  writer's queue fills, the bridge stops taking frames, and the apps' own
  sends wait: a TCP app is slowed by TCP flow control rather than having
  its frames dropped. After the bridge, the radio's TNC queues frames
  again and transmits one at a time when the channel is clear (see
  [When the radio doesn't transmit](../README.md#when-the-radio-doesnt-transmit)).
- **The TNC to apps:** each frame from the TNC is copied to every session's
  queue. A session that can't keep up (64 frames behind) loses frames
  rather than holding up the others; `ktbctl status` counts them as dropped.
- **The TNC link:** connecting runs in its own task, so the loop keeps
  serving apps while a radio takes seconds to answer. While it's connecting,
  frames from apps wait in a separate queue (up to 32 frames, each for at
  most 5 seconds; past that the oldest is dropped and counted) and go to
  the TNC once it's up. A failed
  connect backs off from 1 to 30 seconds. `Unpaired` (an `spp-client` whose
  TNC isn't paired on its adapter) and `Waiting for device` (a `serial`
  port whose device isn't plugged in) are checked every few seconds instead.
  For a TH-D74/D75, every connect starts with the commands that put its TNC
  in KISS mode.

## BLE

Each `ble-server` port registers its own GATT application and advertisement
with BlueZ, named after the bridge's public name, and sets the adapter's
alias to the same name. The service follows the
[BLE KISS API spec](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md):

| UUID | Role | Properties | Direction |
|------|------|------------|-----------|
| `00000001-ba2a-46c9-ae49-01b0961f68bb` | KISS service | primary service | — |
| `00000002-ba2a-46c9-ae49-01b0961f68bb` | TX characteristic | write, write-without-response | app → bridge |
| `00000003-ba2a-46c9-ae49-01b0961f68bb` | RX characteristic | notify | bridge → app |

Both characteristics use BlueZ's file-descriptor I/O, so data is streamed
over sockets rather than with a D-Bus call per write. Frames to a phone are
split to its negotiated MTU and sent as notifications.

A phone connecting ends the advertisement, and the kernel brings it back
only when that phone disconnects. So while there's room for more, the daemon
registers a new advertisement each time a phone connects. Controllers that
can advertise while connected, like the Raspberry Pi 4's built-in chip and
the RTL8761B, then let a second phone find the bridge; on ones that can't, a second phone can connect only once
the first has left. At `bridge.port.max_clients` the advertisement is withdrawn, and a
phone that's already been served can always reconnect.

When a phone's link drops, the kernel turns advertising back on for the
advertisement that phone connected through. By then the daemon has usually
replaced or withdrawn it, and for a missing advertisement the kernel falls
back to the adapter's defaults: non-connectable, from a random address, with
no scan response and so no name. On controllers with legacy advertising, such
as the Raspberry Pi 4's, that's what phones then see: a nameless bridge they
can't connect to. So when a phone the port has served drops its link, the
daemon withdraws its advertisement, which stops the fallback, and registers a
new one 3 seconds later. The wait matters: the kernel starts a new
advertisement at once only when its 2-second instance-rotation timer isn't
running, and otherwise never starts it. The bridge is invisible for those 3
seconds.

iOS keeps the Bluetooth link open after the app disconnects, with no way to
close it short of turning Bluetooth off, and a connected link keeps most
controllers from advertising. So the daemon drops the link of a phone it
has served when that phone is still connected without a session: it checks
every 10 seconds. After a crash, the next daemon drops links the last one
left behind.

### BLE TNCs

A `ble-client` port plays the phone's part against a TNC that follows the
same spec, such as an Ailunce HA2: it connects, subscribes to the RX
characteristic, and writes to the TX characteristic, both through BlueZ's
file-descriptor I/O (`AcquireNotify` and `AcquireWrite`), so the link is a
byte stream like RFCOMM. Writes are split to the MTU; the TNC reassembles
the frames. Nothing is paired.

Some radios carry the same KISS byte stream on a GATT service of their own.
The TNC's `model` picks the service and characteristics:

| `model` | Service | TX (bridge → TNC) | RX (TNC → bridge) |
|---|---|---|---|
| any other | `00000001-ba2a-46c9-ae49-01b0961f68bb` | `00000002-…` | `00000003-…` |
| `td-h9` | `0000af00-0000-1000-8000-00805f9b34fb` | `af01`, write without response | `af02`, notify |

The TD-H9 pads each frame with extra FENDs (five before, three after),
which reassembly skips as empty frames. Its protocol is written up in
[td-h9-ble-kiss.md](td-h9-ble-kiss.md).

BlueZ forgets an unpaired LE device a little while after it last heard it,
so a connect first scans for the TNC when BlueZ doesn't know it. The scan
looks for its address, not a service or a name: the TD-H9 doesn't
advertise the service its KISS link is on, and a name can differ between
radios and firmware versions. A TNC that something else is connected to doesn't
advertise, and the connect fails and backs off as usual. When the bridge
lets go of the link (idle, shutdown, a restart), it drops the LE connection
too, so the TNC advertises again for phones.

## KISS framing

KISS frames are delimited by `FEND` (`0xC0`) bytes with the standard byte
stuffing (`FESC`/`TFEND`/`TFESC`). Every connection, BLE, TCP, Bluetooth
Classic or serial, has a reassembly buffer that collects bytes and extracts
complete frames, so frames split across BLE MTU boundaries or TCP segments
are handled transparently. Frames longer than 8192 bytes are discarded.

## The control socket

`ktbctl` and any other client talk to the daemon over
`/run/kiss-tnc-bridge/control.sock` (mode 0660, group `kiss-tnc-bridge`),
with newline-delimited JSON requests and responses. A client can subscribe
to events: state changes (which mean "read the status again"), activity for
the TUI's lights, and frames for `ktbctl monitor`. The daemon only manages
over this local socket; nothing about management is reachable from the
network. See [control-api.md](control-api.md).
