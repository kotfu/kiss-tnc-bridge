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
   uses less power than WiFi.
2. On Apple's iOS, the [aprs.fi](https://apps.apple.com/us/app/aprs-fi/id922155038) app
   can not use WiFi when it's not the foreground app. But it can use bluetooth, allowing
   it to remain connected in the background.
3. I created a portable APRS station using [Graywolf](https://github.com/chrissnell/graywolf)
   on a Raspberry Pi. Graywolf can be configured to create a TCP KISS TNC so multiple
   apps can use the radio set up in Graywolf. `kiss-tnc-bridge` lets a mobile device use
   Graywolf's TNC without having to have any WiFi available.


## Features

- Multiple TNC definitions, each advertised as a separate BLE GATT service
- Multiple concurrent BLE clients per TNC (configurable limit)
- Per-client KISS frame reassembly across BLE MTU boundaries
- Bidirectional bridging: BLE clients receive all frames from the TNC
- Automatic TCP reconnection with exponential backoff
- Runs from the command line or as a `systemd` service


## Installation

### From release packages

Download the latest release from the [Releases](../../releases) page. Releases are
available in amd64 (x86_64), arm64 (aarch64), and armhf (ARMv6) architectures.

- **Debian/Ubuntu**: `sudo dpkg -i kiss-tnc-bridge_*.deb`
- **RHEL/Fedora**: `sudo rpm -i kiss-tnc-bridge-*.rpm`
- **Generic Linux**: extract the tarball and copy `kiss-tnc-bridge` to `/usr/bin/`

Most Raspberry Pi models run a 64-bit OS and should use the **arm64** packages. The
**armhf** packages are for older 32-bit-only Raspberry Pi models:

- Raspberry Pi 1 Model A/A+/B/B+
- Raspberry Pi Zero and Zero W
- Raspberry Pi Compute Module 1

These older boards require 32-bit Raspberry Pi OS (Bullseye or later). If you're
running one of these boards, download the `armhf` `.deb` or tarball. Note that
none of these boards have built-in Bluetooth Low Energy, so you'll need a USB
Bluetooth adapter — see [Supported Hardware](#supported-hardware) for recommendations.

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
starts. Use the included service file:

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
max_clients = 2
```

Each section (other than `[global]`) defines a KISS TNC TCP server to bridge. The section
name is used as the BLE advertised name.

### Configuration keys

**`[global]` section:**

| Key | Default | Description |
|-----|---------|-------------|
| `log_level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `adapter` | system default | BlueZ adapter name (e.g., `hci0`) |

Most devices only have a single bluetooth adapter, and `kiss-tnc-bridge` can reliably
find it. That means you can usually just not specify the adapter. If you have multiple
or want to specify it, you can get a list of adapters by:
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
| `max_clients` | no | `3` | Maximum concurrent BLE clients |

When the maximum number of clients are connected, `kiss-tnc-bridge` will
stop advertising the service and not allow any additional clients to connect
until one of the existing clients has disconnected. See
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

- Most bluetooth chips have a practical limit to the number of concurrent
  connections, often in the 5-7 range. If you wanna run a dozen clients, best to
  connect to your TNC directly via TCP instead of BLE.
- It may take 5-10 seconds from the time a connected bluetooth device moves out
  of RF range before the operating system notifies `kiss-tnc-bridge` that the
  device is disconnected. This only matters if you have the maximum number of
  clients connected and are antsy to get another one connected.
- So far, I haven't done much testing with multiple TNCs and BLE GATT advertisements.
- A typical APRS TNC doesn't generate that much traffic or load. However, many single
  board computers have made design choices that can impact performance. For example,
  on a Raspberry Pi 4, there is a single chip supporting WiFi and Bluetooth, on a
  single internal antenna. If either WiFi or Bluetooth have meaningful load and/or
  the CPU is under load, you may see bluetooth performance problems or unreliable
  connections. One possible fix is to move WiFi to a 5GHz network to reduce the RF
  interference. Another possible fix is to add a USB Bluetooth dongle with an
  external antenna.


## Supported Hardware

Bluetooth Low Energy (BLE) appeared in version 4.0 of the bluetooth specifications. It is
available in all hardware supporting bluetooth 4.0 or higher (eg 4.2, 5.0, etc). It is
not supported by earlier hardware. You'll need a bluetooth chip or usb adapter which
supports Bluetooth 4.0 or higher in the computer you run `kiss-tnc-bridge` on.

You may also need ethernet or WiFi if you want to connect to KISS TNCs that run on other
servers.

Here's some popular devices which `kiss-tnc-bridge` works with:

- Most Raspberry Pi models. Original Pi Zero and Pi 1 boards do not have built-in
  BLE, but it can be added with a USB Bluetooth adapter. Pi Zero W, Pi Zero 2 W,
  and models 3, 4, and 5 have built-in BLE.
- Most Orange Pi models

If your computer doesn't have a BLE compatible chip, you can buy an inexpensive USB
adapter. If the adapter is supported in Linux and supports Bluetooth version 4.0 or
higher it should work. Here's some popular adapters that are known to work:

- TP-Link UB500 Plus
- ASUS USB-BT500
- EDUP EP-B3536
- Plugable USB Bluetooth 5 Adapter

If you plug in one of these USB bluetooth adapters to a computer that already has a built-in
adapter, you will want to use `hciconfig -a` to find the device of your new adapter and add
that to the `kiss-tnc-bridge.conf` file to ensure that it uses the USB bluetooth device
instead of the built-in device.


## Releases

This project uses [Semantic Versioning](https://semver.org/). See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to create a release.

Release notes for each release are in the GitHub release. We also
[keep a changelog](https://keepachangelog.com/) in [CHANGELOG.md](CHANGELOG.md).


## License

Copyright (C) 2026 Jared Crapo. GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
