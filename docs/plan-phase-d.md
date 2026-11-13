# Phase D: `ktbctl` device support

Status: planned 12 November 2026. Ships in software 1.3.0, with firmware
1.3.0.

`ktbctl` installs our firmware on a device over USB, then manages it over
the same cable, so a Linux or macOS machine is all it takes to set one up
(roadmap decision 13). Two parts, in this order:

- **D1: `ktbctl device flash`**, writing firmware over USB serial,
  including the first install.
- **D2: management over USB serial**, the control API on the device's
  serial port, for the commands `ktbctl` already has and the device-only
  ones.

Not in this phase:

- **BLE management from `ktbctl`** moved to [Phase E](roadmap.md#phase-e-ios-app-and-ktb-gateway-ble-management),
  with the firmware's BLE management service it needs.
- **Windows.** `ktbctl` doesn't build there: it talks to the daemon over a
  Unix socket. Linux and macOS only.
- **The StickC Plus SE.** It isn't released yet, and there's none to test
  on. Nothing here should rule it out (see [Telling the boards
  apart](#3-find-the-device-and-whats-on-it)).

Tested on the two boards there are: the ATOM Lite (FTDI USB serial) and
the Core2 v1.3 (WCH CH9102).

## What it builds on

- [web-installer.md](web-installer.md) is the contract for installing: the
  release files, the manifest, the offsets, update against clean install,
  and the order of the steps. `ktbctl device flash` follows the page's
  steps 3 to 7, so the two behave the same.
- [`firmware/tools/package.py`](../firmware/tools/package.py) and
  [`firmware.yml`](../.github/workflows/firmware.yml) already publish a
  manifest and four parts per board, with each part's SHA-256 and size.
- [control-api.md, USB serial](control-api.md#usb-serial): 115200 8N1,
  newline-delimited JSON, log lines skipped, and the port opened without
  resetting the device (RTS dropped before DTR).
  [`firmware/tools/ktbdev.py`](../firmware/tools/ktbdev.py) does it in
  Python, and D2 replaces it apart from the `debug.*` commands.
- `ktbctl`'s `Client` ([src/ctl/client.rs](../src/ctl/client.rs)) speaks
  the same JSON over the Unix socket, and
  [src/port/serial_list.rs](../src/port/serial_list.rs) lists serial
  devices on Linux with their USB ids from sysfs.

## The command line

A sketch; names can change as it's built.

```
ktbctl device list
ktbctl device flash [--device PORT] [--manifest FILE | --version VERSION]
                    [--prerelease] [--clean] [--board BOARD] [--baud N] [--yes]
ktbctl device info
ktbctl device settings
ktbctl device settings set KEY=VALUE ...
ktbctl device reboot | reset | identify
ktbctl auth code | rotate-code | mode SECONDS | apps

ktbctl --device PORT status | scan | pair | unpair | tnc ... | monitor | ...
```

- **`--device PORT`** is global, with `KTBCTL_DEVICE` as its environment
  variable, and conflicts with `--socket`. It sends commands to a device
  over USB serial instead of to the daemon. Phase E adds `ble:<name>`.
- **Without `--device`,** the `device` and `auth` commands look for one
  device on USB by its chip's ids: FTDI `0403:6001` (the ATOM Lite) and
  WCH `1a86:55d4` (the Core2). One match is used. More than one lists
  them and stops. Other FTDI devices share the ATOM's ids, so a match is
  only a likely device until it answers.
- On macOS the ports are the `/dev/cu.*` ones, never `/dev/tty.*`, which
  wait for carrier detect.

## D1: `ktbctl device flash`

### 1. Spike: `espflash` as a library

[espflash](https://github.com/esp-rs/espflash) talks to the ESP32's ROM
bootloader, loads the flasher stub, and writes compressed images with an
MD5 check. Before depending on it, find out:

- [x] It builds with its default features off, on macOS and with `cross`
      for aarch64 and armhf Linux, **without libudev**. `tokio-serial`
      already uses the `serialport` crate with its default features off,
      and Cargo merges features, so if `espflash` turns on
      `serialport/libudev` it does so for the whole of `ktbctl` and the
      daemon. That breaks the cross builds.
- [x] It connects to the ROM on both boards through their auto-reset
      circuits, and reads the chip, its package, the MAC and the flash
      size: ESP32-PICO-D4 with 4 MB on the ATOM Lite, ESP32-D0WDQ6-V3 with
      16 MB on the Core2. If it doesn't report the package, read it from
      the eFuses.
- [x] The fastest baud rate that writes reliably on each board.
- [x] How long a full install takes on each board, and how much
      `espflash` adds to `ktbctl`'s release binary.

Done 12 November 2026 on macOS, and on a Pi 4 with each board: see
[spikes/espflash-usb](../spikes/espflash-usb/README.md). `espflash` 4.7.0
works on both boards:

- No libudev, and one `serialport` shared with `tokio-serial`. Cross
  builds work for both Pis. It adds about 650 KB to a Linux binary
  (`ktbctl` for armhf is 3.3 MB now).
- **1500000 baud works on both boards; 921600 doesn't on the ATOM Lite.**
  Its USB serial chip is a CH552 presenting itself as FTDI, and it only
  does some rates, on macOS and Linux alike. At 1500000 an update takes
  16 to 17 s on the ATOM Lite and 17 to 20 s on the Core2, and a clean
  install 17 s and 20 s; at 115200 an update takes 71 to 82 s on the
  ATOM Lite and 96 s on the Core2. A clean install gives the device a new
  key and authorization code.
- On Linux, `serialport` lists ports by device name (`/dev/ttyACM0`), so
  a `/dev/serial/by-id` path has to be resolved before matching.
- The package isn't in espflash's `DeviceInfo`; read it from the eFuses.
- Connecting takes 7 to 8 s, 6 s of it waiting on a command the original
  ESP32's ROM doesn't answer. Worth fixing upstream.
- Write every part in one call, and reset the device after a failure:
  a failed flash leaves it in download mode.

Not yet tried: the armhf build on a board, and WCH's macOS driver for
the Core2. They're for D1's testing, not the decision.

Decided 12 November 2026: `espflash` is a library compiled into
`ktbctl`, with no cargo feature to leave it out. The packages still ship
two binaries and need no new libraries; only `ktbctl` grows, since the
daemon doesn't call it. The spike's 650 KB was low: with the flash
command built, `espflash` and its code add 2.0 MB to `ktbctl` on aarch64
(3.35 to 5.38 MB), and the GitHub download (`ureq`, rustls and ring)
another 1.6 MB, to 6.95 MB. The armhf build goes from 3.54 to 6.71 MB.

### 2. Shared groundwork

Needed by both D1 and D2, so built first:

- [x] Opening a device's port without resetting it: opening asserts both
      lines, then drop RTS, then DTR, as `ktbdev.py` does. Opening and
      closing the port doesn't reboot either board on macOS or Linux.
- [x] `device.info` over the port: send it, read lines, skip any that
      don't start with `{`, and wait up to 3 s, sending it again once a
      second.
- [x] Finding devices: `serial_list.rs` on Linux, `serialport`'s list on
      macOS. `ktbctl device list` shows each likely port, and what answers
      on it. `--device` takes a `/dev/serial/by-id` path or a device name;
      resolve both before matching one against the other, since
      `serialport` lists only device names (found in the spike).
      `--device` works only with the `device` commands until D2.

Done 12 November 2026, in `src/ctl/device/`, with `ktbctl device info`
from D2 to try the probe on its own. Tried on macOS, and on aprscc with
the ATOM Lite.

### 3. Find the device and what's on it

Before anything is written, as in the web installer's step 4:

- **Our firmware answers `device.info`:** `ktbctl` shows its board, version
  and fingerprint.
- **Nothing answers:** the device has other firmware or none. `ktbctl`
  connects to the ROM and tells the board from the chip and flash size:
  PICO-D4 with 4 MB is an ATOM Lite, D0WDQ6-V3 with 16 MB a Core2. The
  StickC Plus SE has the ATOM's chip and flash, so a PICO-D4 that
  doesn't answer needs `--board` (already, since a StickC is on hand).
- A chip that matches no board stops here, saying what was found.

Done 12 November 2026. The ROM is asked only when the board is needed
and the firmware doesn't say: to pick a GitHub release, or to check a
manifest's board. The install checks the chip against the board again
before writing.

### 4. Firmware from local files, or from a GitHub release

Both, as options:

- **`--manifest FILE`:** a manifest from a release or from `package.py`,
  with its parts beside it. This covers development builds and machines
  with no internet.
- **Downloaded from GitHub releases** when no `--manifest` is given: the
  newest firmware release that isn't a prerelease, or `--version VERSION`,
  with `--prerelease` to include betas and release candidates. Firmware
  releases are the `firmware-v*` tags, never marked latest (that's the
  daemon's), so `ktbctl` lists releases with GitHub's API and picks the
  newest itself. Unauthenticated, the API allows 60 requests an hour,
  which a CLI doesn't come near. The parts come from the release's
  download URLs; CORS stops a browser fetching them, not `ktbctl`.
- **Cached** under `~/.cache/ktbctl/firmware/<version>/`, since released
  files never change.
- **HTTP** needs a client that cross-builds without OpenSSL: `ureq` with
  rustls and ring, which builds for both Pis.

Done 12 November 2026, in `src/ctl/device/manifest.rs` and `release.rs`.
A manifest's parts have to be plain file names beside it. A version
already in the cache installs without the network.

Whichever way they come, before the device is touched: every part's
SHA-256 and size against the manifest, `chipFamily` `ESP32`, and the
manifest's `board` against the device's. A mismatch writes nothing.

### 5. Update or clean install

The web installer's choices:

- **Update**, the default when the device runs our firmware: write every
  part in the manifest. NVS isn't written, so the settings, the radio
  pairing, the authorized apps, the device key and the authorization code
  stay. The OTA data part makes it boot the first app partition, where the
  new firmware goes.
- **Clean install** with `--clean`, and the only choice over other
  firmware: erase the whole flash first. It asks first, or goes ahead with
  `--yes`, and says that every authorized app is lost and the device gets
  a new key and authorization code.
- **An older version** is allowed, with a warning that settings written by
  a newer one may not all be understood.

### 6. Flash

- [ ] Tell the firmware a flash is coming (see [7](#7-the-install-notice)).
- [x] Reset into the ROM, load the stub, and switch to 1500000 baud,
      falling back to 115200. If anything fails once in the ROM, reset the
      device so it doesn't stay in download mode.
- [x] Erase for a clean install, then write each part compressed and
      check its MD5.
- [x] Progress on stderr: the step (erasing, writing, verifying) and the
      bytes written. With `--json`, progress as JSON lines and a final
      result.
- [x] Reset with RTS, so it boots the new firmware.
- [x] Reopen the port and wait up to 15 s for `device.info`. Check the
      version is the one just written.
- [x] After a clean install, print the authorization code from
      `auth.code`, grouped as the device gives it, and say what it's for.
      Naming the device and the rest of first setup are D2's
      `device settings set`.

Done 12 November 2026, in `src/ctl/device/flash.rs` and `rom.rs`. Tried:

- The Core2 on macOS: an update from a local manifest, 24 s. With no
  `--manifest`, GitHub has no Core2 firmware until 1.2.0 is tagged, and
  `ktbctl` says so.
- The ATOM Lite on aprscc: an update from GitHub (1.1.0, with the warning
  that it's older than the 1.2.0-dev it ran), 21 s with the download; a
  clean install with `--yes`, with its new authorization code; an update
  from a local manifest, keeping it; `--json`; a clean install refused
  without `--yes` and without a terminal; and `--baud 921600`, which
  fails, resets the device, and finishes at 115200 in 82 s.
- The ATOM Lite left in its download mode: `ktbctl` asks the ROM, finds
  the PICO-D4 and refuses without `--board`, and its reset brings the
  firmware back.

Found on the way: a firmware that's starting answers each `device.info`
it was sent while it started, so late answers reach the next request.
Each request now has its own id, and opening the port drops what's
waiting in it.

Not yet tried: a clean install over other firmware (the StickC Plus SE's
factory firmware would do), the ROM telling a Core2 on its own, and the
armhf build on wolfcub.

Errors say what to try, from the web installer's table: a cable that
carries data, another USB port, the port held by another program, the
`dialout` group (`uucp` on Arch), the ROM not answering, a damaged
download, and unplugging partway (install again: the ROM bootloader is in
the chip, so nothing is lost that another install can't fix).

### 7. The install notice

Phase G adds a control API command that tells our firmware a USB install
is coming: the Core2 shows "Installing firmware over USB. Keep it
plugged in; it restarts when done.", and the ATOM Lite holds an LED color
([#22](https://github.com/kotfu/kiss-tnc-bridge/issues/22)). It's defined
and built on the firmware side first, in firmware 1.3.0.

- [ ] Once it's defined: `ktbctl device flash` sends it before resetting
      the device, when `device.info` announces its capability. Firmware
      before 1.3.0 doesn't, and is flashed without it.

## D2: management over USB serial

- [x] **A serial transport for `Client`.** The reader and writer become
      the Unix socket's or the serial port's; the protocol is the same
      newline-delimited JSON, with log lines skipped on serial.
- [x] **`--device PORT` for the existing commands** a device answers:
      `status`, `scan` (Classic, and `--le`), `pair` with the TH-D75's
      confirm step, `unpair`, `tnc update` (model, `spp.address`,
      `ble.address`), `tnc models`, `connect`, `disconnect`, `release`,
      `kick` and `monitor`. `status` shows a device's one bridge without
      `since` times. A command the device refuses with `not_supported`
      says it isn't available on a device, not that something failed.
- [x] **Device-only commands:** `device info`, `device settings`, `device
      settings set` (and which settings need `device reboot`), `device
      reboot`, `device reset` (asks first, or `--yes`: it clears the
      settings, the radio and the authorized apps), and `device identify`.
- [x] **`auth` commands:** `auth info`, `auth code`, `auth rotate-code`
      (asks first), `auth mode SECONDS` and `auth apps`. Phase E reuses
      them over BLE and for `ktb-gateway`.
- [x] **`--json`** for every command, as for the daemon's.
- [x] **`ktbdev.py`** keeps only what `ktbctl` doesn't do: the `debug.*`
      commands, the LED demo and the raw log. Gone with the rest: `watch`
      (`ktbctl monitor` shows frames, not `state` or `activity` events)
      and `raw` (any command, by name).
- [ ] The TUI against a device is not in this phase.

Done 12 November 2026. How it works:

- **Where commands go.** `--device PORT` or `KTBCTL_DEVICE` sends every
  command to the device; without either they go to the daemon. The
  `device` and `auth` commands always go to a device, the only likely one
  on USB if none is named. Connecting asks `device.info` first, so a
  board running other firmware fails at once rather than leaving a
  request waiting.
- **Daemon-only commands** (`setup`, `bridge`, `port`, `tnc add` and
  `rm`, `adapter add` and `rm`, `enable`, `disable`, `switch`, `edit`,
  `check`, `reload`, `serial list`, the TUI) are refused before anything
  is sent.
- **`tnc update`** is new, and for devices only: on the daemon
  `tnc.update` replaces the whole definition, which a few flags would cut
  down, so there it's still `ktbctl edit tnc`.
- **`monitor` on a device** unsubscribes when it's stopped: a device's
  subscriptions outlive the port being closed.
- **The protocol types** gained the `device.*` and `auth.*` commands, so
  they're in the published schema, and the daemon refuses them with
  `not_supported`. `ErrorCode` gained `not_supported`, and reads a code it
  doesn't know as `unknown` instead of failing. `status` fields a device
  leaves out became optional: `source`, a bridge's `editable`, a port's
  `settings` and a session's `since`.

Tried on the Core2 on macOS (every command but `pair`, which needs a
radio in pairing mode, and `kick`, with a phone connected) and the ATOM
Lite on aprscc.

Found in the firmware: `bluetooth.unpair` answers `{}`, where the daemon
answers with the address, the adapter and `paired`, which control-api.md
says a device does too. `ktbctl` takes either.

## Testing

On macOS and Linux (a Pi 4, and a Pi 1 B+ for a slow USB host), with an
ATOM Lite and a Core2:

- [ ] Opening and closing the port with `ktbctl` doesn't reboot either
      board.
- [ ] Clean install over M5Stack's factory firmware, over B.B. Link (ATOM
      Lite), and over a blank flash.
- [ ] Update with the settings, the radio pairing and the authorization
      code kept.
- [ ] Going back to an older version.
- [ ] Unplugging mid-install, then installing again.
- [ ] From a local manifest, and from a GitHub release, cached and not.
- [ ] A part whose hash doesn't match writes nothing.
- [ ] A manifest for the other board is refused.
- [ ] On Linux, ModemManager doesn't grab the port mid-install.
- [ ] The Core2's CH9102 on macOS, with Apple's driver and with WCH's.
- [ ] D2's commands against both boards, including pairing a TH-D75 and
      selecting a TD-H9 with `tnc update`.

## Docs and release

- [ ] `ktbctl`'s help, the README, and a page on installing and managing a
      device from the command line.
- [ ] [control-api.md](control-api.md): "Trying it by hand" points to
      `ktbctl --device`; the USB serial section names `ktbctl`.
- [ ] [web-installer.md](web-installer.md): `ktbctl device flash` as the
      way in without Chrome.
- [ ] The roadmap's Phase D items ticked.
- [ ] Software 1.3.0 and firmware 1.3.0 released together; each changelog
      names the other.

## Open

- **The install notice:** its name, its capability and the Core2's screen
  are defined in Phase G; the ATOM Lite's color in
  [#22](https://github.com/kotfu/kiss-tnc-bridge/issues/22).
