# Phase 0 findings (worksheet)

Working notes for the Phase 0 checklist in
[docs/plan-1.0.md](../docs/plan-1.0.md#phase-0-spikes-before-committing).
Hardware: Raspberry Pi 4 Model B Rev 1.5 (`aprscc`, Debian 13, kernel 6.18,
BlueZ 5.82), built-in Cypress adapter `hci0` (2C:CF:67:6E:2E:BC, UART), TP-Link
UB500 (RTL8761B) dongle `hci1` (AC:A7:F1:29:9C:7F, USB), Kenwood TH-D75
(40:79:12:E4:95:9D, CoD 0x620204, advertises Serial Port, Headset AG, Generic
Audio).

## bb-link source review (done)

**Repos and license.** `islandmagic/bb-link` is upstream (last push June
2026). `halka/bb-link` is not a direct fork of it: it forks `fanlessfan/bb-link`
(which forks upstream) and ports the firmware to the M5Stack ATOM Lite (last
push August 2026). All three carry a GPL-3.0 `LICENSE`, so they're compatible
with this project's GPL-3.0-or-later. Nothing needs porting anyway: the radio
driver is a few command strings.

**Radio driver** (`THD7x.cpp`, byte-identical between forks except for
logging) uses the LA3QMA TH-D74 PC-command set: commands are `XX…\r`, replies
`XX …\r`, an error reply starts with `?`, 1 s reply timeout, up to 3 retries
200 ms apart on `?`. A reply counts as success when it echoes the two command
letters. Commands used:

| Command | Meaning | Used for |
|---|---|---|
| `BT` | Bluetooth on/off (`BT 1`) | KISS-mode probe: a reply means the command parser is listening, silence for 1 s means the TNC is in KISS mode |
| `ID` | Model (`ID TH-D75`) | Identification |
| `TN p1,p2` | TNC mode p1: 0 off, 1 APRS, 2 KISS; band p2: 0 A, 1 B | Read current mode/band; `TN 2,<band>` enters KISS |
| `C0 FF C0` | KISS "return" | Leaves KISS mode |
| `AS p1` | TNC packet baud: 0 = 1200, 1 = 9600 | Only via RadioMail rig control |
| `FQ`, `MD` | Frequency, mode | Only via RadioMail rig control |

**KISS init sequence** (upstream `bleConnectedEnter`, halka
`configureRadioForBLESession`):

1. `BT`. No reply within 1 s → already in KISS mode → remember that, send
   `C0 FF C0`.
2. `TN` → remember mode and band.
3. `TN 2,<band>`. The band is whatever `TN` reported, not a fixed VFO.

Upstream runs this only when a BLE client connects. halka's "explicit KISS
initialisation" is the same sequence, also run when the Classic link to the
radio (re)connects while a BLE client is attached, so a radio power-cycle
re-enters KISS mode. That is the behavior to copy: run init on every TNC
connect.

**Teardown** (`bleConnectedExit`): only if the previous mode was neither KISS
nor unknown: `C0 FF C0`, re-probe with `BT` and exit again if still silent,
then `TN <previous>,<band>`. So bb-link restores APRS or off, and leaves the
radio in KISS mode if that's how it found it. Our `exit_kiss_on_disconnect`
default of `false` is the same spirit; the restore-previous-mode form is a
possible refinement.

**D74 vs D75.** No difference anywhere in the code. One `THD7x` class and the
same strings serve both; Classic discovery filters by CoD `0x620204`, which
both radios report. The README's only model note is the D75 KISS interface
menu (`Menu > Configuration > Interface > KISS (983)` → Bluetooth), which has
no equivalent in the firmware. **So the plan's `model = "th-d74" | "th-d75"`
can be one `th-d7x` profile** (keeping both names as aliases costs nothing).

**Pairing.** ESP32 side: SSP enabled, legacy PIN `0000` as fallback,
`onConfirmRequest` always answers yes. The README says the radio shows a PIN
prompt the user accepts with OK. So the D75 uses SSP; on the adapter side
bb-link auto-confirms. On BlueZ this maps to either Just Works (NoInputNoOutput
agent) or `RequestConfirmation` (DisplayYesNo/KeyboardDisplay agent).
`spp-spike pair` records which callbacks actually fire.

**Connection.** `btSerial.connect(address, channel 0, ESP_SPP_SEC_NONE,
ROLE_MASTER)`: channel 0 makes the ESP32 stack do an SDP lookup for the SPP
UUID, so bb-link also relies on SDP rather than a fixed channel, with no
link-level security requested. The connect is run in a task because it blocks;
while disconnected it retries every 15 s (`RETRY_BTC_CONNECT_INTERVAL`). The
last paired radio is stored in NVS and reconnected at boot. Known issue in the
README: the ESP32 stack can't discover after it has connected once.

**Rig control** (frequency set/restore, baud) is not part of init. RadioMail
drives it with KISS SetHardware (`0x06`) frames carrying bb-link's extended
commands (`0xEA` set frequency, `0xEB` restore, `0xF4` baud, `0x7E`
capabilities query, …), intercepted in `KISSInterceptor`. RadioMail only
sends them after the capabilities query answers with `CAP_RIG_CTRL`. Our
bridge won't answer it, so RadioMail won't send them. Note for the KISS policy
filter: a `0x06` frame from an app is harmless to pass, since the D75's own
SetHardware handling is unknown; `app_params = "drop"` covers the cautious
case.

**Other.** halka lowers the default BLE MTU assumption from 512 to 23 and adds
write queueing, which hints at BLE write-splitting bugs upstream; irrelevant to
us (bluer and iOS negotiate the MTU).

## Host observations (done)

- Both adapters were rfkill soft-blocked (`PowerState: off-blocked`), and
  BlueZ's `Powered = true` fails with `org.bluez.Error.Failed` in that state.
  `bluetoothctl power on` can't fix it; clearing `/sys/class/rfkill/*/soft`
  needs root. **Plan impact:** the daemon runs as root and should clear a soft
  block itself (or at least `-t`/`doctor` and `status` should say "rfkill
  blocked" instead of a bare power-on failure).
- `rfkill` isn't installed on Raspberry Pi OS Trixie; sysfs is the portable
  way.
- Any user can talk to `org.bluez` (`<policy context="default"><allow
  send_destination="org.bluez"/>`), so `ktbctl`-style tooling doesn't need the
  `bluetooth` group for D-Bus; it only needs the daemon's socket.
- Avahi is running; `avahi-browse` is installed. ModemManager is not running.

## USB (done)

Radio menu: the D75's KISS interface is a single selector, **Bluetooth or
USB, never both**: `Menu > Configuration > Interface > KISS (983)`. Confirmed
by the owner. That settles the last checklist item: **a TNC can be in at most
one enabled bridge, over whichever transport**, exactly as the plan assumes.
It also means the radio's Bluetooth can be off while the TNC is on USB, and
`BT 0` is what the command port reports then.

What the Pi sees with the cable in and KISS set to USB:

- `lsusb`: `2166:9023 JVC Kenwood TH-D75`, full speed, four interfaces:
  CDC ACM control + data (`cdc_acm` → `ttyACM0`) and two USB audio
  interfaces (`snd-usb-audio`). One serial port only; the TNC, the PC
  command set and (presumably) GPS output all share it.
- `/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00 → ttyACM0`. **The radio
  reports no USB serial number** (`SerialNumber=0`), so the by-id name has no
  serial in it and two D75s on one host would collide there; by-path
  (`platform-…-usb-0:1.4:1.0`) is the tiebreaker. The `-t` warning rules
  should accept both, and the docs should say by-id is unique per model,
  not per radio.
- udev: `ID_VENDOR=JVCKENWOOD ID_MODEL=TH-D75 ID_VENDOR_ID=2166
  ID_MODEL_ID=9023`. The packaged ModemManager rule should match
  `2166:9023`. ModemManager isn't running on Raspberry Pi OS Trixie, so
  nothing probed the port here.
- No `dialout` issue for the spike (the user is in `dialout`); the daemon
  runs as root anyway.

**KISS over USB uses the same init as over Bluetooth.** `spp-spike serial
… --init-only` at 9600 (ignored by CDC ACM) ran the bb-link sequence:

```
> BT            (no reply in 1 s: already in KISS mode)
> C0 FF C0
> BT            < BT 0         0.9 ms   (Bluetooth is off on the radio)
> ID            < ID TH-D75    0.6 ms
> TN            < TN 0,1       0.6 ms   (mode off after the exit, band B)
> TN 2,1        < TN 2,1       5 ms
```

Replies are sub-millisecond, and `TN 2,<band>` is acknowledged before the
TNC switches. Note `TN` after a KISS exit reports mode 0 (off), so the
mode to restore has to be remembered from the probe, as bb-link does
(`previousTNCMode = KISS` when `BT` was silent). A 90 s bridging run then
decoded a real packet (`KI7DAN-2>APDW18,K0TFU-1*,WIDE1*,WIDE2-1`), so
framing and the AX.25 display decode work. The spike leaves the radio in
KISS mode on exit unless `--restore-on-exit` is given.

## RFCOMM connect with the radio unreachable (done)

With the radio's Bluetooth off (KISS interface on USB, `BT 0`), 22 back-to-back
`Device.ConnectProfile(SPP)` calls on hci0 each failed after **5.13 s** with
`br-connection-page-timeout`. That is the controller's default page timeout
(0x2000 slots × 0.625 ms = 5.12 s); nothing in BlueZ or bluer waits longer, and
the failure is immediate once the controller gives up. Out of range should look
the same, since the page simply gets no response. **Plan impact:** the TNC port
state machine can treat a connect as "about 5 s to fail" when the radio is off,
so a `Connecting` state with a 30 s backoff cap is reasonable, and this is
exactly why the connect must not run inline in the bridge's select loop.

Spike note: a first version of the spike swallowed SIGINT once it had awaited
`ctrl_c()` in one place, so `timeout -s INT` couldn't stop it. Fixed with a
process-wide stop signal; it's a trap for the daemon too, where every long
await needs to be cancellable from one place.

## SPP through a BlueZ profile (done)

Radio switched to KISS over Bluetooth, already paired and trusted on hci0.
`spp-spike -a hci0 connect …` registers an SPP **client** profile
(`RequireAuthentication = true`, `AutoConnect = false`) and calls
`Device.ConnectProfile(0x1101)`; BlueZ does the SDP lookup, opens RFCOMM and
hands the fd to the profile's `NewConnection`, which bluer surfaces as a
`ConnectRequest` to `accept()`.

| Measurement | Result |
|---|---|
| Cold connect (no ACL link up) | **4.77 s** from `ConnectProfile` to `NewConnection` |
| Warm connect (ACL still up from a profile disconnect 40 ms earlier) | **38 ms** |
| Radio unreachable | 5.13 s page timeout (above) |
| Socket security | `Security { level: Medium }`: authenticated and encrypted, courtesy of the pairing |
| `ConnectRequest` properties | `version = 258` (0x0102, SPP 1.2), no features |
| KISS init | Identical to USB: `BT` silent → exit → `BT 1`, `ID TH-D75`, `TN 0,1`, `TN 2,1`; replies **~6 ms** over RFCOMM vs <1 ms over USB, `TN 2,1` acknowledged in 35 ms |
| Frames | A Mic-E packet (`WY7CDL-9>T0RYTW,K0TFU-1*,WIDE1*,WIDE2-1`) decoded 23 s in, broadcast to the TCP side |

Observations:

- `TN` after the KISS exit reports `TN 0,1` on both transports, so the KISS
  exit resets the TNC mode to off and the "previous mode" really has to be
  remembered from the `BT` probe, as bb-link does.
- After `DisconnectProfile`, BlueZ keeps the ACL link up for a while, which
  is why the second connect was almost free. An `idle_timeout` that holds the
  RFCOMM link is still worthwhile, but reconnects within BlueZ's ACL idle
  window are cheap even without it.
- The profile approach needs no channel number and no `sdptool`, and the
  `channel` override in the plan can stay an escape hatch only.
- The connect is a single D-Bus call that blocks for up to ~5 s; it must be
  driven from a task, not the bridge's select loop, as the plan says.

## Pairing (done on hci1, the RTL8761B dongle)

With an agent that implements every callback (BlueZ advertises it as
`KeyboardDisplay`), the D75 pairs by **SSP numeric comparison**:

```
22:57:18.776  pairing with 40:79:12:E4:95:9D
22:57:20.697  agent: RequestConfirmation passkey=774523    ← the radio shows the same six digits
              (owner presses OK on the radio)
22:57:22.315  paired in 3.5s; paired=true trusted=true connected=true
```

The only callback BlueZ made was `RequestConfirmation`; no PIN, no passkey
entry. Our side can answer it immediately (bb-link does the same with
`confirmReply(true)`), so the operator interaction is entirely on the radio:
**press OK within 30 s of the number appearing**. Two earlier attempts failed
with `Authentication Failed` after exactly 30–32 s because the radio's prompt
wasn't acknowledged in that window; the radio kept displaying the stale
passkey afterwards, and a second attempt while that prompt was still up
failed the same way. So the daemon's pairing flow should: tell the operator to
dismiss any prompt and (re)enter Pairing Mode first, start the pairing, show
the passkey it is confirming so the operator can compare, and treat a 30 s
`Authentication Failed` as "the radio's OK wasn't pressed in time, try again".

**Just Works is refused.** After `unpair`, the same pairing with a plain
`NoInputNoOutput` agent (what the 0.5 daemon registers for phones) reached
SSP and failed in 4.8 s with `Authentication Failed`; the radio showed nothing
and stayed in Pairing Mode. (A first try page-timed-out, a transient right
after the unpair dropped the ACL link; the retry is the real result.) So the
D75 requires an authenticated (MITM-protected) pairing. **Plan impact:** open
question 1 is answered: the pairing-window agent is needed. It only has to
implement `request_confirmation` (DisplayYesNo would do; KeyboardDisplay is
fine), answer it immediately, and relay the passkey to the operator so they
can compare it with the radio and press OK. Keeping the agent capability
permanently at DisplayYesNo would turn phone pairing into numeric comparison,
so the swap-in-swap-out design stands.

The device list on hci1 was empty before, so discovery (Classic inquiry) found
the radio in about 1 s once it was in Pairing Mode (CoD 0x620204, RSSI -68 to
-76, SPP/Headset AG/Generic Audio UUIDs). BlueZ left the ACL link up after
pairing (`connected=true` 3 s later) without connecting any profile.

## Coexistence on one controller (started, cut short)

Setup: `spp-spike -a hci0 connect … --listen 127.0.0.1:8002 --forever`
(RFCOMM to the radio on hci0) with the unchanged 0.5 daemon on hci0
(`coexist.conf`, GATT + advertising, TCP to the spike) and aprs.fi on an
iPhone. Logs: `~/ktb-dev/spike/coexist-spike.log` and `coexist-daemon.log`
on the Pi.

First run, 2026-10-12 23:02–23:10 UTC, stopped after 8 min because the phone
had to leave:

| | |
|---|---|
| RFCOMM link | up 491 s, no drop, connected in 0.64 s |
| Daemon | up from 23:06:57, 0 errors or warnings |
| Phone | subscribed 23:07:49 (Just Works pairing, prompt on the phone), so **2 min 48 s** with BLE and RFCOMM live on the same controller |
| Frames | 24 received from the radio, forwarded to the daemon; the owner saw a raw packet arrive in aprs.fi |

Too short to count as the hour. **One thing to watch when it resumes:** the
last frame from the radio arrived at 23:07:55, seconds after the phone
subscribed, and nothing arrived in the remaining 2½ minutes, after 23 frames
in the previous 5. APRS traffic is bursty (the earlier burst was one message
exchange), so this is probably chance, but the resumed run should confirm
that RX keeps flowing with BLE attached on the same chip. iOS name caching
showed the old alias for the adapter's address (the README's known caveat);
pairing and data still worked.

Second run, started 23:15:54 with an iPad instead: the spike's first five
`ConnectProfile` attempts page-timed-out (the radio had been idle since the
teardown at 23:10:37), then attempt 6 at 23:16:45 connected in 653 ms and
frames resumed. `hcitool con` afterwards listed the ACL to the radio as
**incoming** (`< ACL … CENTRAL AUTH ENCRYPT`), and the owner saw the radio
report itself connected. So the D75 seems to (re)connect to its paired host on
its own, headset-style, and a connect that lands while that is happening
rides the radio's ACL. Not conclusive from one occurrence; worth watching in
the hour run and in Phase 2 testing. Either way the retry loop recovered
without help, which is the behavior the plan wants.

The hour run proper started at 23:20:20 UTC (iPad subscribed, daemon TCP to
the spike up; radio link up since 23:16:46), both on hci0.

**Result, 23:20:20–00:21:47 UTC (61 min with the iPad attached; radio link
up 65 min from 23:16:46):**

| | |
|---|---|
| RFCOMM link to the radio (hci0) | Up the whole time, no drop; 398 frames received, 33 318 bytes, all forwarded |
| BLE (hci0) | One iPad on aprs.fi, subscribed at 23:20:20, never disconnected; packets visible in the app |
| 0.5 daemon | 0 errors, 0 warnings, one TCP connect and no reconnects; RSS 5.3 MB after 67 min |
| Spike | 5 page-timeout errors, all before the link came up (the reconnect episode above); none after |
| TX | 0 frames from the app (aprs.fi was receiving only), so uplink under BLE load is untested here; Phase 1's CI test and the Phase 2 matrix cover it |

**Same-controller coexistence is fine on the Pi 4's built-in chip** for a
receive-heavy hour. Both processes were left running afterwards.

To resume: start the spike (`nohup ./spp-spike -a hci0 connect
40:79:12:E4:95:9D --listen 127.0.0.1:8002 --forever > coexist-spike.log
2>&1 &` in `~/ktb-dev/spike`), then the daemon with sudo
(`sudo /usr/bin/kiss-tnc-bridge -c ktb-dev/spike/coexist.conf -d 2>&1 | tee
ktb-dev/spike/coexist-daemon.log`), connect aprs.fi, leave it an hour; then
the same with `adapter = hci1` in `coexist.conf` for the two-adapter case.

## BLE advertising on the Pi 4's built-in controller (done, incidental)

Two things learned while getting the iPad attached for the hour run:

- **The advertised name lives in the scan response.** BlueZ 5.82
  (`src/advertising.c`, `generate_scan_rsp`) always puts `LocalName` into the
  scan response, never the advertising packet, unless extended advertising
  is in use. The advertising packet carries flags (3 bytes) and the 128-bit
  KISS service UUID (18 bytes); the scan response carries the name, so the
  name can be up to 29 bytes, which is where the plan's "about 29 bytes of
  UTF-8" limit comes from. Confirmed from the dongle: `spp-spike -a hci1
  lescan` saw hci0 as `name="TH-D75 via spike"` with the KISS UUID.
- **The built-in Cypress controller does not advertise while a BLE central
  is connected.** With the iPhone's LE link still up (it had auto-reconnected
  after the daemon restart) and `max_clients = 2`, so the daemon kept its
  advertisement registered, the iPad could not see the bridge at all; it
  appeared the moment the iPhone disconnected, and the dongle's LE scan saw
  hci0's advertisement only in its last second, at that same moment. So on
  this chip `max_clients > 1` can't be reached by discovery; a second phone
  can only connect if it already knows the address and reconnects on its
  own. The 0.5 README's "(if hardware supports it)" is exactly this.
  **Plan impact:** `status`/`doctor` should report whether the advertisement
  is actually active (BlueZ's `LEAdvertisingManager1.ActiveInstances`
  after a connection would show it), and the Phase 2 test matrix should
  check whether the RTL8761B keeps advertising with one phone connected,
  which decides whether "two phones" needs two adapters on Pi hardware.

## Coexistence on two adapters (started 01:04 UTC)

Radio on hci0 (the spike, same process as before, link up since 23:16:46),
BLE on hci1 (the 0.5 daemon restarted with `coexist-hci1.conf`). The iPad
subscribed at 01:04:04 UTC. At 01:05:11 a **second** phone subscribed while
the iPad was still attached, and the daemon then logged `max clients reached,
stopping advertisement` (`ActiveInstances` on hci1 went 1 → 0). `hcitool con`
showed both LE peripheral links on hci1 plus the radio's ACL on hci0.

So, unlike the Pi 4's built-in chip, **the RTL8761B keeps advertising while
a central is connected and serves two centrals at once**; the only thing
that stops its advertisement is the daemon's own `max_clients` logic. An LE
scan from hci0 during that window saw nothing from hci1, but it started one
second before the second phone connected and the daemon withdrew the
advertisement, so the scan is inconclusive and the second connection is the
evidence. (The scan also didn't disturb the radio link on hci0.)
**Plan impact:** "two phones" works on a dongle; on the Pi's built-in chip it
needs the second phone to reconnect on its own. The Phase 2 matrix row
"built-in for radio, RTL8761B dongle for BLE" is the recommended layout for
more than one phone.

Stopped by choice after 4 min with both phones attached (01:04–01:08 UTC):
daemon 0 errors, 0 disconnects, 2 clients, RSS 5.3 MB; the radio link on
hci0 ended at 6680 s (1 h 51 min) continuous, 642 frames received across
both coexistence runs, never dropped. Nothing more to learn from a full hour
on this layout.

## Hardware checklist (complete)

- [x] **Pairing on hci1 (RTL8761B):** `RequestConfirmation` only (numeric comparison), OK on the radio within 30 s; Just Works refused. hci0 was paired before Phase 0.
- [x] **SPP connect via BlueZ profile + KISS frames received.**
- [x] **RFCOMM connect time** (4.8 s cold, 38 ms warm) and behavior with the radio off / out of range (5.13 s page timeout, no hang).
- [x] **Coexistence, one controller:** 61 min with an iPad on aprs.fi, no drops, no errors.
- [x] **Coexistence, two adapters:** radio on hci0, BLE on hci1, two phones at once, clean.
- [x] **USB:** `lsusb`, `/dev/serial/by-id`, which interface is the TNC, radio USB menu settings.
- [x] **KISS over USB:** same init commands?
- [x] **TNC on Bluetooth and USB at once?**
