# kiss-tnc-bridge

A TNC switchboard for ham radio enthusiasts. `kiss-tnc-bridge` shares one KISS TNC
with any number of apps using Bluetooth LE and TCP.

```
   apps                                       kiss-tnc-bridge                      TNC

                                   ┌──────────── bridge ktbr0 ────────────┐
   aprs.fi iOS          ── BLE ──┐ │                                      │
   APRSConnect Android  ── BLE ──┴─│─ phones (ble-server) ─┐              │
                                   │                       ├─ radio ──────│── Bluetooth ── TH-D75
   Pat Winlink          ── TCP ────│─ lan (tcp-server) ────┘ (spp-client) │
                                   └──────────────────────────────────────┘
```

Phones and tablets connect over Bluetooth Low Energy (BLE), following the
[BLE KISS API spec](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
and apps on a computer connect over TCP. The TNC can be:

- the TNC built into a radio, such as the Kenwood TH-D75 or Icom ID-5200, over
  Bluetooth or a USB cable
- a serial KISS TNC such as a NinoTNC or TNC-Pi,
- or a TCP KISS TNC server provided by software like
  [Dire Wolf](https://github.com/wb2osz/direwolf),
  [Graywolf](https://github.com/chrissnell/graywolf),
  [Soundmodem](https://uz7.ho.ua/packetradio.htm) or
  [QtSoundModem](https://www.cantab.net/users/john.wiseman/Documents/QtSoundModem.html).

This project was inspired by the discontinued
[B.B. Link](https://github.com/islandmagic/bb-link/) which created a TNC
bridge from the Bluetooth Classic found on the TH-D75 to the Bluetooth Low
Energy method used by Apple devices. This project generalizes that functionality
and offers more capability.

## Why does this exist?

1. The Bluetooth Low Energy spec was finalized in 2009 and the first devices
   implementing this specification were released in 2011. Radio manufacturers
   have been slow to adopt Bluetooth Low Energy for the TNC's in their radios.
   The newly designed Icom ID5200 released in 2026 shares its KISS TNC over
   classic Bluetooth SPP. Apple's phones and tables do not give applications
   access to Bluetooth SPP.
3. A TNC talks to one app at a time. The bridge lets several apps
   share it. You can use APRSConnect on your phone and the aprs.fi app on
   your tablet at the same time with a single TNC.
4. When power matters more than range, Bluetooth Low Energy uses much less power
   than Wi-Fi.
5. I built a portable APRS station using
   [Graywolf](https://github.com/chrissnell/graywolf) on a Raspberry Pi.
   Graywolf provides a TCP KISS TNC that many apps can share, and
   `kiss-tnc-bridge` lets phones use it without Wi-Fi.
6. Building useful tools is fun.


## Features

- Phones and tablets connect over BLE without pairing (see
  [Phones and pairing](#phones-and-pairing) below) and the bridge advertises
  over bluetooth so the apps can easily find it.
- TNCs over Bluetooth Classic (SPP), Bluetooth LE, USB or UART serial, and
  TCP. Kenwood TH-D74/D75 radios are put in KISS mode automatically.
- Apps over BLE and TCP at the same time, on one TNC. TCP apps can find the
  bridge by name with DNS-SD.
- Apps can transmit at the same time. Frames from apps are assembled, queued
  and sent to the TNC in the order they arrive, so they never get mixed
  together. Frames sent from apps while the TNC is connecting wait until
  it is online. An app that sends faster than the TNC can take is slowed
  down rather than having its frames dropped.
- Several bridges, each with its own TNC and its own BLE name. Bridges can
  be disabled and kept for another setup, and switched with one command.
- `ktbctl`, a command line and full-screen terminal UI for the running
  daemon. The setup mode walks you step by step through creating your
  first bridge, including scanning and pairing Bluetooth TNCs. It
  monitors frames passing through the bridge, and let's you add and
  change bridges without restarting the daemon.
- Reconnects to the TNC with backoff without disturbing connected apps. If
  your TNC needs a battery swap or someone turns on the microwave and
  your bluetooth connection drops, everything will reconnect automatically
  without you doing anything.
- Keeps running when a USB Bluetooth dongle is unplugged or an adapter is
  powered off, and picks up again when it's back.
- Works with any Bluetooth 4.0 (or later) hardware that Linux supports, including
  bluetooth chips built into single board computers and USB bluetooth dongles.
- Runs on amd64, arm64 and 32-bit ARMv6, down to the original Raspberry Pi. If
  you have a hardware device that runs linux and won't run this, open an issue
  and I'll see if I can figure out how to make it work.
- The daemon is efficient and small. It uses 8 MB of memory and
  under 1% of the CPU on a Raspberry Pi Model B+ from 2014, while running a
  bridge with a TH-D75, two phone apps connected via Bluetooth, and another
  app connected via TCP.


## Bluetooth background and context

If your radio has Bluetooth, your iPhone has Bluetooth, and they still won't
talk to each other, this section explains why. It also explains the
Bluetooth terms used in the rest of this document. You don't need any of it
to use the bridge, but it makes the rest easier to follow.

### Two kinds of Bluetooth

"Bluetooth" is really two different radio protocols that share a name, a
logo and the 2.4 GHz band. These protocols don't talk to each other.

- **Bluetooth Classic** is the original, from the late 1990s. It's built for
  steady streams of data: headsets, car audio, and serial cables replaced
  by radio. Its formal name is BR/EDR (Basic Rate/Enhanced Data Rate).
- **Bluetooth Low Energy (BLE)** arrived with Bluetooth 4.0 in 2010. It was
  designed from scratch for small devices with constrained power that
  don't send much data. Think fitness bands, sensors, and tags. This is how
  most modern phone apps talk to accessories like the multi-color LED lights
  behind your TV, item trackers like AirTags and Tile Mate, fitness watches
  and trackers, and continuous glucose monitors.

Most devices have a chip that can use both protocols (sometimes called "dual
mode"), but connection between two devices has to choose one or the other
protocol on both sides. A ham radio that says it has a "Bluetooth TNC" doesn't
tell you which protocol it uses.

| | Bluetooth Classic | Bluetooth Low Energy |
|---|---|---|
| Since | 1999 | 2010 (Bluetooth 4.0) |
| How a TNC is shared | **SPP**, the Serial Port Profile: a serial cable emulated over the air | **GATT** services and characteristics; for TNCs, the BLE KISS API |
| Found by | scanning for discoverable devices, usually after putting the device in pairing mode | **advertising**: the device keeps announcing its name, and anything nearby can see it |
| Pairing | required by nearly every device | optional; BLE KISS TNCs don't need it |
| iPhone and iPad apps | **no** (only for accessories in Apple's MFi licensing program) | **yes** |
| Android apps | yes | yes |
| Radios that share a TNC this way | Kenwood TH-D74 and TH-D75, Icom ID-5200, Mobilinkd TNC3 | Vero VR-N76, BTech UV-PRO, Mobilinkd TNC3 and TNC4 |

This table shows why this project exists. A TH-D75 shares its TNC over
Bluetooth Classic, and Apple doesn't let iPhone and iPad apps use Bluetooth
Classic. The same radio works with Android apps, because Android does. `kiss-tnc-bridge`
can speak Bluetooth Classic to the radio and BLE to the phone.

If your radio or TNC already shares its TNC over BLE, iPhone apps can
connect to it directly and you don't need the bridge for that. You might
still want it to share the TNC with several apps at once (see
[Use Cases and Configurations](#use-cases-and-configurations)).

### The terms

**Adapter (or controller).** The Bluetooth hardware in a computer. This can
be a chip on a board, a multi-function chip that does WiFi and Bluetooth,
or a USB dongle. Linux names them `hci0`, `hci1` and so on,
in the order they're found, so the names can change when a dongle is
plugged in. One adapter can talk to a radio over Bluetooth Classic and to
phones over BLE at the same time.

**Bluetooth address.** Every adapter and device has a unique address that
looks like a network MAC address, `40:79:12:E4:95:9D`. The bridge uses it to
find your radio, and to tell your adapters apart.

**SPP and RFCOMM.** The Serial Port Profile makes a Bluetooth Classic link
behave like a serial cable. Whatever bytes are sent on one end come out the
other end. RFCOMM is the layer underneath that carries the traffic on a numbered
channel. A device lists what it offers, and on which channel, through **SDP**
(the Service Discovery Protocol), so the bridge looks the channel up for you.

**GATT, services and characteristics.** BLE devices offer data as **services**
(groups of related values) made up of **characteristics** (single values an app
can read, write, or subscribe to). The BLE KISS API defines one service with two
characteristics. One characteristic is used by the app to write packets, the
other is used by the TNC to send packets back. Each service and characteristic
is identified by a long number called a UUID, which is how an app knows a device
is a TNC, whatever its name.

**Advertising and scanning.** A BLE device that wants to be found **advertises**
by broadcasting a short packet with its name and the services it offers. A phone
**scans** by listening for those broadcasts. When you open the TNC list in a
well made smart phone app, it's scanning for anything advertising the BLE KISS
service. Classic devices are found differently. They have to be made
**discoverable**, which typically requires a user action like pressing a button
or selecting pairing mode.

**Central and peripheral.** In BLE, the side that scans and starts the
connection is the **central** (the phone), and the side that advertises and
waits is the **peripheral** (a TNC, or the bridge). On Linux a peripheral
stops advertising when a central connects, so the bridge starts advertising
again while it has room for more phones; that's how a second phone finds it
(see [Limitations and Caveats](#limitations-and-caveats)).

**Pairing.** Two devices introduce themselves and agree on encryption
keys, so they can trust each other. It happens once, not every time they
connect. Depending on what buttons and screens the devices have, pairing
might ask you to compare a six-digit number on both (**numeric
comparison**, as the TH-D75 does), type a PIN, or nothing at all (which is
appropriately called **Just Works**).

**Bonding.** Pairing plus saving the keys, so the devices recognize each
other next time without pairing again. When a phone's settings list a
device under "My Devices", it's bonded. In everyday speech, and in this
document, "paired" means bonded.

**Trusting.** A Linux (BlueZ) setting on a paired device that lets it
connect without asking for permission each time. `ktbctl pair` pairs and
trusts a radio in one step.

**Connecting.** Opening a link between two devices so they can exchange
data. Pairing is done once. Connecting happens every time you use the
device. A connection can drop (out of range, a battery swap) and be made
again without re-pairing.

**Forgetting (unpairing).** Throwing away the saved keys, on one side or
both. If only one side forgets, the other still thinks they're paired, and
connections fail until both forget and pair again.

**MTU.** Maximum Transmission Unit, or the largest data payload a connection can
carry at once. A BLE MTU is often smaller than a KISS TNC frame, so the frame is
split into pieces on one side and put back together on the other.

**BlueZ.** The Linux Bluetooth software. `bluetoothctl` is BlueZ's command-line
tool, handy for looking at the Bluetooth adapters and devices on your computer.

### How this project uses Bluetooth

- **The radio pairs once.** A radio's TNC over Bluetooth Classic
  needs pairing. You can do that with the computer running `kiss-tnc-bridge`
  with `ktbctl pair` (or `ktbctl setup`). After that the bridge connects
  to the radio whenever an app needs it.
- **BLE TNCs don't pair either.** A radio whose TNC is on Bluetooth LE,
  such as the Ailunce HA2, is found by a scan (`ktbctl scan --le`) and
  connected to the way a phone app would.
- **Phones don't pair at all.** The bridge advertises over BLE, and the
  phone connects from inside the app, **not from the phone's Bluetooth settings**.
  Nothing the bridge offers needs pairing, and leaving it out avoids a whole class of
  problems (see [Phones and pairing](#phones-and-pairing)).
- **Each connected app is a session.** Several phones can be connected to
  the bridge at once, each with its own session, while the bridge keeps a
  single connection to the radio.


## Bridges, ports and TNCs

A **bridge** connects one TNC to any number of apps. Each bridge has an id
(`ktbr0`), which commands and logs use, and a name ("TH-D75"). The name is
broadcast for phones and computers to find.

A bridge defines methods of connection called **ports**. Each one has
an identifier within the bridge (`ktbr0/radio`, `ktbr0/phones`). A bridge has
one **TNC port**, and one or more **app ports**, where apps who want to use the TNC connect.

| Port type | Role | What it connects |
|---|---|---|
| `spp-client` | TNC | A TNC over Bluetooth Classic using the Serial Port Profile, such as a TH-D75 |
| `ble-client` | TNC | A TNC over Bluetooth LE: the BLE KISS API, such as an Ailunce HA2, or a radio's own KISS service, such as a TIDRADIO TD-H9's |
| `serial` | TNC | A TNC on a USB or UART serial device |
| `tcp-client` | TNC | A KISS TCP server: Dire Wolf, Graywolf, Soundmodem |
| `ble-server` | app | Phones and tablets, over Bluetooth LE |
| `tcp-server` | app | Apps over TCP: Pat, Xastir, YAAC, aprs.fi over Wi-Fi |

The TNC port in a bridge connects to the TNC you want to share. This connection
can be made over bluetooth, serial, or TCP. The bridge monitors the status of
the connection and intelligently reconnects if it is lost.

Connecting to an app port creates a **session** (`ktbr0/phones/7C:2A:…`). A
ble-server or tcp-server port can have several sessions, up to its `bridge.port.max_clients`.
Frames from the TNC go to every session. Frames from an app go only to the TNC,
never to the other apps, just as if each app had the TNC to itself. An app hears
another app's transmissions only if the radio hears them back on the air.

A hardware **TNC**, such as the one in a TH-D75, is defined once. Some TNCs
support multiple ways to connect with it (the TH-D75 supports Bluetooth and USB
serial). Defining the TNC outside of a bridge makes it easy to define multiple
bridges in your configuration that all use the same TNC, and only enable the one
you want active right now.

An **adapter** let's you associate a name like `builtin`, `dongle`, or `bt500`
with a Bluetooth adapter's MAC to make your configurations more readable.

## Use Cases and Configurations

Here's some examples of problems and scenarios that hams encounter and how
the bridge can solve it. Each of these require a Linux computer near the
radio. For best results, use a small, low power single board computer
with onboard Bluetooth and WiFi, like:

* Any Raspberry Pi released in the last 10 years
* Any Orange Pi
* Radxa Zero 3W

(see [Recommended Hardware](#recommended-hardware) for more options). `ktbctl
setup` asks you a few simple questions and can build most of these
configurations for you. These configuration snippets show the details so you can
modify them to suit your requirements.

### Use your iPhone or iPad with your radio's built-in TNC

```
   iPhone ── BLE ──> Pi ── Bluetooth Classic ──> TH-D75
```

**The problem:** your radio has a KISS TNC built in, and it shares it over
Bluetooth, but over Bluetooth Classic (the Serial Port Profile). iOS doesn't
let apps use Bluetooth Classic, so none of these apps can use the TNC.

**How the bridge helps:** the Pi connects to the radio over Bluetooth Classic
and offers the TNC to phones over BLE, the way iOS apps expect. The app sees a
Bluetooth TNC with the bridge's name and connects to it like any other. If the
radio's battery dies or it goes out of range, the bridge reconnects to it when
it's back, and the app stays connected the whole time.

**What you need:** a radio with a KISS TNC over Bluetooth Classic and
a Pi with Bluetooth. A single Bluetooth adapter can talk to the radio and the
phone at the same time. If you used B.B. Link for this, or wished you could
still buy a B.B. Link, the bridge does the same job.

**To set it up:** run `ktbctl setup`, pick Bluetooth for the TNC and BLE
for apps. It scans for the radio and pairs it. The steps are in
[Kenwood TH-D74 and TH-D75](#kenwood-th-d74-and-th-d75).

### Let phones use Dire Wolf or Graywolf without Wi-Fi

```
   iPhone ── BLE ──> Pi ── TCP ──> Dire Wolf ── sound card ──> any radio
```

**The problem:** you run a software TNC such as
[Dire Wolf](https://github.com/wb2osz/direwolf) or
[Graywolf](https://github.com/chrissnell/graywolf) with a sound card
interface (a Digirig, a SignaLink, a Pi hat) to your radio. Apps
reach it over TCP, so a phone has to be on the same Wi-Fi. In the car or in
the field there's no Wi-Fi, and running a reliable hotspot on the Pi can be
tricky.

**How the bridge helps:** the bridge connects to the software TNC's KISS
TCP port as one more client and offers it to phones over BLE. The software
TNC keeps doing everything it already does (digipeating, iGating,
beaconing), and the phone gets a Bluetooth TNC that works with no network
at all. Any radio with a sound card interface becomes a Bluetooth TNC for
your phone.

**What you need:** the software TNC with its KISS TCP server turned on
(`KISSPORT` in Dire Wolf, 8001 by default) and Bluetooth on the same Pi.
The software TNC can be on another computer too; give the bridge its
address.

```toml
  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:8001"         # Dire Wolf's KISSPORT

  [[bridge.port]]
  type = "ble-server"
```

If you also add a `tcp-server` port to this bridge on the same Pi, give it
a different TCP port from the software TNC's.

### Share one radio among several apps at once

```
   iPhone (RadioMessenger)     ── BLE ──┐
   iPad (APRSConnect)          ── BLE ──┼──> Pi ──> TH-D75
   laptop (YAAC)               ── TCP ──┘
```

**The problem:** a radio's TNC talks to one app at a time. You want the
aprs.fi map on your iPad, RadioMessenger on your phone and a mapping
program on your laptop, all on the same radio, without unplugging
anything or taking turns.

**How the bridge helps:** every app connected to the bridge hears
everything the radio hears. Whatever any app sends goes to the radio, whole
and in the order it was sent, so two apps transmitting at once never garble
each other's packets. Apps can come and go without disturbing the others or
the radio's connection.

**What you need:** a bridge with a `ble-server` port and, for computers, a
`tcp-server` port. Set `bridge.port.max_clients` on the `ble-server` port to the number
of phones (see [Limitations and Caveats](#limitations-and-caveats) for how
many your Bluetooth chip can serve, and
[Known bugs](#known-bugs) for two phones on older BlueZ). If one app
likes to change the TNC's settings (TXDELAY and friends) and that upsets the
others, set `bridge.app_params = "drop"`.

Apps don't hear each other directly, only what the radio hears on the air,
just as if each one had the radio to itself.

### Turn a USB or serial TNC into a network TNC

```
   laptop in the den  ── Wi-Fi ──┐
   desktop in the shack ── LAN ──┴──> Pi ── USB ──> NinoTNC ──> radio
```

**The problem:** your TNC plugs into one computer by USB or a serial port
(a NinoTNC, a TNC-Pi, a TH-D75 on its USB cable), so only programs on that
computer can use it. You'd like to use it from a laptop anywhere in the
house, or from more than one computer.

**How the bridge helps:** a `tcp-server` port turns any TNC into a KISS TCP
server on your network, which programs like YAAC already know how to use.
The bridge announces it with DNS-SD, so apps that look for TNCs on the
network, such as aprs.fi on iOS, list it by name. When the USB
cable is unplugged or the radio is turned off, the bridge waits for it and
reconnects when it's back.

**What you need:** the TNC on the Pi, and a `tcp-server` port bound to your
network:

```toml
  [[bridge.port]]
  type = "serial"
  tnc = "nino"                       # a [[tnc]] with tnc.serial

  [[bridge.port]]
  type = "tcp-server"
  listen = "eth0:8001"
  allow = ["192.168.1.0/24"]         # your network, nobody else's
```

KISS over TCP has no password. Anyone who can reach the port can transmit
under your callsign, so read [Sharing over the network](#sharing-over-the-network)
before opening it beyond the Pi itself.

### A low-power go-kit or mobile station

```
   iPhone ── BLE ──> Pi Zero 2 W ── Bluetooth ──> TH-D75
                     (5 V from the car or a battery pack)
```

**The problem:** for portable operating, SOTA, POTA, or a car, you want
your phone's APRS app on your radio's TNC, with as little to power and
carry as possible, and nothing to reconfigure when you get there.

**How the bridge helps:** the daemon is small: 8 MB of memory and under 1%
of the CPU on a 2014 Raspberry Pi Model B+. It needs no network, starts at
boot, connects to the radio when an app connects, and reconnects on its own
after the radio's battery is swapped or the phone wanders off. With
`bridge.port.idle_timeout`, the Bluetooth link to the radio stays up for a while after
the app closes, so reopening it is quick.

**What you need:** any Pi with Bluetooth (a Zero W or Zero 2 W has it built
in), the radio, and a 5 V supply. Set the bridge up at home with
`ktbctl setup` and it's ready wherever you take it.

### Keep a setup for home, the car and the field

```
   ktbr0  "Shack"     Dire Wolf on the shack PC ──> phones, laptops
   ktbr1  "D75 BT"    TH-D75 over Bluetooth     ──> phones
   ktbr2  "D75 USB"   TH-D75 on its USB cable   ──> phones
```

**The problem:** you use the same Pi and phone in different places, with a
different TNC in each, and you don't want to edit config files every time
you move.

**How the bridge helps:** define a bridge for each setup and keep the ones
you aren't using disabled. A disabled bridge holds nothing, so they can all
use the same radio, Bluetooth adapter and name. `ktbctl switch ktbr1`
enables one and disables whatever was in its way, in one command, from an
SSH session or the full-screen view.

**What you need:** one bridge per setup; see
[Configuration](#configuration). A radio like the TH-D75 can be defined
once as a `[[tnc]]` with both Bluetooth (`tnc.spp`) and USB (`tnc.serial`), and used by a bridge for
each.

### A station for a club, an event or an EOC

```
   operators' tablets ── BLE ──┐
   net control laptop ── TCP ──┴──> Pi ──> the station's radio and TNC
```

**The problem:** at a public service event, a club station or an emergency
operations center, several operators want to send and read APRS messages
and positions through one well-placed station, using their own phones,
tablets and laptops.

**How the bridge helps:** each operator connects their own app over BLE or
the station's network, and each app sends with its own callsign. The
station's radio settings stay put: `bridge.app_params = "drop"` keeps apps from
changing the TNC's timing, and no app can take the radio out of KISS mode.
`ktbctl status` shows who's connected and `ktbctl kick` disconnects a
session.

**What you need:** a bridge with `ble-server` and `tcp-server` ports, and
for more than a handful of people, a USB Bluetooth dongle or TCP for most
of them (see [Limitations and Caveats](#limitations-and-caveats)). The
station's control operator is responsible for what goes out over it, so
limit the `tcp-server` with `bridge.port.allow`.

### See what's going over the air

```
   $ ktbctl monitor
```

**The problem:** an app says it beaconed but nobody heard it, or a TNC
seems quiet, and you can't tell whether the problem is the app, the
Bluetooth link, the TNC or the radio.

**How the bridge helps:** `ktbctl monitor` shows every frame passing
through a bridge as it happens, in both directions, with the AX.25
addresses decoded, and `ktbctl status` counts frames to and from the TNC.
If frames from the app show up and the radio doesn't transmit, look at the
radio (see [When the radio doesn't transmit](#when-the-radio-doesnt-transmit)).
If nothing comes from the app, look at the app.

### What it doesn't do

To help you avoid spending a lot of time with this project only to find out
that it doesn't meet your needs, here's some things this project doesn't do:

- It isn't a TNC, digipeater or iGate itself. It shares a TNC you already
  have. Use Dire Wolf or Graywolf for those jobs, and the bridge to share
  them.
- It connects to BLE TNCs that follow the
  [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
  and to radios with their own GATT layout only when there's a model for
  them (the TIDRADIO TD-H9 so far).
- Apps can reach it over BLE or KISS over TCP, not over Bluetooth Classic,
  and not with the AGWPE protocol. An Android app that only knows
  Bluetooth Classic TNCs can't connect to it yet; use TCP if the app has
  it.
- The bridge daemon only runs on Linux. Support for additional platforms
  like *BSD, Windows or MacOS is very unlikely. Apps on any operating system can
  connect to the bridge.


## Standards

`kiss-tnc-bridge` uses open standards at every step, so it works with any
app or TNC that follows them, not just the ones it was tested with.

- **KISS.** The host-to-TNC protocol, described by Mike Chepponis K3MC and
  Phil Karn KA9Q in
  [The KISS TNC: A simple Host-to-TNC communications protocol](http://www.ka9q.net/papers/kiss.html)
  (ARRL 6th Computer Networking Conference, 1987; also on
  [ax25.net](https://www.ax25.net/kiss.aspx)). Frames are delimited by
  `FEND` bytes with `FESC` escaping. The bridge passes data frames through
  unchanged, including the port number in the high nibble of the command
  byte, so multi-port TNCs work. KISS parameter commands (TXDELAY,
  persistence, slot time, TX tail, full duplex, SetHardware) pass through
  unless `bridge.app_params = "drop"`; the "return" command never does, and
  unknown commands are dropped.
- **AX.25 and APRS.** What's inside a KISS frame is almost always an AX.25
  frame ([AX.25 Link Access Protocol v2.2](https://www.ax25.net/AX25.2.2-Jul%2098-2.pdf),
  TAPR), and for most hams, an APRS packet (the
  [APRS protocol](https://github.com/wb2osz/aprsspec), APRS 1.0.1 by Bob
  Bruninga WB4APR and its later additions, collected by John Langner
  WB2OSZ). The bridge doesn't need to understand them and never changes
  them; it decodes the AX.25 addresses only to show you frames in
  `ktbctl monitor`. So anything carried over AX.25 works through it, APRS
  or not.
- **KISS over TCP.** There's no formal spec: a KISS TCP server sends and
  receives the same KISS byte stream a serial TNC would, over a TCP
  connection. Dire Wolf, Graywolf, Soundmodem and QtSoundModem all work
  this way, and the bridge's `tcp-client` and `tcp-server` ports do too.
- **Bluetooth Classic, Serial Port Profile.** Radios such as the TH-D75
  share their TNC with the
  [Serial Port Profile](https://www.bluetooth.com/specifications/specs/serial-port-profile-1-2/)
  (SPP), a serial cable emulated over Bluetooth RFCOMM. The bridge
  registers an SPP profile with BlueZ and finds the radio's channel with
  SDP, so there are no channel numbers to configure.
- **BLE KISS.** Heikki Hannikainen OH7LZB wrote the
  [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
  which has been widely adopted and is the practical standard for KISS TNCs
  over Bluetooth Low Energy. It defines one GATT service with a
  characteristic for each direction (the UUIDs are in
  [docs/architecture.md](docs/architecture.md#ble)), and
  KISS frames are streamed across them, split to fit the link. aprs.fi on
  iOS and Android, the Mobilinkd TNC3 and TNC4, and radios like the
  Vero VR-N76, BTech UV-PRO and Ailunce HA2 use it. `kiss-tnc-bridge`
  follows it on both sides: when it advertises to phones and serves them
  (`ble-server`), and when it connects to a BLE TNC as an app would
  (`ble-client`). It is listed among its implementations.
- **KISS TNC discovery on the network.** Heikki also wrote the
  [TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md),
  which lets apps find KISS TCP servers on the local network by name. The
  bridge announces each `tcp-server` port as `_kiss-tnc._tcp` with the
  bridge's public name, using
  [DNS-SD](https://www.rfc-editor.org/rfc/rfc6763) over
  [multicast DNS](https://www.rfc-editor.org/rfc/rfc6762) through
  [Avahi](https://avahi.org/). It doesn't publish the optional `pn` (port
  names) TXT record, which tells apps the server has one port.
- **The Linux Bluetooth stack.** The bridge has no Bluetooth code of its
  own below the protocols above. It uses the Linux kernel's Bluetooth
  drivers and [BlueZ](https://github.com/bluez/bluez), the official Linux
  Bluetooth stack, through BlueZ's D-Bus API: a
  [profile](https://github.com/bluez/bluez/blob/master/doc/org.bluez.Profile.rst)
  for SPP, a
  [GATT application](https://github.com/bluez/bluez/blob/master/doc/org.bluez.GattManager.rst)
  and an
  [advertisement](https://github.com/bluez/bluez/blob/master/doc/org.bluez.LEAdvertisingManager.rst)
  for serving phones over BLE, and BlueZ's
  [GATT client](https://github.com/bluez/bluez/blob/master/doc/org.bluez.GattCharacteristic.rst)
  for BLE TNCs. That's why any Bluetooth 4.0 or later adapter Linux supports
  works, built in or USB, with nothing extra to install.

Some radio manufacturers build similar features without following these
standards. Where we can, we adapt to these irregular implementations so hams can
use their equipment in more ways.


## Installation

### From release packages

Download the latest release from the [Releases](../../releases) page. Releases are
available for amd64 (x86_64), arm64 (aarch64), and armhf (ARMv6) architectures.
Each has the daemon, `kiss-tnc-bridge`, and its management tool, `ktbctl`.

- **Debian/Ubuntu/Raspberry Pi OS**: `sudo dpkg -i kiss-tnc-bridge_*.deb`
- **RHEL/Fedora**: `sudo rpm -i kiss-tnc-bridge-*.rpm`
- **Generic Linux**: extract the tarball and copy `kiss-tnc-bridge` and
  `ktbctl` to `/usr/bin/`

The packages install a systemd service, an empty config at
`/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml`, and a `kiss-tnc-bridge` group
whose members can use `ktbctl`.

Most Raspberry Pi models run a 64-bit OS and should use the **arm64** packages. The
**armhf** packages are for older 32-bit-only Raspberry Pi models:

- Raspberry Pi 1 Model A/A+/B/B+
- Raspberry Pi Zero and Zero W
- Raspberry Pi Compute Module 1

These older boards require 32-bit Raspberry Pi OS (Bullseye or later). None of
them except the Zero W have built-in Bluetooth, so you'll need a USB Bluetooth
adapter; see [Recommended Hardware](#recommended-hardware).

### From source

Requires the Rust toolchain and `libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel` (RHEL/Fedora):

```
$ sudo apt-get install libdbus-1-dev pkg-config   # Debian/Ubuntu
$ cargo build --release
$ sudo cp target/release/kiss-tnc-bridge target/release/ktbctl /usr/bin/
$ sudo groupadd --system kiss-tnc-bridge
```


## Getting started

With the daemon running (see
[Running as a systemd service](#running-as-a-systemd-service)), and once
you're in the `kiss-tnc-bridge` group (`sudo usermod -aG kiss-tnc-bridge
$USER`, then log in again), set up your first bridge:

```
$ ktbctl setup
```

It asks how the TNC is reached (Bluetooth, USB, or the network). For a
radio over Bluetooth it scans for it and pairs it, giving that radio's own
instructions for radios it knows. Then it asks how apps will connect (BLE,
TCP or both), creates the bridge, connects to the TNC, and waits for a first
frame to show it's working.

Then connect an app: in aprs.fi on iOS, pick the bridge's name under the
Bluetooth TNC settings. Before the first phone connects, check
[Phones and pairing](#phones-and-pairing).


## Usage

```
kiss-tnc-bridge [OPTIONS]

Options:
  -c, --config <FILE>      Main config file; conf.d/ next to it is read too
                           [default: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml]
      --state-dir <DIR>    State that persists across restarts [default: /var/lib/kiss-tnc-bridge]
      --runtime-dir <DIR>  Lock file and control socket [default: /run/kiss-tnc-bridge]
      --socket <PATH>      Control socket [default: control.sock in the runtime directory]
  -t, --test-config        Check the config, print the effective bridges and exit
  -d, --debug              Increase log verbosity (-d = debug, -dd = trace)
  -V, --version            Show version and exit
  -h, --help               Show help and exit
```

When running from the command line, `kiss-tnc-bridge` shows its log in the
terminal. Type `Control-C` to quit. The `-d` option overrides the log level in
the configuration file. Only one daemon runs at a time; a second one exits.

### Managing it with `ktbctl`

`ktbctl` manages the running daemon through its control socket,
`/run/kiss-tnc-bridge/control.sock`. Root can use it, and so can members of
the `kiss-tnc-bridge` group. In a terminal, `ktbctl` with no command opens a
full-screen view of the bridges, TNCs and frames; otherwise it prints
`ktbctl status`.

```
$ ktbctl status                         # bridges, ports, sessions, TNCs, adapters
$ ktbctl setup                          # a TNC and a bridge for it, step by step
$ ktbctl monitor                        # live frames, decoded
$ ktbctl switch ktbr2                   # enable ktbr2, disable what's in its way
$ ktbctl edit ktbr1                     # change a bridge in $EDITOR
```

Everything `setup` does can be done a step at a time:

```
$ ktbctl tnc add d75 --model th-d75 --spp-address 40:79:12:E4:95:9D --adapter builtin
$ ktbctl bridge add --name "Portable D75"
created ktbr1
$ ktbctl port add ktbr1 spp-client --tnc d75
$ ktbctl port add ktbr1 ble-server --adapter dongle
$ ktbctl enable ktbr1
ktbr1 started
```

Bridges, TNCs and adapters made with `ktbctl` are **dynamic configuration**, kept by
the daemon in `/var/lib/kiss-tnc-bridge/dynamic.toml`. The files in `/etc` are
**static configuration**: the daemon never writes them, and `ktbctl` can only enable
and disable the bridges they define. See [Configuration](#configuration).

New bridges start disabled, so you can add their ports and enable them when
they're complete. `ktbctl status` tell you about configuration a bridge still
needs in order to be enabled. A change that would disconnect someone
(an app, or a TNC link in use) is shown and asked about first. Without
a terminal it's refused unless you add `--yes`. `ktbctl edit` opens
the dynamic configuration file in your editor and checks it when you
save. Every command takes `--json` which turns all output into JSON
to make it easier for scripts to parse. `ktbctl --help` lists all the
avaialble commands.

The full-screen mode (`ktbctl tui`) has three views: bridges with their
ports and sessions, TNCs and adapters (with scanning and pairing), and
frames. It works over SSH at 80×24, with or without colour. The keys that
work on the selected row are shown at the bottom. Type '?' for help.

The protocol `ktbctl` speaks is described in
[docs/control-api.md](docs/control-api.md), for anyone writing another
client.

### Running as a systemd service

You probably want `kiss-tnc-bridge` to run in the background when the system
starts. The deb and rpm packages set this up. If you build from source or
install a tarball, install the included service file:

```
$ sudo cp kiss-tnc-bridge.service /etc/systemd/system/
$ sudo systemctl daemon-reload
$ sudo systemctl enable --now kiss-tnc-bridge
```

The unit passes the config, state and runtime directories to the daemon and
makes the rest of the filesystem read-only to it. View logs with
`journalctl -u kiss-tnc-bridge -f`.

### Phones and pairing

Phones connect to the bridge without pairing. Nothing the bridge offers needs
it. On many Linux distributions and installations, BlueZ asks every phone that
connects to pair. It wants to read the phone's battery level, which iOS gives only to
paired devices. The bridge doesn't take part in that pairing, so it fails, the
phone is dropped, and it reconnects, over and over. On the device running the
bridge, you have three options to disable this probing, narrowest first:

1. **`ReverseServiceDiscovery = false`** under `[General]` in
   `/etc/bluetooth/main.conf`. BlueZ stops probing devices that connect to
   the host. Connections the host makes itself, to a radio or to BLE
   sensors, are unaffected.
2. **`Client = false`** under `[GATT]` in the same file. BlueZ stops acting
   as a GATT client altogether, so the host can't read BLE devices it
   connects to either.
3. **bluetoothd without its battery plugin.** Add a systemd drop-in with
   `sudo systemctl edit bluetooth`:

   ```
   [Service]
   ExecStart=
   ExecStart=/usr/libexec/bluetooth/bluetoothd -P battery
   ```

   (The path is Debian's and Raspberry Pi OS's; check `systemctl cat
   bluetooth` for yours.) Other plugins that read protected attributes
   could still ask phones to pair.

Then `sudo systemctl restart bluetooth`. `kiss-tnc-bridge -t` warns when a
bridge serves phones and none of these is set, and so does the daemon when
it starts. A phone that paired with the bridge before making the above changes
still has the pairing. Remove it by finding the bridge in the phone's Bluetooth
settings, and tapping on "Forget" or similar. Remove the phone on the host
with `bluetoothctl` (`select` the adapter, then `remove` the phone).


## Configuration

The configuration is split into two TOML files:

- **Static config** in `/etc/kiss-tnc-bridge/`: the main file
  `kiss-tnc-bridge.toml`, plus any `conf.d/*.toml` next to it. Files in
  `conf.d/*.toml` are read in name order, and typically hold one bridge
  per file. You create these files and neither the daemon or `ktbctl` can
  update or change them. `ktbctl enable`, `disable` and `switch` work on bridges
  defined in these files by keeping an override in `/var/lib/kiss-tnc-bridge/state.toml`,
  so switching setups never means editing `/etc`.
- **Dynamic config** in `/var/lib/kiss-tnc-bridge/dynamic.toml`, written by
  the daemon when you use `ktbctl`. Change it with `ktbctl edit` rather than
  by hand while the daemon runs.

Both use the same format, and ids must be unique across them. Pick whichever
suits you. Mix and match as you see fit. Use `ktbctl setup`, which writes to the
dynamic config, for a Raspberry Pi you manage by hand. Static files are great
for a computer in your homelab managed by `ansible` or another configuration
management tool.

The config files use the [TOML](https://toml.io/) file format, which is similar
to the venerable `.ini` format. It has keys and values grouped into tables. A
table starts with a header in square brackets, and the keys under it belong to
it until the next header. This document names each key by its **path**, which
starts with the tables it's inside, then key itself, joined by dots. So `global.log_level`
is the `log_level` key in the `[global]` table:

```toml
[global]
log_level = "info"                 # global.log_level
```

Unlike the `.ini` format, TOML supports list of tables. Double brackets, like
`[[tnc]]`, `[[bridge]]` and `[[bridge.port]]`, start one entry in a list of
tables. Each `[[tnc]]` block is another TNC, and each `[[bridge.port]]` is
another port of the `[[bridge]]` above it, so a port's keys have paths starting
with `bridge.port`:

```toml
[[bridge]]
id = "ktbr0"                       # bridge.id
name = "TH-D75"                    # bridge.name

  [[bridge.port]]
  type = "ble-server"              # bridge.port.type
  max_clients = 2                  # bridge.port.max_clients
```

A path doesn't say which table in the list it means: `bridge.port.max_clients`
is the `max_clients` key in any `[[bridge.port]]`. When this document gives a
path with a value, like `bridge.port.max_clients = 2`, write `max_clients = 2`
in the entry you're changing. (The indenting is only for readability; TOML
ignores it.)

TOML also lets you write a key nested deeper than its table in more than
one way. Each of these options sets `tnc.spp.adapter` to `builtin`. You
can use the one that feels most intuitive to you in your configuration file:

```toml
[[tnc]]                            # an inline table
id = "d75"
spp = { address = "40:79:12:E4:95:9D", adapter = "builtin" }

[[tnc]]                            # dotted keys
id = "d75"
spp.address = "40:79:12:E4:95:9D"
spp.adapter = "builtin"

[[tnc]]                            # a table header for this TNC's spp
id = "d75"

  [tnc.spp]
  address = "40:79:12:E4:95:9D"
  adapter = "builtin"
```

When you run `kiss-tnc-bridge -t`, it reads and validates all the configuration
files, both static and dynamic, and lets you know if there are any errors.
Apply changes with `ktbctl reload`, which interactively asks before disconnecting
any active sessions or TNCs. You can also apply changes with
`sudo systemctl reload kiss-tnc-bridge`, or sending `SIGHUP` to the
`kiss-tnc-bridge` process. A reload changes only the bridges and ports whose
config changed, so if you add a new bridge and reload, the one you already have
running continues uninterrupted.

Here's an example of a bridge for a TH-D75 over Bluetooth, shared with phones, that
mimics the way the B.B. Link device works:

```toml
[[tnc]]
id = "d75"
name = "TH-D75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55" }      # Bluetooth; or [tnc.serial] for USB

[[bridge]]
id = "ktbr0"
name = "B.B. Link"

  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"

  [[bridge.port]]
  type = "ble-server"
```

A software TNC is reached over TCP, with no `[[tnc]]`:

```toml
[global]
log_level = "info"
# adapter = "builtin"              # default adapter for ble-server ports and TNCs

[[adapter]]                        # a readable id for an adapter's MAC
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[bridge]]
id = "ktbr1"
name = "Graywolf"
enabled = true

  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"              # the TNC: a KISS TCP server
  address = "127.0.0.1:6700"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"              # apps on phones, over BLE
  adapter = "builtin"
  max_clients = 2

  [[bridge.port]]
  id = "local"
  type = "tcp-server"              # apps over TCP
  listen = "8001"                  # a TCP port number alone binds 127.0.0.1
```

### Referencing a Bluetooth adapter

Three keys reference a Bluetooth adapter:

- `global.adapter`: the default for the other two
- `tnc.spp.adapter`: the adapter a radio is paired on, for `spp-client`
  ports
- `bridge.port.adapter`: the adapter phones connect to, for `ble-server`
  ports

`ktbctl`'s `--adapter` options take the same values. Each can
identify the adapter in three ways:

| Value | Example | Notes |
|---|---|---|
| An `[[adapter]]` id | `adapter = "dongle"` | Recommended. A name you choose for the adapter's MAC |
| A MAC address | `adapter = "AC:A7:F1:29:9C:7F"` | Always the same hardware. Upper or lower case, six two-digit groups |
| A Linux adapter name | `adapter = "hci1"` | Can change for pluggable adapters like USB bluetooth dongles. Fine for a quick test, but brittle in a config you keep |

Leave `tnc.spp.adapter` or `bridge.port.adapter` out and it uses
`global.adapter`, or, if that isn't set either, the first adapter BlueZ finds.

On a computer with a single adapter, like a Pi Zero W 2, your configuration doesn't
need to mention adapters at all and everything will just work on the built-in adapter.

For setups with more than one adapter, define an `[[adapter]]` for each and use its id
everywhere:

```toml
[[adapter]]
id = "builtin"
mac = "2C:CF:67:6E:2E:BC"           # the Pi's own chip

[[adapter]]
id = "dongle"
mac = "AC:A7:F1:29:9C:7F"           # the USB dongle

[[tnc]]
id = "d75"
model = "th-d75"
spp = { address = "40:79:12:E4:95:9D", adapter = "builtin" }

[[bridge]]
id = "ktbr0"
name = "TH-D75"

  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"

  [[bridge.port]]
  type = "ble-server"
  adapter = "dongle"
```

Here's why:

- **`hciN` numbers aren't fixed.** Linux numbers adapters in the order it
  finds them. Plug a dongle in, or boot with it in a different USB port, and
  `hci0` and `hci1` can swap, so your phones' port lands on the radio's
  adapter. A MAC or an id never moves.
- **Your config says what you mean.** `adapter = "dongle"` is easy to read
  and easy to check; `adapter = "AC:A7:F1:29:9C:7F"` makes you look it up.
  It matters most for `tnc.spp.adapter`, because a radio's pairing
  belongs to the one adapter it was paired on, and a bridge that uses a
  different adapter finds the radio unpaired.
- **One place to change.** When you replace a dongle, change the MAC in its
  `[[adapter]]` and every TNC and port that uses it follows. With bare MACs
  you'd have to find and change each one (and pair the radio again on the
  new dongle, if that's where it was paired).
- **Readable status.** `ktbctl status`, the full-screen view and
  `kiss-tnc-bridge -t` show adapters by id with the MAC alongside:
  `dongle (AC:A7:F1:29:9C:7F)`. A port given a bare MAC is shown the same
  way when an `[[adapter]]` has that MAC.

**Finding an adapter's MAC.** With the daemon running, `ktbctl adapter list`
shows every adapter on the computer, with its MAC and what it is. On a
Raspberry Pi 4 with a USB dongle plugged in:

```
$ ktbctl adapter list
Adapters
  -          hci0   2C:CF:67:6E:2E:BC  powered  built into the board
  -          hci1   AC:A7:F1:29:9C:7F  powered  USB: TP-Link UB500 Adapter
```

The chip on the Pi's board says `built into the board`, and a dongle says
`USB` with its name. The first column is the adapter's `[[adapter]]` id, or
`-` until you give it one. `ktbctl adapter add builtin --mac
2C:CF:67:6E:2E:BC` gives it one in dynamic config. Static config can't
refer to dynamic objects, so a bridge in `/etc` that should use an id needs
the `[[adapter]]` defined in `/etc` too (or a MAC instead).

Without the daemon, `bluetoothctl list` shows each adapter's MAC but not
which is which. `readlink -f /sys/class/bluetooth/hci0` tells them apart:
the Pi's own chip is attached to a serial port, so its path runs through
`serial`, and a dongle's runs through `usb`. Then `btmgmt --index 0 info`
shows the MAC of `hci0` (`--index 1` for `hci1`) on its second line:

```
$ readlink -f /sys/class/bluetooth/hci0
/sys/devices/platform/soc/fe201000.serial/fe201000.serial:0/fe201000.serial:0.0/serial0/serial0-0/bluetooth/hci0
$ btmgmt --index 0 info
hci0:	Primary controller
	addr 2C:CF:67:6E:2E:BC version 9 manufacturer 305 class 0x6c0000
```

### Configuration reference

Many configuration keys have a default value and are therefore not required.

**`[global]`**

| Key | Default | Description |
|-----|---------|-------------|
| `global.log_level` | `info` | `trace`, `debug`, `info`, `warn`, `error` |
| `global.adapter` | first adapter | Default Bluetooth adapter for `ble-server` ports and TNCs: an `[[adapter]]` id, a MAC, or `hciN` |

**`[control]`**

| Key | Default | Description |
|-----|---------|-------------|
| `control.group` | `kiss-tnc-bridge` | The group whose members may use the control socket, and so `ktbctl`. If the group doesn't exist, only root can |

**`[[adapter]]`** gives a bluetooth adapter's MAC a human friendly identifier. `hciN` numbers can
change when a dongle is plugged in, MAC's don't. You can specify MAC's directly on a port, but your
configuration will be easier to read and modify if you define an adapter with a useful name like
`builtin`, `bt500`, or `ub500`. See
[Referencing a Bluetooth adapter](#referencing-a-bluetooth-adapter).

| Key | Description |
|-----|-------------|
| `adapter.id` | Lowercase letters, digits and `-`, at most 15 characters |
| `adapter.mac` | The adapter's address, `XX:XX:XX:XX:XX:XX`; see [Finding an adapter's MAC](#referencing-a-bluetooth-adapter) |

**`[[tnc]]`** is a hardware KISS TNC and the ways to reach it. A TNC can be
in only one enabled bridge, over whichever transport.

| Key | Default | Description |
|-----|---------|-------------|
| `tnc.id` | required | Lowercase letters, digits and `-`, at most 15 characters |
| `tnc.name` | `tnc.id` | Human-friendly |
| `tnc.model` | `generic` | What the TNC gets beyond plain KISS; see [Models](#models). Leave it out for a TNC that isn't listed |
| `tnc.spp.address` | | The TNC's Bluetooth address, for `spp-client` ports |
| `tnc.spp.adapter` | `global.adapter` | The adapter it's paired on: an `[[adapter]]` id, a MAC, or `hciN`. Pairings belong to one adapter |
| `tnc.spp.channel` | found by SDP | An RFCOMM channel, only for devices whose SDP record is wrong |
| `tnc.ble.address` | | The TNC's Bluetooth LE address, for `ble-client` ports. `ktbctl scan --le` finds it |
| `tnc.ble.adapter` | `global.adapter` | The adapter that connects to it: an `[[adapter]]` id, a MAC, or `hciN`. Nothing is paired, so any adapter in range will do |
| `tnc.serial.path` | | The serial device, for `serial` ports. Use its `/dev/serial/by-id/` or `/dev/serial/by-path/` name: `/dev/ttyACM0` can change between boots |
| `tnc.serial.baud` | `9600` | Ignored by USB radios; matters for real UARTs |

**`[[bridge]]`**

| Key | Default | Description |
|-----|---------|-------------|
| `bridge.id` | required | Unique, short and stable: `ktbr0`, `ktbr1`, … |
| `bridge.name` | required | Human-friendly and unique; what phones see |
| `bridge.public_name` | `bridge.name` | The BLE advertised name and DNS-SD name, if it should differ from `bridge.name` |
| `bridge.enabled` | `true` | A disabled bridge holds no resources |
| `bridge.app_params` | `pass` | `drop` stops apps from changing the TNC's KISS parameters (TXDELAY and friends) |

**`[[bridge.port]]`**, with `bridge.port.type` one of:

| Type | Role | Keys |
|------|------|------|
| `spp-client` | TNC | `bridge.port.tnc` (a `[[tnc]]` with `tnc.spp`); `bridge.port.connect`, `bridge.port.idle_timeout`, `bridge.port.exit_kiss_on_disconnect` (below) |
| `ble-client` | TNC | `bridge.port.tnc` (a `[[tnc]]` with `tnc.ble`); `bridge.port.connect`, `bridge.port.idle_timeout`, `bridge.port.exit_kiss_on_disconnect` (below) |
| `serial` | TNC | `bridge.port.tnc` (a `[[tnc]]` with `tnc.serial`); `bridge.port.connect`, `bridge.port.idle_timeout`, `bridge.port.exit_kiss_on_disconnect` (below) |
| `tcp-client` | TNC | `bridge.port.address` (`host:port` of a KISS TCP server such as Dire Wolf or Graywolf); `bridge.port.connect`, `bridge.port.idle_timeout` (below) |
| `ble-server` | app | `bridge.port.adapter` (an `[[adapter]]` id, a MAC, or `hciN`; default: `global.adapter`); `bridge.port.max_clients` (default `1`; many Bluetooth chips can't serve more, see below) |
| `tcp-server` | app | `bridge.port.listen` (required: a TCP port number, which binds `127.0.0.1`, an `address:port`, or an `interface:port` such as `wlan0:8001`); `bridge.port.max_clients` (default: no limit); `bridge.port.allow` (a list of addresses or networks, e.g. `["192.168.4.0/24"]`; default: any peer that can reach the socket); `bridge.port.announce` (DNS-SD, default `true` except on loopback) |

Every port may set `bridge.port.id` (default: the type plus an index,
`ble-server0`) and `bridge.port.role` (`tnc` or `app`, normally implied by
the type). TNC ports also take:

| Key | Default | Description |
|-----|---------|-------------|
| `bridge.port.connect` | `on_demand` | `on_demand` connects the TNC when the first app attaches; `always` keeps it connected with no apps |
| `bridge.port.idle_timeout` | `0s` | With `bridge.port.connect = "on_demand"`, how long to keep the TNC connected after the last app leaves, e.g. `"90s"`. A Bluetooth link to a radio takes a few seconds to come up, so a minute or two avoids a slow reconnect when a phone briefly drops BLE |
| `bridge.port.exit_kiss_on_disconnect` | `false` | `spp-client`, `ble-client` and `serial`: take the TNC out of KISS mode when the bridge lets go of it (a TH-D75 goes back to APRS mode if that's how it was found). By default the radio stays in KISS mode |

Frames from apps are filtered: the KISS "return" command (`C0 FF C0`), which
takes radios like the TH-D75 out of KISS mode for every app, never reaches
the TNC.

When a `ble-server` port has `bridge.port.max_clients` connected, the daemon stops
advertising until one leaves. Several bridges can be defined, but among the
enabled ones a TNC, a listen socket, a public name and a BLE adapter can
each be used only once. Disabled bridges can share them freely, which is
what makes switching between setups work.

### Sharing over the network

**KISS over TCP has no authentication: anyone who can reach a `tcp-server`
can transmit on your radio under your callsign.** That's why a bare TCP port
number binds `127.0.0.1`, and why binding to `0.0.0.0` without a
`bridge.port.allow` list gets a warning. To share a radio with a laptop on the Pi's Wi-Fi access
point, bind to that interface and limit who may connect:

```toml
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "wlan0:8001"              # only on the access point's interface
  allow = ["192.168.4.0/24"]         # only its clients
```

A `tcp-server` that isn't on loopback is announced on the LAN with DNS-SD
(`_kiss-tnc._tcp`, following the
[TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md)),
so apps like aprs.fi on iOS can pick the bridge from a list. The name is the
bridge's `bridge.public_name`, the same name its BLE advertisement uses. An
interface binding is announced only on that interface. This needs Avahi,
which Raspberry Pi OS runs; without it the daemon logs that once and carries
on. If another device on the LAN already uses the name, the bridge takes the
next one ("TH-D75 (2)") for DNS-SD and BLE alike, and keeps it across
restarts in `/var/lib/kiss-tnc-bridge/state.toml`. Changing `bridge.public_name` in
the config drops the adjustment. `avahi-browse -r _kiss-tnc._tcp` on Linux
or `dns-sd -B _kiss-tnc._tcp` on a Mac shows what's announced.

### Validate config

```
$ kiss-tnc-bridge -t
```

Prints the effective TNCs and bridges with their source files, and warnings
such as a `tcp-server` open to the whole network, a TNC that isn't paired on
its adapter, or a serial path that isn't stable. Exits 0 if valid, 1 if there
are errors. With the daemon running, `ktbctl check` does the same and shows
what a reload would change.


## Migrating from 0.5

1.0 reads a new config file, `/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml`. The
0.5 file, `/etc/kiss-tnc-bridge.conf`, is left alone and not migrated. The
daemon and `kiss-tnc-bridge -t` warn that it's there. Rewrite it by hand, or run
`ktbctl setup` once for each TNC.

Each 0.5 TNC section becomes a bridge with a `tcp-client` port for the TNC
and a `ble-server` port for the phones. This 0.5 config:

```ini
[global]
log_level = info
adapter = hci0

[Graywolf TNC]
host = 127.0.0.1
port = 6700
max_clients = 3

[Winlink TNC]
host = 192.168.1.50
port = 8001
```

looks like this in 1.0:

```toml
[global]
log_level = "info"
adapter = "hci0"

[[bridge]]
id = "ktbr0"
name = "Graywolf TNC"

  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:6700"

  [[bridge.port]]
  type = "ble-server"
  max_clients = 3

[[bridge]]
id = "ktbr1"
name = "Winlink TNC"
enabled = false                    # one ble-server per adapter; see below

  [[bridge.port]]
  type = "tcp-client"
  address = "192.168.1.50:8001"

  [[bridge.port]]
  type = "ble-server"
```

| 0.5 | 1.0 |
|---|---|
| `/etc/kiss-tnc-bridge.conf`, INI | `/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml` and `conf.d/*.toml`, TOML |
| The section name, also the BLE name | `bridge.name` (and `bridge.public_name` if the BLE name should differ) |
| `host` and `port` | `bridge.port.address = "host:port"` on a `tcp-client` port |
| `max_clients` | `bridge.port.max_clients` on the `ble-server` port |
| `adapter` in a TNC section | `bridge.port.adapter` on the `ble-server` port |
| `[global] adapter = hci0` | `global.adapter = "hci0"`, or better an adapter's MAC or an `[[adapter]]` id, since `hciN` numbers can change |
| The TCP connection opened with the first phone and closed with the last | `bridge.port.connect = "on_demand"` with `bridge.port.idle_timeout = "0s"`, the defaults |
| `/run/kiss-tnc-bridge.lock` | `/run/kiss-tnc-bridge/kiss-tnc-bridge.lock` |

Two things work differently:

- **One enabled `ble-server` per adapter.** 0.5 could advertise several TNCs
  from one adapter. BlueZ shows every service on an adapter to every phone
  connected to it, so a phone could attach to the wrong TNC, and mobile devices
  remember only one name per adapter anyway. In 1.0, keep the extra
  bridges disabled and switch between them (`ktbctl switch ktbr1`), or give
  each one its own Bluetooth adapter.
- **Phones shouldn't pair.** Set `ReverseServiceDiscovery = false` (see
  [Phones and pairing](#phones-and-pairing)), and if a phone paired with
  the bridge under 0.5, forget it on both sides.

The rest is new: TNCs over Bluetooth and serial, apps over TCP, several
bridges, and `ktbctl`.


## Kenwood TH-D74 and TH-D75

The TH-D74 and TH-D75 have a KISS TNC built in, reachable over Bluetooth or
over the radio's USB cable, but only one of those at a time: the radio's
`Menu > Configuration > Interface > KISS (983)` setting picks which. With
`tnc.model = "th-d75"` (or `th-d74`; they're the same), the bridge puts the TNC
in KISS mode every time it connects, on whichever band the radio's TNC is
set to. You don't need to turn KISS on from the radio's menu.

`ktbctl setup` does everything below for you. The steps are here for
static config, and for when something goes wrong.

### Over Bluetooth

1. On the radio, set `KISS (983)` to Bluetooth and turn Bluetooth on.
2. Pair the radio once on the Pi, on the adapter the bridge will use for it,
   with the daemon running. The radio uses numeric comparison: it shows a
   six-digit number, and you press OK on the radio within 30 seconds. Put
   the radio in pairing mode (`Menu > Bluetooth > Pairing Mode`), then find
   its address and pair it (`hci0` here; an adapter's MAC or `[[adapter]]`
   id works too):

   ```
   $ ktbctl scan --adapter hci0
   40:79:12:E4:95:9D  TH-D75               Kenwood TH-D75 (model th-d75)    -52 dBm
   $ ktbctl pair --address 40:79:12:E4:95:9D --adapter hci0
   Pairing with TH-D75...
   The radio should show 774523. Does it? [Y/n] y
   Now confirm it on the radio (press OK).
   paired with 40:79:12:E4:95:9D on hci0 (2C:CF:67:6E:2E:BC), and trusted
   ```

   If it fails after about 30 seconds because the radio didn't confirm, OK
   wasn't pressed in time: dismiss any prompt still on the radio, put it in
   pairing mode again and retry. Once a TNC is configured for the radio,
   `ktbctl pair d75` and `ktbctl unpair d75` work by its id. (Without
   `ktbctl`, `bluetoothctl` can pair it too: `agent KeyboardDisplay`,
   `scan bredr`, `pair ADDRESS`, then `trust ADDRESS`.)
3. Configure a `[[tnc]]` with the radio's address and the adapter it's
   paired on, and a bridge with an `spp-client` port for it:

   ```toml
   [[adapter]]
   id = "builtin"
   mac = "2C:CF:67:6E:2E:BC"

   [[tnc]]
   id = "d75"
   name = "TH-D75"
   model = "th-d75"
   spp = { address = "40:79:12:E4:95:9D", adapter = "builtin" }

   [[bridge]]
   id = "ktbr0"
   name = "TH-D75"

     [[bridge.port]]
     id = "radio"
     type = "spp-client"
     tnc = "d75"
     idle_timeout = "90s"

     [[bridge.port]]
     id = "phones"
     type = "ble-server"
   ```

   The same can be made with `ktbctl` (`tnc add`, `bridge add`, `port add`;
   see [Managing it with `ktbctl`](#managing-it-with-ktbctl)).
4. Run `kiss-tnc-bridge -t`. It warns if the radio isn't paired on that
   adapter; the bridge would report the port as unpaired and wait, and
   connect a few seconds after `ktbctl pair`.

With the default `bridge.port.connect = "on_demand"`, the bridge connects to the radio
when the first app attaches. A connect takes about 5 seconds from cold, and
a radio that's off or out of range takes about 5 seconds to fail, after
which the bridge retries with backoff while apps stay connected.

### Over USB

1. On the radio, set `KISS (983)` to USB, and connect it to the Pi with its
   USB cable.
2. Find its stable device name (`ktbctl serial list` shows it too):

   ```
   $ ls -l /dev/serial/by-id/
   usb-JVCKENWOOD_TH-D75-if00 -> ../../ttyACM0
   ```

3. Give the TNC a `tnc.serial` section, and the bridge a `serial` port:

   ```toml
   [[tnc]]
   id = "d75"
   name = "TH-D75"
   model = "th-d75"

     [tnc.serial]
     path = "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00"

   [[bridge]]
   id = "ktbr3"
   name = "TH-D75 USB"

     [[bridge.port]]
     id = "radio"
     type = "serial"
     tnc = "d75"

     [[bridge.port]]
     id = "phones"
     type = "ble-server"
   ```

The same `[[tnc]]` can have both a `tnc.spp` and a `tnc.serial` section, with one
bridge for each that you enable as needed (only one at a time, since the
radio's TNC is on one interface at a time). When the cable is unplugged or
the radio turned off, the port waits for the device to come back and
reconnects.

The D75 reports no USB serial number, so its `by-id` name is the same for
every D75. With two of them on one Pi, use their `/dev/serial/by-path/`
names instead, which follow the USB port they're plugged into.

On desktops that run ModemManager (Ubuntu, for example), it probes new USB
serial devices with modem commands, which can confuse a radio and hold its
port. The packages install a udev rule
(`/lib/udev/rules.d/70-kiss-tnc-bridge.rules`) that tells ModemManager to
leave the TH-D75 alone; the file explains how to add other TNCs. Raspberry Pi
OS doesn't run ModemManager.

### When the radio doesn't transmit

If an app sends a beacon and the radio doesn't key up, look for a red **STA**
on the radio's display. It means the radio's TNC has the packet and is
waiting for the channel to clear before transmitting, as packet TNCs always
have. The radio transmits once the channel is clear, and STA goes out.

If STA stays lit, the radio thinks the channel is busy: there's traffic on
the frequency, or the squelch is open (set too low, or opened by noise).
Close the squelch, or check the radio's DCD sense setting (`Detect Data`,
in the APRS settings; menu 507 on some Kenwood radios), which decides what
counts as busy. Turning the radio off and on clears a stuck queue.

The bridge can't see any of this. Once it has handed a frame to the radio,
its counters, logs and `ktbctl` count the frame as sent, whether or not the
radio has transmitted it yet.

### Other TNCs

Any TNC that speaks KISS over Bluetooth SPP or a serial port should work
with the generic model, as long as it's already in KISS mode (most
standalone TNCs always are). For example, a Mobilinkd TNC3 over Bluetooth,
shared to phones over BLE:

```toml
[[tnc]]
id = "mobilinkd"
name = "Mobilinkd TNC3"
spp = { address = "00:11:22:33:44:55" }   # the TNC's Bluetooth address
# Over USB instead, use its stable name from ls /dev/serial/by-id/:
# serial = { path = "/dev/serial/by-id/usb-…-if00" }

[[bridge]]
id = "ktbr0"
name = "Mobilinkd"

  [[bridge.port]]
  id = "tnc"
  type = "spp-client"              # "serial" for USB
  tnc = "mobilinkd"
  idle_timeout = "90s"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
```

Pair it with `ktbctl pair` as for the TH-D75; it may ask for a PIN or
nothing at all instead of a number to compare.

TNCs on Bluetooth LE that follow the
[BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
such as the Ailunce HA2, use a `ble` section and a `ble-client` port. They
aren't paired. `ktbctl scan --le` lists the ones in range, or
`ktbctl setup` finds one for you:

```toml
[[tnc]]
id = "ha2"
name = "Ailunce HA2"
ble = { address = "34:3E:27:3F:E0:10" }   # from ktbctl scan --le

[[bridge]]
id = "ktbr0"
name = "HA2"

  [[bridge.port]]
  id = "radio"
  type = "ble-client"
  tnc = "ha2"
  idle_timeout = "90s"

  [[bridge.port]]
  id = "phones"
  type = "ble-server"
```

A BLE TNC takes one connection at a time and stops advertising while it has
one, so while the bridge is connected, a phone can't reach the radio
directly; it reaches the bridge instead. With `connect = "on_demand"` (the
default) the bridge lets go of the radio when the last app leaves (after
`idle_timeout`), and the radio advertises again. The Mobilinkd TNC4, BTech
UV-PRO and Vero VR-N76 are expected to work the same way, but haven't been
tested yet.

Some radios carry plain KISS over a GATT service of their own instead of
the BLE KISS API's, and need their `model` so the bridge knows where to
look. The TIDRADIO TD-H9 is one: set `model = "td-h9"` in its `[[tnc]]`.
It doesn't advertise a KISS service, and its name may differ between
radios and firmware versions, so `ktbctl scan --le` lists every named
Bluetooth LE device nearby; pick it by address. On the radio, set
Menu > Bluetooth > BT On/Off to On and BT Mode to Receiver, and
Menu > APRS > APRS Switch to On: with APRS Switch off it transmits, but
passes nothing it receives to the bridge. `ktbctl setup` sets the model when you choose
"TIDRADIO TD-H9" from its list of radios. How the TD-H9's protocol was
worked out, and how to use it from your own code, is in
[docs/td-h9-ble-kiss.md](docs/td-h9-ble-kiss.md).

With the generic model the bridge sends the TNC nothing, so it can't tell a
TNC that's quiet from one that isn't working. If apps hear nothing,
`ktbctl status` and `ktbctl monitor` show whether anything is arriving
from the TNC. Please open an issue if you try a TNC, working or not.

### Models

`tnc.model` says what the TNC gets beyond plain KISS. Leaving it
out means `generic`. Any other value is an error, and `kiss-tnc-bridge -t`
and `ktbctl tnc models` list the models.

| `tnc.model` | For | What it does |
|---|---|---|
| `generic` (default) | Any TNC that's always in KISS mode: NinoTNC, TNC-Pi, Mobilinkd | Nothing beyond KISS. With `bridge.port.exit_kiss_on_disconnect`, sends KISS "return" (`C0 FF C0`) when the bridge lets go |
| `td-h9` | TIDRADIO TD-H9 | KISS over BLE on TIDRADIO's own GATT service (`af00`) instead of the BLE KISS API's; otherwise like `generic` |
| `th-d74`, `th-d75` | Kenwood TH-D74 and TH-D75 | Switches the TNC to KISS mode at every connect; fails with a pointer to menu 983 if KISS is on the other interface; reconnects if the radio stops answering (as it does after a USB replug); with `bridge.port.exit_kiss_on_disconnect`, puts the TNC back in the mode it was found in |

Radios that need commands of their own get a new model once someone can
test one.


## Limitations and Caveats

Bluetooth Low Energy is great, but it does have some caveats and limitations you
should be aware of.

- Most Bluetooth chips have a practical limit to the number of concurrent
  connections, often in the 5-7 range. If you want to run a dozen clients, use
  a `tcp-server` port for most of them.
- A second phone can find the bridge only if the Bluetooth controller can
  advertise while a phone is connected. The Raspberry Pi 4's built-in chip
  can, and so can an RTL8761B USB dongle: each served two phones at once in
  testing, the second finding the bridge while the first was connected. If
  your controller can't, a second phone can connect only once the first
  has left. Each time a phone leaves, the bridge disappears from scans for
  about 3 seconds while it starts advertising again.
- It may take 5-10 seconds from the time a connected Bluetooth device moves out
  of RF range before the operating system notifies `kiss-tnc-bridge` that the
  device is disconnected. This only matters if you have the maximum number of
  clients connected and are antsy to get another one connected.
- A typical APRS TNC doesn't generate that much traffic or load. However, many single
  board computers have made design choices that can impact performance. For example,
  on a Raspberry Pi 4, there is a single chip supporting Wi-Fi and Bluetooth, on a
  single internal antenna. If either Wi-Fi or Bluetooth have meaningful load and/or
  the CPU is under load, you may see Bluetooth performance problems or unreliable
  connections. One possible fix is to move Wi-Fi to a 5GHz network to reduce the RF
  interference. Another possible fix is to add a USB Bluetooth dongle with an
  external antenna.
- Both iOS and Android cache BLE peripheral data using the device's Bluetooth MAC
  address. This can cause all kinds of confusing behavior. For example, connect a
  USB Bluetooth dongle to computer A with a `kiss-tnc-bridge`
  which advertises a Bluetooth TNC called "TNC 1". Then move the dongle to computer
  B, which has a `kiss-tnc-bridge` configuration which advertises a Bluetooth TNC
  called "TNC 2". When you scan in `aprs.fi` or another client program, iOS or Android
  will still show the cached value as "TNC 1". On iOS, the only way to flush the cache
  is to toggle Bluetooth off and on in Settings (it doesn't seem that the Control Center
  toggle always works). This also shows up when you rename a bridge on one
  computer. It also seems that connecting to the "old" TNC name and
  disconnecting will update the operating system cache name.


## Known bugs

- **With two phones on one bridge, one leaving can cut the other off.** When
  a phone disconnects while subscribed (it went out of range, turned
  Bluetooth off, or was kicked), BlueZ closes the *other* phone's
  notification channel by mistake. The other phone stays connected but
  stops receiving until it reconnects: the daemon drops its link within
  about 10 seconds, and aprs.fi reconnects on its own. A phone whose app
  disconnects normally doesn't trigger it. The bug is in BlueZ (seen in
  5.82 and 5.83, and present through 5.87). The fix,
  [gatt-database: Fix freeing wrong client notify IO](https://git.kernel.org/pub/scm/bluetooth/bluez.git/commit/?id=f7e2cd91c2669cf50d07fe05caedf794e88f7476),
  was merged into BlueZ on 26 October 2026 and will be in the first release
  after 5.87. Until your distribution ships that release, two phones on one
  bridge will see this now and then.


## Recommended Hardware

Bluetooth Low Energy (BLE) appeared in version 4.0 of the Bluetooth specifications. It is
available in all hardware supporting Bluetooth 4.0 or higher (e.g. 4.2, 5.0, etc.). It is
not supported by earlier hardware. You'll need a Bluetooth chip or USB adapter which
supports Bluetooth 4.0 or higher in the computer you run `kiss-tnc-bridge` on.

You also need Ethernet or Wi-Fi if you want to connect to KISS TNCs that run on other
devices, or share the TNC with apps over TCP.

Here's some popular devices which `kiss-tnc-bridge` works with:

- Original Raspberry Pi Zero and Pi 1 boards do not have built-in BLE, but it can be
  added with a USB Bluetooth adapter. Use the `armhf` builds. A Raspberry Pi
  Model B+ from 2014 with a TP-Link UB500 dongle shared a TH-D75 over
  Bluetooth with two phones and a TCP app for 13 hours, with no frames lost,
  using one dongle for both the radio and the phones.
- Raspberry Pi Zero W, Pi Zero 2 W, and models 3, 4, and 5 have built-in BLE and
  work out of the box.
- The Raspberry Pi 4's built-in Bluetooth (a Cypress chip) is where most of
  the development and testing happened. It handles a radio over Bluetooth
  Classic and a phone over BLE at the same time (an hour of aprs.fi traffic
  with no drops), and serves two phones at once, the second finding the
  bridge while the first is connected.

If your computer doesn't have a BLE compatible chip, you can buy an inexpensive
USB adapter. If the adapter is supported in Linux and supports Bluetooth version
4.0 or higher it should work. Many USB adapters use the Realtek 8761B chipset or
one of its variants, and served two phones at once in testing. As best I can
tell, all these common adapters use that chipset:

- TP-Link UB500 Plus - I have this adapter, and mine has the Realtek 8761B
- ASUS USB-BT500
- EDUP EP-B3536
- Plugable USB Bluetooth 5 Adapter

If you plug in one of these USB Bluetooth adapters to a computer that already has a built-in
adapter, `ktbctl adapter list` shows each adapter's address and which one is built in (see
[Referencing a Bluetooth adapter](#referencing-a-bluetooth-adapter)). Give them ids with
`[[adapter]]` (or `ktbctl adapter add`) so each port uses the adapter you mean; `hciN`
numbers can swap when adapters are plugged in.

One adapter can carry a radio and phones at once, as the UB500 did on the Model B+
above. A second adapter spreads the load: the radio on the built-in chip
(`tnc.spp.adapter`) and the phones on a USB dongle (`bridge.port.adapter` on the
`ble-server` port). That's worth doing when the built-in chip also runs a busy Wi-Fi
link on the same antenna, as on a Pi 4 (see
[Limitations and Caveats](#limitations-and-caveats)). Each adapter can carry only one
enabled `ble-server` port.

On a Pi 1 or Pi Zero, USB power is tight. On the Model B+, a Wi-Fi dongle and a
Bluetooth dongle together, with `dtoverlay=dwc2,dr_mode=host` in
`/boot/firmware/config.txt`, made the Pi reset itself within minutes. With wired
Ethernet and the line removed, it ran for 13 hours. Leave `dwc2` out unless you
need USB gadget mode, and use a powered USB hub for more than one dongle.

If you find hardware that works or doesn't work, please open an issue and I'll be happy
to add it to this list.


## Interoperable Software

I've tested these software applications and they work great through this bridge:

- [aprs.fi](https://apps.apple.com/us/app/aprs-fi/id922155038) iOS app, over
  BLE and over TCP
- APRSConnect iOS app, over BLE
- Beta of RadioMessenger - more details coming when it's released

Open an issue if you find software that should work, but doesn't.


## Technical documentation

`kiss-tnc-bridge` is an asynchronous daemon written in Rust on the
[Tokio](https://tokio.rs/) runtime. It talks to BlueZ and Avahi over D-Bus,
and `ktbctl` talks to the daemon over a Unix socket. The details are in
[docs/](docs/):

- [Architecture](docs/architecture.md): how the daemon works inside. The
  reconciler that applies config changes, what happens inside a bridge
  (the queues between apps and the TNC, and the TNC link's state machine),
  BLE advertising and the GATT service, KISS framing, and the control
  socket.
- [Control API](docs/control-api.md): the protocol `ktbctl` speaks to the
  daemon, newline-delimited JSON over a Unix socket, for anyone writing
  another client or a script. Its messages are described by a
  [JSON Schema](docs/control-api.schema.json).
- [Roadmap](docs/roadmap.md): where the project goes next. An iOS app,
  firmware for the M5Stack ATOM Lite and Core2, and a web installer, all
  managed through the same protocol.
- [Contributing](CONTRIBUTING.md): building from source, cross builds for
  the Pi, hardware tests, and making a release.


## Releases

This project uses [Semantic Versioning](https://semver.org/). See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to create a release.

Release notes for each release are in the GitHub release. We also
[keep a changelog](https://keepachangelog.com/) in [CHANGELOG.md](CHANGELOG.md).


## License

Copyright (C) 2026 Jared Crapo. GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
