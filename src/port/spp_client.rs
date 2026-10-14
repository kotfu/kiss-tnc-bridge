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

//! `spp-client`: a TNC over Bluetooth Classic SPP, such as the one built
//! into a TH-D74 or TH-D75.
//!
//! The link goes through a BlueZ profile rather than a hard-coded RFCOMM
//! channel: the daemon registers one SPP client profile, each connect asks
//! BlueZ to connect that profile on the TNC's device, BlueZ does the SDP
//! lookup, and the RFCOMM socket arrives as the profile's connect request.
//! A `channel` in the TNC's config skips SDP for devices that need it.
//!
//! A TNC that isn't paired on its adapter is reported as unpaired and
//! checked again every few seconds, without paging the radio. Pairing is a
//! manual step with `bluetoothctl` until `ktbctl pair` exists.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bluer::rfcomm::{Profile, ReqError, Role, SocketAddr, Stream};
use bluer::{Address, Device, Session};
use futures::StreamExt;
use tokio::sync::oneshot;

use crate::bluetooth::{self, SPP_UUID};
use crate::bridge::{ConnectFuture, Opened, TncConnector};
use crate::config::{Spp, TncModel};
use crate::error::Error;
use crate::port::tnc_init::{self, Prefixed};

/// Longest a connect may take. A radio that's off fails in about 5 s (the
/// controller's page timeout); this covers a slow SDP lookup on top.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

type Waiters = Arc<Mutex<HashMap<Address, oneshot::Sender<bluer::rfcomm::ConnectRequest>>>>;

/// The daemon's one SPP client profile. BlueZ hands every SPP connection it
/// makes for us to this profile; each goes to the port that asked for it,
/// matched by device address.
pub struct SppProfile {
    waiters: Waiters,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SppProfile {
    fn drop(&mut self) {
        // Dropping the profile handle (owned by the task) unregisters it.
        self.task.abort();
    }
}

impl SppProfile {
    pub async fn register(session: &Session) -> bluer::Result<Arc<SppProfile>> {
        let handle = session
            .register_profile(Profile {
                uuid: SPP_UUID,
                name: Some("kiss-tnc-bridge".into()),
                role: Some(Role::Client),
                require_authentication: Some(true),
                require_authorization: Some(false),
                auto_connect: Some(false),
                ..Default::default()
            })
            .await?;
        let waiters: Waiters = Arc::default();
        let w = waiters.clone();
        let task = tokio::spawn(async move {
            let mut handle = Box::pin(handle);
            while let Some(req) = handle.next().await {
                let waiter = w.lock().unwrap().remove(&req.device());
                match waiter {
                    Some(tx) => {
                        if let Err(req) = tx.send(req) {
                            req.reject(ReqError::Canceled);
                        }
                    }
                    None => {
                        tracing::debug!(device = %req.device(), "SPP connection nobody asked for, rejected");
                        req.reject(ReqError::Rejected);
                    }
                }
            }
        });
        Ok(Arc::new(SppProfile { waiters, task }))
    }

    /// Connect the SPP profile on `device` and take the RFCOMM socket BlueZ
    /// hands over. `ConnectProfile` and the connect request can arrive in
    /// either order.
    async fn connect(&self, device: &Device) -> Result<Stream, String> {
        let addr = device.address();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut w = self.waiters.lock().unwrap();
            if w.contains_key(&addr) {
                return Err(format!("another connect to {addr} is in progress"));
            }
            w.insert(addr, tx);
        }
        let _guard = WaiterGuard {
            waiters: self.waiters.clone(),
            addr,
        };
        let connect = device.connect_profile(&SPP_UUID);
        tokio::pin!(connect);
        let mut connect_done = false;
        let deadline = tokio::time::sleep(CONNECT_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                res = &mut connect, if !connect_done => {
                    connect_done = true;
                    if let Err(e) = res {
                        return Err(explain(&e));
                    }
                }
                req = &mut rx => {
                    let req = req.map_err(|_| "the SPP profile went away".to_string())?;
                    return req.accept().map_err(|e| format!("can't take the RFCOMM socket: {e}"));
                }
                _ = &mut deadline => {
                    return Err(format!("no SPP connection after {}s", CONNECT_TIMEOUT.as_secs()));
                }
            }
        }
    }
}

