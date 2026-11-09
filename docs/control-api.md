# kiss-tnc-bridge control API, version 1

Every member of the family is managed through one API: the daemon, and the
ATOM Lite and Core2 firmware (see the [roadmap](roadmap.md)). `ktbctl` (the
CLI and TUI) is a client, and so will be `ktb-gateway`, which carries the
same messages to web and phone apps, and the iOS app. The types are defined
once, in [`src/control/protocol.rs`](../src/control/protocol.rs), and
published as a JSON Schema in
[`control-api.schema.json`](control-api.schema.json) for apps in other
languages.

The device transports, [authentication](#authentication) and the
[device commands](#devices) are a draft for the firmware (roadmap Phase B):
nothing implements them yet, and parts marked provisional may change.

## Transports

The same messages travel over three transports:

| Transport | Served by | Who can use it |
|---|---|---|
| [Unix socket](#unix-socket) | The daemon | Members of its group |
| [USB serial](#usb-serial) | The ATOM Lite and Core2 firmware | Anyone with the device and a cable |
| [BLE management](#ble-management) | The firmware, and `ktb-gateway` on a Pi | Trusted clients, over an encrypted session (see [Authentication](#authentication)) |

### Unix socket

- The socket is `/run/kiss-tnc-bridge/control.sock` (the runtime
  directory), or wherever the daemon's `--socket` option puts it. `ktbctl`
  takes `--socket` or `KTBCTL_SOCKET`.
- It's mode 0660, owned by root and the group in `[control] group`
  (default `kiss-tnc-bridge`, which the packages create). Members of that
  group can manage the daemon; nobody else can connect. If the group
  doesn't exist, only root can.
- For management the daemon serves nothing else. Nothing on the network
  or over BLE reaches this API except through `ktb-gateway`, which does
  its own authentication.

### USB serial

- The board's USB serial port at 115200 baud, 8N1, speaking the same
  newline-delimited JSON as the socket.
- The port also carries the firmware's log. Protocol messages are the
  lines that start with `{`; clients skip the others.
- **Opening the port must not toggle DTR or RTS.** On both boards they
  drive the ESP32's reset and boot pins, so toggling them reboots the
  device or puts it in its download mode. Clients open the port with both
  deasserted.
- No authentication. Anyone with the device and a cable can reflash it
  anyway (roadmap decision 12), so USB is trusted. It's also how a lost
  setup code is recovered (`auth.code`).
- Firmware updates don't use this transport: `ktbctl device flash` writes
  the flash directly, as the web installer does.

### BLE management

One GATT service, with the same UUIDs on the firmware and on
`ktb-gateway`, so the iOS app has one code path for every device:

| UUID | Characteristic | Properties | Carries |
|---|---|---|---|
| `e3370001-156d-4ba6-90dd-14075e7ca557` | (the service) | | |
| `e3370002-156d-4ba6-90dd-14075e7ca557` | TX | write, write without response | Records from the client |
| `e3370003-156d-4ba6-90dd-14075e7ca557` | RX | notify | Records from the device |
| `e3370004-156d-4ba6-90dd-14075e7ca557` | Data | write without response | Firmware image bytes during an update (see [`firmware.update`](#firmwareupdate)) |

**Records.** Each way, the bytes form a stream of records. A record is a
2-byte big-endian length and then that many bytes, the first of which is
its type:

| Type | Record | Rest of the record | Sent |
|---|---|---|---|
| `0x01` | Session handshake | A Noise handshake message | Both ways, to start a session |
| `0x02` | Enrollment handshake | A Noise handshake message | Both ways, to enroll a new client |
| `0x03` | Message | A Noise transport message: one encrypted request, response or event, as JSON | Both ways, once a handshake is done |
| `0x04` | Refusal | JSON in the clear: `{"code": "…", "message": "…"}` | By the device, before closing the connection |

- Records are split across writes and notifications of at most the ATT
  MTU less 3 bytes, and one write or notification can hold the end of one
  record and the start of the next. The receiver reassembles them by
  length. Devices offer an MTU of 517; clients should ask for the largest
  they can.
- A record is at most 65,535 bytes, so one JSON message is at most 65,518
  bytes (less the type byte and Noise's 16-byte tag). Devices take smaller
  requests: `device.info` gives the largest as `max_message`.
- One session per connection. Devices allow only a few BLE connections in
  all (the ATOM Lite two, shared with KISS apps), so clients disconnect
  when they're done.
- `activity` and `frames` events are throttled over BLE.

**Advertising (provisional).** Devices put the [BLE KISS
API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
service UUID in their advertising data, so KISS apps find them as before,
and the management service UUID in their scan response, with as much of the
name as fits. The full name is the GAP Device Name. Clients find
manageable devices by the management service UUID. A 31-byte advertisement
can't hold two 128-bit UUIDs and a long name, so this layout needs testing
with KISS apps in Phase C. `ktb-gateway` advertises the management service
in its own advertisement.

## Framing

One JSON object per line each way on the socket and USB serial, and one per
message record over BLE. Every message carries `"v": 1`.

```jsonc
→ {"v":1,"id":1,"cmd":"status"}
← {"v":1,"id":1,"ok":true,"result":{ … }}
→ {"v":1,"id":2,"cmd":"bridge.disable","bridge":"ktbr1"}
← {"v":1,"id":2,"ok":false,"error":{"code":"reload_would_disrupt","message":"…","data":{ … }}}
← {"v":1,"event":"state","what":"tnc","object":"ktbr0/radio","state":"connected"}
```

- A **request** has `cmd`, an `id` the client chooses (a number), and the
  command's parameters. `id` always belongs to the request: the objects a
  command acts on are named by their kind (`bridge`, `port`, `tnc`,
  `adapter`, `session`, `object`), and new definitions go under
  `definition`.
- A **response** echoes the `id`, with `"ok": true` and a `result`, or
  `"ok": false` and an `error`. Commands run one at a time, in the order
  the daemon receives them, so each sees the state the last one left.
  A client may send more requests before the answers come: each is
  answered by its id when it's done, which for a scan or a pairing can be
  a while after the requests behind it.
- An **event** has `event` and no `id`; it arrives only on a connection
  that subscribed to its topic, except `pairing`, `found` and `update`
  events, which go to the connection whose `bluetooth.pair`,
  `bluetooth.scan` or firmware update they belong to.
- Over the socket a request line can be up to 1 MiB. Devices take less;
  see `max_message` in [`device.info`](#deviceinfo).

**Versioning.** Additive changes (a new command, a new optional field, a
new error code, a new event field) stay in version 1, so clients should
ignore fields they don't know. Anything that would break a version 1
client needs version 2. A request with another `v` gets
`unsupported_version`.

### Compatibility

The software (the daemon, `ktbctl`, `ktb-gateway`) and the firmware are
released separately, each with its own semver version: `v1.3.0` for the
software, `firmware-v1.0.0` for the firmware (roadmap decision 15).

- **The major version is the protocol version.** Software 1.x and firmware
  1.x speak version 1 of this API, and work together; 2.x of either
  speaks version 2. A major version of either one that breaks nothing in
  the API still moves the other's to match, so the rule stays simple.
- **Within a major, features are found, not assumed.** A firmware feature
  is announced by a capability in [`device.info`](#deviceinfo); a client
  offers it when the capability is there, and otherwise says it needs
  newer firmware. Old firmware keeps working with new software, and the
  reverse.
- **Device support starts with Phase D's `ktbctl`.** Earlier 1.x releases
  don't manage devices.

Release notes say which major a release works with, and name a minimum
version of the other only when a new feature needs it.

## Authentication

Over BLE, management is authenticated and encrypted inside the protocol,
not by Bluetooth pairing. An ATOM Lite or a headless Pi has no screen to
show a passkey, Just Works pairing gives no protection against an attacker
in the middle, and bonding phones causes trouble with BlueZ on a Pi. So
clients connect without pairing, and run a
[Noise](https://noiseprotocol.org/noise.html) handshake inside the
management service instead. The Unix socket and USB serial don't use any of
this: their access is controlled by who can reach them.

### Keys and setup codes

- **The device's key.** Each device, and `ktb-gateway`, makes an X25519
  key pair the first time it starts, and keeps it. Its **fingerprint** is
  the first 10 bytes of the SHA-256 of the public key, written like a setup
  code.
- **The setup code.** Each device also makes a setup code: 80 random bits,
  written as 16 [Crockford base32](https://www.crockford.com/base32.html)
  characters in groups of four, such as `7KQ2-M9XD-4TPA-W3HN`. Clients
  ignore case and dashes, and read `O` as `0` and `I` and `L` as `1`. The
  code is permanent: `auth.rotate_code` makes a new one, and clients that
  are already trusted keep working.
- **Clients' keys.** Each client makes its own X25519 key pair and keeps
  it: the iOS app in the keychain, `ktbctl` in its configuration directory.
- **Trusted clients.** The device keeps the public key of each client it
  trusts, with the client's name, when it enrolled and when it was last
  seen. There's one level: a trusted client can do everything, including
  revoking other clients. Anyone with the device and a USB cable can
  reflash it anyway, so separate owner and guest levels wouldn't protect
  anything. The ATOM Lite keeps up to 8 (provisional).

### The management window

A client can enroll only while the management window is open, for 5
minutes (provisional) after it's opened by:

- a long press on the ATOM Lite's button, 3 seconds (see
  [atom-lite-ui.md](atom-lite-ui.md#the-window))
- a menu item on the Core2
- `ktbctl gateway open` on a Pi, or the Pi starting up
- `auth.window`, from a trusted client

One window lets in one client: it closes after the first successful
enrollment, including a client that was already trusted and enrolled
again, so enrolling two clients takes two windows. A wrong code doesn't
close it, but 3 failed enrollments (provisional) do. Otherwise it stays open
until its time runs out; `auth.window` with `seconds: 0` closes it early,
and so does a single press of the ATOM Lite's button.
The rule is the same however the window was opened (see
[atom-lite-ui.md](atom-lite-ui.md#the-management-window)).

### Handshakes

The client starts the handshake. Both patterns use the prologue
`ktb-mgmt/1` (ASCII), and the payloads are JSON:

```
-> e
<- e, ee, s, es    payload: {"fingerprint": "…", "name": "…", "board": "atom-lite", "protocol": 1}
-> s, se           payload: {"name": "Pat's iPhone"}
```

- **Enrolling** a new client uses enrollment records (`0x02`) and
  `Noise_XXpsk3_25519_ChaChaPoly_SHA256`. The PSK is the SHA-256 of the
  ASCII string `ktb-setup-code/1` followed by the setup code's 10 bytes.
  If the client's key is already trusted, enrolling again only updates its
  name.
- **A session** for a trusted client uses session records (`0x01`) and
  `Noise_XX_25519_ChaChaPoly_SHA256`.

Either way, when the handshake is done the connection carries message
records (`0x03`) under the keys it produced. An enrollment doesn't need a
new connection or a second handshake.

The device refuses with a refusal record and closes the connection when:

| `code` | Why |
|---|---|
| `window_closed` | An enrollment handshake while the window is closed. Sent in answer to the first message |
| `bad_code` | The enrollment's last message didn't decrypt: the client's setup code is wrong |
| `not_trusted` | A session handshake from a client the device doesn't trust (never enrolled, or revoked) |
| `too_many_clients` | An enrollment when the trusted list is full; revoke one first |
| `handshake_failed` | Anything else wrong with the handshake |

The client checks the device too:

- **In a session**, the device's static key (in its handshake message) must
  be the one the client saw when it enrolled. If not, the client stops: the
  device was erased and reinstalled, or it isn't the same device.
- **When enrolling** from a setup URI, the device's key must match the
  fingerprint in the URI.

The X25519, ChaCha20-Poly1305 and SHA-256 this needs are in CryptoKit on
iOS, mbedTLS on the ESP32, and the [`snow`](https://crates.io/crates/snow)
crate in Rust.

### Setup URIs

QR codes carry an `https` URI on the project's web home, so a phone's
camera can open the iOS app straight at enrollment, already knowing which
device it's for:

```
https://www.k0tfu.org/ktb#code=7KQ2M9XD4TPAW3HN&fp=Q4M8ZC2V7XKD1B6N
```

`code` is the setup code and `fp` the device's fingerprint. The device's
name isn't in it: the client learns that from the handshake, and leaving it
out keeps the QR code at 37×37 modules (error correction level M), which a
terminal draws in 41 columns and 21 lines. A client given only the 16
characters enrolls without the fingerprint check, with whichever device in
its window accepts the code.

- **It's a Universal Link.** The iOS app claims `www.k0tfu.org/ktb` in its
  Associated Domains, and the site serves
  `/.well-known/apple-app-site-association` naming the app and that path,
  directly, with no redirect. With the app installed, scanning the code
  opens the app; without it, Safari opens the page, which explains what
  the code is for and links to the app. The rest of the site opens in
  Safari as usual.
- **The code and fingerprint go after `#`**, which browsers never send to a
  server. A phone without the app doesn't put the setup code in the site's
  logs. `/ktb` can redirect anywhere on the site: a browser keeps the
  fragment through a redirect whose target has none of its own.
- **The URI is permanent.** Printed codes keep it for the life of the
  device, so `/ktb` has to keep working. Clients that parse it (the app's
  own scanner, `ktbctl`) read the fragment and ignore the host.

### Getting the code to a client

| Device | Where the code comes from | To the iOS app | To `ktbctl` on another machine |
|---|---|---|---|
| Core2 | Its screen, as a QR code and 16 characters | Scan the screen | Type the 16 characters |
| ATOM Lite or Core2, installed from the web | The installer reads it over Web Serial right after flashing | Scan the QR code on the page | Type the 16 characters |
| ATOM Lite or Core2, installed with `ktbctl` | `ktbctl` reads it over USB, and also enrolls itself (`auth.add`), so it never needs the code | `ktbctl device code` shows a QR code in the terminal | Type the 16 characters |
| Any device already set up | `ktbctl device code` over USB, or `auth.code` from a trusted client | A QR code in the terminal or the app | Type the 16 characters |
| A Pi with `ktb-gateway` | `ktbctl gateway code` on the Pi, locally or over ssh | A QR code in the terminal | Not needed: `ktbctl` on the Pi uses the Unix socket |

Terminal QR codes are drawn with Unicode half blocks, which phone cameras
read from a terminal, over ssh too. Every path still needs the management
window open when the client enrolls.

### `auth` commands

The firmware serves these over USB serial and BLE sessions. `ktb-gateway`
serves them on its own local interface (Phase 6); the daemon's socket
doesn't. Public keys are base64, fingerprints and codes as above.

| Command | Parameters | Result |
|---|---|---|
| `auth.info` | | `fingerprint`, `public_key`, `window` (`open`, `seconds_left`), and `clients`, how many are trusted |
| `auth.code` | | The setup code and its URI: `{"code": "7KQ2-M9XD-4TPA-W3HN", "uri": "https://www.k0tfu.org/ktb#code=…"}` |
| `auth.rotate_code` | | A new setup code and URI. Trusted clients keep working |
| `auth.window` | `seconds` (default 300; 0 closes it) | `open`, `seconds_left` |
| `auth.clients` | | The trusted clients: `fingerprint`, `name`, `enrolled`, `last_seen`, and `this: true` for the one asking |
| `auth.add` | `public_key`, `name` | Trusts a client directly, without the code or the window: how `ktbctl` enrolls itself over USB. Returns the client's `fingerprint` and the device's `public_key`, which the client keeps for its sessions |
| `auth.revoke` | `client` (a fingerprint) | Stops trusting a client and closes its sessions. Returns `{}` |

## Errors

```json
{"code": "static_object_readonly",
 "message": "bridge ktbr0 is static config (static: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml); edit that file instead, or clone it into a dynamic one",
 "object": "static: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml"}
```

`code` is stable, for scripts and clients to match on. `message` is for
people and says what to do next. `object` names what the error is about,
and `data` carries detail for some codes.

| Code | Meaning |
|---|---|
| `bad_request` | Not JSON, not a request, or bad parameters |
| `unknown_command` | No such `cmd`; the message lists them |
| `unsupported_version` | A `v` other than 1 |
| `config_invalid` | The config on disk, or the config a change would produce, has errors. `data` lists them as problems (below) |
| `reload_would_disrupt` | `non_disruptive` was set and the change would disrupt someone. `data` is the plan result; nothing was applied |
| `not_found` | No such bridge, port, TNC, adapter or session |
| `static_object_readonly` | The object is static config (in `/etc`), which the API can't change; `object` names the file. Static bridges can still be enabled and disabled |
| `conflict` | An id or name that's already taken |
| `in_use` | A TNC or adapter something still refers to; the message lists what |
| `revision_conflict` | `config.put` with a revision that's no longer current |
| `changed_on_disk` | `dynamic.toml` was edited by hand since the daemon read it; `reload` first |
| `failed` | A Bluetooth scan or pairing was tried and failed; the message says why (for a pairing, often what to do at the radio) |
| `busy` | Another Bluetooth scan or pairing is running, or another firmware update; one runs at a time |
| `not_supported` | A command this server doesn't do, such as `bridge.create` on a device, or `firmware.update.begin` over USB |
| `image_rejected` | A firmware image failed a check: its SHA-256, board, size or signature. The message says which |
| `internal` | Something went wrong in the daemon |

A **problem** (a config error or warning) is `{"source": "static:
/etc/…/file.toml" | "dynamic", "object": "bridge ktbr0 port lan",
"message": "…"}`.

## Changes, plans and consent

Every command that changes config goes the same way: the daemon makes the
change on a copy, validates the result, works out the **plan** (what each
bridge and port would do), and only then saves and applies it, all in one
step. A change that would leave the config invalid fails with
`config_invalid` and changes nothing.

A plan lists every bridge and its ports with an `action`: `unchanged`,
`updated` (applied live), `restarted`, `started` or `stopped`, the
`changes` (which settings), the sessions it `disconnects`, and whether it
drops a TNC link in use. It's **disruptive** if it disconnects any session,
restarts a TNC port whose link is up, or stops a bridge that's in use. See
the reload rules in [plan-1.0.md](plan-1.0.md#reload-semantics).

Every changing command takes `non_disruptive` (default `false`). With it,
a disruptive change is refused with `reload_would_disrupt`, the plan in
`data`, and nothing applied. This is how `ktbctl` asks before disrupting
anyone: it sends `non_disruptive: true` when it can't confirm, shows the
plan from the refusal, and sends the command again without the flag if the
user says yes. Because the daemon plans and applies in one step, nothing
can change in between.

A change returns a **change result**:

```jsonc
{"id": "ktbr2",              // what it created, if anything
 "applied": true,
 "plan": {"global": [], "bridges": [ … ]},
 "disruptive": false,
 "disruptions": [],
 "warnings": [ … ]}          // config warnings, as problems
```

Static config (the main file and `conf.d/`) is never written, and isn't
re-read by a change: an API change touches only what it edits. `reload`
re-reads everything. Dynamic config lives in
`/var/lib/kiss-tnc-bridge/dynamic.toml`; the daemon writes it atomically,
keeps its comments and formatting, and refuses to overwrite it if it was
edited by hand since the daemon read it (`changed_on_disk`).

## Commands

### `status`

Everything at once. No parameters.

```jsonc
{"version": "1.0.0",
 "adapters": [
   {"id": "builtin", "mac": "2C:CF:67:6E:2E:BC", "hci": "hci0", "present": true, "powered": true, "source": "static: …",
    "devices": [{"address": "40:79:12:E4:95:9D", "name": "TH-D75", "icon": "phone", "paired": true,
                 "known_tnc": "Kenwood TH-D75", "model": "th-d75", "tnc": "d75"}]},
   {"mac": "AC:A7:F1:29:9C:7F", "hci": "hci1", "present": true, "powered": true}],
 "tncs": [
   {"id": "d75", "name": "TH-D75", "model": "th-d74/th-d75",
    "spp": {"address": "40:79:12:E4:95:9D", "adapter": "builtin (2C:CF:67:6E:2E:BC)", "paired": true},
    "serial": {"path": "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00", "baud": 9600, "present": false},
    "in_use_by": "ktbr0", "source": "static: …", "editable": false}],
 "bridges": [
   {"id": "ktbr0", "name": "TH-D75 on aprscc", "enabled": true,
    "public_name": {"configured": "TH-D75 on aprscc", "effective": "TH-D75 on aprscc"},
    "source": "static: …", "editable": false,
    "ports": [
      {"id": "radio", "type": "spp-client", "role": "tnc", "state": "connected",
       "since": "2026-10-16T02:26:54Z", "settings": {"tnc": "d75", "connect": "always", "idle_timeout": "0s", "exit_kiss_on_disconnect": false},
       "sessions": []},
      {"id": "phones", "type": "ble-server", "role": "app", "state": "advertising", "detail": "hci1",
       "settings": {"adapter": "dongle (AC:A7:F1:29:9C:7F)", "max_clients": 2},
       "sessions": [{"id": "ktbr0/phones/40:53:99:D1:B5:60", "since": "2026-10-16T02:27:00Z"}]},
      {"id": "lan", "type": "tcp-server", "role": "app", "state": "listening", "detail": "0.0.0.0:8001",
       "settings": {"listen": "wlan0:8001", "allow": ["192.168.13.0/24"], "announce": true},
       "sessions": []}],
    "counters": {"frames_up": 2, "frames_down": 18, "dropped": 0, "tnc_connects": 1}}]}
```

- Adapters present on the host are listed whether or not an `[[adapter]]`
  names them, and named ones whether or not they're present.
  A present adapter's `devices` are those paired on it, as `bluetooth.scan`
  shows them but without `rssi` (they needn't be on); one with no `tnc`
  is paired but not set up as a TNC.
- A TNC port's `state` is `idle`, `connecting`, `connected`, `backoff`,
  `unpaired` or `waiting for device`, with `detail` saying why or when it
  retries. A `tcp-server` port is `listening`, `waiting for interface`,
  `failed` or `stopped`; a `ble-server` port `advertising`, `full` (at
  `max_clients`, not advertising), `waiting for adapter`, `adapter off`,
  `failed` or `stopped`. Ports of disabled bridges are `disabled`.
- A static bridge's `enabled_override` is there when `bridge.enable` or
  `bridge.disable` has overridden what its file says.
- A disabled bridge that can't be enabled yet lists what it needs in
  `incomplete` (`"no TNC port"`, `"no app port"`, `"2 TNC ports (a bridge
  takes one)"`). Enabled bridges are always complete: changes that would
  leave one incomplete are refused.
- `paired` is absent when it can't be known (no BlueZ, adapter not
  present); `counters` is absent for bridges that aren't running.
- A session's `id` (`ktbr0/lan/192.168.4.20:51515`) is the same in
  status, events and `session.kick`.
- Times are RFC 3339 in UTC.

### `subscribe`

`{"cmd": "subscribe", "topics": ["state", "activity", "frames"]}` streams
events on those topics on this connection until it closes. Subscribing
again adds topics. The result lists the topics now subscribed.

### `reload`

Re-read the config from disk and apply what changed. Parameters:
`non_disruptive`. Result: a change result. An invalid config fails with
`config_invalid` and nothing changes. `SIGHUP` and `systemctl reload` do
the same, always without `non_disruptive`.

### `config.check`

Validate the config on disk, or with `dynamic` (TOML text) standing in for
`dynamic.toml`, and work out the plan a reload would apply. Applies
nothing. An invalid config isn't an API error here: the result says so.

```jsonc
{"valid": true, "errors": [], "warnings": [ … ],
 "plan": { … }, "disruptive": false, "disruptions": []}
```

### `config.get` and `config.put`

`config.get` returns the TOML for one dynamic object (`kind`: `bridge`,
`tnc` or `adapter`; `object`: its id, or a bridge's or TNC's name), or with
neither, all of `dynamic.toml`, with a `revision`:

```json
{"toml": "[[tnc]]\nid = \"nino\"\n…", "revision": "9d3c8f0a1b2e4c57"}
```

`config.put` takes the same `kind` and `object`, the edited `toml`, and the
`revision` from `config.get`. If someone changed the object since, it
fails with `revision_conflict` instead of overwriting their change.
Otherwise it goes through the same checks, plan and consent as any change.
For one object, revision `""` creates it and empty `toml` deletes it.
Static objects can't be read or written this way
(`static_object_readonly`, naming their file).

### Bridges

Definitions use the same keys as a `[[bridge]]` in TOML, with ports in an
array under `port`. `bridge` names one by id or name.

| Command | Parameters | Does |
|---|---|---|
| `bridge.create` | `definition` | A new dynamic bridge. It needs only a `name`, and starts disabled unless the definition says `enabled = true`; ports can come with it or be added one at a time with `port.add`. Without `id`, the lowest free `ktbrN`. Ports without ids get explicit ones (`tcp-server0`). Returns the id |
| `bridge.update` | `bridge`, `definition` | Replace a dynamic bridge's definition. Its id can't change |
| `bridge.delete` | `bridge` | Delete a dynamic bridge |
| `bridge.clone` | `bridge`, `name`, optional `new_id` | Copy any bridge, static or dynamic, into a new dynamic one: disabled, with its public name following its new name. Returns the id |
| `bridge.enable` | `bridge`, optional `reset` | Enable a bridge. On a static bridge this stores an override in `state.toml`; `reset` drops the override and goes back to what the file says. Fails with `config_invalid` if the bridge is incomplete (it needs exactly one TNC port and at least one app port), or if it conflicts with an enabled bridge (the same TNC, listen socket, public name or BLE adapter); the message says which |
| `bridge.disable` | `bridge` | Disable a bridge (an override, for a static one) |
| `bridge.switch` | `bridge` | Enable a bridge and disable every enabled bridge that conflicts with it, in one change |
| `bridge.retry_public_name` | `bridge` | Drop a DNS-SD collision adjustment and use the configured public name again. Works on static bridges too. Returns `{}` |

All but `bridge.retry_public_name` take `non_disruptive`.

### Ports

On dynamic bridges. `port` is the port's id.

| Command | Parameters | Does |
|---|---|---|
| `port.add` | `bridge`, `definition` | Add a port (the keys of a `[[bridge.port]]`). Returns its id |
| `port.update` | `bridge`, `port`, `definition` | Replace a port's definition; its id stays |
| `port.remove` | `bridge`, `port` | Remove a port |
| `port.connect` | `bridge`, `port` | Bring a TNC port's link up now and hold it up, whatever its `connect` policy, until `port.disconnect`. Any bridge. Returns `{}` |
| `port.disconnect` | `bridge`, `port` | Take a TNC port's link down and hold it down until `port.connect`. App sessions stay connected. Any bridge. Returns `{}` |
| `port.release` | `bridge`, `port` | Let a TNC link follow its `connect` policy again after `port.connect` or `port.disconnect`. Returns `{}` |
| `session.kick` | `session` | Disconnect one session: `ktbr0/lan/192.168.4.20:51515`, `ktbr0/phones/7C:2A:…` (BLE sessions are dropped at the link layer). Acts at once. Returns `{}` |

`port.add`, `port.update` and `port.remove` take `non_disruptive`;
`port.connect`, `port.disconnect`, `port.release` and `session.kick` act
immediately. `status` shows a held link in the port's `detail`.

### TNCs and adapters

Dynamic ones. `tnc` names a TNC by id or name; `adapter` an adapter by id.
Definitions use the keys of a `[[tnc]]` (with `spp`, `ble` and `serial` objects)
or an `[[adapter]]`.

| Command | Parameters | Does |
|---|---|---|
| `tnc.create`, `adapter.create` | `definition` (with `id`) | Create one. Returns the id |
| `tnc.update`, `adapter.update` | `tnc` or `adapter`, `definition` | Replace the definition. A different `id` in it renames it, and the dynamic ports and TNCs that refer to it follow. Returns the (new) id |
| `tnc.delete`, `adapter.delete` | `tnc` or `adapter` | Delete it; `in_use` while anything refers to it, listing what |

All take `non_disruptive`. Static config can't refer to dynamic TNCs and
adapters, so only dynamic references matter here.

### `serial.list`

The serial devices present, by their stable udev names, for setting up
`serial` TNCs:

```jsonc
[{"path": "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00",
  "device": "/dev/ttyACM0",
  "also": ["/dev/serial/by-path/platform-fd500000.pcie-pci-0000:01:00.0-usb-0:1.4:1.0"],
  "usb_id": "2166:9023", "description": "JVCKENWOOD TH-D75",
  "known_tnc": "Kenwood TH-D75", "model": "th-d75",
  "tnc": "d75"}]                     // the TNC already using this path
```

`path` is the by-id name when there is one; by-id names are per model for
radios without a USB serial number (the TH-D75), so with two of them, use
their by-path names from `also`.

### `tnc.models`

The TNC models this daemon knows, with the names to use for `model`, what
the daemon does for each, and the radios it knows with that TNC: how to
recognize them (the start of their Bluetooth name, their USB id) and what to
tell someone setting them up. `ktbctl setup`, `scan` and `serial.list` use
the same table.

```jsonc
[{"names": ["generic"], "summary": "the default: any TNC that's always in KISS mode; …"},
 {"names": ["th-d74", "th-d75"], "summary": "Kenwood TH-D74/D75: switched to KISS mode at every connect, …",
  "radios": [{"name": "Kenwood TH-D75", "model": "th-d75",
              "bluetooth": {"name_prefix": "TH-D75", "setup": "On the radio, set Menu > …",
                            "pairing_mode": "Menu > Bluetooth > Pairing Mode",
                            "pairing": "The radio shows a six-digit number. …"},
              "usb": {"usb_id": "2166:9023", "setup": "On the radio, set Menu > … to USB, …"}},
             {"name": "Kenwood TH-D74", "model": "th-d74", …}]}]
```

### Bluetooth: `bluetooth.scan`, `scan_stop`, `pair`, `unpair`

Radios reach the bridge over Bluetooth Classic (RFCOMM), which needs them
paired with the host; phones and BLE TNCs don't (see the README). A device is named by
`tnc` (a TNC's id or name: its `spp` address and adapter) or by `address`
and optionally `adapter` (id, MAC or `hciN`; the default adapter
otherwise).

`bluetooth.scan` looks for `seconds` (default 10, at most 60) on
`adapter` and returns what it found, known radios first. Radios have to be
in pairing mode to be found. Only devices heard during the scan are
listed, so a paired radio that's switched off isn't, though BlueZ still
knows it. With `"transport": "le"` it looks over Bluetooth LE instead, for
`ble-client` TNCs: it lists every LE device with a name, those advertising
the BLE KISS service (`known_tnc` "BLE KISS TNC") and radios the daemon
knows first. It doesn't filter on the service, because some radios (the
TIDRADIO TD-H9) don't advertise the one their KISS link is on. The default
is `"classic"`.

```jsonc
[{"address": "40:79:12:E4:95:9D", "name": "TH-D75", "rssi": -52, "paired": false,
  "known_tnc": "Kenwood TH-D75", "model": "th-d75", "tnc": "d75"},
 {"address": "50:3B:70:E1:1C:11", "name": "jPhone", "icon": "phone", "paired": false}]
```

While it looks, the daemon sends the requesting connection a `found` event
with the request's id and the `device` as each one turns up, and again
when there's news about one already sent (a device often shows up before
its name does; changes in signal strength aren't news). A client can list
devices as they come, and end the scan with `bluetooth.scan_stop` once it
has the one it wants; the scan then answers with what it found so far.

```jsonc
→ {"v":1,"id":3,"cmd":"bluetooth.scan","adapter":"hci0","seconds":60}
← {"v":1,"event":"found","request":3,"device":{"address":"40:79:12:E4:95:9D","paired":false}}
← {"v":1,"event":"found","request":3,"device":{"address":"40:79:12:E4:95:9D","name":"TH-D75","rssi":-52,"paired":false,"known_tnc":"Kenwood TH-D75","model":"th-d75"}}
→ {"v":1,"id":4,"cmd":"bluetooth.scan_stop","request":3}
← {"v":1,"id":4,"ok":true,"result":{}}
← {"v":1,"id":3,"ok":true,"result":[{"address":"40:79:12:E4:95:9D","name":"TH-D75",…}]}
```

A scan whose connection closes stops.

`bluetooth.pair` pairs and trusts the device, and its request stays open
until that's done. If BlueZ hasn't seen the device yet, it looks for it
first (up to 30 s). Meanwhile the daemon sends the requesting connection
`pairing` events, each with the request's id and a `step`:

| `step` | Fields | Reply with `bluetooth.pair_reply` |
|---|---|---|
| `searching` | | — |
| `pairing` | `name` | — |
| `confirm` | `passkey` | `accept`: whether the device shows the same passkey. Confirm on the device too |
| `enter_pin` | | `pin` |
| `enter_passkey` | | `passkey` |
| `show_pin`, `show_passkey` | `pin`, `passkey` | — (enter it on the device) |
| `authorize` | | `accept` |

```jsonc
→ {"v":1,"id":5,"cmd":"bluetooth.pair","tnc":"d75"}
← {"v":1,"event":"pairing","request":5,"step":"pairing","name":"TH-D75"}
← {"v":1,"event":"pairing","request":5,"step":"confirm","passkey":"482916"}
→ {"v":1,"id":6,"cmd":"bluetooth.pair_reply","request":5,"accept":true}
← {"v":1,"id":6,"ok":true,"result":{}}
← {"v":1,"id":5,"ok":true,"result":{"address":"40:79:12:E4:95:9D","adapter":"hci0 (2C:CF:67:6E:2E:BC)","paired":true}}
```

The TH-D75 uses `confirm`, and gives about 30 s to press OK on the radio.
A pairing that isn't answered within 90 s fails. The pairing registers an
agent on the daemon's own D-Bus connection for just this pairing, so the
system's default agent, if it has one, isn't touched. A `spp-client` port
waiting for its TNC to be paired connects a few seconds after the pairing.

`bluetooth.unpair` forgets the pairing. It acts at once: a bridge using the
radio loses its link (`ktbctl unpair` asks first).

One scan or pairing runs at a time; another gets `busy`.

## Events

| Topic | Event | Fields |
|---|---|---|
| `state` | `state` | `what` (`bridge`, `port`, `tnc`, `session`, `config`), `object` (`ktbr0`, `ktbr0/radio`, `ktbr0/lan/192.168.4.20:51515`), `state` (`started`, `stopped`, `connected`, `backoff`, `opened`, `closed`, `reloaded`, `changed`, …) |
| `activity` | `activity` | `object` (a port or session), `dir` (`rx` toward the bridge, `tx` away from it), `bytes` since the last event. At most about 10 a second per object and direction |
| `frames` | `frame` | `object` (the TNC port), `dir` (`rx` from the TNC, `tx` to it), `session` (for `tx`, the session it came from), `at`, `kiss` (the raw frame between FENDs, still stuffed, as hex), `decoded` (`channel`, `command`, `ax25` with `source`, `destination`, `path`, and a one-line `summary`) |
| — | `lagged` | `missed`: this connection fell behind and lost that many events |
| — | `pairing` | A step of this connection's `bluetooth.pair` (see above) |
| — | `found` | A device found by this connection's `bluetooth.scan` (see above) |
| — | `update` | A step of this connection's firmware update (see [`firmware.update`](#firmwareupdate)) |

State events are hints: after one, read `status` for the whole picture.
Frames are decoded for display only; the daemon never acts on what's in
them. Events cost nothing while nobody subscribes, and a client that
doesn't read its events loses them rather than slowing the daemon.

```jsonc
{"v":1,"event":"frame","object":"ktbr0/radio","dir":"rx","at":"2026-10-16T02:30:12Z",
 "kiss":"00a6a8…","decoded":{"channel":0,"command":0,
   "ax25":{"source":"W1AW-9","destination":"APDR16","path":["WIDE1-1*","WIDE2-1"]},
   "summary":"W1AW-9>APDR16,WIDE1-1*,WIDE2-1:!4237.14N/07120.83W>"}}
```

## Devices

The ATOM Lite and Core2 firmware present themselves in the daemon's terms:
one fixed bridge, `ktbr0`, with an `spp-client` TNC port, `radio`, to the
radio's TNC, and a `ble-server` app port, `phones`, serving the BLE KISS
API. Its TNC is `radio`, with a `model` that picks the KISS init sequence,
as on the daemon. A client can tell a device from the daemon by
`device.info`, which the daemon doesn't answer.

### What's the same, and what isn't

| Commands | On a device |
|---|---|
| `status`, `subscribe` | The same, with one bridge, one TNC and the device's one adapter |
| `tnc.update` | Changes the radio's `model` and `spp.address`. The result is a change result with `applied`, `disruptive` and `disruptions`, and an empty `plan`; a change that drops a radio link in use is disruptive, so `non_disruptive` works as on the daemon |
| `tnc.models` | The same, for the models the firmware knows |
| `bluetooth.scan`, `bluetooth.scan_stop`, `bluetooth.pair`, `bluetooth.pair_reply`, `bluetooth.unpair` | The same, Classic only. The TH-D75's `confirm` step goes to the client as on the daemon |
| `port.connect`, `port.disconnect`, `port.release`, `session.kick` | The same |
| `auth.*` | See [Authentication](#authentication) |
| `device.*`, `firmware.update.*` | Devices only; below |
| `reload`, `config.*`, `bridge.*`, `port.add`, `port.update`, `port.remove`, `tnc.create`, `tnc.delete`, `adapter.*`, `serial.list` | `not_supported`: a device's bridge can't be rebuilt |

A device's `status`, trimmed:

```jsonc
{"version": "1.1.0",
 "adapters": [{"id": "builtin", "mac": "B8:7B:4D:18:F2:72", "present": true, "powered": true}],
 "tncs": [{"id": "radio", "name": "TH-D75", "model": "th-d75",
           "spp": {"address": "40:79:12:E4:95:9D", "adapter": "builtin", "paired": true},
           "in_use_by": "ktbr0", "editable": true}],
 "bridges": [{"id": "ktbr0", "name": "TH-D75 bridge", "enabled": true,
   "public_name": {"configured": "TH-D75 bridge", "effective": "TH-D75 bridge"},
   "ports": [{"id": "radio", "type": "spp-client", "role": "tnc", "state": "connected", "sessions": []},
             {"id": "phones", "type": "ble-server", "role": "app", "state": "advertising",
              "sessions": [{"id": "ktbr0/phones/40:53:99:D1:B5:60", "since": "2026-11-08T02:27:00Z"}]}],
   "counters": {"frames_up": 2, "frames_down": 18, "dropped": 0, "tnc_connects": 1}}]}
```

### `device.info`

```jsonc
{"board": "atom-lite",                 // or "core2"
 "firmware": {"version": "1.1.0", "built": "2026-11-08T02:00:00Z",
              "partition": "ota_0", "state": "valid",   // or "pending_verify" on a new image's first boot
              "rolled_back": "1.2.0"},                  // present when the last update was rolled back
 "protocol": 1,
 "capabilities": ["spp-client", "ble-server", "firmware-update"],
 "max_message": 4096,
 "fingerprint": "Q4M8ZC2V7XKD1B6N",
 "bluetooth": {"address": "B8:7B:4D:18:F2:72"}}
```

Clients decide what to offer from `capabilities`, not from `board`, so a new
board needs no app changes for the features it shares. The capability
names are provisional; a Core2 adds `screen` and `battery`, and later
`wifi`. `max_message` is the largest request the device accepts, in bytes.

### `device.settings.get` and `device.settings.set`

Settings live in the device's NVS. `get` returns them all; `set` takes the
ones to change and returns them all again, with `reboot_required` listing
any that take effect only after `device.reboot`:

```jsonc
→ {"v":1,"id":7,"cmd":"device.settings.set","settings":{"cpu_mhz":80}}
← {"v":1,"id":7,"ok":true,"result":{"settings":{"public_name":"TH-D75 bridge","cpu_mhz":80,
     "tx_power":9,"led_brightness":40},"reboot_required":["cpu_mhz"]}}
```

The keys are provisional, from the roadmap's device settings and power
saving tables:

| Key | Values |
|---|---|
| `public_name` | The BLE name KISS apps show, as `public_name` on the daemon |
| `cpu_mhz` | 240 (the default), 160 or 80. On a Tail Bat, 240 (see the roadmap's hardware notes) |
| `tx_power` | Bluetooth transmit power, in dBm, from what the board supports |
| `led_brightness` | 0–100. On the Core2, the screen's brightness |
| `led_normal` | What the LED shows while bridging: `solid`, `breathe`, or `dark`, off until a press (see [atom-lite-ui.md](atom-lite-ui.md#normal-and-dark-mode)) |
| `led_wake_seconds` | In dark mode, how long a press lights the LED |

Unknown keys and bad values fail with `bad_request`, and nothing changes.

### `device.reboot` and `device.reset`

Both answer `{}` and then act. `device.reset` does what the button's reset
does, then reboots: every setting goes back to its default, and the radio
pairing and every trusted client are removed. The firmware, the device's
key and the setup code stay (see [atom-lite-ui.md](atom-lite-ui.md#reset)).

### `device.identify`

Answers `{}` and shows a sequence the device shows for nothing else, so
you can tell which device you're managing when you own several: on the
ATOM Lite, magenta, blue, white, twice, then back to its state (see
[atom-lite-ui.md](atom-lite-ui.md#events)). It shows in dark mode too.

### `firmware.update`

Over BLE only; over USB, `ktbctl device flash` and the web installer write
the flash directly. One update runs at a time (`busy`), and a disconnect
aborts it.

1. **Check the image.** Before sending, the client checks the image's
   signature, its board and its version. The device checks them again.
2. **`firmware.update.begin`** with `size`, `sha256` (hex) and `version`.
   The device erases the space for the image, then answers with
   `{"partition": "ota_1", "window": 22784, "chunk_max": 508, "ack_every":
   4096}`. Erasing takes about 2 s on an ATOM Lite.
3. **Write the image** to the Data characteristic, in order. Each write is
   the image offset (4 bytes, little-endian) and then at most `chunk_max`
   bytes of image. The client keeps at most `window` bytes beyond the last
   acknowledgement in flight. The device sends this connection `update`
   events, each with the `request` id of its `firmware.update.begin`:

   | `step` | Fields | Means |
   |---|---|---|
   | `ack` | `written` | That many bytes are in flash; sent every `ack_every` bytes and at the end |
   | `nak` | `expected` | A write was lost or didn't fit; resend from `expected` |

   The image bytes aren't encrypted. They're accepted only on the
   connection whose session sent `firmware.update.begin`, and the SHA-256
   sent there, inside the session, protects them.
4. **`firmware.update.finish`** once everything is acknowledged. The device
   checks the size, the SHA-256, that the image is for this board, and its
   signature, then makes it the one to boot next. It answers `{"version":
   "1.2.0", "partition": "ota_1"}`, or fails with `image_rejected`.
5. **`device.reboot`.** The new image starts as `pending_verify`, marks
   itself valid once it's running properly, and is rolled back if it
   resets first. `device.info` shows the outcome.

`firmware.update.abort` stops an update at any point and answers `{}`. The
device asks for a 15 ms connection interval during an update; from Linux a
960 KB image then takes about 30 s (see
[spikes/atom-lite-update](../spikes/atom-lite-update/README.md)).

## Trying it by hand

Until `ktbctl` exists, `socat` speaks to the socket:

```bash
sudo socat - UNIX-CONNECT:/run/kiss-tnc-bridge/control.sock
```

Then type requests, one per line:

```json
{"v":1,"id":1,"cmd":"status"}
{"v":1,"id":2,"cmd":"subscribe","topics":["state","frames"]}
```
