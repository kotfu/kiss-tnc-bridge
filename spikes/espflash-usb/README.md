# Spike: `espflash` as a library for `ktbctl device flash`

Phase D, step 1 of D1 ([plan-phase-d.md](../../docs/plan-phase-d.md#1-spike-espflash-as-a-library)).
Can `ktbctl` write our firmware over USB with the
[espflash](https://github.com/esp-rs/espflash) crate, and what does it
cost? Run 12 November 2026 on macOS, with an ATOM Lite and a Core2 v1.3
on USB, and on a Pi 4 with the Core2.

## Answer

Yes. `espflash` 4.7.0 as a library (default features off, `serialport`
on) installs a release manifest on both boards, keeps NVS on an update,
and cross-builds for both Pis without libudev. It adds about 650 KB to a
Linux binary. Two things for `ktbctl` to get right: the ATOM Lite's baud
rate, and resetting the device when a flash fails.

## The tool

```
cargo build --release
target/release/espflash-usb ports                     # serial ports and their USB ids
target/release/espflash-usb probe PORT                # device.info, without resetting
target/release/espflash-usb open-close PORT           # does opening the port reset it?
target/release/espflash-usb chip PORT [--baud N]      # the ROM: chip, package, flash, MAC
target/release/espflash-usb flash PORT MANIFEST [--baud N] [--clean] [--no-verify]
```

`flash` checks every part against the manifest (size and SHA-256), asks
the firmware for `device.info`, connects to the ROM and loads the stub,
writes the parts, resets, and waits up to 15 s for `device.info` again.
`RUST_LOG=debug` shows espflash's commands.

`src/bin/baseline.rs` is the same dependencies without espflash, to
measure what espflash adds.

## Findings

### Builds

- **No libudev.** espflash depends on `serialport` with default features
  off. In the main crate, with espflash added, `serialport` 4.10.1 is one
  copy shared with `mio-serial`, with no features, on aarch64, armhf and
  macOS. The cross-built binaries need only libc, libgcc (and libpthread
  and librt on armhf).
- **Cross builds** on the `ice` Docker context work for
  `aarch64-unknown-linux-gnu` and `arm-unknown-linux-gnueabihf`.
- **Size:** espflash adds 655 KB on aarch64, 651 KB on armhf and 612 KB on
  macOS (release, stripped, LTO). `ktbctl` for armhf is 3.29 MB now, so
  about 3.9 MB with it.
- **Rust 1.95** is espflash 4.7's minimum; CI uses stable.
- On Linux without libudev, `serialport` lists ports from sysfs, with USB
  ids. Not tried on a Pi with a board plugged in yet.

### The boards

| | ATOM Lite | Core2 v1.3 |
|---|---|---|
| macOS port | `/dev/cu.usbserial-3152263B4F` | `/dev/cu.usbserial-5C8D0179491` |
| USB id | `0403:6001`, manufacturer "Hades2001", product "M5stack" | `1a86:55d4`, product "USB Single Serial" (Apple's driver) |
| Chip (ROM) | ESP32 rev 1.1, ESP32-PICO-D4 | ESP32 rev 3.1, ESP32-D0WDQ6-V3 |
| Flash | 4 MB | 16 MB |
| MAC (base) | `b8:7b:4d:18:f2:70` | `1c:8f:57:bc:ef:24` |

- **The ATOM Lite's USB serial isn't an FTDI chip.** It answers with
  FTDI's ids, but the manufacturer string is "Hades2001", which is what
  the CH552 microcontroller M5Stack uses as a USB serial bridge reports.
  That explains the baud rates below. The docs call it FTDI.
- **The package** isn't in espflash's `DeviceInfo` (only "Embedded
  Flash" among its features). The spike reads it from eFuse block 0, word
  3 (`0x3FF5A00C`), as esptool does. PICO-D4 with 4 MB is an ATOM Lite (or,
  later, a StickC Plus SE); D0WDQ6-V3 with 16 MB a Core2.
- **Opening the port doesn't reset either board** when RTS is dropped
  before DTR: five opens and closes each, `uptime` carried on.

### Baud rates

The stub is loaded at 115200, then the rate changes.

| Board | Baud | Result |
|---|---|---|
| ATOM Lite | 115200 | Works: 74 s to write, 82 s in all |
| ATOM Lite | 230400, 460800, 921600 | Fails: `SpiAttach` (or the next command) times out right after the rate changes |
| ATOM Lite | 250000, 500000 | The ROM connects at that rate (not flashed) |
| ATOM Lite | 750000 | Works: 14 s to write, 22 s in all |
| ATOM Lite | 1500000 | Works: 9 s to write, 17 s in all |
| Core2 | 921600 | Works: 15 s to write, 24 s in all |
| Core2 | 1500000 | Works: 13 s to write, 20 s in all |

"In all" is from connecting to the ROM to the reset; the firmware
answers `device.info` 1.9 s (ATOM Lite) or 3.1 s (Core2) after that. The
images were 1.1 MB (ATOM Lite) and 1.7 MB (Core2), compressed on the wire.

The CH552 only does some rates: 1500000 and 750000 work, 921600 doesn't.
The firmware README's "921600 didn't hold on the ATOM Lite" is the same
thing. **1500000 works on both boards**, so it can be `ktbctl`'s rate,
falling back to 115200. The web installer's spec says 921600, which
fails on the ATOM Lite.

### Connecting takes 7 to 8 s

About 6 s of it is espflash sending `GetSecurityInfo` twice (once to
check for secure download mode, once to identify the chip), and the
original ESP32's ROM never answering it, so each waits out a 3 s
timeout. Nothing in espflash's API skips it. It's worth an issue or a
pull request upstream: knowing the chip is an ESP32 (from its magic
value) is enough to skip it.

### Using the library

- **Write every part in one `write_bins_to_flash` call.** Calling it once
  per part wrote the bootloader, then hung on the next part (stopped after
  5 minutes). That left the ATOM Lite with a new bootloader and the old
  app; another flash fixed it. espflash's own CLI writes all the segments
  in one call.
- **A failed flash leaves the chip in its download mode**: after the
  `SpiAttach` timeouts, `device.info` didn't answer until the next flash
  reset it. `ktbctl` has to reset the device (`Flasher::try_connect`
  gives the connection back on an error) and say what happened.
- `Flasher::connect` with `verify` checks each part's MD5 after writing it.
- The parts are written as they are: espflash doesn't change the flash
  mode, frequency or size in the bootloader's header.
- An update kept NVS on both boards: the fingerprint was the same before
  and after.
- The ATOM Lite took the release build (1.1.0, the release key) over a
  development build, and a development build over that: every part is
  written, so the bootloader and the app always match.

### Linux

On aprscc (Pi 4, Debian 13), aarch64 build, with the Core2:

- The Core2 is `/dev/ttyACM0`, on the kernel's `cdc_acm` driver, and
  `/dev/serial/by-id/usb-1a86_USB_Single_Serial_5C8D017949-if00`.
  ModemManager isn't running there.
- `serialport` lists it from sysfs, with its USB ids, without libudev.
- **`serialport` lists ports by device name, not by the by-id symlink**,
  so looking a port's USB ids up by the name the user gave failed for a
  by-id path ("isn't a USB serial port"). Both sides have to be resolved
  with `canonicalize` before comparing. `ktbctl` should accept and show
  the by-id names, as `ktbctl serial` already does.
- `chip` at 1500000 baud works, by either name: 7.1 to 7.3 s to connect.

| Install | Baud | Connect | Erase | Write | In all | Firmware answers |
|---|---|---|---|---|---|---|
| Update | 1500000 | 7.3 s | | 9.4 s | 16.9 s | 3.1 s later |
| Update | 115200 | 7.0 s | | 88.6 s | 95.8 s | 3.1 s later |
| Clean | 1500000 | 8.2 s | 2.6 s | 8.7 s | 19.8 s | 3.1 s later |

At 1500000 the Pi writes faster than the Mac did (9.4 s against 12.5 s).
The update kept NVS. The clean install erased it: the Core2 came back
with a new fingerprint (`EDKDSH1GJF3B93S4` before, `J7C8Z7C5STDZD2NH`
after), so a new device key and authorization code, and no radio pairing.
Erasing all 16 MB took only 2.6 s, so the fingerprint is the check that
it happened.

### Not tried

- **The ATOM Lite on Linux**, and a clean install on it. The Core2 has
  done both ([Linux](#linux)).
- **The armhf build on a board** (wolfcub). It builds; it hasn't run.
- **WCH's own driver** for the Core2 on macOS; Apple's worked.

## The boards now

Both run firmware 1.2.0-dev built from `main` (12 November 2026). The
ATOM Lite (was 1.0.0-dev) kept its settings. The Core2 was installed
clean from aprscc: a new key and authorization code, and no radio.
