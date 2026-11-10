# kiss-tnc-bridge firmware

Firmware for the M5Stack ATOM Lite (and the Core2, in Phase G): a KISS
bridge from a Kenwood TH-D74 or TH-D75 over Bluetooth Classic to phones over
BLE, managed with the [control API](../docs/control-api.md). The plan is
Phase C of the [roadmap](../docs/roadmap.md#phase-c-atom-lite-firmware-and-web-installer);
the button and LED are designed in [atom-lite-ui.md](../docs/atom-lite-ui.md).

C++ on ESP-IDF 6.1, with Bluedroid. The Phase A spikes in
[`spikes/`](../spikes/README.md) are its reference.

## Status

Phase C, milestone 1 of 4:

| Milestone | What | State |
|---|---|---|
| 1 | Flash layout, signed images and rollback; settings in NVS; the device key and setup code; the control API over USB serial; the LED and button | Done |
| 2 | The radio: SPP to the TH-D74/D75, KISS init, reconnect, pairing with the button and over USB | |
| 3 | The BLE KISS service and bridging | |
| 4 | The error list, the Tail Bat measurement, and CI release builds for the web installer | |

Until milestone 2, pairing is a stand-in: a double press shows the pairing
state for its 60 seconds, then fails. BLE management (Noise enrollment) is
Phase E and updates over BLE are Phase F; until then the management window
opens and closes, but nothing can enroll.

## Layout

| File | What |
|---|---|
| `partitions.csv` | The flash layout, fixed from the first release: table at 0xA000, two 1.875 MB app partitions, NVS at the end |
| `sdkconfig.defaults` | The build configuration; `sdkconfig.dev` adds the `debug.*` commands |
| `version.txt` | The version for local builds; release builds pass `-DPROJECT_VER` |
| `main/main.cpp` | Start-up: reset at power-on, then settings, key, UI and the control API |
| `main/console.*` | The USB serial port, shared by the log and the control API, one whole line at a time |
| `main/control.*` | The control API commands |
| `main/settings.*` | `public_name`, `led_normal`, `led_wake_seconds`, `led_brightness` in NVS |
| `main/auth.*` | The X25519 device key, the setup code, the management window |
| `main/ui.*`, `main/led.*` | Gestures, LED states and events, dark mode |
| `main/radio.*` | The radio link (a stand-in until milestone 2) |
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
tools/ktbdev.py monitor              # the log
```

`-v` prints the log alongside. The `debug` commands and `demo` need the
development build.

Opening the port must not reboot the device. The ATOM's reset circuit
pulls EN low while RTS is asserted without DTR, so a client has to drop
RTS before DTR, or both at once; pyserial drops DTR first, so `ktbdev.py`
opens with DTR asserted and drops it after.
