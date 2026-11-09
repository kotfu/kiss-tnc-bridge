# ATOM Lite BLE update spike

Phase A spike from [docs/roadmap.md](../../docs/roadmap.md#phase-a-spikes):
can an ATOM Lite update its own firmware over BLE, with two app partitions,
rollback after a failed boot and a signature check, and how fast?

It's the [memory spike](../atom-lite-memory/README.md) (SPP to the radio,
the BLE KISS service, a stand-in management service) plus an update path.
The client is [`ble-update`](../ble-update), a Rust program using `bluer`
on Linux, so the code can move into `ktbctl device update` later. Nothing
here ships.

No eFuses are burned (roadmap decision 12): no hardware secure boot and no
flash encryption. The ATOM can always be reflashed over USB.

## How it works

- **Partitions:** two app partitions of 1.875 MB, `otadata`, and NVS at the
  end of flash. The partition table is at 0xA000; see the findings.
- **Commands:** length-prefixed JSON on the management service:
  `info`, `update.begin` (size, `erase` all or as it goes, `stop_radio`,
  an optional connection interval), `update.finish`, `update.abort`,
  `reboot`. The device answers with events: `ready`, `ack`, `nak`,
  `finished`, `error`, `conn_params`.
- **Image data:** a fourth characteristic on the management service
  (`…0004`), write without response. Each write is a 4-byte little-endian
  offset and up to 508 bytes of image (Bluedroid's 512-byte attribute
  limit). The GATT callback only checks the offset and queues the bytes in
  a 24 KB ring buffer. A task writes them to flash and sends an `ack` every
  4 KB. The client keeps at most a window (22.8 KB, set by the device) of
  unacknowledged bytes in flight. A write at the wrong offset, or one that
  doesn't fit, gets a `nak` naming the offset to resend from.
- **Checks:** `update.finish` calls `esp_ota_end`, which checks the image
  header, its SHA-256 and its ECDSA signature against the key built into
  the running app. Only then does it switch the boot partition. The
  bootloader checks the signature again at every boot.
- **Rollback:** a new image boots as `pending_verify`. It marks itself
  valid once Bluetooth and both GATT services are up and advertising has
  started. If it resets before that, the bootloader marks it aborted and
  boots the previous app.
- **Disconnects:** if the client disconnects mid-transfer, the device
  aborts the update and keeps running the old app.

## Build, sign and flash

```
. ~/src/esp/esp-idf/export.sh
cd spikes/atom-lite-update
espsecure.py generate_signing_key --version 1 signing_key.pem   # once; gitignored
idf.py -DPROJECT_VER=v1 build
idf.py -p /dev/cu.usbserial-XXXX -b 115200 erase-flash flash
```

`PROJECT_VER` is the version `info` reports. Build other versions into
their own directories (`idf.py -B build-v2 -DPROJECT_VER=v2 build`). The
image that fails its self-test on purpose:

```
idf.py -B build-broken -DSDKCONFIG=build-broken/sdkconfig \
  -DSDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.broken" -DPROJECT_VER=v3-broken build
```

A flash from the memory spike's layout loses the radio bond (NVS moved),
so the radio has to be paired again.

## Sending an update from Linux

```
spikes/ble-update/deploy.sh --release
ssh pi ktb-dev/spike/ble-update B8:7B:4D:18:F2:72 info
ssh pi ktb-dev/spike/ble-update B8:7B:4D:18:F2:72 send atom-v2.bin --interval 12,12 --reboot
```

`--erase seq` erases as writes arrive, `--window` overrides the device's
window, `--stop-radio` closes the radio link for the update, and
`--cut-at N` disconnects after N bytes. The ATOM's BLE address is the one
its log shows as `own address`.

## Findings (8 November 2026)

From aprscc (Raspberry Pi 4, built-in adapter), 1 m away. A phone was also
connected to the ATOM with KISS notifications on. The radio wasn't
connected (not yet paired again).

### Everything works

| Test | Result |
|---|---|
| Update v1 to v2, reboot | v2 boots, passes its self-test, marks itself valid |
| Update back to v1 | Works the same way: there's no anti-rollback, and none is wanted |
| One byte flipped | Rejected at `update.finish` (`Checksum failed`), still running v1 |
| Signed with another key | Rejected (`Secure boot signature verification failed`) |
| Unsigned | Rejected (`image without a signature?`) |
| Crashes on first boot | Bootloader boots v1 again and marks the new image aborted; `info` shows `last_invalid` |
| Client disconnects at 400 KB | Device aborts, keeps running v1; the half-written partition is never marked bootable |

