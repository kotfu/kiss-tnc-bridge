# kiss-tnc-bridge

A KISS switchboard: bridges one KISS TNC to any number of apps, over Bluetooth LE and TCP.

Advertises Bluetooth Low Energy (BLE) General Attribute Profile (GATT) following
the [BLE KISS API spec](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md).
This allows APRS apps to connect via Bluetooth and use a KISS TNC, which can be:

- the TNC built into a radio, such as the Kenwood TH-D74 or TH-D75, over
  Bluetooth or its USB cable (a stand-in for the discontinued
  [B.B. Link](https://github.com/islandmagic/bb-link/)),
- a serial KISS TNC such as a NinoTNC or TNC-Pi,
- or a TCP KISS TNC server provided by applications like:

- [Dire Wolf](https://github.com/wb2osz/direwolf)
- [Graywolf](https://github.com/chrissnell/graywolf)
- [Soundmodem](https://uz7.ho.ua/packetradio.htm)
- [QtSoundModem](https://www.cantab.net/users/john.wiseman/Documents/QtSoundModem.html)


## Why does this exist?

If you already have a KISS TNC server running on TCP, why would you want it to work via
Bluetooth?

1. In a scenario where power consumption matters more than range, Bluetooth Low Energy
   uses less power than Wi-Fi.
2. On Apple's iOS, the [aprs.fi](https://apps.apple.com/us/app/aprs-fi/id922155038) app
   can not use Wi-Fi when it's not the foreground app. But it can use Bluetooth, allowing
   it to remain connected in the background.
3. I created a portable APRS station using [Graywolf](https://github.com/chrissnell/graywolf)
   on a Raspberry Pi. Graywolf can be configured to provide a multiplexed TCP KISS TNC
   allowing many apps to use the radio. `kiss-tnc-bridge` lets mobile devices use
   Graywolf's TNC without requiring Wi-Fi.


## Features

- "Just Works" BLE pairing: client devices don't require a code to pair and
  only prompt to pair one time.
- TNCs over Bluetooth Classic (SPP), USB or UART serial, and TCP. Kenwood
  TH-D74/D75 radios are put in KISS mode automatically
- Several bridges, each advertised as its own BLE service; disabled bridges
  can be kept for other setups
- Apps over TCP too, announced on the LAN with DNS-SD so they can find the
  bridge by name
- Multiple concurrent BLE clients (if hardware supports it) per TNC
- Per-client KISS frame reassembly across BLE MTU boundaries
- Bidirectional bridging: BLE clients receive all frames from the TNC
- Automatic TNC reconnection with exponential backoff, without disturbing
  connected apps. A radio that's off, unpaired, or unplugged is waited for
- Resilient to adapters going away: if a USB Bluetooth dongle is unplugged or a
  built-in adapter is powered off, the daemon keeps running and automatically
  resumes when the adapter becomes available again
- Works with any Bluetooth 4.0 (or higher) hardware supported by Linux
- Runs on a range of operating systems and CPU architectures, including 32-bit
  ARMv6 processors like the original Raspberry Pi
- Runs from the command line or as a `systemd` service
- Minimal resource consumption. Uses 5MB of memory and negligible CPU
  on a Raspberry Pi Model B+ from 2014 while bridging a moderate suburban
  APRS feed (10 packets per minute).


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
  -c, --config <FILE>      Main config file; conf.d/ next to it is read too
                           [default: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml]
      --state-dir <DIR>    State that persists across restarts [default: /var/lib/kiss-tnc-bridge]
      --runtime-dir <DIR>  Lock file and control socket [default: /run/kiss-tnc-bridge]
      --socket <PATH>      Control socket [default: control.sock in the runtime directory]
  -t, --test-config        Check the config, print the effective bridges and exit
  -d, --debug              Increase log verbosity (-d = debug, -dd = trace)
  -V, --version            Show version and exit
  -h, --help               Show help and exit
```

When running from the command line, `kiss-tnc-bridge` shows its log in the
terminal. Type `Control-C` to quit. The `-d` option overrides the log level in
the configuration file.

The daemon is managed through its control socket,
`/run/kiss-tnc-bridge/control.sock`, which members of the
`kiss-tnc-bridge` group can use. `ktbctl` is its client; the protocol is
described in [docs/control-api.md](docs/control-api.md).

### Running as a systemd service

You probably want `kiss-tnc-bridge` to run in the background when the system
starts. The deb and rpm packages set this up. If you build from source or
install a tarball, install the included service file:

```
$ sudo cp kiss-tnc-bridge.service /etc/systemd/system/
$ sudo systemctl daemon-reload
$ sudo systemctl enable --now kiss-tnc-bridge
```

The unit passes the config, state and runtime directories to the daemon and
makes the rest of the filesystem read-only to it. View logs with
`journalctl -u kiss-tnc-bridge -f`.


## Configuration

The configuration is TOML in `/etc/kiss-tnc-bridge/`: the main file
`kiss-tnc-bridge.toml`, plus any `conf.d/*.toml` next to it (read in name
order, conventionally one bridge per file). The daemon never writes these
files. Check them with `kiss-tnc-bridge -t`.

A **bridge** connects one KISS TNC to any number of apps. It has an id
(`ktbr0`), a name (what phones see), and ports: exactly one **TNC port**,
where the TNC is, and one or more **app ports**, where apps attach. Bridges
can be disabled and kept around for another setup.

A hardware TNC, such as the one in a TH-D75, is defined once as a `[[tnc]]`
with each way to reach it, and bridges refer to it by id:

```toml
[[tnc]]
id = "d75"
name = "TH-D75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55" }      # Bluetooth; or [tnc.serial] for USB

[[bridge]]
id = "ktbr0"
name = "TH-D75"

  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"

  [[bridge.port]]
  type = "ble-server"
```

A software TNC is reached over TCP, with no `[[tnc]]`:

```toml
[global]
log_level = "info"
# adapter = "builtin"              # default adapter for ble-server ports and TNCs

[[adapter]]                        # a readable id for an adapter's MAC
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[bridge]]
id = "ktbr1"
name = "Graywolf"
enabled = true

  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"              # the TNC: a KISS TCP server
  address = "127.0.0.1:6700"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"              # apps on phones, over BLE
  adapter = "builtin"
  max_clients = 2

  [[bridge.port]]
  id = "local"
  type = "tcp-server"              # apps over TCP
  listen = "8001"                  # a TCP port number alone binds 127.0.0.1
```

### Configuration keys

**`[global]`**

| Key | Default | Description |
|-----|---------|-------------|
| `log_level` | `info` | `trace`, `debug`, `info`, `warn`, `error` |
| `adapter` | first adapter | Default Bluetooth adapter for `ble-server` ports and TNCs: an `[[adapter]]` id, a MAC, or `hciN` |

**`[[adapter]]`** gives an adapter's MAC a readable id. `hciN` numbers can
change when a dongle is plugged in; MACs don't, so ids or MACs are the
reliable way to pick an adapter.

| Key | Description |
|-----|-------------|
| `id` | Lowercase letters, digits and `-`, at most 15 characters |
| `mac` | The adapter's address, `XX:XX:XX:XX:XX:XX` (`bluetoothctl list` shows them) |

**`[[tnc]]`** is a hardware KISS TNC and the ways to reach it. A TNC can be
in only one enabled bridge, over whichever transport.

| Key | Default | Description |
|-----|---------|-------------|
| `id` | required | Lowercase letters, digits and `-`, at most 15 characters |
| `name` | the id | Human-friendly |
| `model` | `generic` | What the TNC gets beyond plain KISS; see [Models](#models). Leave it out for a TNC that isn't listed |
| `spp.address` | | The TNC's Bluetooth address, for `spp-client` ports |
| `spp.adapter` | `[global] adapter` | The adapter it's paired on. Pairings belong to one adapter |
| `spp.channel` | found by SDP | An RFCOMM channel, only for devices whose SDP record is wrong |
| `serial.path` | | The serial device, for `serial` ports. Use its `/dev/serial/by-id/` or `/dev/serial/by-path/` name: `/dev/ttyACM0` can change between boots |
| `serial.baud` | `9600` | Ignored by USB radios; matters for real UARTs |

**`[[bridge]]`**

| Key | Default | Description |
|-----|---------|-------------|
| `id` | required | Unique, short and stable: `ktbr0`, `ktbr1`, … |
| `name` | required | Human-friendly and unique; what phones see |
| `public_name` | the name | The BLE advertised name and DNS-SD name, if it should differ from `name` |
| `enabled` | `true` | A disabled bridge holds no resources |
| `app_params` | `pass` | `drop` stops apps from changing the TNC's KISS parameters (TXDELAY and friends) |

**`[[bridge.port]]`**, with `type` one of:

| Type | Role | Keys |
|------|------|------|
| `spp-client` | TNC | `tnc` (a `[[tnc]]` with an `spp` section); `connect`, `idle_timeout`, `exit_kiss_on_disconnect` (below) |
| `serial` | TNC | `tnc` (a `[[tnc]]` with a `serial` section); `connect`, `idle_timeout`, `exit_kiss_on_disconnect` (below) |
| `tcp-client` | TNC | `address` (`host:port` of a KISS TCP server such as Dire Wolf or Graywolf); `connect`, `idle_timeout` (below) |
| `ble-server` | app | `adapter` (default: `[global] adapter`); `max_clients` (default `1`; many Bluetooth chips can't serve more, see below) |
| `tcp-server` | app | `listen` (required: a TCP port number, which binds `127.0.0.1`, an `address:port`, or an `interface:port` such as `wlan0:8001`); `max_clients` (default: no limit); `allow` (a list of addresses or networks, e.g. `["192.168.4.0/24"]`; default: any peer that can reach the socket); `announce` (DNS-SD, default `true` except on loopback) |

Every port may set `id` (default: the type plus an index, `ble-server0`) and
`role` (`tnc` or `app`, normally implied by the type). TNC ports also take:

| Key | Default | Description |
|-----|---------|-------------|
| `connect` | `on_demand` | `on_demand` connects the TNC when the first app attaches; `always` keeps it connected with no apps |
| `idle_timeout` | `0s` | With `on_demand`, how long to keep the TNC connected after the last app leaves, e.g. `"90s"`. A Bluetooth link to a radio takes a few seconds to come up, so a minute or two avoids a slow reconnect when a phone briefly drops BLE |
| `exit_kiss_on_disconnect` | `false` | `spp-client` and `serial`: take the TNC out of KISS mode when the bridge lets go of it (a TH-D75 goes back to APRS mode if that's how it was found). By default the radio stays in KISS mode |

Frames from apps are filtered: the KISS "return" command (`C0 FF C0`), which
takes radios like the TH-D75 out of KISS mode for every app, never reaches
the TNC.

When a `ble-server` port has `max_clients` connected, the daemon stops
advertising until one leaves. Several bridges can be defined, but among the
enabled ones a listen socket, a public name and a BLE adapter can each be
used only once.

### Sharing over the network

**KISS over TCP has no authentication: anyone who can reach a `tcp-server`
can transmit on your radio under your callsign.** That's why a bare TCP port
number binds `127.0.0.1`, and why binding to `0.0.0.0` without an `allow`
list gets a warning. To share a radio with a laptop on the Pi's Wi-Fi access
point, bind to that interface and limit who may connect:

```toml
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "wlan0:8001"              # only on the access point's interface
  allow = ["192.168.4.0/24"]         # only its clients
```

A `tcp-server` that isn't on loopback is announced on the LAN with DNS-SD
(`_kiss-tnc._tcp`, following the
[TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md)),
so apps like aprs.fi on iOS can pick the bridge from a list. The name is the
bridge's `public_name`, the same name its BLE advertisement uses. An
interface binding is announced only on that interface. This needs Avahi,
which Raspberry Pi OS runs; without it the daemon logs that once and carries
on. If another device on the LAN already uses the name, the bridge takes the
next one ("TH-D75 (2)") for DNS-SD and BLE alike, and keeps it across
restarts in `/var/lib/kiss-tnc-bridge/state.toml`. Changing `public_name` in
the config drops the adjustment. `avahi-browse -r _kiss-tnc._tcp` on Linux
or `dns-sd -B _kiss-tnc._tcp` on a Mac shows what's announced.

### Validate config

```
$ kiss-tnc-bridge -t
```

Prints the effective TNCs and bridges with their source files, and warnings
such as a `tcp-server` open to the whole network, a TNC that isn't paired on
its adapter, or a serial path that isn't stable. Exits 0 if valid, 1 if there
are errors. A leftover 0.5 config at `/etc/kiss-tnc-bridge.conf` is ignored with
a warning; it isn't migrated.


## Kenwood TH-D74 and TH-D75

The TH-D74 and TH-D75 have a KISS TNC built in, reachable over Bluetooth or
over the radio's USB cable, but only one of those at a time: the radio's
`Menu > Configuration > Interface > KISS (983)` setting picks which. With
`model = "th-d75"` (or `th-d74`; they're the same), the bridge puts the TNC
in KISS mode every time it connects, on whichever band the radio's TNC is
set to. You don't need to turn KISS on from the radio's menu.

### Over Bluetooth

1. On the radio, set `KISS (983)` to Bluetooth and turn Bluetooth on.
2. Pair the radio once on the Pi, on the adapter the bridge will use for it.
   The radio uses numeric comparison: it shows a six-digit number, and you
   press OK on the radio within 30 seconds. Put the radio in pairing mode
   (`Menu > Bluetooth > Pairing Mode`), then:

   ```
   $ bluetoothctl
   [bluetooth]# list                           # adapters, if you have more than one
   [bluetooth]# select 2C:CF:67:6E:2E:BC       # the adapter to pair on
   [bluetooth]# agent KeyboardDisplay
   [bluetooth]# scan bredr
   [NEW] Device 40:79:12:E4:95:9D TH-D75
   [bluetooth]# scan off
   [bluetooth]# pair 40:79:12:E4:95:9D
   [agent] Confirm passkey 774523 (yes/no): yes
   ```

   Check that the number matches the radio's and press OK on the radio. If
   pairing fails with `Authentication Failed` after about 30 seconds, OK
   wasn't pressed in time: dismiss any prompt still on the radio, put it in
   pairing mode again and retry. Then trust it, so reconnects need no
   prompt:

   ```
   [bluetooth]# trust 40:79:12:E4:95:9D
   [bluetooth]# quit
   ```

   The daemon can stay running while you pair.
3. Configure a `[[tnc]]` with the radio's address and the adapter it's
   paired on, and a bridge with an `spp-client` port for it:

   ```toml
   [[adapter]]
   id = "builtin"
   mac = "2C:CF:67:6E:2E:BC"

   [[tnc]]
   id = "d75"
   name = "TH-D75"
   model = "th-d75"
   spp = { address = "40:79:12:E4:95:9D", adapter = "builtin" }

   [[bridge]]
   id = "ktbr0"
   name = "TH-D75"

     [[bridge.port]]
     id = "radio"
     type = "spp-client"
     tnc = "d75"
     idle_timeout = "90s"

     [[bridge.port]]
     id = "phones"
     type = "ble-server"
   ```

4. Run `kiss-tnc-bridge -t`. It warns if the radio isn't paired on that
   adapter; the bridge would report the port as unpaired and wait.

With the default `connect = "on_demand"`, the bridge connects to the radio
when the first app attaches. A connect takes about 5 seconds from cold, and
a radio that's off or out of range takes about 5 seconds to fail, after
which the bridge retries with backoff while apps stay connected.

### Over USB

1. On the radio, set `KISS (983)` to USB, and connect it to the Pi with its
   USB cable.
2. Find its stable device name:

   ```
   $ ls -l /dev/serial/by-id/
   usb-JVCKENWOOD_TH-D75-if00 -> ../../ttyACM0
   ```

3. Give the TNC a `serial` section, and the bridge a `serial` port:

   ```toml
   [[tnc]]
   id = "d75"
   name = "TH-D75"
   model = "th-d75"

     [tnc.serial]
     path = "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00"

   [[bridge]]
   id = "ktbr3"
   name = "TH-D75 USB"

     [[bridge.port]]
     id = "radio"
     type = "serial"
     tnc = "d75"

     [[bridge.port]]
     id = "phones"
     type = "ble-server"
   ```

The same `[[tnc]]` can have both an `spp` and a `serial` section, with one
bridge for each that you enable as needed (only one at a time, since the
radio's TNC is on one interface at a time). When the cable is unplugged or
the radio turned off, the port waits for the device to come back and
reconnects.

The D75 reports no USB serial number, so its `by-id` name is the same for
every D75. With two of them on one Pi, use their `/dev/serial/by-path/`
names instead, which follow the USB port they're plugged into.

On desktops that run ModemManager (Ubuntu, for example), it probes new USB
serial devices with modem commands, which can confuse a radio and hold its
port. The packages install a udev rule
(`/lib/udev/rules.d/70-kiss-tnc-bridge.rules`) that tells ModemManager to
leave the TH-D75 alone; the file explains how to add other TNCs. Raspberry Pi
OS doesn't run ModemManager.

### When the radio doesn't transmit

If an app sends a beacon and the radio doesn't key up, look for a red **STA**
on the radio's display. It means the radio's TNC has the packet and is
waiting for the channel to clear before transmitting, as packet TNCs always
have. The radio transmits once the channel is clear, and STA goes out.

If STA stays lit, the radio thinks the channel is busy: there's traffic on
the frequency, or the squelch is open (set too low, or opened by noise).
Close the squelch, or check the radio's DCD sense setting (`Detect Data`,
in the APRS settings; menu 507 on some Kenwood radios), which decides what
counts as busy. Turning the radio off and on clears a stuck queue.

The bridge can't see any of this. Once it has handed a frame to the radio,
its counters, logs and `ktbctl` count the frame as sent, whether or not the
radio has transmitted it yet.

### Other TNCs

Any TNC that speaks KISS over Bluetooth SPP or a serial port should work
with the generic model, as long as it's already in KISS mode (most
standalone TNCs always are). For example, a Mobilinkd TNC3 over Bluetooth,
shared to phones over BLE:

```toml
[[tnc]]
id = "mobilinkd"
name = "Mobilinkd TNC3"
spp = { address = "00:11:22:33:44:55" }   # the TNC's Bluetooth address
# Over USB instead, use its stable name from ls /dev/serial/by-id/:
# serial = { path = "/dev/serial/by-id/usb-…-if00" }

[[bridge]]
id = "ktbr0"
name = "Mobilinkd"

  [[bridge.port]]
  id = "tnc"
  type = "spp-client"              # "serial" for USB
  tnc = "mobilinkd"
  idle_timeout = "90s"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
```

Pair it once with `bluetoothctl` as for the TH-D75; it may ask for a PIN or
nothing at all instead of a number to compare. TNCs whose Bluetooth is BLE
only, such as the Mobilinkd TNC4, BTech UV-PRO and Vero VR-N76, can only be
used over USB for now; connecting to BLE TNCs comes after 1.0.

With the generic model the bridge sends the TNC nothing, so it can't tell a
TNC that's quiet from one that isn't working. If apps hear nothing, the
frame counts in the log (`frames_down` when the TNC link closes) show
whether anything arrived from the TNC. Please open an issue if you try a
TNC, working or not.

### Models

`model` in a `[[tnc]]` says what the TNC gets beyond plain KISS. Leaving it
out means `generic`. Any other value is an error, and `kiss-tnc-bridge -t`
lists the models.

| `model` | For | What it does |
|---|---|---|
| `generic` (default) | Any TNC that's always in KISS mode: NinoTNC, TNC-Pi, Mobilinkd | Nothing beyond KISS. With `exit_kiss_on_disconnect`, sends KISS "return" (`C0 FF C0`) when the bridge lets go |
| `th-d74`, `th-d75` | Kenwood TH-D74 and TH-D75 | Switches the TNC to KISS mode at every connect; fails with a pointer to menu 983 if KISS is on the other interface; reconnects if the radio stops answering (as it does after a USB replug); with `exit_kiss_on_disconnect`, puts the TNC back in the mode it was found in |

Radios that need commands of their own get a new model once someone can
test one.


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
- Some Bluetooth controllers stop advertising while a phone is connected, so a
  second phone can't find the bridge even with `max_clients = 2`. The Raspberry
  Pi 4's built-in chip does this. The RTL8761B in common USB dongles keeps
  advertising and served two phones at once in testing.


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
- The Raspberry Pi 4's built-in Bluetooth (a Cypress chip) handles a radio over
  Bluetooth Classic and a phone over BLE at the same time: an hour of aprs.fi
  traffic in testing with no drops. But it stops advertising while a phone is
  connected, so a second phone can't discover the bridge.

If your computer doesn't have a BLE compatible chip, you can buy an inexpensive
USB adapter. If the adapter is supported in Linux and supports Bluetooth version
4.0 or higher it should work. Many USB adapters use the Realtek 8761B chipset or
one of its variants. It keeps advertising with a phone connected, and served
two phones at once in testing. As best I can tell, all these common adapters
use that chipset:

- TP-Link UB500 Plus - I have this adapter, and mine has the Realtek 8761B
- ASUS USB-BT500
- EDUP EP-B3536
- Plugable USB Bluetooth 5 Adapter

If you plug in one of these USB Bluetooth adapters to a computer that already has a built-in
adapter, use `bluetoothctl list` to find each adapter's address, and give them ids with
`[[adapter]]` so each port uses the adapter you mean; `hciN` numbers can swap when
adapters are plugged in.

For a radio over Bluetooth plus more than one phone, the recommended layout on a Pi is the
radio on the built-in chip (the TNC's `spp.adapter`) and the phones on a USB dongle (the
`ble-server` port's `adapter`). Each adapter can carry only one enabled `ble-server` port.

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

BlueZ remembers a bonded phone's subscription to the RX characteristic after
its link drops, and when the phone comes back and subscribes again, BlueZ
doesn't tell the daemon, so the phone would look connected but get nothing.
When a subscribed phone leaves without unsubscribing (it went out of range,
turned Bluetooth off, or was kicked), the daemon registers the service again,
which makes BlueZ forget. It does that as soon as no other phone is
connected; if others are, it waits until the phone comes back and fails to
subscribe, then does it anyway, and the other phones resubscribe when BlueZ
tells them the service changed.

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

Note that some Bluetooth controllers, such as the Raspberry Pi 4's built-in
chip, stop advertising on their own once a single client connects, regardless
of the `max_clients` setting (see [Limitations and Caveats](#limitations-and-caveats)).
On that hardware a second phone can only connect if it reconnects on its own,
without discovering the bridge.


## Releases

This project uses [Semantic Versioning](https://semver.org/). See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to create a release.

Release notes for each release are in the GitHub release. We also
[keep a changelog](https://keepachangelog.com/) in [CHANGELOG.md](CHANGELOG.md).


## License

Copyright (C) 2026 Jared Crapo. GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
