# Firmware changelog

Changes to the device firmware, released as `firmware-v<version>` tags,
apart from the daemon's (see the [daemon's changelog](../CHANGELOG.md)).
CI takes a release's notes from its section here, so each version needs a
`## <version> (<date>)` heading before it's tagged.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the firmware adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- The StickC Plus SE (`stickc-plus-se`) is released, beside the ATOM
  Lite and the Core2 v1.3. It runs the ATOM Lite's UI until it has its
  own: its whole screen is the LED, and button A the button.

### Changed

- The Core2's board id is `core2-v1.3`, in `device.info` and in a
  release's file names (1.2.0 used `core2`).

## 1.2.0 (2026-11-12)

The M5Stack Core2 v1.3, with a touchscreen, beside the ATOM Lite.

### Added

- The Core2 v1.3 (`core2`), from the same code as the ATOM Lite, with its
  own flash layout in its 16 MB: two 4 MB app partitions, NVS at
  `0x810000`, a crash dump partition and storage kept for later. The
  StickC Plus SE (`stickc-plus-se`) builds too, but isn't released until
  it has a UI.
- The Core2's touchscreen UI ([core2-ui.md](../docs/core2-ui.md)): the
  Bridge, TNC and System pages under tabs, the circles below the screen
  as labelled soft keys, the battery, a list of the radios found to pair
  or select, comparing the number when pairing a Kenwood, errors as
  banners, the boot splash, the reset gesture and power off. The
  `screen_normal` (`bright`, `dim`, `dark`) and `screen_timeout_seconds`
  settings take the place of the LED's. Forget on the TNC page, the
  System page's power line (external power is USB, the AXP192's ACIN; 99%
  until the charger stops) and a powering-off screen.
- A Kenwood renamed on the radio is found by its Class of Device
  (`0x620204`) as well as by a name starting `TH-D74` or `TH-D75`, and
  is taken for a TH-D75 until its answer to `ID` says which it is.
  `bluetooth.scan` reports a Classic device's `class`.

### Fixed

- The Bluetooth controller no longer crashes (an assert in `llc.c`, line
  135) when a TD-H9 connects while a KISS TNC app is already connected:
  it did whenever the radio came back, or the device started, with an
  app connected first. While the TD-H9 is away, the device now looks for
  it with a scan and keeps advertising to apps (before, it advertised
  through a pending connect, which the controller can't do safely with an
  app connected), and stops advertising only for the connect.
- Pairing the radio the device already has, then cancelling, no longer
  leaves it unpaired. The pairing it has is tried first; only if the
  radio turns that down does the device pair afresh.

## 1.1.0 (2026-11-12)

The TIDRADIO TD-H9 as the radio, for the M5Stack ATOM Lite, beside the
TH-D74 and TH-D75.

### Added

- The TIDRADIO TD-H9 as the radio, over BLE: the ATOM connects to its
  KISS service as a GATT client. A double press selects the first TD-H9
  it finds, as well as looking for a TH-D74 or TH-D75 to pair; the TD-H9
  isn't paired, and the ATOM keeps its address. While it's off or away,
  the ATOM waits for it and reconnects within about a second of it coming
  back. Each frame goes to it in one write, since it doesn't reassemble
  a frame across writes.
- `bluetooth.scan` with `"transport": "le"`, `tnc.update` with `model`
  `td-h9` and `ble.address`, the `td-h9` model in `tnc.models`, and a
  `ble-client` radio port in `status`.
- `device.info` reports `heap`: free internal RAM now, the least since
  starting, and the largest block. `ble-client` is in its
  `capabilities`.

### Changed

- The controller allows four BLE links (a TD-H9, two phones and, from
  Phase E, a management app). Phones are still limited to two.

## 1.0.0 (2026-11-10)

The first release, for the M5Stack ATOM Lite. It's the same firmware as
1.0.0-rc2, tested with KISS TNC apps and a real radio.

### Added

- A bridge from a Kenwood TH-D74 or TH-D75 over Bluetooth Classic to up to
  two phones running KISS TNC apps over the BLE KISS API, as B.B. Link
  does. The radio's TNC is put into KISS mode at every connect, and the
  link reconnects every 15 s while the radio is off or out of range.
- Pairing a radio with a double press, or over USB.
- The LED shows the radio link, phones, pairing, authorization mode and
  errors, in `bright`, `dim`, `breathe` or `dark` mode.
- Reset: hold the button while plugging in.
- The control API over USB serial: status, events, settings, Bluetooth
  scan and pairing, the radio's TNC model, holding the radio link, and the
  authorization code.
- The device's key and authorization code, made on first start.
- Signed images in two app partitions with rollback, ready for updates
  over BLE.

Management apps over BLE and updates over BLE come in later releases.
Until then, authorization mode starts and ends, but no management app
can be authorized over BLE.

## 1.0.0-rc2 (2026-11-10)

### Changed

- An error blinks red three times, then pauses, so it doesn't look like
  pairing's fast blink.
- Identify shows magenta, blue and white three times instead of twice.
- Identify and power-on use the same 1 s dark gap as the other events.
- The `led_normal` setting's `solid` is now `bright`, so the choices are
  `bright`, `dim`, `breathe` and `dark`. A device set to `solid` shows
  `bright`.
- The project's vocabulary: the setup code is now the authorization code,
  the management window is authorization mode, and clients that manage
  the device are management apps. In the control API, `auth.window` is
  now `auth.mode` and `auth.clients` is `auth.apps`, and `auth.info`
  reports `mode` and `apps` instead of `window` and `clients`.
  `auth.code` and `auth.rotate_code` are unchanged, and so is a device's
  code.

## 1.0.0-rc.1 (2026-11-10)

The first release, for the M5Stack ATOM Lite.

### Added

- A bridge from a Kenwood TH-D74 or TH-D75 over Bluetooth Classic to up to
  two phones running KISS apps over the BLE KISS API, as B.B. Link does.
  The radio's TNC is put into KISS mode at every connect, and the link
  reconnects every 15 s while the radio is off or out of range.
- Pairing a radio with a double press, or over USB.
- The LED shows the radio link, phones, pairing, the management window and
  errors, in `solid`, `dim`, `breathe` or `dark` mode.
- Reset: hold the button while plugging in.
- The control API over USB serial: status, events, settings, Bluetooth
  scan and pairing, the radio's TNC model, holding the radio link, and the
  setup code.
- The device's key and setup code, made on first start.
- Signed images in two app partitions with rollback, ready for updates
  over BLE.
