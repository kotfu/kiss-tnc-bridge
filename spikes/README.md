# Spikes

Throwaway code and procedures. Nothing here ships.

## Phase A (ESP32 firmware)

For the Phase A checklist in
[docs/roadmap.md](../docs/roadmap.md#phase-a-spikes). Each spike is an
ESP-IDF project with its procedure and findings in its own README.

- [`atom-lite-memory`](atom-lite-memory/README.md): heap on an ATOM Lite
  with SPP, the BLE KISS service and a management service all running.

## Phase 0 (the daemon)

For the Phase 0 checklist in
[docs/plan-1.0.md](../docs/plan-1.0.md#phase-0-spikes-before-committing).
Results go in [FINDINGS.md](FINDINGS.md); the condensed version is appended
to the plan when Phase 0 closes.

### `spp-spike`

One binary with everything the Bluetooth and serial questions need. It is
Linux-only (bluer) and is cross-built for the Pi:

```
spikes/spp-spike/deploy.sh          # cross build + rsync to the Pi from scripts/pi-deploy.local
ssh pi ktb-dev/spike/spp-spike --help
```

It needs no root: BlueZ's D-Bus policy lets any user talk to `org.bluez`, and
the serial device only needs the `dialout` group. The one root step is
clearing rfkill if an adapter shows `PowerState: off-blocked`
(`sudo sh ktb-dev/spike/unblock-bt.sh`).

| Subcommand | Answers |
|---|---|
| `adapters` | Which adapters exist, their MACs, power and rfkill state, and the devices BlueZ knows per adapter |
| `scan` | Classic discovery; flags Kenwood handsets (CoD `0x620204`) and SPP devices |
| `pair ADDR [-a hciN] [--pin 0000]` | Pairs with an agent that implements and logs every BlueZ callback (PIN, passkey, confirmation, authorization), so the record shows exactly what the D75 asks for |
| `unpair ADDR` | Removes the pairing on that adapter |
| `connect ADDR [-a hciN] [--listen 127.0.0.1:8002] [--forever] [--init-only] [--channel N] [--no-auth]` | Registers an SPP client profile, `ConnectProfile(SPP)`, takes the RFCOMM stream, runs the bb-link KISS init, bridges to a KISS TCP server. Logs connect time, every command and reply, every frame (with AX.25 addresses), and a stats line a minute. `--forever` keeps reconnecting and logs each attempt's duration, which is how page timeouts get measured. `--channel` bypasses the profile for comparison |
| `serial PATH [--baud 9600] [--listen …] [--forever]` | The same over USB/UART |

`-v` adds raw bytes.

### `ble-relay`

A BLE man in the middle for learning a radio's GATT protocol from its own
app (Linux-only, cross-built like `spp-spike` with
`spikes/ble-relay/deploy.sh`). It connects to the radio, copies its GATT
services (same UUIDs and properties) into a GATT application of its own,
and advertises that under another name. The app connects to the relay;
every read, write, subscription and notification is passed through and
logged with a timestamp. Notifications on the radio are turned on only when
the app subscribes, so the radio sees what it would see from the app.

```
ssh -t pi 'cd ktb-dev && ./spike/ble-relay FB:5D:C0:5E:D5:7E | tee h9-relay.log'
```

`--name` sets the advertised name (default `TD-H9-RELAY`), `--adv-uuid` the
advertised service UUIDs (default: the radio's own), `-a` the adapter.

Found with it (5 November 2026): the TIDRADIO
TD-H9 carries plain KISS on service `af00`, writes to `af01` (write without
response) and notifications on `af02`, each frame whole in one BLE packet,
padded with extra FENDs (`c0 c0 c0 c0 c0 00 … c0 c0 c0`). The app sends
the standard KISS parameters (TXDELAY 30, persistence 63, slot time 10, TX
tail 0, full duplex off) at connect; the radio transmits without them. Its
`ff00` service isn't used for KISS. The full write-up is
[docs/td-h9-ble-kiss.md](../docs/td-h9-ble-kiss.md).

### Coexistence test (BLE GATT + RFCOMM on one controller)

Today's daemon is unchanged and sits in front of the spike's TCP server, so
the chain is the real Phase 1 data path minus the code:

```
TH-D75 ──SPP──> spp-spike (hci0, --listen 127.0.0.1:8002) <──TCP── kiss-tnc-bridge 0.5 (hci0) <──BLE── iPhone aprs.fi
```

1. `ssh pi ktb-dev/spike/spp-spike -a hci0 connect 40:79:12:E4:95:9D --listen 127.0.0.1:8002 --forever`
2. In another terminal: `ssh -t pi sudo /usr/bin/kiss-tnc-bridge -c ktb-dev/spike/coexist.conf -d`
   (`coexist.conf` points the daemon at 127.0.0.1:8002 on hci0; the installed
   service is left stopped).
3. Connect aprs.fi on the iPhone to "TH-D75 via spike", leave it for an hour,
   and watch both logs for drops. The spike's stats line shows frames and the
   time since the last received frame.
4. Repeat with `-a hci1` on the daemon side (edit `adapter` in the conf) for
   the two-adapter case: radio on the built-in chip, phones on the dongle.

### USB

With the D75 plugged in, `ssh pi sh ktb-dev/spike/usb-inspect.sh` dumps
`lsusb`, `/dev/serial/by-id`, the udev properties of each tty and the kernel
log. Then `spp-spike serial /dev/serial/by-id/… --init-only` shows whether the
same `BT`/`ID`/`TN` init works over USB.
