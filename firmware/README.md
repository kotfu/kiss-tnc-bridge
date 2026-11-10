# kiss-tnc-bridge firmware

Firmware for the M5Stack ATOM Lite (and the Core2, in Phase G): a KISS
bridge from a Kenwood TH-D74 or TH-D75 over Bluetooth Classic to phones over
BLE, managed with the [control API](../docs/control-api.md). The plan is
Phase C of the [roadmap](../docs/roadmap.md#phase-c-atom-lite-firmware-and-web-installer);
the button and LED are designed in [atom-lite-ui.md](../docs/atom-lite-ui.md).

C++ on ESP-IDF 6.1, with Bluedroid. The Phase A spikes in
[`spikes/`](../spikes/README.md) are its reference.

## Status

Phase C, milestone 3 of 4:

| Milestone | What | State |
|---|---|---|
| 1 | Flash layout, signed images and rollback; settings in NVS; the device key and setup code; the control API over USB serial; the LED and button | Done |
| 2 | The radio: SPP to the TH-D74/D75, KISS init, reconnect, pairing with the button and over USB | Done |
| 3 | The BLE KISS service and bridging | Done |
| 4 | The error list, the Tail Bat measurement, and CI release builds for the web installer | |

The ATOM bridges the radio to up to two phones. BLE management (Noise
enrollment) is Phase E and updates over BLE are Phase F; until then the
management window opens and closes, but nothing can enroll.

## Layout

| File | What |
|---|---|
| `partitions.csv` | The flash layout, fixed from the first release: table at 0xA000, two 1.875 MB app partitions, NVS at the end |
| `sdkconfig.defaults` | The build configuration; `sdkconfig.dev` adds the `debug.*` commands |
| `version.txt` | The version for local builds; release builds pass `-DPROJECT_VER` |
| `main/main.cpp` | Start-up: reset at power-on, then settings, key, UI and the control API |
| `main/console.*` | The USB serial port, shared by the log and the control API, one whole line at a time |
| `main/control.*` | The control API: framing, replies now and later, events, and the device and auth commands |
| `main/control_radio.cpp` | `status`, `subscribe`, `tnc.*`, `bluetooth.*` and `port.*` |
| `main/settings.*` | `public_name`, `led_normal`, `led_wake_seconds`, `led_brightness` in NVS |
| `main/auth.*` | The X25519 device key, the setup code, the management window |
| `main/ui.*`, `main/led.*` | Gestures, LED states and events, dark mode |
| `main/radio.*` | The radio: one task for the SPP link, KISS init, reconnecting, scanning and pairing |
| `main/kiss.*` | KISS framing, and AX.25 decoding for frame events |
| `main/phones.*` | The BLE KISS service, advertising, and phones' sessions |
| `main/activity.*` | The control API's activity events |
| `main/storage.*` | NVS, and what reset clears |
| `main/firmware.*` | The running image, rollback and the self-test deadline |
| `tools/ktbdev.py` | A development client for the control API over USB |

## Build and flash

```
. ~/src/esp/esp-idf/export.sh
cd firmware
tools/dev-key.sh        # once: the development signing key, never committed
idf.py -B build-dev -DSDKCONFIG=build-dev/sdkconfig \
  -DSDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.dev" build
idf.py -B build-dev -p /dev/cu.usbserial-XXXX -b 115200 flash
```

`erase-flash` before `flash` makes a clean install: a new device key and
setup code. Flash at 115200 baud; 921600 didn't hold on the ATOM Lite.

Images are signed, and the bootloader checks the signature at every boot,
so an image signed with one key won't boot under a bootloader built with
another. A USB flash writes the bootloader too, so switching between the
development key and the release key only needs a flash.

## Trying it

`tools/ktbdev.py` speaks the control API over the port:

```
tools/ktbdev.py info                 # device.info
tools/ktbdev.py code                 # the setup code and its URI
tools/ktbdev.py set led_normal=dark led_wake_seconds=10
tools/ktbdev.py window 120           # open the management window
tools/ktbdev.py demo                 # every LED state and event in turn
tools/ktbdev.py debug radio=connected phones=1
tools/ktbdev.py status
tools/ktbdev.py watch                # state and frame events, decoded
tools/ktbdev.py watch activity       # bytes per port and session
tools/ktbdev.py scan 10              # bluetooth.scan
tools/ktbdev.py pair                 # bluetooth.pair, asking you to compare the number
tools/ktbdev.py port disconnect      # and connect, release
tools/ktbdev.py tnc model=th-d74     # tnc.update
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

Opening the port must not reboot the device. The ATOM's reset circuit
pulls EN low while RTS is asserted without DTR, so a client has to drop
RTS before DTR, or both at once; pyserial drops DTR first, so `ktbdev.py`
opens with DTR asserted and drops it after.

## Phones

KISS apps find the ATOM by the BLE KISS service, which its advertisement
carries; the scan response carries the management service's UUID and the
first 11 bytes of `public_name`. Apps that connect read the whole name.
The ATOM takes two BLE connections. A phone that subscribes to KISS
notifications is a session; its writes are rebuilt into whole frames
before they go to the radio, and frames from the radio go to every session
whole. Phones aren't bonded: one that asks for encryption gets it without
a bond.

iOS remembers a device's name by its address, so an ATOM that ran other
firmware may show its old name in apps until the phone's Bluetooth cache
is cleared.
