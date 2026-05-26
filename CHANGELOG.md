# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.4.5 (26 May 2026)

### Changed

- only allow a single instance of `kiss-tnc-bridge` to run, running multiple
  instances concurrently, especially fron the same config file can cause
  race conditions and malformed data sent to the TCP TNC.
- the default `max_clients` is now 1, since not all bluetooth chips support
  multiple simultaneous connects on the same BLE GATT advertisement


## 0.4.4 (25 May 2026)

### Improved

- more improvements for disappearing bluetooth adapters


## 0.4.3 (25 May 2026)

### Improved

- better resiliance to bluetooth adapters being powered off or disappearing


## 0.4.2 (25 May 2026)

### Fixed

- connection counting bug


## 0.4.1 (25 May 2026)

### Improved

- log output to stderr instead of stdout
- added more trace logging to debug multiplexing with multiple BLE clients


## 0.4.0 (25 May 2026)

### New

- per TNC override of the bluetooth adapter to use

### Improved

- bluetooth adapter detection and error handling


## 0.3.1 (24 May 2026)

### Improved

- minor packaging enhancements


## 0.3.0 (24 May 2026)

### Fixed

- builds cleanly on x86_64, aarch64, and on ancient 32 bit armv6


## 0.2.0 (24 May 2026)

Initial release with the following features:

- Bridges a TCP KISS TNC to a Bluetooth Low Energy device
- Multiple TCP servers
- Multiple bluetooth clients per TCP server
- Runs on Linux: debs and rpms for amd64 and arm64, debs for armhf (32 bit armv6) so
  it will run on the original Raspberry Pi, generic tarballs
