# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
