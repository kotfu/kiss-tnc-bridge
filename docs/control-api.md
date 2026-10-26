# kiss-tnc-bridge control API, version 1

The daemon is managed through one API: a Unix socket speaking
newline-delimited JSON. `ktbctl` (the CLI and TUI) is its client, and so
will be `ktb-gateway`, which carries the same messages to web and phone
apps. The types are defined once, in
[`src/control/protocol.rs`](../src/control/protocol.rs), and published as a
JSON Schema in [`control-api.schema.json`](control-api.schema.json) for
apps in other languages.

## Transport

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

## Framing

One JSON object per line, each way. Every message carries `"v": 1`.

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
- An **event** has `event` and no `id`; it arrives only on a connection
  that subscribed to its topic.
- A request line can be up to 1 MiB.

**Versioning.** Additive changes (a new command, a new optional field, a
new error code, a new event field) stay in version 1, so clients should
ignore fields they don't know. Anything that would break a version 1
client needs version 2. A request with another `v` gets
`unsupported_version`.

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
   {"id": "builtin", "mac": "2C:CF:67:6E:2E:BC", "hci": "hci0", "present": true, "powered": true, "source": "static: …"},
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
| `session.kick` | `session` | Disconnect one session: `ktbr0/lan/192.168.4.20:51515`, `ktbr0/phones/7C:2A:…` (BLE sessions are dropped at the link layer). Acts at once. Returns `{}` |

`port.add`, `port.update` and `port.remove` take `non_disruptive`;
`port.connect`, `port.disconnect` and `session.kick` exist to disrupt, and
act immediately.

### TNCs and adapters

Dynamic ones. `tnc` names a TNC by id or name; `adapter` an adapter by id.
Definitions use the keys of a `[[tnc]]` (with `spp` and `serial` objects)
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

## Events

| Topic | Event | Fields |
|---|---|---|
| `state` | `state` | `what` (`bridge`, `port`, `tnc`, `session`, `config`), `object` (`ktbr0`, `ktbr0/radio`, `ktbr0/lan/192.168.4.20:51515`), `state` (`started`, `stopped`, `connected`, `backoff`, `opened`, `closed`, `reloaded`, `changed`, …) |
| `activity` | `activity` | `object` (a port or session), `dir` (`rx` toward the bridge, `tx` away from it), `bytes` since the last event. At most about 10 a second per object and direction |
| `frames` | `frame` | `object` (the TNC port), `dir` (`rx` from the TNC, `tx` to it), `session` (for `tx`, the session it came from), `at`, `kiss` (the raw frame between FENDs, still stuffed, as hex), `decoded` (`channel`, `command`, `ax25` with `source`, `destination`, `path`, and a one-line `summary`) |
| — | `lagged` | `missed`: this connection fell behind and lost that many events |

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
