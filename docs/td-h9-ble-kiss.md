# TIDRADIO TD-H9 Bluetooth (BLE) KISS TNC protocol

The TIDRADIO TD-H9 handheld has a built-in APRS TNC that it shares over
Bluetooth Low Energy. It works with the radio's own app and some third-party
APRS apps, but not with apps that only know the standard
[BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
(the one the Mobilinkd TNC4, BTech UV-PRO, Vero VR-N76 and Ailunce HA2 use),
because the TD-H9 puts its TNC on a GATT service of its own.

Even though it uses a different GATT service, it sends and receives ordinary
KISS. If your app, script or bridge can talk to a BLE KISS TNC, it can talk to a
TD-H9 by using different UUIDs. This page has everything needed to do that, and
how it was worked out, for anyone who wants to use the TD-H9 as a KISS TNC from
Linux, a phone app, a microcontroller or anything else with Bluetooth LE.

`kiss-tnc-bridge` supports it with `model = "td-h9"`; see
[Using it with kiss-tnc-bridge](#using-it-with-kiss-tnc-bridge).

## Discovery and test setup

Everything here comes from testing a single TD-H9. The radio shows its versions in two
menus, with different labels in each. They're produced here exactly as the
radio shows them:

| Menu | Shown |
|---|---|
| Menu > Radio Info > Radio Info | `Firmware Ver: V1.0.32`, `Hardware Ver: V1.0.2` |
| Menu > Radio Info > APRS Info | `FW Version: V1.0.15`, `HW Version: V1.0.0` |

Corrections and findings from other firmware versions are welcome as
[issues](https://github.com/kotfu/kiss-tnc-bridge/issues).

## Setting up the radio

Four settings are needed before the TNC works in both directions:

| Menu | Setting |
|---|---|
| Menu > Bluetooth > BT On/Off | On |
| Menu > Bluetooth > BT Mode | Receiver |
| Menu > APRS > APRS Switch | On |
| The APRS channel's bandwidth | Wide |

With APRS Switch off, the TNC still transmits the frames you write to it,
but passes nothing it receives back over Bluetooth. Turning it on was
enough to start receiving, with the connection left up: in testing, the
radio delivered nothing for eight minutes with the switch off, then
packets within seconds of turning it on, and nothing again once it was
turned off, while a nearby receiver kept hearing traffic it had passed on
before.

With narrow FM, the TNC still transmits, and other stations decode it,
but in testing it received nothing: not a TH-D75 transmitting beside it,
nor a nearby digipeater, over about 15 minutes. Back on wide, it decoded
the digipeater's beacon within minutes. APRS on 2 m uses wide FM.

You also need to ensure any other Bluetooth apps are disconnected from the radio. It
only handles one connection at a time, and it doesn't advertise the GATT service while
an app is connected.

## The short version

| | |
|---|---|
| GATT service | `0000af00-0000-1000-8000-00805f9b34fb` (16-bit `0xAF00`) |
| Write frames to (host → radio) | `0000af01-0000-1000-8000-00805f9b34fb` (`0xAF01`), write without response |
| Notifications from (radio → host) | `0000af02-0000-1000-8000-00805f9b34fb` (`0xAF02`), notify |
| Framing | Standard KISS: `FEND` (`0xC0`) delimiters, `FESC` escaping, command byte `0x00` for data on port 0 |
| Pairing | None. It accepts a connection from any central without bonding |
| Initialization | Nothing over Bluetooth. It sends and receives as soon as you subscribe to `0xAF02`, if APRS Switch is on (see [Setting up the radio](#setting-up-the-radio)) |
| Connections | One at a time. It stops advertising while connected |

Write one complete KISS frame per write to `0xAF01`: the radio doesn't
reassemble a frame split across writes. Subscribe to `0xAF02`
and feed every notification into a KISS decoder. Each frame arrives whole
in one notification, padded with extra `FEND` bytes, which a KISS decoder
skips as empty frames.

## Finding the radio

The TD-H9 advertises:

| | |
|---|---|
| Name | `TD-H9-2660(BLE)` on the radio tested. Menu > Bluetooth > BT Name shows `TD-H9-2660`, and doesn't let you change it on this firmware. The number may well differ from radio to radio, and the name could change in a firmware update, so don't rely on it to recognize a TD-H9 |
| Service UUIDs in the advertisement | `0xFF00` and `0xAF30` |
| Flags | `0x06` (LE General Discoverable, BR/EDR not supported) |

It does **not** advertise `0xAF00`, the service its TNC is on. A scan
filtered on the KISS service, the standard one or `0xAF00`, won't find it.
Look for it by address, or list nearby LE devices and let the user pick
it. `0xAF30` is also advertised by other radios built on similar Bluetooth
chips (an Ailunce HA2 advertises it too), so it doesn't identify a TD-H9
either.

BlueZ reported the address type as public. The negotiated ATT MTU on a
Raspberry Pi 4 was 517 (512 bytes of payload for notifications).

## The GATT table

As BlueZ read it from one radio. Handles are the attribute handles of each
declaration (the numbers in BlueZ's object paths).

| Handle | Service | Characteristic | Properties | What it's for |
|---|---|---|---|---|
| `0x0001` | `0x1800` Generic Access | `0x2A00` Device Name | read, write | The advertised name |
| `0x0004` | `0xAE30` | `0xAE01` | write without response | Not investigated |
| | | `0xAE02` | notify | Not investigated |
| `0x000A` | `0xFF00` | `0xFF01` | notify | Not KISS; probably TIDRADIO's control protocol |
| | | `0xFF02` | write, write without response | |
| | | `0xFF21` | notify | |
| | | `0xFF22` | write | |
| **`0x0015`** | **`0xAF00`** | **`0xAF01`** | **write without response** | **KISS frames to the radio** |
| | | **`0xAF02`** | **notify** | **KISS frames from the radio** |
| `0x0080` | `0xAE00` | `0xAE01` | write without response | Not investigated |
| | | `0xAE02` | notify | Not investigated |

The `0xAE00`/`0xAE30` services look like the serial and firmware-update
channels that Bluetooth chip vendors build in; the Ailunce HA2 has an
`0xAE00` service too. Nothing on them, or on `0xFF00`, was needed for KISS.

## KISS over 0xAF00

### Host → radio (`0xAF01`)

Write each KISS frame, delimited by `FEND`s, as one write without
response. The radio doesn't put a frame back together across writes: in
testing from an ESP32, the same 61-byte status packet written in two
pieces (20 bytes, then 41) was never transmitted, twice, while written
whole it was transmitted and digipeated. So a frame must fit in one
write, which with an MTU of 517 is up to 514 bytes after escaping. APRS
frames are far shorter. A 53-byte status packet that the radio
transmitted, and the local digipeater repeated:

```
c0 00 82 a0 b4 96 a8 84 60 96 60 a8 8c aa 40 6e
ae 92 88 8a 62 40 63 03 f0 3e 6b 69 73 73 2d 74
6e 63 2d 62 72 69 64 67 65 20 74 64 2d 68 39 20
74 65 73 74 c0
```

That's `K0TFU-7>APZKTB,WIDE1-1:>kiss-tnc-bridge td-h9 test`: `c0`, the KISS
command byte `00` (data, port 0), the AX.25 frame without its FCS, and `c0`.
The radio keyed up and sent it within two seconds.

KISS parameter commands are standard too. One app sends these when it
connects:

```
c0 01 1e c0     TXDELAY 30 (300 ms)
c0 02 3f c0     persistence 63
c0 03 0a c0     slot time 10 (100 ms)
c0 04 00 c0     TX tail 0
c0 05 00 c0     full duplex off
```

The radio transmits without them. Whether it acts on them wasn't tested.

### Radio → host (`0xAF02`)

Each received packet arrives as one notification holding one KISS frame,
with five `FEND`s before it and three after. The start and end of a
125-byte notification, a position beacon from a local digipeater (the
middle of the beacon's text is left out):

```
c0 c0 c0 c0 c0 00 82 a0 8e a4 ae 9e e0 96 60 a8
8c aa 40 62 ae 92 88 8a 62 40 62 ae 92 88 8a 64
40 63 03 f0 21 …
… 31 35 27 20 41 47 4c c0 c0 c0
```

After the padding: command byte `00`, then the AX.25 frame
(`APGRWO` ← `K0TFU-1` via `WIDE1-1,WIDE2-1`, UI frame, PID `F0`, then the
APRS information field), then `FEND`. The frames seen were up to 125 bytes,
well inside the MTU. Whether a frame longer than the MTU is split across
notifications wasn't seen; a KISS decoder that reassembles across
notifications handles both cases.

## How this was worked out

On 5 November 2026, with the TD-H9 described in
[The radio tested](#the-radio-tested) and a Raspberry Pi 4 running Debian 13
and BlueZ 5.82.

1. **Scan and connect.** `bluetoothctl scan le` found it, and
   `bluetoothctl connect` connected without pairing.
2. **List the GATT table.** With the radio connected, `busctl tree
   org.bluez` lists the services and characteristics, and
   `busctl get-property org.bluez <path> org.bluez.GattCharacteristic1
   Flags` gives each one's properties. That gave the table above, and
   showed there was no standard KISS service.
3. **Put a relay in the middle.** Subscribing to every notify
   characteristic and listening for ten minutes showed nothing. So
   [`spikes/ble-relay`](../spikes/ble-relay/src/main.rs), a small BLE man
   in the middle, connected to the radio, copied its GATT services into a
   GATT server of its own on the Pi, and advertised them under another
   name. A phone app known to work with the TD-H9 connected to the relay
   instead of the radio, and the relay passed every write, subscription and
   notification through, logging each with a timestamp. That needs no
   sniffer hardware and nothing installed on the phone. The log showed the
   app using only `0xAF00`, and plain KISS in both directions.
4. **Confirm it.** `kiss-tnc-bridge` with `model = "td-h9"` connected to
   `0xAF00`, received packets from the air, and sent a packet that the local
   digipeater repeated.
5. **Find the APRS Switch.** In a longer test, with aprs.fi on an iPhone
   connected to the radio through the bridge, the radio transmitted
   everything aprs.fi sent but delivered nothing it received for eight
   minutes. The bridge was subscribed the whole time. Menu > APRS >
   APRS Switch had been off; turned on, with nothing else changed,
   packets arrived within seconds, including the replies to a message sent
   from aprs.fi. Turned off again, the radio still transmitted, but
   delivered nothing, while a Graywolf receiver nearby kept hearing
   stations the TD-H9 had passed on minutes before. So the TNC receives
   over Bluetooth only with APRS Switch on (see
   [Setting up the radio](#setting-up-the-radio)).

The same relay works for other BLE radios. Give it the radio's address, and
connect the radio's app to the relay's name. See
[spikes/README.md](../spikes/README.md#ble-relay).

## Using it with kiss-tnc-bridge

Find the radio's address with `ktbctl scan --le` (it lists every named LE
device; the TD-H9 is recognized by its default name if it still has it),
or let `ktbctl setup` walk you through it and pick "TIDRADIO TD-H9". The
config it makes looks like this, here sharing the TD-H9 with phone apps
over the standard BLE KISS API:

```toml
[[tnc]]
id = "h9"
name = "TD-H9"
model = "td-h9"
ble = { address = "XX:XX:XX:XX:XX:XX" }   # from ktbctl scan --le

[[bridge]]
id = "ktbr0"
name = "TD-H9"

  [[bridge.port]]
  id = "radio"
  type = "ble-client"
  tnc = "h9"
  idle_timeout = "90s"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
```

That also makes the TD-H9 usable from apps that only know the standard BLE
KISS API, such as aprs.fi, by connecting them to the bridge instead of the
radio.

## Using it from your own code

Any BLE library works. The steps, in any of them:

1. Connect to the radio's address. No pairing. (BT On/Off and BT Mode set
   as in [Setting up the radio](#setting-up-the-radio).)
2. Find service `0000af00-0000-1000-8000-00805f9b34fb`.
3. Subscribe to notifications on `0000af02-…`. Append each notification's
   bytes to a KISS decoder's buffer, and take complete frames from it,
   skipping empty ones (the padding).
4. To send, KISS-encode the AX.25 frame (`c0 00`, the frame with `c0` and
   `db` escaped, `c0`) and write it to `0000af01-…` as one write without
   response.
5. Disconnect when done, so the radio advertises again for other apps.

To watch it by hand with BlueZ:

```
bluetoothctl
[bluetoothctl]> scan le
[bluetoothctl]> connect XX:XX:XX:XX:XX:XX
[TD-H9-2660(BLE)]> menu gatt
[TD-H9-2660(BLE)]> select-attribute 0000af02-0000-1000-8000-00805f9b34fb
[TD-H9-2660(BLE)]> notify on
```

Each received packet then shows up as a value change on `0xAF02`.

## Open questions

- What the `0xFF00` service carries: probably the radio's settings and
  control protocol, used by TIDRADIO's own app.
- Whether the radio acts on KISS parameter commands (TXDELAY and the rest).
- Whether frames longer than the MTU are split across notifications.
- What the radio does with a frame longer than the MTU, which can't be
  written whole.
- Why the first listen (step 3 above) heard nothing for ten minutes, when
  later connections received packets right away without sending anything
  first. APRS Switch being off would explain it, but its setting then
  wasn't recorded.

Corrections and findings from other firmware versions are welcome as
[issues](https://github.com/kotfu/kiss-tnc-bridge/issues).
