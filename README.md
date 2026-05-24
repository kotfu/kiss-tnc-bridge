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
   it to remain connected in the background
3. I created a portable APRS station using [Graywolf](https://github.com/chrissnell/graywolf)
   on a Raspberry Pi. Graywolf can be configured to create a TCP KISS TNC so multiple
   apps can use the radio set up in Graywolf. `kiss-tnc-bridge` let's a mobile device use
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

Download the latest release from the [Releases](../../releases) page:

- **Debian/Ubuntu**: `sudo dpkg -i kiss-tnc-bridge_*.deb`
- **RHEL/Fedora**: `sudo rpm -i kiss-tnc-bridge-*.rpm`
- **Generic Linux**: extract the tarball and copy `kiss-tnc-bridge` to `/usr/bin/`

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

### Running as a systemd service

```
sudo cp kiss-tnc-bridge.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now kiss-tnc-bridge
```

View logs: `journalctl -u kiss-tnc-bridge -f`


## Configuration

The config file is `/etc/kiss-tnc-bridge.conf` (INI format):

```ini
[global]
log_level = info
# adapter = hci0

[APRS iGate]
host = 127.0.0.1
port = 8001
max_clients = 3

[Winlink TNC]
host = 192.168.1.50
port = 8001
max_clients = 2
```

Each section (other than `[global]`) defines a KISS TNC TCP server to bridge. The section name is used as the BLE advertised name.

### Configuration keys

**`[global]` section:**

| Key | Default | Description |
|-----|---------|-------------|
| `log_level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `adapter` | system default | BlueZ adapter name (e.g., `hci0`) |

**TNC sections:**

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `host` | yes | — | TCP host of the KISS TNC server |
| `port` | yes | — | TCP port of the KISS TNC server |
| `max_clients` | no | `3` | Maximum concurrent BLE clients |

### Validate config

```
kiss-tnc-bridge -t -c /etc/kiss-tnc-bridge.conf
```

Exits 0 if valid, 1 if there are errors.



## Releasing

This project uses [semver](https://semver.org/). Releases are built automatically by GitHub Actions when a tag starting with `v` is pushed:

```
git tag v0.1.0
git push --tags
```

This produces:

- Generic Linux binaries (x86_64 and arm64) as `.tar.gz`
- Debian packages (`.deb`) for amd64 and arm64
- RPM packages (`.rpm`) for x86_64 and aarch64

All artifacts are attached to the GitHub Release.

## License

Copyright (C) 2026 Jared Crapo. GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
