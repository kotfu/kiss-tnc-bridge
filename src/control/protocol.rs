// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The control protocol, version 1: the messages the daemon, `ktbctl` and
//! (later) `ktb-gateway` exchange. One set of types for all of them, so
//! they can't drift; a JSON Schema generated from them is published for
//! apps in other languages (`docs/control-api.schema.json`).
//!
//! Framing is newline-delimited JSON. Every message carries `"v": 1`.
//! Requests have an `id`; responses echo it with `ok` and a `result` or an
//! `error`; events have no `id`. Additive changes stay in version 1.
//!
//! In a request, `id` is the request's own. Objects a command acts on are
//! named by their kind (`bridge`, `port`, `tnc`, `adapter`, `session`),
//! and new definitions go under `definition`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ax25::Decoded;
use crate::config::Problem;
use crate::reconcile::Plan;

pub const VERSION: u32 = 1;

// ── Requests ────────────────────────────────────────────────────────────

/// `{"v":1,"id":7,"cmd":"status"}`
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Request {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}

/// What a request asks for, by `cmd`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "cmd")]
pub enum Command {
    /// Adapters, TNCs, bridges, ports, sessions, states and counters.
    #[serde(rename = "status")]
    Status,
    /// Events on the given topics, on this connection, until it closes.
    #[serde(rename = "subscribe")]
    Subscribe { topics: Vec<Topic> },
    /// Re-read the config from disk and apply it. With `non_disruptive`, a
    /// plan that would disrupt anyone is refused with
    /// `reload_would_disrupt` and nothing is applied.
    #[serde(rename = "reload")]
    Reload {
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Validate the config on disk, or with `dynamic` standing in for
    /// `dynamic.toml`, and return the plan a reload would apply. Applies
    /// nothing.
    #[serde(rename = "config.check")]
    ConfigCheck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dynamic: Option<String>,
    },
    /// The TOML for one dynamic object (`kind` and `object`, its id), or
    /// all of `dynamic.toml`, with its revision for `config.put`.
    #[serde(rename = "config.get")]
    ConfigGet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ObjectKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object: Option<String>,
    },
    /// Replace that TOML if `revision` is still current, through the same
    /// checks and consent as any change. For one object, revision `""`
    /// creates it and empty TOML deletes it.
    #[serde(rename = "config.put")]
    ConfigPut {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ObjectKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object: Option<String>,
        toml: String,
        revision: String,
        #[serde(default)]
        non_disruptive: bool,
    },

    // Bridges, named by id or name in `bridge`. Definitions use the same
    // keys as a `[[bridge]]` in TOML, with ports under `port`.
    /// A new dynamic bridge. It needs only a `name`, and starts disabled
    /// unless the definition says `enabled = true`; ports can come with it
    /// or be added with `port.add`. Without an `id`, the lowest free `ktbrN`.
    #[serde(rename = "bridge.create")]
    BridgeCreate {
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Replace a dynamic bridge's definition.
    #[serde(rename = "bridge.update")]
    BridgeUpdate {
        bridge: String,
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    #[serde(rename = "bridge.delete")]
    BridgeDelete {
        bridge: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Copy any bridge, static or dynamic, into a new dynamic one, disabled.
    #[serde(rename = "bridge.clone")]
    BridgeClone {
        bridge: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_id: Option<String>,
        name: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Enable a bridge (on a static one, an override in `state.toml`;
    /// `reset` drops the override instead). Refused if it conflicts with an
    /// enabled bridge; `bridge.switch` disables those.
    #[serde(rename = "bridge.enable")]
    BridgeEnable {
        bridge: String,
        #[serde(default)]
        reset: bool,
        #[serde(default)]
        non_disruptive: bool,
    },
    #[serde(rename = "bridge.disable")]
    BridgeDisable {
        bridge: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Enable a bridge and disable every enabled bridge in its way.
    #[serde(rename = "bridge.switch")]
    BridgeSwitch {
        bridge: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Drop a DNS-SD collision adjustment and try the configured public
    /// name again.
    #[serde(rename = "bridge.retry_public_name")]
    BridgeRetryPublicName { bridge: String },

    // Ports, on dynamic bridges; `port` is the port's id.
    #[serde(rename = "port.add")]
    PortAdd {
        bridge: String,
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    #[serde(rename = "port.update")]
    PortUpdate {
        bridge: String,
        port: String,
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    #[serde(rename = "port.remove")]
    PortRemove {
        bridge: String,
        port: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Bring a bridge's TNC link up now, and keep it up, whatever its
    /// `connect` policy, until `port.disconnect`.
    #[serde(rename = "port.connect")]
    PortConnect { bridge: String, port: String },
    /// Take a bridge's TNC link down, and keep it down until
    /// `port.connect`. App sessions stay connected.
    #[serde(rename = "port.disconnect")]
    PortDisconnect { bridge: String, port: String },
    /// Disconnect one session: `ktbr0/lan/192.168.4.20:51515`.
    #[serde(rename = "session.kick")]
    SessionKick { session: String },

    // Dynamic TNCs (by id or name) and adapters (by id).
    #[serde(rename = "tnc.create")]
    TncCreate {
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Replace a dynamic TNC's definition; a new `id` in it renames the TNC
    /// and the dynamic ports that use it.
    #[serde(rename = "tnc.update")]
    TncUpdate {
        tnc: String,
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Refused while a port uses it.
    #[serde(rename = "tnc.delete")]
    TncDelete {
        tnc: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    #[serde(rename = "adapter.create")]
    AdapterCreate {
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Replace a dynamic adapter's definition; a new `id` renames it and
    /// the dynamic references to it.
    #[serde(rename = "adapter.update")]
    AdapterUpdate {
        adapter: String,
        definition: Value,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Refused while anything refers to it.
    #[serde(rename = "adapter.delete")]
    AdapterDelete {
        adapter: String,
        #[serde(default)]
        non_disruptive: bool,
    },
    /// Serial devices present, by stable path, flagging known TNCs.
    #[serde(rename = "serial.list")]
    SerialList,
    /// The TNC models this daemon knows, and what each does.
    #[serde(rename = "tnc.models")]
    TncModels,

    // Bluetooth Classic, for radios. A device is named by `tnc` (its
    // `spp` address and adapter) or by `address` and optionally `adapter`
    // (id, MAC or hciN; the default adapter otherwise).
    /// Look for Bluetooth Classic devices for `seconds` (default 10), likely
    /// TNCs first. Radios have to be in pairing mode to be found.
    #[serde(rename = "bluetooth.scan")]
    BluetoothScan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adapter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<u64>,
    },
    /// Pair with a device and trust it. The request stays open until the
    /// pairing is done; meanwhile the daemon sends this connection
    /// `pairing` events, answered with `bluetooth.pair_reply`.
    #[serde(rename = "bluetooth.pair")]
    BluetoothPair {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tnc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adapter: Option<String>,
    },
    /// The answer to a `pairing` event that asks for one: `accept` for a
    /// passkey to confirm, `pin` or `passkey` for one to enter. `request`
    /// is the `bluetooth.pair` request's id.
    #[serde(rename = "bluetooth.pair_reply")]
    BluetoothPairReply {
        request: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accept: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pin: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passkey: Option<u32>,
    },
    /// Forget a pairing.
    #[serde(rename = "bluetooth.unpair")]
    BluetoothUnpair {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tnc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adapter: Option<String>,
    },
}

impl Command {
    /// Every `cmd` this version knows, to tell an unknown command from a
    /// known one with bad parameters.
    pub const NAMES: &'static [&'static str] = &[
        "status",
        "subscribe",
        "reload",
        "config.check",
        "config.get",
        "config.put",
        "bridge.create",
        "bridge.update",
        "bridge.delete",
        "bridge.clone",
        "bridge.enable",
        "bridge.disable",
        "bridge.switch",
        "bridge.retry_public_name",
        "port.add",
        "port.update",
        "port.remove",
        "port.connect",
        "port.disconnect",
        "session.kick",
        "tnc.create",
        "tnc.update",
        "tnc.delete",
        "adapter.create",
        "adapter.update",
        "adapter.delete",
        "serial.list",
        "tnc.models",
        "bluetooth.scan",
        "bluetooth.pair",
        "bluetooth.pair_reply",
        "bluetooth.unpair",
    ];
}

/// The kinds of object config files define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Adapter,
    Tnc,
    Bridge,
}

impl From<ObjectKind> for crate::config::store::Kind {
    fn from(k: ObjectKind) -> Self {
        use crate::config::store::Kind;
        match k {
            ObjectKind::Adapter => Kind::Adapter,
            ObjectKind::Tnc => Kind::Tnc,
            ObjectKind::Bridge => Kind::Bridge,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Things starting, stopping, connecting and disconnecting.
    State,
    /// Bytes moving on ports and sessions, at most about 10 events a second
    /// per port or session.
    Activity,
    /// Every frame to and from each TNC, with its AX.25 header decoded.
    Frames,
}

// ── Responses ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Response {
    pub v: u32,
    /// The request's id; `null` when the request couldn't be read at all.
    pub id: Option<u64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            v: VERSION,
            id: Some(id),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<u64>, error: ApiError) -> Self {
        Self {
            v: VERSION,
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiError {
    /// Stable and machine-readable: scripts and clients match on it.
    pub code: ErrorCode,
    /// For people: what went wrong and what to do next.
    pub message: String,
    /// The object concerned: `bridge ktbr0`, `tnc d75`, a config path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// More detail: the problems for `config_invalid`, the plan for
    /// `reload_would_disrupt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            object: None,
            data: None,
        }
    }

    pub fn object(mut self, object: impl Into<String>) -> Self {
        self.object = Some(object.into());
        self
    }

    pub fn data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Not JSON, not a request, or bad parameters.
    BadRequest,
    UnknownCommand,
    /// A `v` this daemon doesn't speak.
    UnsupportedVersion,
    /// The config (on disk or a candidate) has errors; `data` lists them.
    ConfigInvalid,
    /// `non_disruptive` and the change would disrupt; `data` is the plan.
    ReloadWouldDisrupt,
    /// No such bridge, port, TNC, adapter or session.
    NotFound,
    /// Static objects (in /etc) can't be changed through the API, only
    /// enabled and disabled; `object` names the file.
    StaticObjectReadonly,
    /// An id or name that's already taken.
    Conflict,
    /// A TNC or adapter that something still refers to; the message says
    /// what.
    InUse,
    /// `config.put` with a revision that's no longer current: someone else
    /// changed it since `config.get`.
    RevisionConflict,
    /// `dynamic.toml` was edited by hand since the daemon read it; reload
    /// first.
    ChangedOnDisk,
    /// A Bluetooth operation (scan, pairing) was tried and failed; the
    /// message says why.
    Failed,
    /// Another Bluetooth operation (scan, pairing) is running; try again
    /// when it's done.
    Busy,
    Internal,
}

// ── Results ─────────────────────────────────────────────────────────────

/// One config error or warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProblemJson {
    /// `static: /etc/…/file.toml` or `dynamic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `bridge ktbr0 port lan`, empty for the config as a whole.
    pub object: String,
    pub message: String,
}

impl From<&Problem> for ProblemJson {
    fn from(p: &Problem) -> Self {
        Self {
            source: p.source.as_ref().map(|s| s.to_string()),
            object: p.object.clone(),
            message: p.message.clone(),
        }
    }
}

pub fn problems(ps: &[Problem]) -> Vec<ProblemJson> {
    ps.iter().map(ProblemJson::from).collect()
}

/// The result of `reload` and `config.check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanResult {
    /// Whether the change was applied (`reload`) or would be (`config.check`).
    pub applied: bool,
    pub plan: Plan,
    /// Why the plan is disruptive; empty if it isn't.
    pub disruptions: Vec<String>,
    pub disruptive: bool,
    pub warnings: Vec<ProblemJson>,
}

impl PlanResult {
    pub fn new(plan: Plan, applied: bool, warnings: &[Problem]) -> Self {
        let disruptions = plan.disruptions();
        Self {
            applied,
            disruptive: !disruptions.is_empty(),
            disruptions,
            plan,
            warnings: problems(warnings),
        }
    }
}

/// The result of a command that changes config: the plan it applied, and
/// the id of anything it created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChangeResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub plan: PlanResult,
}

/// The result of `config.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigText {
    pub toml: String,
    /// Pass back to `config.put`.
    pub revision: String,
}

/// One serial device, as `serial.list` finds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SerialEntry {
    /// The stable path to use in config: `/dev/serial/by-id/…`, or
    /// `/dev/serial/by-path/…` for devices with no by-id name.
    pub path: String,
    /// What it points to now: `/dev/ttyACM0`.
    pub device: String,
    /// Other stable paths to the same device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A TNC this daemon knows: `Kenwood TH-D75`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_tnc: Option<String>,
    /// The `model` to use for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The TNC already defined with this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tnc: Option<String>,
}

/// One TNC model, as `tnc.models` lists them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelEntry {
    /// What to write for `model`; models that share a profile have several.
    pub names: Vec<String>,
    /// What the daemon does for it.
    pub summary: String,
}

/// A Bluetooth Classic device, as `bluetooth.scan` finds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BtDevice {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Signal strength in dBm, when the scan saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i16>,
    /// What BlueZ makes of its class: `phone`, `audio-headset`, ….
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub paired: bool,
    /// A TNC this daemon knows: `Kenwood TH-D75`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_tnc: Option<String>,
    /// The `model` to use for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The TNC already defined with this address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tnc: Option<String>,
}

/// The result of `bluetooth.pair` and `bluetooth.unpair`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PairResult {
    pub address: String,
    /// The adapter, as `hciN (MAC)`.
    pub adapter: String,
    pub paired: bool,
}

/// What a pairing needs from the person at the other end, or tells them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum PairingStep {
    /// The device isn't known yet; looking for it (it must be in pairing
    /// mode).
    Searching,
    /// Found; pairing now.
    Pairing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Does the device show this passkey? Reply with `accept`, and confirm
    /// on the device too.
    Confirm { passkey: String },
    /// Reply with the device's `pin` (legacy pairing).
    EnterPin,
    /// Reply with the `passkey` the device shows.
    EnterPasskey,
    /// Enter this on the device; no reply.
    ShowPin { pin: String },
    /// Enter this on the device; no reply.
    ShowPasskey { passkey: String },
    /// May the device pair? Reply with `accept`.
    Authorize,
}

impl PairingStep {
    /// Whether the daemon waits for a `bluetooth.pair_reply`.
    pub fn needs_reply(&self) -> bool {
        matches!(
            self,
            PairingStep::Confirm { .. }
                | PairingStep::EnterPin
                | PairingStep::EnterPasskey
                | PairingStep::Authorize
        )
    }
}

/// The result of `config.check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CheckResult {
    pub valid: bool,
    pub errors: Vec<ProblemJson>,
    pub warnings: Vec<ProblemJson>,
    /// What a reload would do, if the config is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    pub disruptive: bool,
    pub disruptions: Vec<String>,
}

/// The result of `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Status {
    pub version: String,
    pub adapters: Vec<AdapterStatus>,
    pub tncs: Vec<TncStatus>,
    pub bridges: Vec<BridgeStatus>,
}

/// A Bluetooth adapter: present on this host, named by an `[[adapter]]`,
/// or both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdapterStatus {
    /// The `[[adapter]]` id, if one names this MAC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub mac: String,
    /// `hci0`, while present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hci: Option<String>,
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powered: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TncStatus {
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spp: Option<SppStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<SerialStatus>,
    /// The enabled bridge using it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use_by: Option<String>,
    pub source: String,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SppStatus {
    pub address: String,
    /// As configured: an adapter id with its MAC, a MAC, `hciN`, or the
    /// default adapter.
    pub adapter: String,
    /// Whether it's paired on that adapter; absent if that can't be known
    /// (no BlueZ, adapter not present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SerialStatus {
    pub path: String,
    pub baud: u32,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BridgeStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// The `state.toml` override behind `enabled`, for static bridges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_override: Option<bool>,
    pub public_name: PublicName,
    pub source: String,
    /// Dynamic bridges can be changed through the API; static ones can only
    /// be enabled and disabled.
    pub editable: bool,
    /// What it still needs before it can be enabled ("no TNC port", "no
    /// app port"); empty when it's complete. Only disabled bridges can be
    /// incomplete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
    pub ports: Vec<PortStatus>,
    /// While running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters: Option<Counters>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PublicName {
    pub configured: String,
    /// What's advertised and announced: differs after a DNS-SD collision.
    pub effective: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Counters {
    /// Frames from apps forwarded to the TNC.
    pub frames_up: u64,
    /// Frames from the TNC, counted once however many sessions got them.
    pub frames_down: u64,
    /// Frames dropped by policy, a full queue or a slow session.
    pub dropped: u64,
    pub tnc_connects: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PortStatus {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    /// `connected`, `backoff`, `unpaired`, `listening`, `advertising`,
    /// `waiting for interface`, `stopped`, …
    pub state: String,
    /// More about the state: why it's waiting, when it retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When the TNC port entered its state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The port's settings, as in the config: `tnc`, `listen`, `adapter`,
    /// `max_clients`, ….
    pub settings: Value,
    pub sessions: Vec<SessionStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionStatus {
    /// `bridge/port/peer`: `ktbr0/phones/7C:2A:…`,
    /// `ktbr0/lan/192.168.4.20:51515`, as `session.kick` takes it.
    pub id: String,
    pub since: String,
}

// ── Events ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventMessage {
    pub v: u32,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Something started, stopped, connected or disconnected. Re-read
    /// `status` for the whole picture.
    State {
        /// `bridge`, `tnc`, `session`, `port` or `config`.
        what: String,
        /// `ktbr0`, `ktbr0/radio`, `ktbr0/lan/192.168.4.20:51515`.
        object: String,
        /// `started`, `stopped`, `connected`, `opened`, `closed`, `reloaded`, ….
        state: String,
    },
    /// Bytes moved on a port (`ktbr0/radio`) or session
    /// (`ktbr0/phones/7C:2A:…`). `rx` is toward the bridge.
    Activity {
        object: String,
        dir: Dir,
        bytes: u64,
    },
    /// A frame to (`tx`) or from (`rx`) a TNC.
    Frame {
        /// The TNC port: `ktbr0/radio`.
        object: String,
        dir: Dir,
        /// For `tx`, the session it came from: `ktbr0/lan/192.168.4.20:51515`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        at: String,
        /// The raw KISS frame between FENDs, still stuffed, as hex.
        kiss: String,
        decoded: Decoded,
    },
    /// This connection fell behind and missed `missed` events.
    Lagged { missed: u64 },
    /// A step of the pairing started by this connection's `bluetooth.pair`
    /// request `request`. Sent only to that connection.
    Pairing {
        request: u64,
        #[serde(flatten)]
        step: PairingStep,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    Rx,
    Tx,
}

impl Event {
    pub fn topic(&self) -> Option<Topic> {
        match self {
            Event::State { .. } => Some(Topic::State),
            Event::Activity { .. } => Some(Topic::Activity),
            Event::Frame { .. } => Some(Topic::Frames),
            Event::Lagged { .. } | Event::Pairing { .. } => None,
        }
    }
}

/// RFC 3339 in UTC, to the second: `2026-10-12T14:02:11Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn requests_read_and_write() {
        let r: Request =
            serde_json::from_str(r#"{"v":1,"id":3,"cmd":"reload","non_disruptive":true}"#).unwrap();
        assert_eq!(
            r,
            Request {
                v: 1,
                id: 3,
                command: Command::Reload {
                    non_disruptive: true
                }
            }
        );
        let r: Request = serde_json::from_str(r#"{"v":1,"id":4,"cmd":"status"}"#).unwrap();
        assert_eq!(r.command, Command::Status);
        let r: Request =
            serde_json::from_str(r#"{"v":1,"id":5,"cmd":"subscribe","topics":["state","frames"]}"#)
                .unwrap();
        assert_eq!(
            r.command,
            Command::Subscribe {
                topics: vec![Topic::State, Topic::Frames]
            }
        );
        let r: Request = serde_json::from_str(r#"{"v":1,"id":6,"cmd":"config.check"}"#).unwrap();
        assert_eq!(r.command, Command::ConfigCheck { dynamic: None });
        assert!(serde_json::from_str::<Request>(r#"{"v":1,"id":6,"cmd":"nope"}"#).is_err());
    }

    #[test]
    fn pairing_events_and_replies_on_the_wire() {
        let ev = EventMessage {
            v: VERSION,
            event: Event::Pairing {
                request: 7,
                step: PairingStep::Confirm {
                    passkey: "012345".into(),
                },
            },
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"v": 1, "event": "pairing", "request": 7, "step": "confirm", "passkey": "012345"})
        );
        let back: EventMessage =
            serde_json::from_value(serde_json::to_value(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
        assert!(
            PairingStep::Confirm {
                passkey: "1".into()
            }
            .needs_reply()
        );
        assert!(!PairingStep::Searching.needs_reply());

        let req: Request = serde_json::from_value(
            json!({"v": 1, "id": 8, "cmd": "bluetooth.pair_reply", "request": 7, "accept": true}),
        )
        .unwrap();
        assert_eq!(
            req.command,
            Command::BluetoothPairReply {
                request: 7,
                accept: Some(true),
                pin: None,
                passkey: None
            }
        );
    }

    #[test]
    fn a_request_id_never_names_an_object() {
        let r: Request = serde_json::from_str(
            r#"{"v":1,"id":9,"cmd":"port.remove","bridge":"ktbr0","port":"lan"}"#,
        )
        .unwrap();
        assert_eq!(r.id, 9);
        assert_eq!(
            r.command,
            Command::PortRemove {
                bridge: "ktbr0".into(),
                port: "lan".into(),
                non_disruptive: false
            }
        );
        // No command takes a parameter called id: it's the request's.
        let schema_like = serde_json::to_value(&r.command).unwrap();
        assert!(schema_like.get("id").is_none());
        // Every variant's cmd is listed in NAMES.
        for name in Command::NAMES {
            let v = serde_json::json!({"cmd": name});
            if let Err(e) = serde_json::from_value::<Command>(v) {
                assert!(e.to_string().contains("missing field"), "{name}: {e}");
            }
        }
    }

    #[test]
    fn responses_and_events_have_the_documented_shape() {
        let ok = Response::ok(1, json!({"x": 1}));
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"v":1,"id":1,"ok":true,"result":{"x":1}})
        );
        let err = Response::err(
            None,
            ApiError::new(ErrorCode::UnknownCommand, "no such command").object("frob"),
        );
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"v":1,"id":null,"ok":false,"error":{"code":"unknown_command","message":"no such command","object":"frob"}})
        );
        let ev = EventMessage {
            v: VERSION,
            event: Event::Activity {
                object: "ktbr0/radio".into(),
                dir: Dir::Rx,
                bytes: 73,
            },
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"v":1,"event":"activity","object":"ktbr0/radio","dir":"rx","bytes":73})
        );
    }

    #[test]
    fn times_are_rfc3339_utc() {
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_813_731);
        assert_eq!(rfc3339(t), "2026-10-12T14:02:11Z");
        let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00Z");
    }
}
