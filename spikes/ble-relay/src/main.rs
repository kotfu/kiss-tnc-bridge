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

//! A BLE man in the middle, for learning a radio's GATT protocol from its
//! own app.
//!
//! It connects to the radio as a central, copies the radio's GATT services
//! into a GATT application of its own (same UUIDs, same properties), and
//! advertises that under another name. The vendor's app connects to the
//! relay instead of the radio; every read, write and notification is passed
//! through and logged with a timestamp, so the log is the conversation
//! between the app and the radio.
//!
//! Notifications on the radio are turned on only when the app subscribes,
//! and off again when it unsubscribes, so the radio sees the same sequence
//! it would see from the app directly.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bluer::adv::Advertisement;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, ReqError, Service,
};
use bluer::gatt::remote::Characteristic as RemoteChar;
use bluer::gatt::{CharacteristicReader, WriteOp};
use bluer::{Adapter, AdapterEvent, Address, Device, DeviceEvent, DeviceProperty, DiscoveryFilter, DiscoveryTransport};
use clap::Parser;
use futures::{FutureExt, StreamExt};
use uuid::Uuid;

/// Relay a BLE radio's GATT services to its app, logging everything.
#[derive(Parser)]
struct Args {
    /// The radio's Bluetooth address
    radio: Address,
    /// The adapter to use for both sides
    #[arg(short, long, default_value = "hci0")]
    adapter: String,
    /// The name to advertise
    #[arg(short, long, default_value = "TD-H9-RELAY")]
    name: String,
    /// Service UUIDs to put in the advertisement, as the radio's own
    /// advertisement has them [default: the radio's advertised UUIDs]
    #[arg(long = "adv-uuid")]
    adv_uuids: Vec<Uuid>,
}

/// GAP and GATT: BlueZ serves its own.
const SKIP: [u16; 2] = [0x1800, 0x1801];

fn short(u: &Uuid) -> String {
    let (hi, mid, lo, rest) = u.as_fields();
    if mid == 0 && lo == 0x1000 && rest == &[0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb] && hi <= 0xffff {
        format!("{hi:04x}")
    } else {
        u.to_string()
    }
}

/// `HH:MM:SS.mmm` in UTC.
fn now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let s = d.as_secs() % 86400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, d.subsec_millis())
}

fn log(dir: &str, label: &str, what: &str, data: &[u8]) {
    let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
    let ascii: String = data
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
        .collect();
    println!("{} {dir:<9} {label:<10} {what:<6} {:>3}  {}  |{ascii}|", now(), data.len(), hex.join(" "));
}

fn note(msg: impl AsRef<str>) {
    println!("{} --        {}", now(), msg.as_ref());
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let session = bluer::Session::new().await?;
    let adapter = session.adapter(&args.adapter)?;
    adapter.set_powered(true).await?;

    let device = find(&adapter, args.radio).await?;
    let adv_uuids: Vec<Uuid> = if args.adv_uuids.is_empty() {
        // Before connecting, Device1.UUIDs holds what it advertises.
        device.uuids().await?.unwrap_or_default().into_iter().collect()
    } else {
        args.adv_uuids.clone()
    };
    connect(&device).await?;

    let services = mirror(&device).await?;
    let app = adapter
        .serve_gatt_application(Application {
            services,
            ..Default::default()
        })
        .await
        .context("registering the GATT application")?;
    let adv = adapter
        .advertise(Advertisement {
            advertisement_type: bluer::adv::Type::Peripheral,
            service_uuids: adv_uuids.iter().copied().collect(),
            local_name: Some(args.name.clone()),
            discoverable: Some(true),
            ..Default::default()
        })
        .await
        .context("advertising")?;
    note(format!(
        "advertising as {:?} with {}; connect the app to it (Ctrl-C stops)",
        args.name,
        adv_uuids.iter().map(short).collect::<Vec<_>>().join(", ")
    ));

    // Note the app's comings and goings, and stop if the radio goes.
    let mut radio_events = device.events().await?;
    let mut adapter_events = adapter.events().await?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = radio_events.next() => match ev {
                Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) | None => {
                    note("the radio disconnected");
                    break;
                }
                _ => {}
            },
            Some(AdapterEvent::DeviceAdded(addr)) = adapter_events.next() => {
                if addr != args.radio
                    && let Ok(d) = adapter.device(addr)
                    && d.is_connected().await.unwrap_or(false)
                {
                    note(format!("{addr} connected ({})", d.name().await.ok().flatten().unwrap_or_default()));
                }
            }
        }
    }
    drop(adv);
    drop(app);
    let _ = device.disconnect().await;
    note("stopped");
    Ok(())
}

/// The radio's device, scanning for it if BlueZ doesn't know it.
async fn find(adapter: &Adapter, addr: Address) -> Result<Device> {
    if !adapter.device_addresses().await?.contains(&addr) {
        note(format!("scanning for {addr}"));
        adapter
            .set_discovery_filter(DiscoveryFilter {
                transport: DiscoveryTransport::Le,
                ..Default::default()
            })
            .await?;
        let events = adapter.discover_devices().await?;
        futures::pin_mut!(events);
        let heard = async {
            while let Some(ev) = events.next().await {
                if matches!(ev, AdapterEvent::DeviceAdded(a) if a == addr) {
                    return true;
                }
            }
            false
        };
        if !tokio::time::timeout(Duration::from_secs(30), heard).await.unwrap_or(false) {
            bail!("{addr} isn't advertising (off, or something else is connected to it?)");
        }
    }
    Ok(adapter.device(addr)?)
}

