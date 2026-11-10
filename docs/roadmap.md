# kiss-tnc-bridge family roadmap

Status: decided 7 November 2026; firmware toolchain, eFuses, USB flashing
and web home 8 November 2026; nothing built yet

## Goal

Grow kiss-tnc-bridge from one Linux daemon into a small family of products
that share one protocol:

- **kiss-tnc-bridge**, the daemon, on a Raspberry Pi or other Linux machine.
- **Firmware for the M5Stack ATOM Lite**, a pocket-sized Bluetooth Classic to
  BLE bridge for a radio's built-in TNC. It replaces the discontinued B.B.
  Link adapter.
- **Firmware for the M5Stack Core2 v1.3**, the same bridge with a screen,
  buttons and a built-in battery. Wi-Fi is added later if it proves reliable.
- **An iOS app** that configures all of them over Bluetooth.
- **`ktbctl`**, which already manages the daemon, also manages the ESP32
  devices.
- **A web installer** that flashes either device from a Chromium browser. A
  ham can buy an ATOM Lite or Core2 and have it bridging their radio in a few
  minutes, with no development tools.

```
                         ┌──────────────── one management protocol ────────────────┐
                         │                                                          │
  iOS app ──BLE mgmt──┬──> ktb-gateway ──Unix socket──> kiss-tnc-bridge (Linux)     │
                      ├──> ATOM Lite firmware                                       │
                      └──> Core2 firmware                                           │
                                                                                    │
  ktbctl ──Unix socket──> kiss-tnc-bridge                                           │
         ──USB serial──> ATOM Lite / Core2 (config, and flashing)                   │
         ──BLE mgmt────> ATOM Lite / Core2 (on Linux)                               │
                                                                                    │
  web installer ──Web Serial──> ATOM Lite / Core2 (flashing, then first setup)      │
                         └──────────────────────────────────────────────────────────┘
```

## Decisions

1. **An iOS app configures kiss-tnc-bridge over Bluetooth.** It connects
   through `ktb-gateway`'s BLE management service, so a headless Pi with no
   network can be set up from a phone.
