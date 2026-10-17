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

//! DNS-SD announcement of `tcp-server` ports, following the
//! [TCP KISS DNS-SD spec](https://github.com/hessu/aprs-specs/blob/master/TCP-KISS-DNS-SD.md):
//! service type `_kiss-tnc._tcp`, instance name = the bridge's public name,
//! the same name its `ble-server` ports advertise.
//!
//! Each bridge is one Avahi entry group holding a service per listening
//! `tcp-server` port, scoped to the interface (and IP version) the port
//! is bound to. A port is announced while it's listening; a port waiting
//! for its interface isn't.
//!
//! When another device on the LAN already uses the name, Avahi reports a
//! collision. The bridge then takes the next name ("TH-D75 (2)"), keeps it
//! in `state.toml` so it's stable across restarts, and publishes it on a
//! watch channel so the bridge's BLE advertisement and adapter alias follow.
//!
//! Announcing goes through Avahi's D-Bus API rather than our own mDNS
//! responder, which would fight Avahi over UDP port 5353. Without Avahi
//! the daemon logs it once and carries on; it announces if Avahi starts
//! later.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::config::Listen;
use crate::config::state::StateStore;

pub const SERVICE_TYPE: &str = "_kiss-tnc._tcp";

/// Which interfaces a service is announced on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Iface {
    Any,
    /// A kernel interface index, which is what Avahi uses.
    Index(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proto {
    Any,
    V4,
    V6,
}

/// One announced `tcp-server` port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Service {
    pub port: u16,
    pub iface: Iface,
    pub proto: Proto,
}

impl Service {
    /// Where a port bound as `listen`, now listening on `bound`, is
    /// announced: an interface binding on that interface (IPv4: it binds
    /// 0.0.0.0), a specific address on the interface that holds it, and a
    /// wildcard everywhere.
    pub fn for_listen(listen: &Listen, bound: SocketAddr) -> Service {
        let port = bound.port();
        match listen {
            Listen::Interface { name, .. } => Service {
                port,
                iface: if_index(name).map_or(Iface::Any, Iface::Index),
                proto: Proto::V4,
            },
            Listen::Addr(a) if a.ip().is_unspecified() => Service {
                port,
                iface: Iface::Any,
                // [::] is dual-stack on Linux; 0.0.0.0 is IPv4 only.
                proto: if a.is_ipv6() { Proto::Any } else { Proto::V4 },
            },
            Listen::Addr(a) => Service {
                port,
                iface: iface_holding(a.ip()).map_or(Iface::Any, Iface::Index),
                proto: if a.is_ipv6() { Proto::V6 } else { Proto::V4 },
            },
        }
    }

    /// Two services with one name would collide in Avahi if their
    /// interfaces and protocols overlap.
    fn overlaps(&self, other: &Service) -> bool {
        let iface =
            self.iface == other.iface || self.iface == Iface::Any || other.iface == Iface::Any;
        let proto =
            self.proto == other.proto || self.proto == Proto::Any || other.proto == Proto::Any;
        iface && proto
    }
}

/// The name to try after a collision, as the spec suggests: "TH-D75" →
/// "TH-D75 (2)" → "TH-D75 (3)".
pub fn next_name(name: &str) -> String {
    if let Some(stem) = name.strip_suffix(')')
        && let Some((base, n)) = stem.rsplit_once(" (")
        && let Ok(n) = n.parse::<u32>()
    {
        return format!("{base} ({})", n + 1);
    }
    format!("{name} (2)")
}

/// The public name a bridge starts with: an adjustment from an earlier
/// collision if it was made from the name configured now, otherwise the
/// configured name (and a stale adjustment is dropped).
pub fn effective_public_name(store: &mut StateStore, bridge: &str, configured: &str) -> String {
    if let Some(adjusted) = store.state().adjusted_public_name(bridge, configured) {
        return adjusted.to_string();
    }
    if store
        .state()
        .bridges
        .get(bridge)
        .is_some_and(|b| b.public_name.is_some())
    {
        tracing::info!(
            bridge,
            "public name changed in config; dropping the old collision adjustment"
        );
        if let Err(e) = store.update(|s| s.clear_adjusted_public_name(bridge)) {
            tracing::warn!("can't write {}: {e}", store.path().display());
        }
    }
    configured.to_string()
}

