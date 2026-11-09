# ATOM Lite memory spike

Phase A spike from [docs/roadmap.md](../../docs/roadmap.md#phase-a-spikes):
how much heap is left on an ATOM Lite (ESP32-PICO-D4, no PSRAM) with SPP to
the radio, the BLE KISS service and a BLE management service all running,
during a bridged session.

It's C++ on ESP-IDF's own APIs (roadmap decisions 10 and 11), with
dual-mode Bluedroid trimmed to SPP and a GATT server, so the numbers are the
best case. Nothing here ships.

## What it does

- **Radio:** an SPP client to the TH-D74/D75 set in `menuconfig` (default:
  the dev TH-D75). It runs the bb-link KISS init on every connect, then
  bridges, and reconnects every 15 s after a drop.
- **Phones:** one GATT server with two services. One is the
  [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
  service, with the same UUIDs as the daemon's `ble-server`. The other is a
  stand-in management service: same shape as the planned one, made-up UUIDs,
  4-byte big-endian length prefix. It answers every message with the heap
  numbers. Up to two phones at once, MTU up to 517.
- **Reports:** heap at each start-up stage (`heap @ …`), a status line every
  10 s, and task stack high-water marks every minute.
- **Partition table:** the two 1.875 MB app partitions the firmware needs
  for updates, so the image-size check is the real one.

## Build and flash

ESP-IDF v6.1 lives in `~/src/esp/esp-idf` (`./install.sh esp32` once).

```
. ~/src/esp/esp-idf/export.sh
cd spikes/atom-lite-memory
idf.py build
idf.py -p /dev/cu.usbserial-XXXX -b 115200 flash monitor
```

Flash at 115200: 921600 fails on the ATOM Lite's USB serial chip. After
changing `sdkconfig.defaults`, delete `sdkconfig` so the new defaults apply.

## Pairing

While it isn't paired with the radio, the spike stays Classic-discoverable
and waits. Put the radio in Pairing Mode, then press the ATOM's button. The
radio shows six digits and the log shows the same
(`pairing: confirm 082344`). Press OK on the radio within 30 s.

## Second phone

`tools/mgmt_soak.py` (needs `bleak`) is a second BLE client. It subscribes to
management notifications (and, with `--kiss`, to KISS) and cycles
length-prefixed messages of 16, 500, 2,000 and 4,096 bytes:

```
python -I tools/mgmt_soak.py --kiss
```

On macOS the app that runs it needs Bluetooth permission. Without it,
scanning hangs silently.

## Findings (8 November 2026)

Free internal heap, from the `heap @` and status lines:

| Stage | Free | Change |
|---|---|---|
| `app_main` | 218.5 KB | |
| NVS and the two stream buffers (6 KB) | 210.4 KB | −8.1 KB |
| Controller initialised, dual mode | 191.6 KB | −18.8 KB |
| Bluedroid enabled | 169.0 KB | −22.5 KB |
| SPP initialised | 161.6 KB | −7.3 KB |
| Both GATT services, advertising | 159.1 KB | −2.5 KB |
| Radio connected, KISS init, bridging | 154.2 KB | −5.0 KB |
| One phone, KISS subscribed, MTU 517 | 145.9 KB | −8.2 KB |
| Two phones, both KISS subscribed, management traffic up to 4 KB | **137.7 KB** (min 136.0 KB) | −8.2 KB |

The largest free block stayed at 110.6 KB throughout. Each phone costs about
8 KB, 4 KB of which is the management reassembly buffer the spike reserves
per connection.

- **Memory isn't the constraint the roadmap feared.** With everything
  running in the worst case, 136 KB is still free. There's room for the real
  management protocol and JSON handling, and probably for Arduino as a
  component if it's ever wanted (open question 2).
- **Image size:** 0xd54c0 bytes (about 853 KB), 56% of an app partition
  free. Static DRAM: 51.6 KB of 124.6 KB.
- **`CONFIG_BT_GATT_MAX_SR_PROFILES` must be at least 4.** Bluedroid's own
  GAP and GATT services take a handle block each, so with 2 our services
  fail with `GATTS_ReserveHandles: no free handle blocks`.
- **Pairing works when the ATOM starts it** while the radio is in Pairing
  Mode: SSP numeric comparison, as the Pi saw. Retrying on a timer while
  unpaired doesn't: each attempt waits 30 s for an OK the operator can't
  match up, and stale prompts pile up on the radio. The firmware should
  pair on a button press (or a command), not on a timer. Not yet known:
  whether the radio's Pairing Mode search lists the ATOM, so the operator
  could start pairing from the radio.
- **KISS init is identical to the Pi's:** `BT` silent (TNC already in KISS),
  `C0 FF C0`, then `BT 1`, `ID TH-D75`, `TN 0,1` and `TN 2,1`.
- **Bridging works both ways** with an iOS BLE KISS app: frames to and from
  the radio, no drops, no failed notifications. iOS and macOS both
  negotiated an MTU of 517.
- **Bluedroid puts the radio link in sniff mode** (500 ms interval) when
  it's idle, and leaves it when there's traffic. That's a power saving with
  a latency cost; see the roadmap's Power saving table.

### Soak with sniff mode on

From 16:20 to 17:00, the radio was bridged the whole time, and from 16:30
two phones were connected. One was the iOS app with KISS traffic both ways.
The other was `mgmt_soak.py --kiss`, sending 338 management messages of up
to 4 KB.

- **Heap:** the lowest ever was 134.6 KB, the largest block stayed at
  110.6 KB, and free heap returned to the same level after each disconnect.
  No leak, no crash, no failed notifications.
- **Stack headroom** after 40 minutes, the tightest tasks: `ipc0` 372 B,
  `ipc1` 540 B, `IDLE0` 980 B, `IDLE1` 1,088 B (all ESP-IDF's own),
  `BTC_TASK` 1,356 B, `hciT` 1,412 B. The spike's tasks had at least 1.7 KB
  to spare.
- **The radio link dropped twice (16:41:40 and 17:00:15), both times the
  same way.** The ATOM asked to leave sniff mode, the D75 never answered,
  and the link timed out 6 s later (HCI reason 0x08, supervision timeout).
  33 of 35 sniff exits worked; the two that didn't are exactly the two
  drops. The spike reconnected, re-ran KISS init and was bridging again
  within 20 s each time. The bond survived: Bluedroid's "Deleting unpaired
  device" on each disconnect is misleading. The frame in flight was lost
  (63 bytes the first time).
- **A separate problem: the D75 held packets with STA lit on a clear
  channel** until PTT was pressed. The cause was the radio's TNC
  busy-detection setting (menu 507) on "Detect Data": with it on "Off
  (Ignore)", STA lit only while transmitting. Squelch was at level 2 the
  whole time and STA came on with nothing breaking squelch, so the TNC's
  data detection seems to ignore squelch, or something else triggers it.
  Raising squelch isn't a known fix. The bridge had delivered
  every frame; the radio was holding them. It isn't specific to the ATOM:
  the daemon on the Pi saw STA hold packets in October. Users of either
  should know about menu 507.

### Soak with sniff mode off

`CONFIG_SPIKE_NO_SNIFF=y` clears sniff from the radio link's policy when
SPP opens (`bta_sys_clear_policy`, a Bluedroid internal: there's no public
API). Same setup as above, with menu 507 on "Off (Ignore)", from 17:10 to
18:10.

- **No drops in an hour.** The link never entered sniff mode, so the
  failure seen above (a sniff exit the D75 doesn't answer) couldn't happen.
  22 frames from the radio, 10 to it, 633 management messages, nothing
  lost. App traffic was light in the second half hour.
- **Heap unchanged:** 137.8 KB free, lowest 136.1 KB. Keeping the link
  active costs no memory, only power.

**Conclusion:** the firmware keeps the radio link out of sniff mode. A
dropped link loses the frame in flight and takes 20 s to recover, which is
worse than the power sniff saves. `CONFIG_SPIKE_NO_SNIFF` now defaults to
on. Still unknown: whether the D74 behaves the same, and whether a longer
sniff interval or sniff subrating would be reliable enough for a battery
profile.

## Answer for Phase A

Memory isn't a problem on the ATOM Lite. With SPP, the BLE KISS service, a
management service, two phones and 4 KB management messages, the heap never
fell below 134.6 KB, and the largest free block stayed at 110.6 KB. The
image uses 44% of an update partition.
