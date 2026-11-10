# Firmware changelog

Changes to the device firmware, released as `firmware-v<version>` tags,
apart from the daemon's (see the [daemon's changelog](../CHANGELOG.md)).
CI takes a release's notes from its section here, so each version needs a
`## <version> (<date>)` heading before it's tagged.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the firmware adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Changed

- An error blinks red three times, then pauses, so it doesn't look like
  pairing's fast blink.
- Identify shows magenta, blue and white three times instead of twice.

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
