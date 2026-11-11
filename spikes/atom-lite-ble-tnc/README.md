# ATOM Lite BLE TNC spike

The ATOM Lite as a GATT client to a TIDRADIO TD-H9's KISS service
([docs/td-h9-ble-kiss.md](../../docs/td-h9-ble-kiss.md)), for firmware 1.1
(roadmap, Firmware 1.1: the TIDRADIO TD-H9). Bluetooth is configured as
1.1 would have it: dual mode, the GATT client on, four BLE links. Nothing
else from the firmware runs: no phones, no SPP link.

## What it answers

1. Does Bluedroid on the ATOM connect to a TD-H9, get a large MTU,
   subscribe to `0xAF02`, and pass KISS both ways?
2. Does the TD-H9 put back together a KISS frame written in two pieces?
   (The daemon splits writes longer than the MTU.)
3. How long does reconnecting take after the radio is turned off and on,
   and does anything drop the link while it's idle?
4. Does a BLE scan find a TD-H9 while a Classic inquiry runs, as a double
   press would need, and how far apart are the signal strengths near and
   across the room?
5. What does it draw connected, and while it looks for the radio?
6. Heap with the GATT client and four BLE links configured.

## Running it

Set up the TD-H9 as in the protocol notes (BT On, BT Mode Receiver, APRS
Switch On) with no app connected to it. Then:

```
. ~/src/esp/esp-idf/export.sh
idf.py -p /dev/cu.usbserial-3152263B4F flash
python3 tools/console.py --reset > spike.log &
echo help > /tmp/ktb-spike.fifo
```

It searches for 60 s at start, with a Classic inquiry alongside, and
connects to the first device whose name starts with `TD-H9`. Set
`SPIKE_RADIO_ADDR` in menuconfig to connect to an address instead.

| Command | Does |
|---|---|
| `beacon` | Sends an APRS status packet in one write. It transmits, from `SPIKE_CALLSIGN` |
| `split` | The same in two writes: 20 bytes, then the rest |
| `drop` / `connect` | Disconnects and stays down / connects again |
| `search` | Forgets the radio and searches, as a double press would |
| `watch` | Logs every TD-H9 advertisement for 30 s without connecting, for signal strength |
| `params MIN MAX [LATENCY [TIMEOUT]]` | Asks for a connection interval, in ms |
| `status` | The status line now. One also comes every 10 s: heap, MTU, interval, counts |

Flashing this replaces the firmware's bootloader and partition table, so
put the firmware back with the web installer afterwards.

## Findings

Run on 10 November 2026 with one TD-H9 (firmware V1.0.32) and an ATOM
Lite, powered from USB.

1. **It works.** A search found the TD-H9 in 5.7 s with a Classic inquiry
   running alongside. The link was up about 100 ms after starting to
   connect, and ready, with an MTU of 517 and notifications on, about
   830 ms after (845, 826 and 817 ms in three connects). `0xAF01` was handle `0x0017` (write without
   response), `0xAF02` handle `0x0019` (notify). The TD-H9 asks for a 30 ms
   interval, latency 10 and a 6 s timeout right after connecting.
2. **KISS both ways.** A beacon written in one write was transmitted,
   digipeated and gated to APRS-IS, and the digipeated copy came back over
   BLE 3.5 s later: the radio passes on its own packets when repeated.
   Received packets arrive whole, one per notification, padded (a
   117-byte frame in a 125-byte notification).
3. **Split writes don't work.** The same beacon written in two pieces (20
   bytes, then 41) was never transmitted, twice. Each frame has to go in
   one write: up to 514 bytes after escaping at an MTU of 517. Issue 16
   tracks the daemon, which splits writes longer than the MTU.
4. **Reconnecting.** With the radio turned off, the link dropped about 6 s
   later (supervision timeout, `0x08`). A direct connect then runs for
   30 s before Bluedroid gives up (`0x100`, and status `0x85` on the
   open), and the next one starts at once. The pending attempt caught the
   radio within a second of it turning on (22:06:18 by the clock, link up
   at 22:06:17.8 by the host's log), and it was ready 0.7 s later. Long
   back-to-back attempts are enough; no short retries needed.
5. **The Classic inquiry** found a device at `FB:5D:C0:59:B7:62`, the same
   prefix as the TD-H9's BLE address `FB:5D:C0:5E:D5:7E`: probably the
   radio's Classic side. Its name wasn't read.
6. **Signal strength**, 30 s of advertisements each, with about 1,500
   advertisements from other devices in the room:

   | Distance | Heard | First after | Mostly | Range |
   |---|---|---|---|---|
   | About 1 m | 144 | 76 ms | −57 to −62 dBm | −57 to −70 |
   | About 7 m, across the room | 85 | 73 ms | −73 to −76 and −81 to −84 | −73 to −91 |
   | Close (the first search) | | 5.7 s, during the inquiry | −36 | |

   Distance is worth about 20 dB here, and the near and far ranges don't
   overlap. Either distance is found within a tenth of a second when the
   scan has the radio to itself.
7. **Current**, on a USB meter: about 50 mA connected and idle (±10%),
   with the LED dark. While reconnecting, pulses between 50 and 100 mA,
   8–10 a second: Bluedroid's fast scan, a 30 ms window every 60 ms.
   Slower scanning is deferred to power saving (roadmap, Power saving).
8. **Heap**, with the GATT client on and four BLE links configured: 216.3
   KB at `app_main`, 192.0 KB after the controller, 170.0 KB after
   Bluedroid, 161.1 KB connected to the TD-H9, never below 155.2 KB. Up to
   Bluedroid that's within 1 KB of `atom-lite-memory` with the GATT client
   off and two links, so the client and the extra links cost little until
   used; the TD-H9's link costs about 9 KB, like a phone's.
9. **The radio's own settings.** On narrow FM the TD-H9 decoded nothing it
   heard, though it transmitted; on wide it decoded within minutes
   (`docs/td-h9-ble-kiss.md` now lists wide). Once, on wide, it played a
   packet's audio without decoding it. Nothing reaches Bluetooth that the
   radio doesn't decode.
