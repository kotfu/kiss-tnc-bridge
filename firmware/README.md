# kiss-tnc-bridge firmware

Firmware for the M5Stack ATOM Lite, StickC Plus SE and Core2 v1.3 (see
[Boards](#boards)): a KISS
bridge from a Kenwood TH-D74 or TH-D75 over Bluetooth Classic, or a
TIDRADIO TD-H9 over BLE, to phones over BLE, managed with the
[control API](../docs/control-api.md). The plan is
Phase C of the [roadmap](../docs/roadmap.md#phase-c-atom-lite-firmware-and-web-installer);
the ATOM's button and LED are designed in [atom-lite-ui.md](../docs/atom-lite-ui.md),
and the Core2's screen in [core2-ui.md](../docs/core2-ui.md).

C++ on ESP-IDF 6.1, with Bluedroid. The Phase A spikes in
[`spikes/`](../spikes/README.md) are its reference.

## Status

Phase C, milestone 4 of 4:

| Milestone | What | State |
|---|---|---|
| 1 | Flash layout, signed images and rollback; settings in NVS; the device key and authorization code; the control API over USB serial; the LED and button | Done |
| 2 | The radio: SPP to the TH-D74/D75, KISS init, reconnect, pairing with the button and over USB | Done |
| 3 | The BLE KISS service and bridging | Done |
| 4 | The error list, the Tail Bat measurement, and CI release builds for the web installer | Done |

Firmware 1.1 adds the TIDRADIO TD-H9 (roadmap, Firmware 1.1), as a GATT
client to its KISS service.

The ATOM bridges the radio to up to two phones. BLE management (Noise
authorization) is Phase E and updates over BLE are Phase F; until then
authorization mode starts and ends, but no management app can be
authorized over BLE.

## Layout

| File | What |
|---|---|
| `partitions.csv` | The flash layout, fixed from the first release: table at 0xA000, two 1.875 MB app partitions, NVS at the end |
| `sdkconfig.defaults` | The build configuration every board shares; `sdkconfig.<board>` adds a board's, `sdkconfig.dev` adds the `debug.*` commands, and `sdkconfig.release` points at the release key |
| `main/board.h` | Each board's name and pins |
| `version.txt` | The version for local builds; release builds pass `-DPROJECT_VER` |
| `CHANGELOG.md` | The firmware's changes; a release's notes come from its section |
| `main/main.cpp` | Start-up: reset at power-on, then settings, key, UI and the control API |
| `main/console.*` | The USB serial port, shared by the log and the control API, one whole line at a time |
| `main/control.*` | The control API: framing, replies now and later, events, and the device and auth commands |
| `main/control_radio.cpp` | `status`, `subscribe`, `tnc.*`, `bluetooth.*` and `port.*` |
| `main/settings.*` | `public_name`, `led_normal`, `led_wake_seconds`, `led_brightness` in NVS |
| `main/auth.*` | The X25519 device key, the authorization code, authorization mode |
| `main/ui.*`, `main/led.*` | Gestures, LED states and events, dark mode |
| `main/radio.*` | The radio: one task for the SPP link, KISS init, reconnecting, scanning and pairing |
| `main/kiss.*` | KISS framing, and AX.25 decoding for frame events |
| `main/phones.*` | The BLE KISS service, advertising, and phones' sessions |
| `main/activity.*` | The control API's activity events |
| `main/storage.*` | NVS, and what reset clears |
| `main/errors.*` | The errors the LED shows red and `device.info` lists |
| `main/firmware.*` | The running image, rollback and the self-test deadline |
| `tools/ktbdev.py` | A development management app for the control API over USB |
| `tools/package.py` | Names a build's images for a release and writes the web installer's manifest |

## Build and flash

```
. ~/src/esp/esp-idf/export.sh
cd firmware
tools/dev-key.sh        # once: the development signing key, never committed
BOARD=atom-lite          # or stickc-plus-se, core2
idf.py -B build-dev-$BOARD -DSDKCONFIG=build-dev-$BOARD/sdkconfig \
  -DSDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.$BOARD;sdkconfig.dev" build
idf.py -B build-dev-$BOARD -p /dev/cu.usbserial-XXXX -b 115200 flash
```

Each board builds in its own directory: `idf.py` keeps the configuration
it first made in the build directory, so switching boards in one directory
doesn't take.

`erase-flash` before `flash` makes a clean install: a new device key and
authorization code. Flash at 115200 baud; 921600 didn't hold on the ATOM Lite.

Images are signed, and the bootloader checks the signature at every boot,
so an image signed with one key won't boot under a bootloader built with
another. A USB flash writes the bootloader too, so switching between the
development key and the release key only needs a flash.

## Trying it

`tools/ktbdev.py` speaks the control API over the port:

```
tools/ktbdev.py info                 # device.info
tools/ktbdev.py code                 # the authorization code and its link
tools/ktbdev.py set led_normal=dark led_wake_seconds=10
tools/ktbdev.py mode 120             # start authorization mode
tools/ktbdev.py demo                 # every LED state and event in turn
tools/ktbdev.py debug radio=connected phones=1
tools/ktbdev.py status
tools/ktbdev.py watch                # state and frame events, decoded
tools/ktbdev.py watch activity       # bytes per port and session
tools/ktbdev.py scan 10              # bluetooth.scan
tools/ktbdev.py scan --le 10         # over BLE, for a TD-H9
tools/ktbdev.py pair                 # bluetooth.pair, asking you to compare the number
tools/ktbdev.py port disconnect      # and connect, release
tools/ktbdev.py tnc model=th-d74     # tnc.update
tools/ktbdev.py tnc model=td-h9 ble=FB:5D:C0:5E:D5:7E
tools/ktbdev.py monitor              # the log
```

`-v` prints the log alongside. The `debug` commands and `demo` need the
development build; `debug radio=...` changes what the LED shows, not the
real link, until `debug radio=real`.

## Pairing a radio

Put the radio in pairing mode (Menu > Bluetooth > Pairing Mode on a
TH-D75), then double press the button. The ATOM searches for up to 60
seconds for a device whose name starts with `TH-D74` or `TH-D75` (both
radios report a phone's Class of Device, so the name decides). The radio
shows six digits; press OK on it within 30 seconds. The ATOM confirms its
side at once. A single press cancels, and the ATOM goes back to the radio
it had.

Over USB, `ktbdev.py pair` does the same for an address, and asks you to
compare the number first.

The same double press selects a TIDRADIO TD-H9: the scan looks for one
over BLE too, and the first device whose advertised name starts with
`TD-H9` becomes the radio. It isn't paired; the ATOM keeps its address and
connects to it as a GATT client (see
[td-h9-ble-kiss.md](../docs/td-h9-ble-kiss.md) for the radio's settings).
While it's off or away, the ATOM waits in a direct connect for it to
advertise, and is ready within about a second of it coming back. Each
frame goes to the radio in one write, which it needs.
Opening the port must not reboot the device. The ATOM's reset circuit
pulls EN low while RTS is asserted without DTR, so a management app has to
drop RTS before DTR, or both at once; pyserial drops DTR first, so `ktbdev.py`
opens with DTR asserted and drops it after.

## Phones

KISS TNC apps find the ATOM by the BLE KISS service, which its advertisement
carries; the scan response carries the management service's UUID and the
first 11 bytes of `public_name`. Apps that connect read the whole name.
The ATOM takes two phones. The controller allows four BLE links: the
others are a TD-H9 and, from Phase E, a management app. A phone that subscribes to KISS
notifications is a session; its writes are rebuilt into whole frames
before they go to the radio, and frames from the radio go to every session
whole. Phones aren't bonded: one that asks for encryption gets it without
a bond.

iOS remembers a device's name by its address, so an ATOM that ran other
firmware may show its old name in apps until the phone's Bluetooth cache
is cleared.

## Errors

The LED blinks red fast for anything the ATOM can't fix by retrying:
NVS that can't be used, Bluetooth or the BLE KISS service not starting,
or a missing device key. The control API keeps working over USB whatever
fails, and `tools/ktbdev.py info` shows each error's code and what to do;
[atom-lite-ui.md](../docs/atom-lite-ui.md#errors) has the list. With the
development build, `ktbdev.py debug error=true` turns on a `debug` error.

## Boards

Every board is an ESP32 with Bluetooth Classic and BLE, so one project
builds them all: `sdkconfig.defaults` is the shared configuration, and a
board's `sdkconfig.<board>` picks it (`CONFIG_KTB_BOARD_*`) and adds what
differs. `main/board.h` holds each board's name, as `device.info` reports
it, and its pins. All three use the same flash layout and signing key.

| Board | `<board>` | Chip | Flash | PSRAM | State |
|---|---|---|---|---|---|
| M5Stack ATOM Lite | `atom-lite` | ESP32-PICO-D4 | 4 MB | — | The LED and button UI |
| M5Stack StickC Plus SE | `stickc-plus-se` | ESP32-PICO-D4 | 4 MB | — | Builds; no UI yet. Button A works as the ATOM's button; the screen and red LED are dark |
| M5Stack Core2 v1.3 | `core2` | ESP32-D0WDQ6-V3 | 16 MB | 8 MB | Builds; no UI yet. No button: pair a radio over USB |

The StickC Plus SE and Core2 haven't been tried on a device yet. Both have
an AXP192 power chip, which the firmware doesn't set up yet.

## Releases

[`.github/workflows/firmware.yml`](../.github/workflows/firmware.yml)
builds every board on pull requests that touch the firmware, signed with
a throwaway key. Pushes to `main` don't build; to build `main` without
tagging it, run the workflow by hand from the Actions tab. Pushing a `firmware-v<version>` tag
builds with the release key, from the `FIRMWARE_SIGNING_KEY` repository
secret, and publishes a GitHub release, never marked latest (that stays
the daemon's), with the files the
[web installer](../docs/web-installer.md#what-a-release-publishes) reads.
A version with a hyphen is a prerelease. Betas and release candidates
are numbered with no dot: `1.1.0-beta1`, `1.1.0-rc1`. (`1.0.0-rc.1` came
before this convention.)

To release:

1. Give the version a section in `CHANGELOG.md`, `## 1.0.0 (20 November 2026)`.
2. Tag and push: `git tag firmware-v1.0.0 && git push origin firmware-v1.0.0`.

The tag sets the version; `version.txt` is only for local builds. A build
like CI's, with your own key in `release_signing_key.pem`:

```
BOARD=atom-lite
idf.py -B build-release-$BOARD -DSDKCONFIG=build-release-$BOARD/sdkconfig \
  -DSDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.$BOARD;sdkconfig.release" \
  -DPROJECT_VER=1.0.0 build
tools/package.py build-release-$BOARD $BOARD 1.0.0 dist
```