/// Removes a connect's waiter however the connect ends, including when the
/// bridge aborts it.
struct WaiterGuard {
    waiters: Waiters,
    addr: Address,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        self.waiters.lock().unwrap().remove(&self.addr);
    }
}

/// BlueZ's errors, with what they usually mean for a radio.
fn explain(e: &bluer::Error) -> String {
    let msg = e.to_string();
    if msg.contains("page-timeout") || msg.contains("Host is down") {
        format!("{msg}: the radio didn't answer (off, out of range, or its Bluetooth is off)")
    } else if msg.contains("refused") || msg.contains("profile-unavailable") {
        format!("{msg}: is KISS set to Bluetooth on the radio?")
    } else {
        msg
    }
}

pub struct SppConnector {
    session: Session,
    profile: Arc<SppProfile>,
    tnc_id: String,
    spp: Spp,
    model: TncModel,
    exit_kiss_on_disconnect: bool,
    /// The adapter is powered on at the first connect if it needs to be;
    /// after that, an adapter that's off was turned off by someone, and the
    /// port waits for them to turn it back on.
    tried_power: Arc<AtomicBool>,
}

pub fn connector(
    session: Session,
    profile: Arc<SppProfile>,
    tnc_id: String,
    spp: Spp,
    model: TncModel,
    exit_kiss_on_disconnect: bool,
) -> SppConnector {
    SppConnector {
        session,
        profile,
        tnc_id,
        spp,
        model,
        exit_kiss_on_disconnect,
        tried_power: Arc::default(),
    }
}

impl TncConnector for SppConnector {
    fn connect(&self) -> ConnectFuture {
        let session = self.session.clone();
        let profile = self.profile.clone();
        let tnc_id = self.tnc_id.clone();
        let spp = self.spp.clone();
        let model = self.model;
        let exit_kiss = self.exit_kiss_on_disconnect;
        let tried_power = self.tried_power.clone();
        Box::pin(async move {
            let fail = |msg: String| Error::Connect(format!("tnc {tnc_id}: {msg}"));
            let adapter = bluetooth::find_adapter(&session, &spp.adapter)
                .await
                .map_err(|e| fail(format!("BlueZ: {e}")))?
                .ok_or_else(|| fail(format!("{} isn't present", spp.adapter)))?;
            let adapter_desc = bluetooth::describe(&adapter).await;
            if !adapter.is_powered().await.unwrap_or(false) {
                if tried_power.swap(true, Ordering::Relaxed) {
                    return Err(fail(format!("adapter {adapter_desc} is powered off")));
                }
                bluetooth::power_on(&adapter).await.map_err(fail)?;
            }
            tried_power.store(true, Ordering::Relaxed);

            let addr = Address(spp.address.0);
            let device = adapter
                .device(addr)
                .map_err(|e| fail(format!("BlueZ: {e}")))?;
            if !device.is_paired().await.unwrap_or(false) {
                return Err(Error::Unpaired(format!(
                    "tnc {tnc_id} ({addr}) is not paired on adapter {adapter_desc}; pair it with bluetoothctl (see the README)"
                )));
            }

            let mut stream = match spp.channel {
                None => profile.connect(&device).await.map_err(fail)?,
                Some(ch) => {
                    let local = adapter
                        .address()
                        .await
                        .map_err(|e| fail(format!("BlueZ: {e}")))?;
                    let socket = bluer::rfcomm::Socket::new()
                        .map_err(|e| fail(format!("RFCOMM socket: {e}")))?;
                    socket
                        .bind(SocketAddr::new(local, 0))
                        .map_err(|e| fail(format!("RFCOMM bind: {e}")))?;
                    tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(SocketAddr::new(addr, ch)))
                        .await
                        .map_err(|_| fail(format!("RFCOMM channel {ch}: timed out")))?
                        .map_err(|e| fail(format!("RFCOMM channel {ch}: {e}")))?
                }
            };
            tracing::debug!(tnc = %tnc_id, %addr, adapter = %adapter_desc, "RFCOMM link up");
            let out = tnc_init::init(&mut stream, model, exit_kiss).await?;
            Ok(Opened {
                link: Box::new(Prefixed::new(stream, &out.frames)),
                teardown: out.teardown,
            })
        })
    }

    fn describe(&self) -> String {
        format!("tnc {} at {} over Bluetooth", self.tnc_id, self.spp.address)
    }
}
