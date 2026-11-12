# Firmware changelog

Changes to the device firmware, released as `firmware-v<version>` tags,
apart from the daemon's (see the [daemon's changelog](../CHANGELOG.md)).
CI takes a release's notes from its section here, so each version needs a
`## <version> (<date>)` heading before it's tagged.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the firmware adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

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
