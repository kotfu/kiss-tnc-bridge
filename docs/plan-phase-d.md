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

Done 12 November 2026 on macOS, and on a Pi 4 with the Core2: see
[spikes/espflash-usb](../spikes/espflash-usb/README.md). `espflash` 4.7.0
works on both boards:

- No libudev, and one `serialport` shared with `tokio-serial`. Cross
  builds work for both Pis. It adds about 650 KB to a Linux binary
  (`ktbctl` for armhf is 3.3 MB now).
- **1500000 baud works on both boards; 921600 doesn't on the ATOM Lite.**
  Its USB serial chip is a CH552 presenting itself as FTDI, and it only
  does some rates. An update takes 17 s on the ATOM Lite and 17 to 20 s
  on the Core2 at 1500000; at 115200, 82 s on the ATOM Lite and 96 s on
  the Core2. A clean install of the Core2 takes 20 s, and gives it a new
  key and authorization code.
- On Linux, `serialport` lists ports by device name (`/dev/ttyACM0`), so
  a `/dev/serial/by-id` path has to be resolved before matching.
- The package isn't in espflash's `DeviceInfo`; read it from the eFuses.
- Connecting takes 7 to 8 s, 6 s of it waiting on a command the original
  ESP32's ROM doesn't answer. Worth fixing upstream.
- Write every part in one call, and reset the device after a failure:
  a failed flash leaves it in download mode.

Not yet tried: the ATOM Lite on Linux, the armhf build on a board, and
WCH's macOS driver for the Core2. They're for D1's testing, not the
decision.

Decided 12 November 2026: `espflash` is a library compiled into
`ktbctl`, with no cargo feature to leave it out. The packages still ship
two binaries and need no new libraries; only `ktbctl` grows, by about
650 KB, since the daemon doesn't call it.

### 2. Shared groundwork

Needed by both D1 and D2, so built first:

- [ ] Opening a device's port without resetting it: assert DTR, drop RTS,
      then drop DTR, as `ktbdev.py` does. Check on both boards and both
      platforms that opening and closing the port doesn't reboot them.
- [ ] `device.info` over the port: send it, read lines, skip any that
      don't start with `{`, and wait up to 3 s, sending it again once a
      second.
- [ ] Finding devices: `serial_list.rs` on Linux, `serialport`'s list on
      macOS. `ktbctl device list` shows each likely port, and what answers
      on it. `--device` takes a `/dev/serial/by-id` path or a device name;
      resolve both before matching one against the other, since
      `serialport` lists only device names (found in the spike).

### 3. Find the device and what's on it

Before anything is written, as in the web installer's step 4:

- **Our firmware answers `device.info`:** `ktbctl` shows its board, version
  and fingerprint.
- **Nothing answers:** the device has other firmware or none. `ktbctl`
  connects to the ROM and tells the board from the chip and flash size:
  PICO-D4 with 4 MB is an ATOM Lite, D0WDQ6-V3 with 16 MB a Core2. The
  StickC Plus SE has the ATOM's chip and flash, so once it's released a
  PICO-D4 is ambiguous and needs `--board`.
- A chip that matches no board stops here, saying what was found.

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
  rustls is the likely choice.

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
- [ ] Reset into the ROM, load the stub, and switch to 1500000 baud,
      falling back to 115200. If anything fails once in the ROM, reset the
      device so it doesn't stay in download mode.
- [ ] Erase for a clean install, then write each part compressed and
      check its MD5.
- [ ] Progress on stderr: the step (erasing, writing, verifying) and the
      bytes written. With `--json`, progress as JSON lines and a final
      result.
- [ ] Reset with RTS, so it boots the new firmware.
- [ ] Reopen the port and wait up to 15 s for `device.info`. Check the
      version is the one just written.
- [ ] After a clean install, print the authorization code from
      `auth.code`, grouped as the device gives it, and say what it's for.
      Naming the device and the rest of first setup are D2's
      `device settings set`.

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

- [ ] **A serial transport for `Client`.** The reader and writer become
      the Unix socket's or the serial port's; the protocol is the same
      newline-delimited JSON, with log lines skipped on serial.
- [ ] **`--device PORT` for the existing commands** a device answers:
      `status`, `scan` (Classic, and `--le`), `pair` with the TH-D75's
      confirm step, `unpair`, `tnc update` (model, `spp.address`,
      `ble.address`), `tnc models`, `connect`, `disconnect`, `release`,
      `kick` and `monitor`. `status` shows a device's one bridge without
      `since` times. A command the device refuses with `not_supported`
      says it isn't available on a device, not that something failed.
- [ ] **Device-only commands:** `device info`, `device settings`, `device
      settings set` (and which settings need `device reboot`), `device
      reboot`, `device reset` (asks first, or `--yes`: it clears the
      settings, the radio and the authorized apps), and `device identify`.
- [ ] **`auth` commands:** `auth code`, `auth rotate-code` (asks first),
      `auth mode SECONDS` and `auth apps`. Phase E reuses them over BLE
      and for `ktb-gateway`.
- [ ] **`--json`** for every command, as for the daemon's.
- [ ] **`ktbdev.py`** keeps only what `ktbctl` doesn't do: the `debug.*`
      commands and the LED demo.
- [ ] The TUI against a device is not in this phase.

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