// ── The announcer ───────────────────────────────────────────────────────

/// What the supervisor tells the announcer.
#[derive(Debug)]
pub enum Cmd {
    /// A bridge that may announce. `name` holds its effective public name;
    /// the announcer changes it after a collision. Sent again for a bridge
    /// already known (its configured public name changed), it republishes
    /// under the current name.
    Bridge {
        id: String,
        configured: String,
        name: watch::Sender<String>,
    },
    /// A bridge that stopped: withdraw it.
    Forget {
        id: String,
    },
    Up {
        bridge: String,
        port: String,
        service: Service,
    },
    Down {
        bridge: String,
        port: String,
    },
}

/// An Avahi entry group.
pub type GroupId = String;

/// What the backend reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Established(GroupId),
    Collision(GroupId),
    Failure(GroupId, String),
    /// Avahi is (again) ready to take services.
    ServerUp,
    /// Avahi went away or must re-register everything (host name change).
    ServerDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    Collision,
    Unavailable(String),
    Other(String),
}

/// The mDNS side, Avahi in the daemon and a fake in tests.
pub trait Backend: Send {
    /// Publish `services` under `name` in `group` (a new group if `None`),
    /// replacing whatever the group held.
    fn publish(
        &mut self,
        group: Option<GroupId>,
        name: &str,
        services: &[Service],
    ) -> impl Future<Output = Result<GroupId, PublishError>> + Send;

    fn withdraw(&mut self, group: GroupId) -> impl Future<Output = ()> + Send;
}

struct BridgeAnn {
    configured: String,
    name: watch::Sender<String>,
    services: BTreeMap<String, Service>,
    group: Option<GroupId>,
}

pub struct Announcer<B> {
    backend: B,
    state: Arc<Mutex<StateStore>>,
    bridges: BTreeMap<String, BridgeAnn>,
    /// Whether Avahi is there to talk to.
    available: bool,
}

impl<B: Backend> Announcer<B> {
    pub fn new(backend: B, state: Arc<Mutex<StateStore>>, available: bool) -> Self {
        Self {
            backend,
            state,
            bridges: BTreeMap::new(),
            available,
        }
    }

