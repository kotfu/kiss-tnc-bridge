# kiss-tnc-bridge

Daemon to bridge Bluetooth KISS TNCs to TCP KISS TNCs.

Advertises Bluetooth Low Energy (BLE) General Attribute Profile (GATT) following
the [BLE KISS API spec](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md).
This allows APRS apps to connect via Bluetooth and have their KISS frames
forwarded to a TCP KISS TNC server like [Dire Wolf](https://github.com/wb2osz/direwolf)
or [Graywolf](https://github.com/chrissnell/graywolf).

## Why does this exist?

If you already have a KISS TNC server running on TCP, why would you want it to work via
Bluetooth?

1. In a scenario where power consumption matters more than range, Bluetooth Low Energy
   uses less power than Wi-Fi.
2. On Apple's iOS, the [aprs.fi](https://apps.apple.com/us/app/aprs-fi/id922155038) app
   can not use Wi-Fi when it's not the foreground app. But it can use Bluetooth, allowing
   it to remain connected in the background.
3. I created a portable APRS station using [Graywolf](https://github.com/chrissnell/graywolf)
   on a Raspberry Pi. Graywolf can be configured to create a TCP KISS TNC so multiple
   apps can use the radio set up in Graywolf. `kiss-tnc-bridge` lets a mobile device use
   Graywolf's TNC without having to have any Wi-Fi available.


## Features

- "Just Works" BLE pairing (no pairing code prompts) for headless operation
- Multiple TNC definitions, each advertised as a separate BLE GATT service
- Multiple concurrent BLE clients (if hardware supports it) per TNC
- Per-client KISS frame reassembly across BLE MTU boundaries
- Bidirectional bridging: BLE clients receive all frames from the TNC
- Automatic TCP reconnection with exponential backoff
- Resilient to adapters going away: if a USB Bluetooth dongle is unplugged or a
  built-in adapter is powered off, the daemon keeps running and automatically
  resumes when the adapter becomes available again
- Works with any Bluetooth 4.0 (or higher) hardware supported by Linux
- Runs on a range of operating systems and CPU architectures, including 32-bit
  ARMv6 processors like the original Raspberry Pi
- Runs from the command line or as a `systemd` service


## Installation

### From release packages

Download the latest release from the [Releases](../../releases) page. Releases are
available for amd64 (x86_64), arm64 (aarch64), and armhf (ARMv6) architectures.

- **Debian/Ubuntu**: `sudo dpkg -i kiss-tnc-bridge_*.deb`
- **RHEL/Fedora**: `sudo rpm -i kiss-tnc-bridge-*.rpm`
- **Generic Linux**: extract the tarball and copy `kiss-tnc-bridge` to `/usr/bin/`

Most Raspberry Pi models run a 64-bit OS and should use the **arm64** packages. The
**armhf** packages are for older 32-bit-only Raspberry Pi models:

- Raspberry Pi 1 Model A/A+/B/B+
- Raspberry Pi Zero and Zero W
- Raspberry Pi Compute Module 1

These older boards require 32-bit Raspberry Pi OS (Bullseye or later). If you're
running one of these boards, download the package or tarball for the `armhf`
architecture. Note that none of these boards have built-in Bluetooth Low Energy,
so you'll need a USB Bluetooth adapter — see [Recommended Hardware](#recommended-hardware).


### From source

Requires Rust toolchain and `libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel` (RHEL/Fedora):

```
$ sudo apt-get install libdbus-1-dev pkg-config   # Debian/Ubuntu
$ cargo build --release
$ sudo cp target/release/kiss-tnc-bridge /usr/bin/
```


## Usage

```
kiss-tnc-bridge [OPTIONS]

Options:
  -c, --config <FILE>   Path to config file [default: /etc/kiss-tnc-bridge.conf]
  -t, --test-config     Parse the config file and exit (0 = valid, 1 = error)
  -d, --debug           Increase log verbosity (-d = debug, -dd = trace)
  -v, --version         Show version and exit
  -h, --help            Show help and exit
```

When running from the command line, `kiss-tnc-bridge` will show the log
of events in the terminal. Type `Control-C` to quit.

The `-d` option overrides the log level in the configuration file.

### Running as a systemd service

You probably want `kiss-tnc-bridge` to run in the background when the system
starts. If you install from deb or rpm packages, this will be automatically set
up for you. If you build from source or install a tarball, you'll need to
manually install the included service file:

```
$ sudo cp kiss-tnc-bridge.service /etc/systemd/system/
$ sudo systemctl daemon-reload
$ sudo systemctl enable --now kiss-tnc-bridge
```

View logs: `journalctl -u kiss-tnc-bridge -f`


## Configuration

The config file is `/etc/kiss-tnc-bridge.conf` (INI format):

```ini
[global]
log_level = info
# adapter = hci0

[Graywolf TNC]
host = 127.0.0.1
port = 6700
max_clients = 3

[Winlink TNC]
host = 192.168.1.50
port = 8001
```

Each section (other than `[global]`) defines a KISS TNC TCP server to bridge. The section
name is used as the BLE advertised name.

### Configuration keys

**`[global]` section:**

| Key | Default | Description |
|-----|---------|-------------|
| `log_level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `adapter` | system default | BlueZ adapter name (e.g., `hci0`) |

Most computers only have a single Bluetooth adapter, and `kiss-tnc-bridge` can reliably
find it. In most scenarios, you can just omit the adapter and everything will work great.
If you have multiple adapters, or want to specify it, you can get a list:
```
$ hciconfig -a
```

**TNC sections:**

Every additional section you create in the config file represents
a new BLE GATT service advertisement. The name of the section is what
consumers will see in their applications when scanning for services.

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `host` | yes | — | TCP host of the KISS TNC server |
| `port` | yes | — | TCP port of the KISS TNC server |
| `max_clients` | no | `1` | Maximum concurrent BLE clients |
| `adapter` | no | global setting | BlueZ adapter for this TNC (overrides `[global]` adapter) |

When the maximum number of clients are connected, `kiss-tnc-bridge` will
stop advertising the service and not allow any additional clients to connect
until one of the existing clients has disconnected. Some Bluetooth chips do
not allow multiple clients to connect. See
[Limitations and Caveats](#limitations-and-caveats) below for more info about
maximum clients.


### Validate config

```
$ kiss-tnc-bridge -t -c /etc/kiss-tnc-bridge.conf
```

Exits 0 if valid, 1 if there are errors.


## Limitations and Caveats

Bluetooth Low Energy is great, but it does have some caveats and limitations you
should be aware of.

- Most Bluetooth chips have a practical limit to the number of concurrent
  connections, often in the 5-7 range. If you want to run a dozen clients, it's best
  to connect to your TNC directly via TCP instead of BLE.
- It may take 5-10 seconds from the time a connected Bluetooth device moves out
  of RF range before the operating system notifies `kiss-tnc-bridge` that the
  device is disconnected. This only matters if you have the maximum number of
  clients connected and are antsy to get another one connected.
- So far, I haven't done much testing with multiple TNCs and BLE GATT advertisements.
- A typical APRS TNC doesn't generate that much traffic or load. However, many single
  board computers have made design choices that can impact performance. For example,
  on a Raspberry Pi 4, there is a single chip supporting Wi-Fi and Bluetooth, on a
  single internal antenna. If either Wi-Fi or Bluetooth have meaningful load and/or
  the CPU is under load, you may see Bluetooth performance problems or unreliable
  connections. One possible fix is to move Wi-Fi to a 5GHz network to reduce the RF
  interference. Another possible fix is to add a USB Bluetooth dongle with an
  external antenna.
- Both iOS and Android cache BLE peripheral data using the device's Bluetooth MAC
  address. This can cause all kinds of confusing behavior. For example, connect a
  USB Bluetooth dongle to computer A with a `kiss-tnc-bridge`
  which advertises a Bluetooth TNC called "TNC 1". Then move the dongle to computer
  B, which has a `kiss-tnc-bridge` configuration which advertises a Bluetooth TNC
  called "TNC 2". When you scan in `aprs.fi` or another client program, iOS or Android
  will still show the cached value as "TNC 1". On iOS, the only way to flush the cache
  is to toggle Bluetooth off and on in Settings (it doesn't seem that the Control Center
  toggle always works). This could also show up if you have a single computer and edit
  the config file to change the TNC name and restart `kiss-tnc-bridge`. It also seems
  that connecting to the "old" TNC name and disconnecting will update the operating
  system cache name.
- Some Bluetooth chipsets have limited extended advertising support, meaning that when
  a client connects, they sometimes stop advertising until that client disconnects.
  The RTL8761B has limited extended advertising. The TP-Link UB500 Plus I have for
  testing stops advertising after the first client connects, even if you have
  `max_clients` set at 2 in `kiss-tnc-bridge.conf`.


## Recommended Hardware

Bluetooth Low Energy (BLE) appeared in version 4.0 of the Bluetooth specifications. It is
available in all hardware supporting Bluetooth 4.0 or higher (e.g. 4.2, 5.0, etc.). It is
not supported by earlier hardware. You'll need a Bluetooth chip or USB adapter which
supports Bluetooth 4.0 or higher in the computer you run `kiss-tnc-bridge` on.

You also need Ethernet or Wi-Fi if you want to connect to KISS TNCs that run on other
devices.

Here's some popular devices which `kiss-tnc-bridge` works with:

- Original Raspberry Pi Zero and Pi 1 boards do not have built-in BLE, but it can be
  added with a USB Bluetooth adapter. Use the `armhf` platform builds, which are
  compiled for armv6 architecture, which is 32 bit, and have been verified to work
  on old Pi hardware.
- Raspberry Pi Zero W, Pi Zero 2 W, and models 3, 4, and 5 have built-in BLE and
  work out of the box
- The Raspberry Pi 4 built-in Bluetooth uses a Cypress chip, which allows multiple
  clients to simultaneously connect.

If your computer doesn't have a BLE compatible chip, you can buy an inexpensive
USB adapter. If the adapter is supported in Linux and supports Bluetooth version
4.0 or higher it should work. Many USB adapters use the Realtek 8761B chipset or
one of its variants. This chipset works great, but does not allow multiple
clients to simultaneously connect. As best I can tell, all these common adapters
use that chipset:

- TP-Link UB500 Plus - I have this adapter, and mine has the Realtek 8761B
- ASUS USB-BT500
- EDUP EP-B3536
- Plugable USB Bluetooth 5 Adapter

If you plug in one of these USB Bluetooth adapters to a computer that already has a built-in
adapter, you will want to use `hciconfig -a` to find the device of your new adapter and add
that to the `kiss-tnc-bridge.conf` file to ensure that it uses the USB Bluetooth device
instead of the built-in device.

If you find hardware that works or doesn't work, please open an issue and I'll be happy
to add it to this list.


## Interoperable Software

I've tested these software applications and they work great through this bridge:

- [aprs.fi](https://apps.apple.com/us/app/aprs-fi/id922155038) iOS app
- Beta of RadioMessenger - more details coming when it's released

Open an issue if you find software that should work, but doesn't.


## Technical Architecture

`kiss-tnc-bridge` is an asynchronous daemon written in Rust on top of the
[Tokio](https://tokio.rs/) runtime. It talks to the Linux Bluetooth stack
(BlueZ) over D-Bus using the [`bluer`](https://crates.io/crates/bluer) crate,
acting as a BLE GATT **peripheral** that APRS apps connect to.

### Process and adapter supervision

At startup the daemon acquires an exclusive `flock` on `/run/kiss-tnc-bridge.lock`
and exits if another instance already holds it, so only one instance runs at a
time. It then opens a BlueZ session, registers a `NoInputNoOutput` pairing agent
(for "Just Works" pairing), and enters a **supervisor loop**.

The supervisor watches adapter lifecycle events and owns the bridge for each
adapter:

- BlueZ session events (`AdapterAdded` / `AdapterRemoved`) detect USB Bluetooth
  dongles being plugged in or unplugged.
- Per-adapter events (`Powered` on/off) detect a built-in adapter being toggled,
  e.g. via `rfkill`.

When an adapter is available, the supervisor powers it on, makes it pairable,
sets its alias, registers the GATT application, and spawns a `BridgeManager`
task per configured TNC. When an adapter disappears or is powered off, the
supervisor tears down that adapter's GATT application and bridge tasks but keeps
running, then re-establishes everything automatically when the adapter returns.

### GATT layer

Each configured TNC is advertised as one primary GATT service following the
[BLE KISS API spec](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
with two characteristics:

| UUID | Role | Properties | Direction |
|------|------|------------|-----------|
| `00000001-ba2a-46c9-ae49-01b0961f68bb` | KISS service | primary service | — |
| `00000002-ba2a-46c9-ae49-01b0961f68bb` | TX characteristic | write, write-without-response | client → bridge |
| `00000003-ba2a-46c9-ae49-01b0961f68bb` | RX characteristic | notify | bridge → client |

Both characteristics use BlueZ's I/O (file-descriptor) method, so reads and
writes are streamed over sockets rather than per-attribute D-Bus calls.

### The bridge manager

Each `BridgeManager` runs a single `tokio::select!` loop and owns:

- **one** TCP connection to the TNC server (shared by all BLE clients),
- a map of connected clients (each with its own KISS reassembly buffer),
- a map of subscribed clients (each with its own notification channel and
  writer task).

The TCP connection is opened lazily when the first client connects or
subscribes, and closed when the last client disconnects. If the TCP read or
write fails while clients are still connected, the manager reconnects with
exponential backoff (1s, doubling up to 30s).

### Data flow

Multiple BLE clients are multiplexed onto the single shared TCP socket:

```
  Uplink (BLE -> TCP), multiplexed onto one socket:

    BLE client A --TX--> reader task --> per-client KISS reassembly --+
                                                                      +--> complete frames --> TCP socket
    BLE client B --TX--> reader task --> per-client KISS reassembly --+

  Downlink (TCP -> BLE), broadcast to every client:

    BLE client A <--RX-- writer task <--+
                                        +-- broadcast <-- KISS reassembly <-- TCP socket
    BLE client B <--RX-- writer task <--+
```

- **Uplink (BLE → TCP):** each client has its own reader task and its own KISS
  reassembly buffer, so fragments from different clients are never mixed. Once a
  complete KISS frame is reassembled it is written to the shared TCP socket.
  Because writes happen inside the single select loop, frames are serialized and
  never interleaved mid-frame.
- **Downlink (TCP → BLE):** frames read from the TCP socket are reassembled and
  **broadcast** to every subscribed client via its notification channel. A
  per-client writer task chunks each frame to the negotiated MTU and sends it as
  GATT notifications. This matches KISS/APRS shared-channel semantics — every
  client hears everything the radio receives.

**Note:** the bridge never relays frames directly between BLE clients. If
client A sends a message intended for client B, `kiss-tnc-bridge` forwards it
only to the TNC. Client B will receive it only if the TNC re-broadcasts it
back over the TCP connection (for example, if the radio digipeats the frame
or the message is heard and repeated). In other words, BLE clients
communicate through the radio channel, not through the bridge itself.

### Multiple TNCs and adapters

Everything above describes a single TNC. Each TNC section in the config file is
**fully independent**: it gets its own GATT service (advertised under the section
name), its own `BridgeManager` task, and its own TCP connection to its own
configured `host:port`. The managers run concurrently as separate Tokio tasks
and share no client lists or sockets.

So the data flow scales cleanly, and the isolation extends across TNCs as well
as across clients. A client connected to one TNC's service only ever exchanges
frames with that TNC's TCP socket — there is no cross-TNC routing. A frame from
a client on "TNC 1" goes only to TNC 1's server; a client on "TNC 2" never sees
it. The two share nothing but the daemon process and the radio hardware.

TNCs are grouped onto Bluetooth adapters. By default every TNC uses the global
adapter, but each TNC can override this with its own `adapter` key. The
supervisor registers one GATT application per adapter (containing a service for
each TNC assigned to it) and spawns one `BridgeManager` per TNC. A single adapter
can therefore host several TNC services at once, or you can dedicate a separate
adapter — for example a USB dongle — to each TNC.

```
    BLE clients         One kiss-tnc-bridge process          TCP KISS TNCs

    A --+
        +-- "TNC 1" --> BridgeManager 1 (hci0) -- socket --> 127.0.0.1:6700
    B --+

    C --+
        +-- "TNC 2" --> BridgeManager 2 (hci0) -- socket --> 192.168.1.50:8001
    D --+

    E --+
        +-- "TNC 3" --> BridgeManager 3 (hci1) -- socket --> 10.0.0.5:8001
    F --+
```

In this example a single daemon advertises three TNC services. "TNC 1" and
"TNC 2" share adapter `hci0`, while "TNC 3" runs on a second adapter `hci1`.
Each `BridgeManager` multiplexes its own clients onto its own TCP socket exactly
as described in [Data flow](#data-flow) above; the three are otherwise completely
separate.

### KISS framing

KISS frames are delimited by `FEND` (`0xC0`) bytes with the standard byte
stuffing (`FESC`/`TFEND`/`TFESC`). Both the BLE and TCP sides use a reassembly
buffer that accumulates bytes and extracts complete frames, so frames split
across BLE MTU boundaries or TCP segments are handled transparently.

### Client admission and advertising

A configurable `max_clients` limit caps concurrent connections per TNC. When the
limit is reached the daemon stops advertising so new devices don't discover it,
and resumes advertising once a client disconnects and capacity frees up.
Reconnections from an already-known device address don't count against the limit.

Note that some Bluetooth chipsets have limited extended advertising support and
stop advertising on their own once a single client connects, regardless of the
`max_clients` setting (see [Limitations and Caveats](#limitations-and-caveats)).
On that hardware a TNC is effectively limited to one client at a time even if
`max_clients` is higher.


## Releases

This project uses [Semantic Versioning](https://semver.org/). See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to create a release.

Release notes for each release are in the GitHub release. We also
[keep a changelog](https://keepachangelog.com/) in [CHANGELOG.md](CHANGELOG.md).


## License

Copyright (C) 2026 Jared Crapo. GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