2. **New firmware for the M5Stack ATOM Lite bridges Bluetooth Classic to
   BLE.** It does everything [B.B. Link](https://github.com/islandmagic/bb-link)
   does, but it's a re-implementation that belongs to the kiss-tnc-bridge
   family: the same object model, protocol, terms and specs. It is not a fork
   of B.B. Link.
3. **The iOS app configures the ATOM Lite firmware**, and updates its
   firmware, if over-the-air updates prove workable (see
   [Firmware updates](#firmware-updates)).
4. **A kiss-tnc-bridge tool configures the ATOM Lite firmware.** That's
   either a new CLI program or part of an existing one; see
   [Device management from the CLI](#device-management-from-the-cli).
5. **A web page flashes firmware onto an ATOM Lite**, so a ham radio operator
   can buy one and have it working in a few minutes with nothing but a
   Chromium-based browser.
6. **New firmware for the M5Stack Core2 v1.3 bridges at least Bluetooth
   Classic to BLE.** Wi-Fi is added only if Wi-Fi, Classic and BLE together
   prove reliable. It shares the ATOM Lite firmware's architecture and adds
   features for the Core2's screen and buttons.
7. **The iOS app manages the Core2**, including firmware updates if they
   prove workable.
8. **A kiss-tnc-bridge tool configures the Core2**, probably the same one as
   for the ATOM Lite.
9. **A web page flashes firmware onto a Core2.** It's probably the same page
   as the ATOM Lite's, with a choice of board.
10. **The firmware builds with ESP-IDF**, not the stock Arduino ESP32 core.
    Arduino compiles its ESP-IDF configuration into precompiled libraries, so
    a project can't change it, and the firmware has to: Bluedroid trimmed to
    fit the ATOM Lite's memory, and signed images for firmware updates.
    Libraries such as M5Unified are added as ESP-IDF components.
11. **The firmware is written in C++.** It's the well-trodden path for
    ESP-IDF: Espressif's Bluetooth APIs and SPP examples, M5Unified and
    B.B. Link are all C or C++. Rust with `esp-idf-svc` would share protocol
    types with the daemon, but its support for Classic SPP is unproven and
    its binaries are large for the ATOM Lite's 1.9 MB app partitions. The
    published JSON Schema keeps the firmware and the daemon in step instead.
12. **No eFuses: no hardware secure boot and no flash encryption**, in the
    spikes or the firmware. Burning eFuses can't be undone, and a device
    that only accepts signed bootloaders can't be recovered or reflashed by
    its owner. Firmware updates over BLE are still signed, using ESP-IDF's
    signed app images without secure boot, which need no eFuses. USB stays
    the way to reflash anything.
13. **`ktbctl` writes firmware over USB**, so the first install works from a
    Linux machine with no browser. The web installer is the other way in.
14. **The project's web home is
    [www.k0tfu.org/kiss-tnc-bridge](https://www.k0tfu.org/kiss-tnc-bridge)**:
    documentation, install pages and the web installer. The source code
    stays on GitHub. The site is a Rails app, so it can serve the setup
    link (`https://www.k0tfu.org/ktb`, see
    [Setup URIs](control-api.md#setup-uris)) and the files the iOS app's
    Universal Links need.
15. **The firmware has its own semver releases, and the major version
    says what works together.** Firmware is tagged `firmware-v<version>`,
    starting at `firmware-v1.0.0`; the daemon and its tools keep
    `v<version>`, and each tag has its own CI. One firmware version covers
    every board. Firmware 1.x works with software 1.x (from the first
    `ktbctl` with device support), and neither works with the other's 2.x:
    the major version is the control API's protocol version, so the two
    move together. Within a major, a new feature is announced by a
    capability in `device.info`, and clients check for it, so neither side
    needs a minimum version of the other to keep working. See
    [Compatibility](control-api.md#compatibility).

## Non-goals

- **Android app.** The protocol is published with a JSON Schema, so a Kotlin
  app is possible later, but it's not planned.
- **Safari, Firefox and phones for the web installer.** They don't support
  Web Serial, so the first install needs a computer running Chrome, Edge or
  Opera, or `ktbctl` (decision 13).
- **Wi-Fi on the ATOM Lite.** It needs Classic and BLE at the same time, and
  its 4 MB flash and lack of PSRAM leave no room to spare.
- **Boards without Bluetooth Classic.** The ESP32-S3, C3, C6, H2 and C5 are
  BLE-only, so they can't talk to a TH-D74 or TH-D75 over SPP. That rules out
  the M5StickS3 and similar boards.
- **The M5StickC Plus2.** It would fit well, but it's end-of-life and
  unavailable.
- **Compatibility with B.B. Link Configurator.** The new iOS app replaces it.
  The firmware doesn't need to answer the Configurator's commands. The
  in-band commands RadioMail sends are a separate question; see
  [Open questions](#open-questions).

---

## Shared architecture

### One management protocol

The [control API](control-api.md) is the management protocol for every
member of the family, not just the daemon. The firmware implements a subset
of it, using the same JSON messages, error codes and JSON Schema.

| Member | Transports |
|---|---|
| kiss-tnc-bridge (daemon) | Unix socket. BLE management, the LAN and the web go through `ktb-gateway` |
| ATOM Lite firmware | BLE management GATT service, USB serial |
| Core2 firmware | BLE management GATT service, USB serial. Wi-Fi later, if it's added |

The BLE management service is the one already planned for `ktb-gateway` (see
[plan-1.0.md, `ktb-gateway`](plan-1.0.md#ktb-gateway-1x)): its own 128-bit
service UUID, separate from the KISS service, with a write characteristic and
a notify characteristic carrying length-prefixed messages split into MTU-sized
chunks. Specifying it once means the iOS app has one Bluetooth code path for
a Pi, an ATOM Lite and a Core2.

Over USB serial, the firmware speaks the same newline-delimited JSON as the
Unix socket.

### A device is a bridge that can't be rebuilt

The firmware presents itself in the daemon's terms: one fixed bridge, `ktbr0`,
with a `spp-client` TNC port and a `ble-server` app port, plus a
`tcp-server` port on a Core2 with Wi-Fi. Its TNC is a `[[tnc]]` object with a
model (`th-d74`, `th-d75`, `generic`) that selects the KISS init sequence,
exactly as on the daemon.

- `status`, `subscribe`, `scan.start` / `scan.stop`, `pair` / `unpair`, and
  `tnc.update` work the same as on the daemon.
- Commands that build or remove bridges, ports and adapters fail with a
  stable error code such as `not_supported`.
- New, additive v1 commands cover what only devices have. The names are
  provisional:
  - `device.info`: board, hardware revision, firmware version, protocol
    version, capabilities, and the largest message it accepts.
  - `device.settings.get` / `device.settings.set`: per-device settings (see
    below).
  - `device.reboot`, `device.reset` and `device.identify`.
  - `firmware.update.*`: see [Firmware updates](#firmware-updates).
- Clients decide what to show from `device.info`'s capabilities, not from the
  board name, so a new board doesn't need app changes for features it shares.

### Device settings

B.B. Link fixes most of its behavior at compile time. The new firmware keeps
these in NVS and makes them settings, so the app, `ktbctl` and the web page
can change them without a rebuild:

| Setting | Why |
|---|---|
| Public name | The BLE name apps show, as `public_name` on the daemon |
| TNC model | KISS init sequence for the radio |
| CPU clock: 240, 160 or 80 MHz | The ATOM Lite on an M5Stack Tail Bat needs 240 MHz (see [Hardware notes](#hardware-notes)). Other power sources can save power at 80 MHz. The default is 240, and nothing offers to change it until Phase D or E |
| Bluetooth transmit power | Range against battery life |
| LED or screen brightness | Battery life, and not lighting up a dark tent |
| Rig control on or off | If RadioMail frequency control is kept (see [Open questions](#open-questions)) |

Pairing (the radio's address and keys) stays in the TNC object and in the
Bluetooth stack's own storage, as on the daemon.

### Power saving

Every power-saving mechanism trades something, usually latency, for battery
life. A device on mains power gains nothing from that trade, so these are
candidates for settings. They're probably better grouped under one power
profile (battery or mains) than exposed one by one. This list collects them
as they're found.

| Mechanism | Saves | Costs | Spike default |
|---|---|---|---|
| CPU clock (240, 160 or 80 MHz) | CPU power | Processing headroom. On a Tail Bat, too little draw cuts the power | 240 MHz |
| Classic sniff mode on the radio link | Radio time on an idle SPP link: Bluedroid parks it in sniff mode with a 500 ms interval and wakes it when there's traffic | Up to about 500 ms extra latency on the first frame after a quiet spell, in either direction. **With the TH-D75 it also drops the link**: 2 of 35 sniff exits went unanswered (see [Hardware notes](#hardware-notes)) | Off. Bluedroid's power-management table for SPP turns it on, and there's no Kconfig option or public API; the spike clears it from the link policy with `bta_sys_clear_policy`. A battery profile could only bring it back after testing a longer interval or sniff subrating |
| Bluetooth controller modem sleep (`CONFIG_BTDM_CTRL_MODEM_SLEEP`) | Powers the radio down between scheduled Bluetooth events | A little wake-up latency. The ATOM Lite has no 32 kHz crystal, so it uses the main crystal as the sleep clock | On (ORIG mode, main crystal) |
| BLE advertising interval | Advertising airtime | Phones take longer to find and reconnect to the device | 100–150 ms |
| BLE connection interval and peripheral latency | Radio time on each phone link | Latency to and from the phone. The phone chooses, but the device can ask for different values | The phone's choice |
| Bluetooth transmit power | Transmit current | Range | Default |
| Automatic light sleep (`CONFIG_PM_ENABLE`) | CPU power between events | Wake-up latency. Can trip the Tail Bat's low-current cut-off | Off |
| Reconnect interval to the radio | Paging attempts while the radio is away | Slower recovery after the radio comes back | 15 s |
| LED or screen brightness | LED or backlight current | Visibility | — |
| Wi-Fi power save (Core2, if Wi-Fi is added) | Wi-Fi radio time | Latency on the TCP port. ESP-IDF may require Wi-Fi modem sleep while Bluetooth is running; to confirm | — |

The Tail Bat turns this upside down: its power chip switches off below
45 mA (see [Hardware notes](#hardware-notes)), so a battery profile there
has to keep the draw up rather than minimise it. The right profile depends
on the power source, not just on whether there's a battery.

### Security

These devices control a transmitter under your callsign, so the firmware uses
the same model planned for `ktb-gateway`:

- **Authentication inside the protocol, not Bluetooth pairing.** Each
  device makes its own setup code, 80 random bits, and keeps it. A client
  that knows the code enrolls once, with a Noise handshake, and is trusted
  from then on by its own key. Every session is encrypted. The ATOM Lite has
  no screen, so its code comes from the web installer or `ktbctl` over USB,
  either as 16 characters or as a QR code with a URI in it. The Core2 shows
  its QR code on screen. See [Authentication](control-api.md#authentication)
  in the control API.
- **The code is permanent.** It can be rotated over USB or by a trusted
  client; clients already trusted keep working.
- **One level of trusted clients**, and a trusted client can do everything.
  Anyone with the device and a USB cable can reflash it anyway (decision
  12), so owner and guest levels would add complexity without adding
  protection.
- **One enrollment flow** in the iOS app for every device: a Pi running
  `ktb-gateway`, an ATOM Lite and a Core2.
- **Management window.** A new client is accepted only for a few minutes
  after a button press: a long press on the ATOM Lite (see
  [atom-lite-ui.md](atom-lite-ui.md)), or a menu item on the Core2. A trusted client can open it too.
  Each window lets in one client.
- **Firmware updates are signed** and checked before the device boots them.
  This protects updates over BLE, not USB: with no eFuses burned (decision
  12), anyone with the device and a USB cable can reflash it, which is also
  how its owner recovers it.

### Relationship to B.B. Link

B.B. Link's source (GPL-3.0) and ios-bblink-config (Apache-2.0) are
references for behavior: the TH-D74/D75 SPP details, KISS-mode init,
reconnect timing, and what RadioMail expects. The new code is written for
this family rather than adapted. B.B. Link's in-band KISS `SetHardware`
configuration commands aren't the management protocol; the control API is.

---

## Workstreams

### Protocol (in this repo)

- [ ] Specify the BLE management GATT service in
      [control-api.md](control-api.md): UUIDs, chunking, and the largest
      message size. Drafted 8 November 2026.
- [ ] Add the device subset and the `device.*`, `auth.*` and
      `firmware.update.*` commands to `src/control/protocol.rs` and the
      published JSON Schema. Specified in control-api.md; types not yet
      written.
- [ ] Specify the setup-code exchange and the management window, shared by
      `ktb-gateway` and the firmware. Drafted 8 November 2026: a Noise
      handshake keyed by an 80-bit setup code, then a key per client.

### `ktb-gateway` BLE management (decision 1)

This is Phase 6 in [plan-1.0.md](plan-1.0.md#phase-6-ktb-gateway-1x). The iOS
app needs at least its BLE management transport, setup codes and management
window to reach the daemon. The HTTP, WebSocket and LAN transports can follow
later.

### iOS app (decisions 1, 3, 7)

A new SwiftUI app in its own repository, with types generated from the
published JSON Schema.

- [ ] Discover devices advertising the BLE management service: Pis running
      `ktb-gateway`, ATOM Lites and Core2s.
- [ ] Enter a setup code, and store the credential per device.
- [ ] Status: bridges, ports, TNC state, connected apps, activity.
- [ ] Scan for and pair a radio, and set its TNC model.
- [ ] Daemon management: enable, disable and switch bridges. Create and edit
      dynamic bridges.
- [ ] Device settings.
- [ ] Firmware updates for the ATOM Lite and Core2, if they prove workable.

ios-bblink-config can be borrowed from, since Apache-2.0 allows it, but it's
UIKit with storyboards and knows only B.B. Link's protocol, so starting fresh
is simpler. The app's license has to be compatible with the App Store, which
GPL-3.0 generally isn't. The app shares no code with the daemon, so it can use
Apache-2.0 or MIT.

### ATOM Lite firmware (decisions 2, 3, 4)

In [`firmware/`](../firmware/README.md), built in four milestones during
Phase C.

B.B. Link parity:

- [x] Connect to a TH-D74 or TH-D75 over Bluetooth Classic SPP, with
      automatic reconnect (milestone 2).
- [x] Put the radio's TNC into KISS mode, using the TNC model
      (milestone 2).
- [ ] Serve the [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
      service, the same one the daemon's `ble-server` ports use.
- [x] Scan for and pair a radio: with a double press, or over USB
      (milestone 2).
- [x] Reset: clear all settings, the radio pairing and trusted clients
      (milestone 1).
- [x] RGB LED status and button controls (milestone 1).
- [ ] RadioMail frequency control, if it's kept (see
      [Open questions](#open-questions)).

Family features:

- [ ] Control API subset over BLE management and USB serial.
- [x] Device settings in NVS (milestone 1).
- [ ] Setup code and management window.
- [ ] Two app partitions for over-the-air updates, with rollback. The
      partition table sits at 0xA000 with NVS at the end of flash, because a
      bootloader that checks signatures doesn't fit before 0x8000. Updates
      never replace the bootloader or the table, so this is fixed from the
      first release.
- [ ] A deadline for a new image to mark itself valid: a hang on first
      boot doesn't roll back on its own.

### Device management from the CLI (decisions 4, 8)

The device speaks the control API, so managing it is the same as managing the
daemon over a different transport. The recommendation is to extend `ktbctl`
rather than add a program: a transport option such as `--device
/dev/ttyUSB0` or `--device ble:<name>`, plus device-only subcommands (`ktbctl
device info`, `settings`, `flash`, `update`). A separate binary is the
fallback if the device commands make `ktbctl` confusing.

- [ ] USB serial on Linux, macOS and Windows.
- [ ] BLE management, on Linux first. `bluer` is Linux-only, so macOS would
      need another Bluetooth library.
- [ ] `ktbctl device flash`: write firmware over USB, including the first
      install, so a Linux machine is all it takes (decision 13). The
      [espflash](https://github.com/esp-rs/espflash) crate does this from
      Rust.
- [ ] `ktbctl device update`: an over-the-air update over BLE, using the same
      `firmware.update.*` commands as the iOS app.

### Web installer (decisions 5, 9)

A page on the project's web home (decision 14), specified in
[web-installer.md](web-installer.md) and built in the site's repository.
It flashes over Web Serial, which needs HTTPS. The firmware images and
manifests are served from the same site: a browser can't fetch GitHub
release assets, because GitHub doesn't send CORS headers for them (checked
9 November 2026).

- [ ] Write the page's functional spec. Drafted:
      [web-installer.md](web-installer.md).
- [ ] CI builds each board's firmware on every release and publishes the
      images and a manifest per board.
- [ ] One page with a choice of board: ATOM Lite or Core2.
- [ ] After flashing, the same page connects to the device over Web Serial and
      runs first setup over the control API: show the setup code, name the
      device, and choose the LED mode. Pairing is on the device's button,
      and the radio's model comes from its Bluetooth name.
- [ ] Explain the "erase device" choice: a clean install wipes the radio
      pairing and settings.
- [ ] Link to USB-to-serial drivers for Windows machines that need them.
- [ ] Serve `/ktb`, the setup link in QR codes, and
      `/.well-known/apple-app-site-association` for the iOS app's
      Universal Links, on `www.k0tfu.org`, with bare `k0tfu.org`
      redirecting to it.

Limits: Chrome, Edge and Opera on a desktop or laptop only. Safari, Firefox,
iPhone and iPad don't support Web Serial. The page's code lives in the
site's repository, in the site's design; this repository publishes the
firmware and the spec the page follows.

### Core2 v1.3 firmware (decisions 6, 7, 8)

The same firmware core as the ATOM Lite, built for a second board, plus:

- [ ] Status screen: TNC state, connected apps, battery, and recently heard
      stations, decoded for display only, as `ktbctl monitor` does.
- [ ] On-device setup: scan for and pair a radio, show the setup code, open
      the management window, change settings.
- [ ] Battery level and charging through M5Unified, with a power-off menu
      item.
- [ ] Wi-Fi, if it passes the soak test below: station mode only, a
      `tcp-server` port announced with DNS-SD as `_kiss-tnc._tcp`, matching
      the daemon.

---

## Firmware updates

Decisions 3 and 7 say "if we can figure it out". Here's what's known.

- **Over USB it's solved.** The web installer, `ktbctl device flash`, and
  `idf.py flash` can all reflash a device. A bad update can always be fixed over
  USB.
- **Over BLE it's feasible, and was done before.** B.B. Link's history has a
  hardened BLE OTA, added in commit `748bc8c` and later removed. It required a
  3-second button hold, opened a 5-minute window, wrote through an encrypted
  characteristic, marked the new image valid only after it initialized, and
  rolled back otherwise.
- **The ATOM Lite has room, barely.** Its 4 MB flash holds two app
  partitions of about 1.9 MB each. B.B. Link's image is about 1.4 MB, so the
  new firmware has to stay under 1.9 MB. The Core2's 16 MB flash has plenty
  of room.
- **Rollback and signature checks are ESP-IDF configuration.** The firmware
  builds with ESP-IDF (decision 10), so it can turn both on in the bootloader
  and the app. Signatures use signed app images without secure boot, so no
  eFuses are burned (decision 12). The ATOM Lite's chip is revision 1.1,
  which rules out the RSA scheme (revision 3 and later); the ECDSA scheme
  works on every ESP32 revision, so one key can sign both boards' images.
- **Time.** A 960 KB image takes about 30 s from Linux at a 15 ms
  connection interval, which is also the shortest interval iOS allows. BLE
  is the limit, not flash. The radio link can stay up during an update: an
  idle bridged link made no difference that stood out from run-to-run
  noise. The update buffer (24 KB) should be allocated only during an
  update.
- **Where images come from.** GitHub Releases. The app and `ktbctl` check the
  signature, the board and the version before sending an image, and the
  device checks them again before booting it.

The Phase A update spike showed this works on the ATOM Lite:
[spikes/atom-lite-update](../spikes/atom-lite-update/README.md).

---

## Hardware notes

### M5Stack ATOM Lite

- ESP32-PICO-D4 with Bluetooth Classic and BLE, 4 MB flash, 520 KB SRAM, and
  no PSRAM. Memory turned out not to be a limit: with Classic, BLE, a
  management service and two phones all running, the heap never fell below
  134.6 KB (Phase A memory spike).
- **The radio link has to stay out of sniff mode.** The TH-D75 sometimes
  doesn't answer a sniff exit, and the link drops with a supervision timeout
  (2 of 35 exits in the spike). With sniff off, an hour ran without a drop.
- **Tail Bat power cut-off.** The Tail Bat's IP5303 power chip switches its
  output off when the load stays under 45 mA for 32 seconds. At 80 MHz the
  ATOM Lite's idle draw sits right at that line, so the default for a Tail
  Bat has to be 240 MHz. Deep sleep can't be used on a Tail Bat: power is cut,
  and the ATOM's button can't turn it back on.

### M5Stack Core2 v1.3

- ESP32-D0WDQ6-V3 with Bluetooth Classic and BLE, 16 MB flash, 8 MB PSRAM, a
  320×240 touchscreen with three touch buttons below it, and a 500 mAh
  battery.
- **Wi-Fi with Classic and BLE together.** Espressif's coexistence table
  lists Wi-Fi station mode with BLE and station mode with Classic as
  supported, but has no entry for all three. A soft access point with
  connected clients is marked unstable. So Wi-Fi means station mode only,
  and only after a soak test passes.

---

## Phases

This is the proposed order. Each phase leaves something usable.

### Phase A: spikes

The spikes are written in C++ against ESP-IDF's own APIs (decisions 10 and
11), with Bluedroid trimmed to the profiles the firmware uses, so their
numbers are the best case.

- [x] **ATOM Lite memory:** free heap with SPP, the BLE KISS service and the
      BLE management service all running, during a bridged session.
      Done 8 November 2026: never below 134.6 KB with two phones, and it also
      found the D75's sniff-mode drops. See
      [spikes/atom-lite-memory](../spikes/atom-lite-memory/README.md).
- [x] **ATOM Lite BLE update:** two 1.9 MB app partitions, throughput from an
      iPhone, rollback after a failed boot, and a signature check.
      Done 8 November 2026 from Linux, with a Rust client for `ktbctl`:
      signed updates, rollback, and rejection of corrupt and wrongly signed
      images all work; about 30 s for a 960 KB image. The iPhone's
      throughput waits for the app. See
      [spikes/atom-lite-update](../spikes/atom-lite-update/README.md).
- [ ] **Core2 Wi-Fi soak:** station mode, SPP to a TH-D75, a BLE KISS
      client and a TCP KISS client together for 24 hours, with no dropped
      links and stable heap.

### Phase B: protocol

The BLE management transport, the device subset, the `device.*` and
`firmware.update.*` commands, and the setup-code exchange. Published in
[control-api.md](control-api.md) and the schema.

### Phase C: ATOM Lite firmware and web installer

B.B. Link parity, management over USB serial, device settings, and the web
installer with first setup. At the end of this phase a ham can buy an ATOM
Lite and set it up from a browser, with no app.

- [ ] **Design the ATOM Lite's user interface first**, in
      [atom-lite-ui.md](atom-lite-ui.md). Its only controls are one button
      and one RGB LED.
  - [x] What you can do on the device alone: pair a radio, open the
        management window, and reset (clear all settings; the firmware,
        the device key and the setup code stay). Everything else goes to
        a trusted client, USB, or unplugging.
  - [x] One enrollment per management window.
  - [x] The LED's states and events, in precedence order, with `solid`,
        `dim`, `breathe` and `dark` modes for the normal states, and the wake
        rule: while the LED is dark, a press only wakes it.
  - [x] Reset: hold the button at power-on, through a 3-second warning.
  - [x] Gestures: a single press wakes the LED in dark mode, cancels
        pairing or closes the management window; a double press pairs; a
        long press opens the window.
  - [x] The LED's timing, judged on a device (milestone 1). The
        gestures' durations are the first draft's, unchanged so far.

### Phase D: `ktbctl` device support

Device management over USB serial, `ktbctl device flash`, then BLE
management on Linux.

### Phase E: iOS app and `ktb-gateway` BLE management

BLE management in the ATOM Lite firmware and in `ktb-gateway`, and the iOS
app's first release, managing both.

### Phase F: firmware updates over BLE

In the ATOM Lite firmware, the iOS app and `ktbctl`, if Phase A showed it's
workable.

### Phase G: Core2 firmware

The shared core on the Core2, the screen and button features, a Core2 choice
on the web installer, and Core2 support in the app and `ktbctl`.

### Phase H: Core2 Wi-Fi

Only if the Phase A soak test passed: station mode, a `tcp-server` port, and
DNS-SD.

### Phase I: USB TNC and USB app ports

After Phase F, when BLE management works everywhere: the ATOM Lite and
Core2 can use their USB serial port for KISS, in one of two modes, chosen
at setup. Firmware 1.5 or 2.0, depending on what the API needs.

- **USB TNC:** the TNC is software on the computer serving KISS on a serial
  port, and phones connect to the device over BLE. The device is a BLE
  KISS dongle for TNC software on macOS and Windows, where the daemon
  doesn't run, or on a Linux machine without usable BLE. Bluetooth Classic
  isn't used.
- **USB app:** a KISS program on the computer reaches the radio through the
  device, over Bluetooth Classic.

In both modes the USB port carries only KISS: no log and no control API,
which would mix badly with KISS. The device is managed over BLE, and
reset (holding the button at power-on) brings back the control API over
USB. To work out:

- [ ] KISS software that drives RTS (hardware flow control, or RTS for
      PTT) resets the ESP32 or puts it in download mode. Document what
      works, and test the programs people use.
- [ ] The baud rate, as a setting.
- [ ] How setup chooses the mode, and whether both modes, or USB and the
      radio together, make sense in one bridge.

---

## Open questions

1. **RadioMail frequency control.** B.B. Link answers in-band KISS
   `SetHardware` commands from RadioMail that set and restore the radio's
   frequency. Does the new firmware keep them, and should the daemon get the
   same feature for parity? B.B. Link forks also advertise the name `B.B.
   Link`. Does RadioMail need that name to enable the feature? This needs
   testing, or a question to RadioMail's developer.
2. **Arduino as an ESP-IDF component.** The firmware builds with ESP-IDF in
   C++ (decisions 10 and 11). Adding Arduino as a component would bring its
   libraries, at a cost in memory and flash that the ATOM Lite may not
   have. Decided after the Phase A spikes show how much room is left; the
   default is no.
3. **Repositories and names.** Likely one firmware repository with a shared
   core and a target per board, plus a repository for the iOS app. The web
   installer and documentation live on the web home (decision 14). The
   names of the firmware, the app and the devices' default BLE names are
   still open.
4. **`ktbctl` or a new program** for device management. `ktbctl` is the
   recommendation.
5. **Firmware signing keys:** where the private key lives, and how CI signs
   releases without exposing it.
6. **More boards later.** The LILYGO T-Display (original ESP32, 1.14" screen)
   fits the same architecture for people willing to add their own battery and
   case.