    pub async fn run(
        mut self,
        mut cmds: mpsc::Receiver<Cmd>,
        mut events: mpsc::Receiver<Event>,
        shutdown: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => self.on_cmd(cmd).await,
                    None => break,
                },
                Some(ev) = events.recv() => self.on_event(ev).await,
            }
        }
        let groups: Vec<GroupId> = self
            .bridges
            .values_mut()
            .filter_map(|b| b.group.take())
            .collect();
        for g in groups {
            self.backend.withdraw(g).await;
        }
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Bridge {
                id,
                configured,
                name,
            } => match self.bridges.get_mut(&id) {
                Some(b) => {
                    b.configured = configured;
                    b.name = name;
                    self.sync(&id).await;
                }
                None => {
                    self.bridges.insert(
                        id,
                        BridgeAnn {
                            configured,
                            name,
                            services: BTreeMap::new(),
                            group: None,
                        },
                    );
                }
            },
            Cmd::Forget { id } => {
                if let Some(g) = self.bridges.remove(&id).and_then(|b| b.group) {
                    self.backend.withdraw(g).await;
                }
            }
            Cmd::Up {
                bridge,
                port,
                service,
            } => {
                let Some(b) = self.bridges.get_mut(&bridge) else {
                    return;
                };
                if b.services.insert(port, service) != Some(service) {
                    self.sync(&bridge).await;
                }
            }
            Cmd::Down { bridge, port } => {
                let Some(b) = self.bridges.get_mut(&bridge) else {
                    return;
                };
                if b.services.remove(&port).is_some() {
                    self.sync(&bridge).await;
                }
            }
        }
    }

    async fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Established(g) => {
                if let Some((id, b)) = self.bridge_for(&g) {
                    tracing::info!(bridge = %id, name = %*b.name.borrow(), "announced with DNS-SD");
                }
            }
            Event::Collision(g) => {
                if let Some(id) = self.bridge_for(&g).map(|(id, _)| id.clone()) {
                    self.rename(&id).await;
                }
            }
            Event::Failure(g, why) => {
                if let Some((id, _)) = self.bridge_for(&g) {
                    tracing::error!(bridge = %id, "DNS-SD announcement failed: {why}");
                }
            }
            Event::ServerUp => {
                if !self.available {
                    tracing::info!("Avahi is available; announcing tcp-server ports");
                }
                self.available = true;
                // Only what isn't announced: a repeated "up" mustn't make
                // established groups re-register.
                let ids: Vec<String> = self
                    .bridges
                    .iter()
                    .filter(|(_, b)| b.group.is_none())
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.sync(&id).await;
                }
            }
            Event::ServerDown => {
                let groups: Vec<GroupId> = self
                    .bridges
                    .values_mut()
                    .filter_map(|b| b.group.take())
                    .collect();
                for g in groups {
                    self.backend.withdraw(g).await;
                }
                if self.available {
                    tracing::warn!(
                        "Avahi went away; tcp-server ports aren't announced until it's back"
                    );
                }
                self.available = false;
            }
        }
    }

    fn bridge_for(&self, group: &str) -> Option<(&String, &BridgeAnn)> {
        self.bridges
            .iter()
            .find(|(_, b)| b.group.as_deref() == Some(group))
    }

    /// Make Avahi's view of one bridge match ours.
    async fn sync(&mut self, id: &str) {
        if !self.available {
            return;
        }
        // A rename after a collision republishes, which may collide again;
        // a handful of tries covers any LAN, and stops a loop if something
        // is badly wrong.
        for _ in 0..20 {
            let Some(b) = self.bridges.get_mut(id) else {
                return;
            };
            let services = distinct(&b.services);
            if services.is_empty() {
                if let Some(g) = b.group.take() {
                    self.backend.withdraw(g).await;
                }
                return;
            }
            let name = b.name.borrow().clone();
            match self.backend.publish(b.group.take(), &name, &services).await {
                Ok(g) => {
                    if let Some(b) = self.bridges.get_mut(id) {
                        b.group = Some(g);
                    }
                    return;
                }
                Err(PublishError::Collision) => {
                    tracing::info!(bridge = %id, %name, "DNS-SD name already in use");
                    if !self.adjust_name(id) {
                        return;
                    }
                }
                Err(PublishError::Unavailable(why)) => {
                    tracing::warn!(
                        "Avahi isn't available, so tcp-server ports aren't announced ({why})"
                    );
                    self.available = false;
                    return;
                }
                Err(PublishError::Other(why)) => {
                    tracing::error!(bridge = %id, "can't announce with DNS-SD: {why}");
                    return;
                }
            }
        }
        tracing::error!(bridge = %id, "giving up on DNS-SD: every name tried was taken");
    }

    async fn rename(&mut self, id: &str) {
        if let Some(b) = self.bridges.get(id) {
            tracing::info!(bridge = %id, name = %*b.name.borrow(), "DNS-SD name collision on the LAN");
        }
        if self.adjust_name(id) {
            self.sync(id).await;
        }
    }

    /// Move a bridge to its next name, remember it, and tell its BLE ports.
    fn adjust_name(&mut self, id: &str) -> bool {
        let Some(b) = self.bridges.get(id) else {
            return false;
        };
        let next = next_name(&b.name.borrow());
        tracing::warn!(bridge = %id, name = %next, "using an adjusted public name for DNS-SD and BLE");
        let mut store = self.state.lock().unwrap();
        if let Err(e) = store.update(|s| s.set_adjusted_public_name(id, &b.configured, &next)) {
            tracing::warn!(
                "can't write {}: {e}; the adjusted name won't survive a restart",
                store.path().display()
            );
        }
        drop(store);
        b.name.send_replace(next);
        true
    }
}

