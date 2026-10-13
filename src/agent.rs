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

//! The pairing agent phones bond with: `NoInputNoOutput`, so pairing is
//! Just Works, and the phone asks its user once.
//!
//! Right after a phone connects, BlueZ reads one of the phone's protected
//! attributes, which makes the phone start pairing. For a pairing the
//! remote side starts, bluetoothd asks the agent's `RequestAuthorization`
//! before it accepts Just Works. An agent that refuses (as bluer's does
//! when it has no callback for it) fails every pairing, so the phone never
//! bonds, prompts again on every connection, and shows up under a new
//! private address each time.
//!
//! bluer derives an agent's capability from which callbacks it has, and an
//! authorization callback would make it `DisplayYesNo`, turning phone
//! pairing into numeric comparison. So this agent is registered over D-Bus
//! directly. It authorizes LE devices with random (private) addresses,
//! which is what phones use, and rejects everything else, including
//! Bluetooth Classic pairing from strangers. Radios are paired with
//! `bluetoothctl`'s own agent, which isn't affected.

use std::sync::Arc;
use std::time::Duration;

use dbus::MethodErr;
use dbus::message::MatchRule;
use dbus::nonblock::stdintf::org_freedesktop_dbus::Properties;
use dbus::nonblock::{Proxy, SyncConnection};
use dbus_crossroads::Crossroads;
use futures::StreamExt;

const BLUEZ: &str = "org.bluez";
const AGENT_INTERFACE: &str = "org.bluez.Agent1";
const MANAGER_INTERFACE: &str = "org.bluez.AgentManager1";
const AGENT_PATH: &str = "/kiss_tnc_bridge/agent";
const CAPABILITY: &str = "NoInputNoOutput";
const TIMEOUT: Duration = Duration::from_secs(10);

/// The registered agent. BlueZ forgets it when the daemon's D-Bus
/// connection goes away, so there's nothing to undo by hand.
pub struct Agent {
    _conn: Arc<SyncConnection>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn rejected(why: &str) -> MethodErr {
    MethodErr::from(("org.bluez.Error.Rejected", why))
}

fn reject(why: &str) -> Result<(), MethodErr> {
    Err(rejected(why))
}

/// Register the agent and make it BlueZ's default.
pub async fn register() -> Result<Agent, String> {
    let (resource, conn) =
        dbus_tokio::connection::new_system_sync().map_err(|e| format!("system D-Bus: {e}"))?;
    let mut tasks = vec![tokio::spawn(async move {
        let err = resource.await;
        tracing::warn!("pairing agent lost its D-Bus connection: {err}");
    })];

    let mut cr = Crossroads::new();
    cr.set_async_support(Some((
        conn.clone(),
        Box::new(|x| {
            tokio::spawn(x);
        }),
    )));
    let lookup = conn.clone();
    let token = cr.register(AGENT_INTERFACE, move |b| {
        b.method("Release", (), (), |_, _, ()| Ok(()));
        b.method("Cancel", (), (), |_, _, ()| Ok(()));
        b.method(
            "RequestPinCode",
            ("device",),
            ("pincode",),
            |_, _, (_,): (dbus::Path<'static>,)| -> Result<(String,), MethodErr> {
                Err(rejected("no PIN entry here"))
            },
        );
        b.method(
            "DisplayPinCode",
            ("device", "pincode"),
            (),
            |_, _, (_, _): (dbus::Path<'static>, String)| reject("no display here"),
        );
        b.method(
            "RequestPasskey",
            ("device",),
            ("passkey",),
            |_, _, (_,): (dbus::Path<'static>,)| -> Result<(u32,), MethodErr> {
                Err(rejected("no passkey entry here"))
            },
        );
        b.method(
            "DisplayPasskey",
            ("device", "passkey", "entered"),
            (),
            |_, _, (_, _, _): (dbus::Path<'static>, u32, u16)| Ok(()),
        );
        b.method(
            "RequestConfirmation",
            ("device", "passkey"),
            (),
            |_, _, (_, _): (dbus::Path<'static>, u32)| reject("no display here"),
        );
        b.method(
            "AuthorizeService",
            ("device", "uuid"),
            (),
            |_, _, (_, _): (dbus::Path<'static>, String)| reject("not authorized"),
        );
        let lookup = lookup.clone();
        b.method_with_cr_async(
            "RequestAuthorization",
            ("device",),
            (),
            move |mut ctx, _, (device,): (dbus::Path<'static>,)| {
                let lookup = lookup.clone();
                async move {
                    let res = authorize(&lookup, device).await;
                    ctx.reply(res)
                }
            },
        );
    });
    cr.insert(AGENT_PATH, &[token], ());

    let calls = conn
        .add_match(MatchRule::new_method_call())
        .await
        .map_err(|e| format!("D-Bus: {e}"))?;
    let serve = conn.clone();
    tasks.push(tokio::spawn(async move {
        let (_calls, mut stream) = calls.msg_stream();
        while let Some(msg) = stream.next().await {
            let _ = cr.handle_message(msg, &*serve);
        }
    }));

    let manager = Proxy::new(BLUEZ, "/org/bluez", TIMEOUT, conn.clone());
    let path = dbus::Path::from(AGENT_PATH);
    manager
        .method_call::<(), _, _, _>(
            MANAGER_INTERFACE,
            "RegisterAgent",
            (path.clone(), CAPABILITY),
        )
        .await
        .map_err(|e| format!("can't register the pairing agent: {e}"))?;
    manager
        .method_call::<(), _, _, _>(MANAGER_INTERFACE, "RequestDefaultAgent", (path,))
        .await
        .map_err(|e| format!("can't make the pairing agent the default: {e}"))?;
    tracing::debug!(capability = CAPABILITY, "pairing agent registered");
    Ok(Agent { _conn: conn, tasks })
}

/// Just Works for phones: LE devices with random addresses.
async fn authorize(
    conn: &Arc<SyncConnection>,
    device: dbus::Path<'static>,
) -> Result<(), MethodErr> {
    let proxy = Proxy::new(BLUEZ, device.clone(), TIMEOUT, conn.clone());
    let address_type: String = proxy
        .get("org.bluez.Device1", "AddressType")
        .await
        .map_err(|e| rejected(&format!("can't look up {device}: {e}")))?;
    let name: Option<String> = proxy.get("org.bluez.Device1", "Alias").await.ok();
    let who = name.as_deref().unwrap_or("?");
    if address_type == "random" {
        tracing::info!(device = %device, name = who, "pairing a phone (Just Works)");
        Ok(())
    } else {
        tracing::warn!(device = %device, name = who, address_type, "pairing refused: only phones (LE, private address) may pair with the bridge");
        Err(rejected("only LE devices with private addresses may pair"))
    }
}