A rejected image costs the whole transfer, since the signature is checked
at the end. The client checks the signature, board and version before
sending, as the roadmap already says.

### Throughput

960 KB image, erasing the whole area first, one run each:

| Connection interval | Transfer | Rate |
|---|---|---|
| 48.75 ms (BlueZ's default, nothing asked) | 43.7 s | 22.0 KB/s |
| 30 ms | 36.6 s | 26.2 KB/s |
| **15 ms** | 24.1 s and 28.3 s | **39.8 and 34.0 KB/s** |
| 11.25 ms | 49.9 s | 19.2 KB/s |
| 7.5 ms | 36.5 s | 26.3 KB/s |

- **BLE is the limit, not flash.** Writing 960 KB to flash took 2.0–2.4 s of
  each transfer, verifying 0.65–0.76 s, and erasing 1.6 s (less when the
  area was partly blank). The ring buffer never filled, so even a 60 KB
  window got no overflows. The `nak` path is written but never ran.
- **15 ms is the interval to ask for.** It was the fastest, and it's also
  the shortest interval iOS allows, so an iPhone should land near the same
  rate if it sends as many packets per interval as BlueZ. That's the
  number still to measure. Shorter intervals were slower; the results are
  noisy, and the phone sharing the ATOM's radio time may be part of it. Linux
  accepted every interval the ATOM asked for.
- **Erase first.** Erasing as writes arrive was slower (29.6 KB/s at
  15 ms), because each sector erase stalls the flash writes.
- **About 30 s for an update at 15 ms**, including erase, verify and
  reboot. The firmware will be a similar size: this image is 983 KB.

### Memory and size

- The 24 KB ring buffer and the update task cost about 31 KB of heap at
  start-up (128.2 KB free after the GATT services, against 159.1 KB in the
  memory spike). The real firmware can allocate the buffer only during an
  update. The lowest free heap during an update was 138 KB.
- The image grew from 853 KB to 983 KB (cJSON, update code, signature
  checks); 50% of an app partition is still free.

### Fixed before the first release

- **The partition table moves to 0xA000.** A bootloader that checks
  signatures is 31 KB (0x7c30), too big for the default space before
  0x8000. NVS moves to the 192 KB the two app partitions leave free at the
  end of flash. Updates over BLE replace only the app, never the bootloader
  or the partition table, so this layout and the bootloader's options are
  fixed for every device from its first USB flash.
- **The bootloader's signature check adds about 330 ms to each boot.**
  It could be turned off (the app still checks updates), but then a device
  would boot an image written over USB without a signature check, which is
  fine since USB is trusted anyway. Keep it on unless boot time matters.
- **A hang needs a deadline.** A crash on first boot rolls back, but a hang
  wouldn't: the task watchdog doesn't reset by default. The firmware
  should restart if it hasn't marked itself valid within a set time, or
  turn on the watchdog's panic.
- **Self-test criteria.** The spike marks an image valid once BLE is
  advertising. The firmware should at least wait for the GATT services and
  NVS settings to load. It shouldn't wait for the radio, which may be
  off.

### With the radio bridging

After pairing the D75 again, at a 15 ms interval, the radio link idle
(sniff mode off, so Classic polls it continuously):

| Radio during the update | Runs |
|---|---|
| Left bridging | 42.4 and 35.7 KB/s |
| Closed with `--stop-radio` | 46.6 and 27.3 KB/s |

The spread between runs is bigger than any cost of keeping the link up, so
the firmware doesn't need to stop bridging for an update. A link carrying
traffic wasn't tested. With `--stop-radio` the link came back after the
update, within the usual 15 s reconnect delay.

Heap with everything at once (radio bridging, two phones, the update
buffer): never below 101.7 KB, and 132 KB at the lowest point measured
during a transfer.

## Answer for Phase A

Firmware updates over BLE work on the ATOM Lite: two app partitions,
rollback after a failed first boot, and ECDSA-signed images with no eFuses
burned. A 960 KB image takes about 30 s from Linux. The iPhone's rate is
the one number still open, until there's an app to measure it with.