/// A bridge's services, minus any that would collide with one already
/// taken (two ports on one bridge announced on overlapping interfaces).
fn distinct(services: &BTreeMap<String, Service>) -> Vec<Service> {
    let mut out: Vec<Service> = Vec::new();
    for (port, s) in services {
        if out.iter().any(|t| t.overlaps(s)) {
            tracing::debug!(
                port,
                "not announced: overlaps another port of the same bridge"
            );
            continue;
        }
        out.push(*s);
    }
    out
}

// ── Host lookups ────────────────────────────────────────────────────────

fn if_index(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: a NUL-terminated name; 0 means no such interface.
    let i = unsafe { libc::if_nametoindex(c.as_ptr()) };
    (i != 0).then_some(i)
}

/// The interface that has `ip` assigned.
fn iface_holding(ip: IpAddr) -> Option<u32> {
    let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list we walk read-only and free below.
    if unsafe { libc::getifaddrs(&mut addrs) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = addrs;
    while !cur.is_null() {
        // SAFETY: cur is a node of the list getifaddrs returned.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: ifa_addr points at a sockaddr whose family says its type.
        let addr = unsafe {
            match (*ifa.ifa_addr).sa_family as i32 {
                libc::AF_INET => {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    Some(IpAddr::from(
                        u32::from_be(sin.sin_addr.s_addr).to_be_bytes(),
                    ))
                }
                libc::AF_INET6 => {
                    let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    Some(IpAddr::from(sin6.sin6_addr.s6_addr))
                }
                _ => None,
            }
        };
        if addr == Some(ip) {
            // SAFETY: ifa_name is a NUL-terminated interface name.
            let i = unsafe { libc::if_nametoindex(ifa.ifa_name) };
            if i != 0 {
                found = Some(i);
                break;
            }
        }
    }
    // SAFETY: freeing the list getifaddrs allocated, once.
    unsafe { libc::freeifaddrs(addrs) };
    found
}

// ── Avahi over D-Bus ────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub mod avahi {
    use std::sync::Arc;
    use std::time::Duration;

    use dbus::message::MatchRule;
    use dbus::nonblock::{MsgMatch, Proxy, SyncConnection};
    use futures::StreamExt;
    use tokio::sync::mpsc;

    use super::{Backend, Event, GroupId, Iface, Proto, PublishError, SERVICE_TYPE, Service};

    const DEST: &str = "org.freedesktop.Avahi";
    const SERVER: &str = "org.freedesktop.Avahi.Server";
    const GROUP: &str = "org.freedesktop.Avahi.EntryGroup";
    const TIMEOUT: Duration = Duration::from_secs(5);
    /// `AVAHI_SERVER_RUNNING`.
    const SERVER_RUNNING: i32 = 2;

    pub struct Avahi {
        conn: Arc<SyncConnection>,
        /// Signal subscriptions, kept alive with the backend.
        _matches: Vec<MsgMatch>,
    }

    /// Connect to the system bus and follow Avahi's signals. Returns the
    /// backend and whether Avahi is running right now.
    pub async fn connect(events: mpsc::Sender<Event>) -> Result<(Avahi, bool), String> {
        let (resource, conn) =
            dbus_tokio::connection::new_system_sync().map_err(|e| format!("system D-Bus: {e}"))?;
        tokio::spawn(async move {
            let err = resource.await;
            tracing::warn!("lost the system D-Bus connection: {err}");
        });

        let mut matches = Vec::new();

        let rule = MatchRule::new_signal(GROUP, "StateChanged").with_sender(DEST);
        let (m, mut stream) = conn
            .add_match(rule)
            .await
            .map_err(|e| e.to_string())?
            .stream::<(i32, String)>();
        matches.push(m);
        let tx = events.clone();
        tokio::spawn(async move {
            while let Some((msg, (state, error))) = stream.next().await {
                let Some(path) = msg.path() else { continue };
                let g = path.to_string();
                let ev = match state {
                    2 => Event::Established(g),
                    3 => Event::Collision(g),
                    4 => Event::Failure(g, error),
                    _ => continue,
                };
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });

        let rule = MatchRule::new_signal(SERVER, "StateChanged").with_sender(DEST);
        let (m, mut stream) = conn
            .add_match(rule)
            .await
            .map_err(|e| e.to_string())?
            .stream::<(i32, String)>();
        matches.push(m);
        let tx = events.clone();
        tokio::spawn(async move {
            while let Some((_, (state, _))) = stream.next().await {
                let ev = match state {
                    SERVER_RUNNING => Event::ServerUp,
                    // Registering a new host name, or a host name collision:
                    // every group has to be added again.
                    1 | 3 => Event::ServerDown,
                    _ => continue,
                };
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });

        // Avahi starting or stopping.
        let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
            .with_sender("org.freedesktop.DBus");
        let (m, mut stream) = conn
            .add_match(rule)
            .await
            .map_err(|e| e.to_string())?
            .stream::<(String, String, String)>();
        matches.push(m);
        let tx = events;
        let c = conn.clone();
        tokio::spawn(async move {
            while let Some((_, (name, _old, new))) = stream.next().await {
                if name != DEST {
                    continue;
                }
                if new.is_empty() {
                    if tx.send(Event::ServerDown).await.is_err() {
                        break;
                    }
                    continue;
                }
                // A new Avahi may reach "running" before it owns its name,
                // so its StateChanged could pass us by: ask until it's up.
                let server = Proxy::new(DEST, "/", TIMEOUT, c.clone());
                for _ in 0..20 {
                    if let Ok((SERVER_RUNNING,)) = server
                        .method_call::<(i32,), _, _, _>(SERVER, "GetState", ())
                        .await
                    {
                        let _ = tx.send(Event::ServerUp).await;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });

        let server = Proxy::new(DEST, "/", TIMEOUT, conn.clone());
        let running = matches!(
            server
                .method_call::<(i32,), _, _, _>(SERVER, "GetState", ())
                .await,
            Ok((SERVER_RUNNING,))
        );
        Ok((
            Avahi {
                conn,
                _matches: matches,
            },
            running,
        ))
    }

    fn classify(e: dbus::Error) -> PublishError {
        let name = e.name().unwrap_or("");
        let msg = e.message().unwrap_or("").to_string();
        if name == "org.freedesktop.Avahi.CollisionError" {
            PublishError::Collision
        } else if name.starts_with("org.freedesktop.DBus.Error.ServiceUnknown")
            || name.starts_with("org.freedesktop.DBus.Error.NameHasNoOwner")
        {
            PublishError::Unavailable(msg)
        } else {
            PublishError::Other(format!("{name}: {msg}"))
        }
    }

    impl Backend for Avahi {
        async fn publish(
            &mut self,
            group: Option<GroupId>,
            name: &str,
            services: &[Service],
        ) -> Result<GroupId, PublishError> {
            let server = Proxy::new(DEST, "/", TIMEOUT, self.conn.clone());
            let path = match group {
                Some(g) => {
                    let p = Proxy::new(DEST, g.clone(), TIMEOUT, self.conn.clone());
                    p.method_call::<(), _, _, _>(GROUP, "Reset", ())
                        .await
                        .map_err(classify)?;
                    g
                }
                None => {
                    let (p,): (dbus::Path<'static>,) = server
                        .method_call(SERVER, "EntryGroupNew", ())
                        .await
                        .map_err(classify)?;
                    p.to_string()
                }
            };
            let g = Proxy::new(DEST, path.clone(), TIMEOUT, self.conn.clone());
            for s in services {
                let iface: i32 = match s.iface {
                    Iface::Any => -1,
                    Iface::Index(i) => i as i32,
                };
                let proto: i32 = match s.proto {
                    Proto::Any => -1,
                    Proto::V4 => 0,
                    Proto::V6 => 1,
                };
                let txt: Vec<Vec<u8>> = Vec::new();
                let res = g
                    .method_call::<(), _, _, _>(
                        GROUP,
                        "AddService",
                        (iface, proto, 0u32, name, SERVICE_TYPE, "", "", s.port, txt),
                    )
                    .await;
                if let Err(e) = res {
                    let _ = g.method_call::<(), _, _, _>(GROUP, "Free", ()).await;
                    return Err(classify(e));
                }
            }
            if let Err(e) = g.method_call::<(), _, _, _>(GROUP, "Commit", ()).await {
                let _ = g.method_call::<(), _, _, _>(GROUP, "Free", ()).await;
                return Err(classify(e));
            }
            Ok(path)
        }

        async fn withdraw(&mut self, group: GroupId) {
            let g = Proxy::new(DEST, group, TIMEOUT, self.conn.clone());
            let _ = g.method_call::<(), _, _, _>(GROUP, "Free", ()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::state::StateStore;

    #[test]
    fn names_count_up() {
        assert_eq!(next_name("TH-D75"), "TH-D75 (2)");
        assert_eq!(next_name("TH-D75 (2)"), "TH-D75 (3)");
        assert_eq!(next_name("TH-D75 (9)"), "TH-D75 (10)");
        assert_eq!(next_name("Shack (home)"), "Shack (home) (2)");
        assert_eq!(next_name("Pi (2"), "Pi (2 (2)");
    }

    #[test]
    fn scopes_follow_the_binding() {
        let any4 = Service::for_listen(
            &"0.0.0.0:8001".parse().unwrap(),
            "0.0.0.0:8001".parse().unwrap(),
        );
        assert_eq!(
            any4,
            Service {
                port: 8001,
                iface: Iface::Any,
                proto: Proto::V4
            }
        );
        let any6 = Service::for_listen(&"[::]:8001".parse().unwrap(), "[::]:8001".parse().unwrap());
        assert_eq!(any6.proto, Proto::Any);
        // The loopback address lives on the loopback interface.
        let lo = Service::for_listen(
            &"127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
        );
        assert!(matches!(lo.iface, Iface::Index(_)), "{lo:?}");
        assert_eq!(lo.proto, Proto::V4);
        // The bound port wins (port 0 in config means "any").
        let p = Service::for_listen(
            &"0.0.0.0:0".parse().unwrap(),
            "0.0.0.0:41234".parse().unwrap(),
        );
        assert_eq!(p.port, 41234);
        let missing = Service::for_listen(
            &"nosuchif9:8001".parse().unwrap(),
            "0.0.0.0:8001".parse().unwrap(),
        );
        assert_eq!(missing.iface, Iface::Any);
    }

    #[test]
    fn overlapping_ports_announce_once() {
        let s = |port, iface, proto| Service { port, iface, proto };
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), s(8001, Iface::Index(3), Proto::V4));
        m.insert("b".to_string(), s(8002, Iface::Index(4), Proto::V4));
        m.insert("c".to_string(), s(8003, Iface::Any, Proto::V4));
        m.insert("d".to_string(), s(8004, Iface::Index(3), Proto::V6));
        assert_eq!(
            distinct(&m),
            vec![
                s(8001, Iface::Index(3), Proto::V4),
                s(8002, Iface::Index(4), Proto::V4),
                s(8004, Iface::Index(3), Proto::V6),
            ]
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Publish(Option<GroupId>, String, Vec<Service>),
        Withdraw(GroupId),
    }

    /// Records calls; fails publishes as told, one queued result each.
    struct Fake {
        calls: Arc<Mutex<Vec<Call>>>,
        results: Arc<Mutex<std::collections::VecDeque<Result<(), PublishError>>>>,
        next: u32,
    }

    impl Backend for Fake {
        async fn publish(
            &mut self,
            group: Option<GroupId>,
            name: &str,
            services: &[Service],
        ) -> Result<GroupId, PublishError> {
            self.calls.lock().unwrap().push(Call::Publish(
                group.clone(),
                name.to_string(),
                services.to_vec(),
            ));
            self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
            Ok(group.unwrap_or_else(|| {
                self.next += 1;
                format!("/g{}", self.next)
            }))
        }

        async fn withdraw(&mut self, group: GroupId) {
            self.calls.lock().unwrap().push(Call::Withdraw(group));
        }
    }

    struct Rig {
        cmds: mpsc::Sender<Cmd>,
        events: mpsc::Sender<Event>,
        calls: Arc<Mutex<Vec<Call>>>,
        results: Arc<Mutex<std::collections::VecDeque<Result<(), PublishError>>>>,
        name: watch::Receiver<String>,
        dir: tempfile::TempDir,
        shutdown: CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    const SVC: Service = Service {
        port: 8001,
        iface: Iface::Any,
        proto: Proto::V4,
    };

    async fn rig(available: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = StateStore::open(dir.path());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let results = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let fake = Fake {
            calls: calls.clone(),
            results: results.clone(),
            next: 0,
        };
        let (cmds, cmd_rx) = mpsc::channel(16);
        let (events, ev_rx) = mpsc::channel(16);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(
            Announcer::new(fake, Arc::new(Mutex::new(store)), available).run(
                cmd_rx,
                ev_rx,
                shutdown.clone(),
            ),
        );
        let (name_tx, name) = watch::channel("TH-D75".to_string());
        cmds.send(Cmd::Bridge {
            id: "ktbr0".into(),
            configured: "TH-D75".into(),
            name: name_tx,
        })
        .await
        .unwrap();
        Rig {
            cmds,
            events,
            calls,
            results,
            name,
            dir,
            shutdown,
            task,
        }
    }

    impl Rig {
        async fn settle(&self) {
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
        }

        fn calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }

        async fn up(&self, port: &str, service: Service) {
            self.cmds
                .send(Cmd::Up {
                    bridge: "ktbr0".into(),
                    port: port.into(),
                    service,
                })
                .await
                .unwrap();
            self.settle().await;
        }
    }

    #[tokio::test]
    async fn ports_are_announced_while_listening() {
        let r = rig(true).await;
        r.up("lan", SVC).await;
        assert_eq!(
            r.calls(),
            vec![Call::Publish(None, "TH-D75".into(), vec![SVC])]
        );
        // The same state again changes nothing.
        r.up("lan", SVC).await;
        assert!(r.calls().is_empty());
        r.cmds
            .send(Cmd::Down {
                bridge: "ktbr0".into(),
                port: "lan".into(),
            })
            .await
            .unwrap();
        r.settle().await;
        assert_eq!(r.calls(), vec![Call::Withdraw("/g1".into())]);
        r.shutdown.cancel();
        r.task.await.unwrap();
    }

    #[tokio::test]
    async fn a_collision_renames_persists_and_tells_ble() {
        let r = rig(true).await;
        r.up("lan", SVC).await;
        r.calls();
        r.events.send(Event::Collision("/g1".into())).await.unwrap();
        r.settle().await;
        assert_eq!(*r.name.borrow(), "TH-D75 (2)");
        assert_eq!(
            r.calls(),
            vec![Call::Publish(
                Some("/g1".into()),
                "TH-D75 (2)".into(),
                vec![SVC]
            )]
        );
        let (store, _) = StateStore::open(r.dir.path());
        assert_eq!(
            store.state().adjusted_public_name("ktbr0", "TH-D75"),
            Some("TH-D75 (2)")
        );

        // A local collision at publish time also moves on to the next name.
        r.results
            .lock()
            .unwrap()
            .extend([Err(PublishError::Collision), Ok(())]);
        r.up(
            "other",
            Service {
                port: 8002,
                iface: Iface::Index(9),
                proto: Proto::V6,
            },
        )
        .await;
        assert_eq!(*r.name.borrow(), "TH-D75 (3)");
        let calls = r.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            matches!(&calls[1], Call::Publish(None, n, s) if n == "TH-D75 (3)" && s.len() == 2)
        );
        let calls = r.calls.clone();
        r.shutdown.cancel();
        r.task.await.unwrap();
        assert!(matches!(
            calls.lock().unwrap().as_slice(),
            [Call::Withdraw(_)]
        ));
    }

    #[tokio::test]
    async fn waits_for_avahi_and_follows_restarts() {
        let r = rig(false).await;
        r.up("lan", SVC).await;
        assert!(r.calls().is_empty());
        r.events.send(Event::ServerUp).await.unwrap();
        r.settle().await;
        assert_eq!(
            r.calls(),
            vec![Call::Publish(None, "TH-D75".into(), vec![SVC])]
        );
        // Hearing it's up again (signal and poll both) changes nothing.
        r.events.send(Event::ServerUp).await.unwrap();
        r.settle().await;
        assert!(r.calls().is_empty());
        // Avahi restarts: groups are gone, then everything is added again.
        r.events.send(Event::ServerDown).await.unwrap();
        r.settle().await;
        assert_eq!(r.calls(), vec![Call::Withdraw("/g1".into())]);
        r.up("lan2", Service { port: 8002, ..SVC }).await;
        assert!(r.calls().is_empty());
        r.events.send(Event::ServerUp).await.unwrap();
        r.settle().await;
        assert_eq!(
            r.calls(),
            vec![Call::Publish(None, "TH-D75".into(), vec![SVC])]
        );
        // Unavailable at publish time: quietly waits again.
        r.results
            .lock()
            .unwrap()
            .push_back(Err(PublishError::Unavailable("gone".into())));
        let v6 = |port| Service {
            port,
            iface: Iface::Index(2),
            proto: Proto::V6,
        };
        r.up("lan3", v6(8003)).await;
        assert_eq!(r.calls().len(), 1);
        r.up("lan4", v6(8004)).await;
        assert!(r.calls().is_empty());
        r.shutdown.cancel();
        r.task.await.unwrap();
    }

    #[tokio::test]
    async fn renamed_bridges_republish_and_stopped_ones_withdraw() {
        let r = rig(true).await;
        r.up("lan", SVC).await;
        r.calls();
        // The configured public name changed: same group, new name.
        let (name_tx, _rx) = watch::channel("Portable".to_string());
        r.cmds
            .send(Cmd::Bridge {
                id: "ktbr0".into(),
                configured: "Portable".into(),
                name: name_tx,
            })
            .await
            .unwrap();
        r.settle().await;
        assert_eq!(
            r.calls(),
            vec![Call::Publish(
                Some("/g1".into()),
                "Portable".into(),
                vec![SVC]
            )]
        );
        r.cmds
            .send(Cmd::Forget { id: "ktbr0".into() })
            .await
            .unwrap();
        r.settle().await;
        assert_eq!(r.calls(), vec![Call::Withdraw("/g1".into())]);
        // Ports of a forgotten bridge are ignored.
        r.up("lan", SVC).await;
        assert!(r.calls().is_empty());
        r.shutdown.cancel();
        r.task.await.unwrap();
    }

    #[test]
    fn effective_names_drop_stale_adjustments() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = StateStore::open(dir.path());
        store
            .update(|s| s.set_adjusted_public_name("ktbr0", "TH-D75", "TH-D75 (2)"))
            .unwrap();
        assert_eq!(
            effective_public_name(&mut store, "ktbr0", "TH-D75"),
            "TH-D75 (2)"
        );
        assert_eq!(effective_public_name(&mut store, "ktbr1", "Other"), "Other");
        // The configured name changed: the adjustment no longer applies.
        assert_eq!(
            effective_public_name(&mut store, "ktbr0", "Portable"),
            "Portable"
        );
        assert!(store.state().bridges.is_empty());
    }
}