async fn connect(device: &Device) -> Result<()> {
    // An LE connect right after a scan often aborts at once
    // (le-connection-abort-by-local); a second try a moment later works.
    let mut tries = 0;
    while !device.is_connected().await? {
        tries += 1;
        note(format!("connecting to {} (try {tries})", device.address()));
        match tokio::time::timeout(Duration::from_secs(20), device.connect()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if tries < 5 => {
                note(format!("connect failed: {e}; trying again"));
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => bail!("no connection after 20 s"),
        }
    }
    for _ in 0..100 {
        if device.is_services_resolved().await? {
            note(format!(
                "connected to {} ({})",
                device.address(),
                device.name().await?.unwrap_or_default()
            ));
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("its services weren't resolved within 20 s")
}

/// Local copies of the radio's services, each characteristic relaying to
/// its original.
async fn mirror(device: &Device) -> Result<Vec<Service>> {
    let mut out = Vec::new();
    for service in device.services().await? {
        let su = service.uuid().await?;
        if SKIP.iter().any(|s| short(&su) == format!("{s:04x}")) {
            continue;
        }
        let mut chars = Vec::new();
        for c in service.characteristics().await? {
            let cu = c.uuid().await?;
            let flags = c.flags().await?;
            let label = format!("{}/{}", short(&su), short(&cu));
            let mut props = Vec::new();
            let c = Arc::new(c);

            let read = flags.read.then(|| {
                props.push("read");
                let (c, label) = (c.clone(), label.clone());
                CharacteristicRead {
                    read: true,
                    fun: Box::new(move |_req| {
                        let (c, label) = (c.clone(), label.clone());
                        async move {
                            match c.read().await {
                                Ok(v) => {
                                    log("radio→app", &label, "read", &v);
                                    Ok(v)
                                }
                                Err(e) => {
                                    note(format!("{label}: read failed: {e}"));
                                    Err(ReqError::Failed)
                                }
                            }
                        }
                        .boxed()
                    }),
                    ..Default::default()
                }
            });

            let write = (flags.write || flags.write_without_response).then(|| {
                if flags.write {
                    props.push("write");
                }
                if flags.write_without_response {
                    props.push("write-without-response");
                }
                let (c, label) = (c.clone(), label.clone());
                CharacteristicWrite {
                    write: flags.write,
                    write_without_response: flags.write_without_response,
                    method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                        let (c, label) = (c.clone(), label.clone());
                        async move {
                            let (what, op) = match req.op_type {
                                WriteOp::Command => ("cmd", WriteOp::Command),
                                _ => ("req", WriteOp::Request),
                            };
                            log("app→radio", &label, what, &value);
                            let r = bluer::gatt::remote::CharacteristicWriteRequest {
                                op_type: op,
                                ..Default::default()
                            };
                            c.write_ext(&value, &r).await.map_err(|e| {
                                note(format!("{label}: write to the radio failed: {e}"));
                                ReqError::Failed
                            })
                        }
                        .boxed()
                    })),
                    ..Default::default()
                }
            });

            let notify = (flags.notify || flags.indicate).then(|| {
                props.push(if flags.notify { "notify" } else { "indicate" });
                let (c, label) = (c.clone(), label.clone());
                CharacteristicNotify {
                    notify: flags.notify,
                    indicate: flags.indicate,
                    method: CharacteristicNotifyMethod::Fun(Box::new(move |mut notifier| {
                        let (c, label) = (c.clone(), label.clone());
                        async move {
                            note(format!("{label}: the app subscribed"));
                            let mut values = match subscribe(&c).await {
                                Ok(v) => v,
                                Err(e) => {
                                    note(format!("{label}: can't subscribe on the radio: {e}"));
                                    return;
                                }
                            };
                            loop {
                                tokio::select! {
                                    v = values.next() => {
                                        let Some(v) = v else {
                                            note(format!("{label}: the radio's notifications ended"));
                                            break;
                                        };
                                        log("radio→app", &label, "notify", &v);
                                        if let Err(e) = notifier.notify(v).await {
                                            note(format!("{label}: can't pass it on: {e}"));
                                            break;
                                        }
                                    }
                                    _ = notifier.stopped() => {
                                        note(format!("{label}: the app unsubscribed"));
                                        break;
                                    }
                                }
                            }
                        }
                        .boxed()
                    })),
                    ..Default::default()
                }
            });

            note(format!("mirroring {label} ({})", props.join(", ")));
            chars.push(Characteristic {
                uuid: cu,
                read,
                write,
                notify,
                ..Default::default()
            });
        }
        out.push(Service {
            uuid: su,
            primary: true,
            characteristics: chars,
            ..Default::default()
        });
    }
    if out.is_empty() {
        bail!("the radio has no services to relay");
    }
    Ok(out)
}

/// The radio's notifications on `c`, as values. Prefers the fd I/O, which
/// keeps each notification whole; falls back to D-Bus value changes.
async fn subscribe(c: &RemoteChar) -> Result<futures::stream::BoxStream<'static, Vec<u8>>> {
    match c.notify_io().await {
        Ok(reader) => Ok(notifications(reader).boxed()),
        Err(_) => Ok(c.notify().await?.boxed()),
    }
}

fn notifications(reader: CharacteristicReader) -> impl futures::Stream<Item = Vec<u8>> {
    futures::stream::unfold(reader, |reader| async move {
        match reader.recv().await {
            Ok(v) if !v.is_empty() => Some((v, reader)),
            _ => None,
        }
    })
}
