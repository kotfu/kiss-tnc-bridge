# Web installer: functional spec

The web installer is a page on the project's web home (roadmap decision
14), at
[www.k0tfu.org/kiss-tnc-bridge/install](https://www.k0tfu.org/kiss-tnc-bridge/install).
The address is permanent. It installs the firmware on an ATOM Lite (later a
Core2) from a browser over Web Serial, then sets the device up over the
same cable. A ham with a new ATOM Lite and a computer can have it bridging
their radio in a few minutes, with no development tools.

The page is built in the site's repository, in the site's own design
language. This document is the contract between the two repositories: it
says what this repository publishes, and what the page has to do with it.
How the page looks is left to the site.

Status: draft, 9 November 2026. The ATOM Lite firmware speaks the control
API over USB serial as described here, and CI builds the release files and
the manifest; no firmware release has been made yet.

Decided:

- **Tags:** firmware releases are tagged `firmware-v<version>` (semver,
  starting at `firmware-v1.0.0`), with their own CI, and are published as
  not "latest", so GitHub's latest release stays the daemon's. See roadmap
  decision 15 and [Compatibility](control-api.md#compatibility).
- **One firmware version for every board**, in one release. A version
  that changes nothing for a board still ships that board's files.
- **The page uses [`esptool-js`](https://github.com/espressif/esptool-js)
  directly**, not ESP Web Tools, so the page draws every screen in the
  site's design. The manifest is in ESP Web Tools' format all the same.
- **The page installs; it doesn't manage.** It shows the authorization code
  after installing, and that's all it does with an installed device. A lost code,
  a lost phone or a setting to change means reinstalling, until a
  management page (`/kiss-tnc-bridge/manage` or similar) is built, if it
  ever is.

## The browser

- **Chrome, Edge or Opera on a desktop or laptop**: Windows, macOS, Linux
  or ChromeOS. They support [Web Serial](https://developer.mozilla.org/en-US/docs/Web/API/Web_Serial_API).
- **Not Safari, Firefox, or any phone or tablet.** They don't have Web
  Serial. The page checks for `navigator.serial` before anything else, and
  without it explains what's needed and points to `ktbctl device flash`
  as the other way in (roadmap decision 13).
- **HTTPS.** Web Serial works only on a secure origin; `localhost` counts,
  which helps development.
- **Linux:** the browser runs as the user, who needs permission to open the
  serial device: membership of `dialout` on Debian and Ubuntu (`uucp` on
  Arch). The page says so when opening the port fails on Linux.
- **Windows:** the ATOM Lite's USB serial chip (FTDI, below) has a driver
  that Windows installs by itself. The Core2's (WCH CH9102) may need WCH's
  driver on older Windows; the page links to it.

## Where the firmware comes from

Firmware is released from this repository on GitHub, and served to the
page by the site.

**The browser can't fetch release files from GitHub.** Checked on 9
November 2026: a release download (`github.com/…/releases/download/…`)
answers with a redirect to `release-assets.githubusercontent.com`, and
neither sends `Access-Control-Allow-Origin`, so a page on another origin
can't read the file. GitHub's API (`api.github.com`) does allow it, but
unauthenticated it allows 60 requests an hour per IP address, and a
release's assets still come from the download URLs. So the site fetches
releases on its own server and serves them to the page from its own
origin.

### What a release publishes

This repository's CI ([`firmware.yml`](../.github/workflows/firmware.yml))
builds the firmware for each board when a firmware version is tagged, and
attaches these files to the GitHub release:

| File | What it is |
|---|---|
| `ktb-<board>-<version>-manifest.json` | The [manifest](#the-manifest): the parts, where they go, and their hashes |
| `ktb-<board>-<version>-bootloader.bin` | The second-stage bootloader, at `0x1000` |
| `ktb-<board>-<version>-partition-table.bin` | The partition table, at `0xA000` |
| `ktb-<board>-<version>-ota-data-initial.bin` | Blank OTA data, at `0xB000`. Writing it makes the device boot the first app partition |
| `ktb-<board>-<version>-app.bin` | The signed firmware, at `0x10000`. The same file is the image for updates over BLE |

`<board>` is `atom-lite`, `stickc-plus-se` or `core2`, as in `device.info`'s `board`.
`<version>` is the firmware's version, without a `v`.

Each board's layout is fixed from its first release (see the roadmap's
ATOM Lite firmware items). Every board has the bootloader at `0x1000`, the
partition table at `0xA000`, OTA data at `0xB000` and the first app
partition at `0x10000`, so the parts above go to the same places on all
three. The ATOM Lite and StickC Plus SE have a second app partition at
`0x1F0000` and NVS, which holds the settings, the device's key and its
authorization code, at `0x3D0000`. The Core2 has 4 MB app partitions, at
`0x10000` and `0x410000`, and NVS at `0x810000`. Every version for a board
uses that board's layout, so any version can be installed over any other.

Firmware releases are tagged `firmware-v<version>`, apart from the
daemon's `v<version>` tags, and are published as not "latest", so
GitHub's latest release stays the daemon's. One release carries every
board's files.

### The manifest

One per board per release. It follows
[ESP Web Tools' manifest](https://esphome.github.io/esp-web-tools/) format,
so either tool can use it, with fields of our own added:

```jsonc
{"name": "kiss-tnc-bridge",
 "version": "1.0.0",
 "board": "atom-lite",                       // ours
 "built": "2026-11-20T02:00:00Z",            // ours
 "flash": {"size": "4MB", "mode": "dio", "freq": "40m"},  // ours: the images are built for these
 "builds": [{
   "chipFamily": "ESP32",
   "parts": [
     {"path": "ktb-atom-lite-1.0.0-bootloader.bin",        "offset": 4096,  "sha256": "…", "size": 31792},
     {"path": "ktb-atom-lite-1.0.0-partition-table.bin",   "offset": 40960, "sha256": "…", "size": 3140},
     {"path": "ktb-atom-lite-1.0.0-ota-data-initial.bin",  "offset": 45056, "sha256": "…", "size": 8192},
     {"path": "ktb-atom-lite-1.0.0-app.bin",               "offset": 65536, "sha256": "…", "size": 1048564}
   ]}]}
```

`path` is relative to the manifest. `sha256` (hex) and `size` are ours.
[`firmware/tools/package.py`](../firmware/tools/package.py) writes it from
the build's own offsets and flash settings. The app image is signed and
padded to a 64 KB boundary, so its size barely changes between versions.
The images are written as built: the page doesn't change the flash mode,
frequency or size in the bootloader's header.

### What the site serves

How the site gets and stores releases is the site's business. What the
page needs from it, on the site's own origin:

- **A list of firmware releases:** for each, the version, the date,
  whether it's a prerelease, the boards it has, and a link to its release
  notes on GitHub. Newest first.
- **Each release's manifest and parts**, by board and version, with the
  manifest's relative paths resolving to the parts.

A suggestion: the server lists releases with GitHub's API (authenticated,
so the rate limit doesn't bite), keeps those whose tag starts with
`firmware-v`, downloads their files once, checks each part against the
manifest's `sha256`, and keeps them. Released files never change, so they
can be cached forever. A GitHub webhook on `release` events, or a check
every few minutes, picks up new releases.

## The page, step by step

The page is one flow: install, then set up. It doesn't manage a device
that's already installed, beyond offering to install over it.

### 1. Check the browser

Without `navigator.serial`, the page explains which browsers work, and
stops. Nothing else on the page needs Web Serial to be read: the
instructions and release notes are useful from a phone.

### 2. Choose the firmware

- The latest firmware release that isn't a prerelease is chosen by default.
- A version picker lists the others, newest first. Prereleases appear only
  when the user asks for them ("show test versions"), marked as such.
- Each version links to its release notes.
- Older versions can be chosen. Every version uses the same flash layout,
  so going back works; settings written by a newer version might not all
  be understood by an older one, and a clean install fixes that.

The board isn't chosen here: the page finds it in step 4.

### 3. Connect the device

The user plugs the device in with a USB cable that carries data (some
carry only power: the page says so if no port appears), and clicks to
connect. The page calls `navigator.serial.requestPort()` with filters for
the boards' USB serial chips, so the browser's port chooser lists only
likely devices:

| Board | USB serial chip | `usbVendorId` | `usbProductId` | Notes |
|---|---|---|---|---|
| ATOM Lite | FTDI, product name "M5stack" | `0x0403` | `0x6001` | Read from the project's ATOM Lite. Other FTDI devices share these ids, so the filter narrows the list, it doesn't identify the board |
| Core2 v1.3 | WCH CH9102 | `0x1a86` | `0x55d4` | From M5Stack's documentation; check when the Core2 arrives |

A link under the button connects without filters, for a board whose chip
isn't listed.

A port the user granted before is in `navigator.serial.getPorts()`, so on
a later visit the page can offer it without the chooser.

### 4. Find out what's on the device

Before writing anything, the page finds out whether the device runs this
firmware, by speaking the [control API over USB
serial](control-api.md#usb-serial):

1. Open the port at 115200 baud, 8N1.
2. Deassert RTS, then DTR: `port.setSignals({requestToSend: false})`, then
   `port.setSignals({dataTerminalReady: false})`. They drive the ESP32's
   reset and boot pins: the ATOM resets while RTS is asserted without DTR,
   so dropping DTR first reboots it (see
   [control-api.md](control-api.md#usb-serial)). Some platforms assert both
   when a port opens, which the ATOM's reset circuit ignores; if the device
   reboots anyway, it's back within a few seconds.
3. Send `{"v":1,"id":1,"cmd":"device.info"}` and a newline, and read lines,
   skipping any that don't start with `{` (the firmware's log shares the
   port). Wait up to 3 seconds, sending the request again once a second.

If it answers, the page knows the board, the firmware version and the
device's fingerprint, and shows them. If it doesn't, the device has other
firmware (M5Stack's factory test, B.B. Link, or anything else) or none.

The page then connects to the chip's ROM bootloader with `esptool-js`,
which resets the chip into download mode. That gives the chip (`ESP32-PICO-D4` on the
ATOM Lite) and the flash size (4 MB). The page checks them against the
board: an ATOM Lite is an ESP32-PICO-D4 with 4 MB of flash. If the chip
doesn't match any board the release has, the page stops and says what it
found.

Step 4 can run before step 2, so the picker can show only versions with
firmware for the board found.

### 5. Choose how to install

| Choice | When it's offered | What's written | What happens to settings |
|---|---|---|---|
| **Update** | The device runs this firmware | Every part in the manifest | Kept: the settings, the radio pairing, the authorized management apps, the device's key and its authorization code are all in NVS, which isn't written |
| **Clean install** | Always. The only choice when the device runs other firmware | The whole flash is erased first, then every part | Gone. The device makes a new key and authorization code when it starts, so an authorization code kept from before is useless, and every management app has to be authorized again |

Update is the default when it's offered. The page explains clean install
in a sentence before the user picks it: it's how to start completely
fresh, and the way to get a new device key; reset (holding the button at
power-on, see [atom-lite-ui.md](atom-lite-ui.md#reset)) clears the
settings but keeps the key and code.

Writing the OTA data part on an update matters: after updates over BLE
the device may be running from its second app partition, and the blank
OTA data makes it boot the first, where the new firmware was just written.

### 6. Install

1. Download the manifest and its parts from the site. Check each part's
   SHA-256 with `crypto.subtle.digest` against the manifest; on a mismatch,
   stop and write nothing.
2. Connect to the ROM bootloader (if step 4 didn't leave it connected),
   load the flasher stub, and switch to 921600 baud, falling back to
   115200 if that fails.
3. Clean install: erase the whole flash.
4. Write each part at its offset, compressed, and have the chip verify
   each one's MD5 after writing.
5. Reset the chip with RTS, so it boots the new firmware.

Progress: a bar for the whole install, with the step (erasing, writing,
verifying) and the bytes written. An install of the ATOM Lite takes under
a minute. The device's LED isn't ours while it's being written: what it
shows doesn't mean anything (see [atom-lite-ui.md](atom-lite-ui.md)).

The user mustn't unplug the device while it's being written. If the
install fails partway, nothing is lost that another install can't fix:
the ROM bootloader is in the chip, so the page can always install again.

### 7. Wait for the firmware

After the reset the page talks to the firmware as in step 4, waiting up to
15 seconds for `device.info`. It checks that the version is the one just
installed. The device shows its power-on colors, then no radio (yellow and
blue) or radio not connected (yellow), and the page can say so: "the
light on the device should be blinking yellow".

If the firmware doesn't answer, the page says what to try: unplug and
plug in the device, then connect again (it picks up at step 4); a clean
install; a different cable or USB port.

### 8. Set up the device

Over the control API on the same port. USB needs no authentication. Three
things, in this order:

1. **The authorization code.** `auth.code` gives the 16 characters. The
   page shows them, grouped as the device gives them
   (`7KQ2-M9XD-4TPA-W3HN`), and tells the user to keep them: it's how
   management apps are authorized to manage the device over Bluetooth
   (`ktbctl`, Phase D), and it doesn't change unless someone changes it or
   the device is installed clean. No QR code yet: nothing scans one until the iOS app (Phase E),
   and the page adds it then, from `auth.code`'s `uri`.
2. **The name.** `device.settings.set` with `public_name`: the name KISS
   TNC apps show when they scan. The page shows the current name and lets
   the user change it. Only the first 11 characters or so fit in the
   device's advertisement (provisional: see
   [advertising](control-api.md#ble-management)); apps that connect see
   the whole name.
3. **The LED.** `led_normal`: `bright`, `dim`, `breathe` or `dark`, with a sentence
   on each (see [atom-lite-ui.md](atom-lite-ui.md#normal-and-dark-mode)).

Then it says what's next: put the radio in pairing mode and double press
the device's button (see [atom-lite-ui.md](atom-lite-ui.md#pairing)),
with the radio's own instructions from `tnc.models`. The page doesn't
pair the radio itself; the button does. The CPU clock isn't asked: the
firmware runs at 240 MHz, which a Tail Bat needs.

The page ends with a summary: the firmware version, the name, the LED
mode, and the authorization code again.

## The serial connection

- One page holds the port at a time; another program using it (a serial
  monitor, `ktbctl`, an IDE) makes opening fail. The page says to close
  other programs that might be using the device.
- The page closes the port when it's done, or when the user leaves, so
  `ktbctl` or another tab can use it.
- Unplugging the device fires the port's `disconnect` event; the page
  stops what it's doing and says so. Plugging it back in is a fresh start
  at step 4, through `getPorts()` without the chooser.
- The control API and the ROM bootloader can't share the port at once:
  the page closes its control API reader before connecting to the
  bootloader, and reopens the port after the reset.

## Privacy

- **The authorization code never leaves the browser.** It's never put in a URL,
  a log, analytics or an error report. When the page shows a QR code
  (Phase E), it draws it with its own JavaScript, not a server or a QR
  service.
- The page sends nothing about the device to the site: not its
  fingerprint, address or settings. The site sees only which firmware
  files were downloaded.

## Errors

| What happened | What the page says |
|---|---|
| No `navigator.serial` | Which browsers work, and `ktbctl` as the alternative |
| No port in the chooser | Check the cable carries data, try another port, Windows driver link (Core2) |
| The port won't open | Another program has it; on Linux, the `dialout` group |
| The chip doesn't match a board | What chip was found, and that it isn't a supported device |
| Can't reach the ROM bootloader | Unplug and plug in, then try again; another cable or USB port. (The ATOM's button isn't a boot pin, so holding it doesn't help) |
| A part's hash doesn't match | The download is damaged; nothing was written; try again |
| Writing or verifying failed | Try again; the device can always be installed again over USB |
| The firmware doesn't answer after install | Unplug and plug in, then connect; a clean install |
| Unplugged mid-install | Plug it back in and install again |

## Testing

In Phase C, on the project's ATOM Lite:

- [ ] Opening the port in Chrome on macOS, Windows and Linux doesn't reset
      the device, or if it does, the page copes.
- [ ] The highest baud rate that installs reliably over the ATOM Lite's
      FTDI chip.
- [ ] Install over M5Stack's factory firmware, over B.B. Link, and over a
      blank flash, all as clean installs.
- [ ] Update with settings kept, including after a BLE update left the
      device on its second app partition.
- [ ] Going back to an older version.
- [ ] Unplugging mid-install, then installing again.
- [ ] The authorization code from the page authorizes `ktbctl` over BLE (Phase D).
