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

//! `ktbctl setup`: a TNC and a bridge for it, step by step.
//!
//! 1. How the bridge reaches the TNC: Bluetooth, USB/serial, or the
//!    network (a KISS TCP server, which needs nothing more).
//! 2. For Bluetooth and USB, which radio it is, from the radios in the
//!    daemon's model table, or another; its entry gives the instructions
//!    from here on, and generic ones stand in for another radio.
//! 3. Bluetooth: an existing TNC, or pick the adapter, scan, pick the
//!    radio, and pair it. USB: pick the device from the stable paths.
//! 4. The TNC: one already at that address or path, a new section on the
//!    same radio's TNC, or a new TNC.
//! 5. The bridge: how apps reach it (BLE and on which adapter, TCP and
//!    where, with the exposure warning, or both), and when to connect.
//! 6. Create it, add its ports, enable it (offering `switch` if another
//!    bridge is in the way).
//! 7. Connect, and wait for the first frame or 30 s of silence.
//!
//! Every step goes through the control API; nothing here reads config.

use std::time::Duration;

use serde_json::{Value, json};

use super::cli::BtWho;
use super::client::{Client, CtlError, Message};
use super::run::{Ctl, ask_line, confirm, interactive};
use crate::control::protocol::{
    AdapterStatus, BtDevice, Command, ConfigText, ErrorCode, Event, ModelEntry, ObjectKind,
    RadioEntry, SerialEntry, Status, Topic,
};

/// How the bridge reaches the TNC.
enum Link {
    Bluetooth,
    Serial,
}

/// The bridge's TNC port, once the TNC side is settled.
enum TncSide {
    /// A `[[tnc]]` by id.
    Tnc(String),
    /// A KISS TCP server, at `host:port`.
    Network(String),
}

impl Ctl {
    pub(super) async fn setup(&self, c: &mut Client) -> Result<(), CtlError> {
        if !interactive() {
            return Err(CtlError::Message(
                "setup asks questions; run it in a terminal".into(),
            ));
        }
        println!("This sets up a TNC and a bridge to share it. Ctrl-C stops at any point;");
        println!("anything made by then stays, and `ktbctl status` shows it.\n");
        let s: Status = c.call_as(Command::Status).await?;
        let how = menu(
            "How will the bridge reach the TNC?",
            &[
                "Bluetooth".into(),
                "USB or serial".into(),
                "On the network (a KISS TCP server, such as Dire Wolf or Graywolf)".into(),
            ],
            0,
        )
        .await;
        let side = match how {
            0 => {
                let radio = self.pick_radio(c, Link::Bluetooth).await?;
                TncSide::Tnc(self.setup_bluetooth(c, &s, radio.as_ref()).await?)
            }
            1 => {
                let radio = self.pick_radio(c, Link::Serial).await?;
                TncSide::Tnc(self.setup_serial(c, &s, radio.as_ref()).await?)
            }
            _ => {
                println!(
                    "\nThe TNC's KISS TCP address. Dire Wolf and Graywolf listen on port 8001 by default."
                );
                TncSide::Network(ask_default("Address (host:port)", "127.0.0.1:8001").await)
            }
        };
        self.setup_bridge(c, side).await
    }

    /// Which radio it is, from the radios the daemon knows that connect
    /// this way; `None` for another radio or TNC.
    async fn pick_radio(&self, c: &mut Client, link: Link) -> Result<Option<RadioEntry>, CtlError> {
        let models: Vec<ModelEntry> = c.call_as(Command::TncModels).await?;
        let radios: Vec<RadioEntry> = models
            .into_iter()
            .flat_map(|m| m.radios)
            .filter(|r| match link {
                Link::Bluetooth => r.bluetooth.is_some(),
                Link::Serial => r.usb.is_some(),
            })
            .collect();
        let mut options: Vec<String> = radios.iter().map(|r| r.name.clone()).collect();
        options.push("Another radio or TNC".into());
        let pick = menu("Which radio is it?", &options, 0).await;
        Ok(radios.get(pick).cloned())
    }

    /// Steps 2 and 4 over Bluetooth. Returns the TNC's id.
    async fn setup_bluetooth(
        &self,
        c: &mut Client,
        s: &Status,
        radio: Option<&RadioEntry>,
    ) -> Result<String, CtlError> {
        let bt = radio.and_then(|r| r.bluetooth.as_ref());
        let pairing_mode = match bt {
            Some(b) => format!("Put the radio in pairing mode ({}).", b.pairing_mode),
            None => "Make it discoverable (pairing mode); its manual says how.".into(),
        };
        let existing: Vec<_> = s.tncs.iter().filter(|t| t.spp.is_some()).collect();
        if !existing.is_empty() {
            let mut options: Vec<String> = existing
                .iter()
                .map(|t| {
                    let spp = t.spp.as_ref().expect("filtered");
                    let paired = if spp.paired == Some(true) {
                        "paired"
                    } else {
                        "not paired"
                    };
                    format!(
                        "{} ({}, {} on {}, {paired})",
                        t.id, t.name, spp.address, spp.adapter
                    )
                })
                .collect();
            options.push("One not set up yet".into());
            let pick = menu(
                "Is it a TNC that's already set up?",
                &options,
                options.len() - 1,
            )
            .await;
            if let Some(t) = existing.get(pick) {
                if t.spp.as_ref().and_then(|s| s.paired) != Some(true) {
                    println!("\n{pairing_mode}");
                    if let Some(b) = bt {
                        println!("{}", b.pairing);
                    }
                    self.pair(
                        c,
                        BtWho {
                            tnc: Some(t.id.clone()),
                            address: None,
                            adapter: None,
                        },
                    )
                    .await?;
                }
                return Ok(t.id.clone());
            }
        }

        println!(
            "\n{}",
            bt.map(|b| b.setup.clone()).unwrap_or_else(|| {
                "Turn on its Bluetooth, and its KISS TNC if it has a switch for it; its manual says how."
                    .into()
            })
        );
        let present: Vec<&AdapterStatus> = s.adapters.iter().filter(|a| a.present).collect();
        let adapter = match present.as_slice() {
            [] => {
                return Err(CtlError::Message(
                    "no Bluetooth adapter is present on this host".into(),
                ));
            }
            [one] => *one,
            many => {
                let options: Vec<String> = many.iter().map(|a| describe_adapter(a)).collect();
                many[menu(
                    "Which of this computer's adapters should talk to the radio? \
                     (The one it's paired on, or will be.)",
                    &options,
                    0,
                )
                .await]
            }
        };
        let adapter_value = self.adapter_value(c, adapter, s).await?;

        let device = loop {
            println!("\n{pairing_mode}");
            if !confirm("Ready to look for it?", true).await {
                return Err(CtlError::Message("stopped".into()));
            }
            println!("Looking for 10 s...");
            let found: Vec<BtDevice> = c
                .call_as(Command::BluetoothScan {
                    adapter: Some(adapter_value.clone()),
                    seconds: Some(10),
                })
                .await?;
            let mut options: Vec<String> = found
                .iter()
                .map(|d| {
                    let what = d.known_tnc.clone().or(d.icon.clone()).unwrap_or_default();
                    format!(
                        "{}  {}  {what}",
                        d.address,
                        d.name.as_deref().unwrap_or("(no name)")
                    )
                })
                .collect();
            options.push("Look again".into());
            if found.is_empty() {
                println!("Nothing found.");
            }
            // The radio they said it is, if the scan saw one.
            let likely = bt
                .and_then(|b| {
                    found.iter().position(|d| {
                        d.name
                            .as_deref()
                            .is_some_and(|n| n.starts_with(&b.name_prefix))
                    })
                })
                .unwrap_or(0);
            let pick = menu("Which is it?", &options, likely).await;
            if let Some(d) = found.get(pick) {
                break d.clone();
            }
        };
        if let Some(t) = &device.tnc {
            println!("That's TNC {t}, already set up.");
        }
        if !device.paired {
            if let Some(b) = bt {
                println!("{}", b.pairing);
            }
            self.pair(
                c,
                BtWho {
                    tnc: None,
                    address: Some(device.address.clone()),
                    adapter: Some(adapter_value.clone()),
                },
            )
            .await?;
        }
        if let Some(t) = device.tnc {
            return Ok(t);
        }
        let model = model_for(radio, device.known_tnc.as_deref(), device.model.as_deref());
        let name = device.name.clone().or(radio.map(|r| short_name(&r.name)));
        let section = json!({"address": device.address, "adapter": adapter_value});
        self.setup_tnc(c, s, "spp", section, &model, name.as_deref())
            .await
    }

    /// Steps 3 and 4 over USB or serial. Returns the TNC's id.
    async fn setup_serial(
        &self,
        c: &mut Client,
        s: &Status,
        radio: Option<&RadioEntry>,
    ) -> Result<String, CtlError> {
        let usb = radio.and_then(|r| r.usb.as_ref());
        println!(
            "\n{}",
            usb.map(|u| u.setup.clone()).unwrap_or_else(|| {
                "Connect it to this computer, and turn on its KISS TNC if it has a switch for it; \
                 its manual says how."
                    .into()
            })
        );
        let entry = loop {
            if !confirm("Ready?", true).await {
                return Err(CtlError::Message("stopped".into()));
            }
            let list: Vec<SerialEntry> = c.call_as(Command::SerialList).await?;
            let mut options: Vec<String> = list
                .iter()
                .map(|e| {
                    let what = e
                        .known_tnc
                        .clone()
                        .or(e.description.clone())
                        .unwrap_or_default();
                    format!("{}  {what}", e.path)
                })
                .collect();
            options.push("Look again".into());
            if list.is_empty() {
                println!("No serial devices found.");
            }
            let likely = radio
                .and_then(|r| {
                    list.iter()
                        .position(|e| e.model.as_deref() == Some(r.model.as_str()))
                })
                .unwrap_or(0);
            let pick = menu("Which is it?", &options, likely).await;
            if let Some(e) = list.get(pick) {
                break e.clone();
            }
        };
        if let Some(t) = entry.tnc {
            println!("That's TNC {t}, already set up.");
            return Ok(t);
        }
        let mut section = json!({"path": entry.path});
        // USB radios ignore the baud rate; a real serial line needs it.
        if entry.usb_id.is_none() {
            let baud = ask_default("Baud rate", "9600").await;
            section["baud"] = json!(baud.parse::<u32>().unwrap_or(9600));
        }
        let model = model_for(radio, entry.known_tnc.as_deref(), entry.model.as_deref());
        let name = radio
            .map(|r| r.name.clone())
            .or(entry.known_tnc.clone())
            .map(|n| short_name(&n));
        self.setup_tnc(c, s, "serial", section, &model, name.as_deref())
            .await
    }

    /// Step 4: add `section` (`spp` or `serial`) to the same radio's TNC, or
    /// make a new TNC with it. Returns the TNC's id.
    async fn setup_tnc(
        &self,
        c: &mut Client,
        s: &Status,
        kind: &str,
        section: Value,
        model: &str,
        name: Option<&str>,
    ) -> Result<String, CtlError> {
        // The same radio set up the other way (a D75 over Bluetooth, now
        // over USB too): its TNC gets the new section.
        let lacking: Vec<_> = s
            .tncs
            .iter()
            .filter(|t| {
                t.editable
                    && if kind == "spp" {
                        t.spp.is_none()
                    } else {
                        t.serial.is_none()
                    }
            })
            .collect();
        for t in lacking {
            if confirm(
                &format!(
                    "Is this the same radio as TNC {} ({})? If so, it gets this connection too.",
                    t.id, t.name
                ),
                false,
            )
            .await
            {
                let text: ConfigText = c
                    .call_as(Command::ConfigGet {
                        kind: Some(ObjectKind::Tnc),
                        object: Some(t.id.clone()),
                    })
                    .await?;
                let toml = add_section(&text.toml, kind, &section)?;
                self.change_result(c, |nd| Command::ConfigPut {
                    kind: Some(ObjectKind::Tnc),
                    object: Some(t.id.clone()),
                    toml: toml.clone(),
                    revision: text.revision.clone(),
                    non_disruptive: nd,
                })
                .await?;
                println!("added it to TNC {}", t.id);
                return Ok(t.id.clone());
            }
        }

        let ids: Vec<&str> = s.tncs.iter().map(|t| t.id.as_str()).collect();
        loop {
            let id = ask_default("An id for the TNC", &suggest_tnc_id(model, &ids)).await;
            let name = ask_default("Its name", name.unwrap_or(&id)).await;
            let mut def = json!({"id": id, "name": name, "model": model});
            def[kind] = section.clone();
            match self
                .change_result(c, |nd| Command::TncCreate {
                    definition: def.clone(),
                    non_disruptive: nd,
                })
                .await
            {
                Ok(_) => {
                    println!("created TNC {id}");
                    return Ok(id);
                }
                Err(CtlError::Api(e))
                    if matches!(e.code, ErrorCode::Conflict | ErrorCode::ConfigInvalid) =>
                {
                    self.report(&CtlError::Api(e));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Steps 5 to 7.
    async fn setup_bridge(&self, c: &mut Client, side: TncSide) -> Result<(), CtlError> {
        // Read again: step 4 may have made a TNC or an adapter.
        let s: Status = c.call_as(Command::Status).await?;
        // The TNC port, the name to suggest for the bridge, and the radio's
        // own adapter (best not also used for BLE).
        let (tnc_port, default_name, radio_adapter) = match &side {
            TncSide::Network(address) => (
                json!({"id": "tnc", "type": "tcp-client", "address": address}),
                format!("KISS at {address}"),
                String::new(),
            ),
            TncSide::Tnc(tnc) => {
                let t = s
                    .tncs
                    .iter()
                    .find(|t| t.id == *tnc)
                    .ok_or_else(|| CtlError::Message(format!("TNC {tnc} went away")))?;
                let kind = if t.spp.is_some() && t.serial.is_some() {
                    let pick = menu(
                        "Which way should this bridge reach the radio?",
                        &["Bluetooth".into(), "USB or serial".into()],
                        0,
                    )
                    .await;
                    if pick == 0 { "spp-client" } else { "serial" }
                } else if t.spp.is_some() {
                    "spp-client"
                } else {
                    "serial"
                };
                (
                    json!({"id": "radio", "type": kind, "tnc": tnc}),
                    t.name.clone(),
                    t.spp
                        .as_ref()
                        .map(|s| s.adapter.clone())
                        .unwrap_or_default(),
                )
            }
        };
        let tnc_port_id = tnc_port["id"].as_str().unwrap_or("tnc").to_string();

        let share = menu(
            "How should apps reach it?",
            &[
                "Bluetooth LE (phone and tablet apps, such as aprs.fi)".into(),
                "TCP (desktop apps, or apps on other computers)".into(),
                "Both".into(),
            ],
            0,
        )
        .await;
        let mut ble = None;
        if share != 1 {
            let present: Vec<&AdapterStatus> = s.adapters.iter().filter(|a| a.present).collect();
            let taken = ble_adapters_in_use(&s);
            ble = match present.as_slice() {
                [] => {
                    println!("No Bluetooth adapter is present, so not over Bluetooth LE.");
                    None
                }
                [one] => Some(adapter_ref(one)),
                many => {
                    let options: Vec<String> = many
                        .iter()
                        .map(|a| {
                            let mut d = describe_adapter(a);
                            if taken.contains(&a.mac) {
                                d.push_str("  (another enabled bridge uses it for BLE)");
                            }
                            d
                        })
                        .collect();
                    // Best: one no bridge advertises on, and not the
                    // radio's own, which is busy with the radio.
                    let default = many
                        .iter()
                        .position(|a| !taken.contains(&a.mac) && !radio_adapter.contains(&a.mac))
                        .or_else(|| many.iter().position(|a| !taken.contains(&a.mac)))
                        .unwrap_or(0);
                    Some(adapter_ref(
                        many[menu("Which adapter should advertise it?", &options, default).await],
                    ))
                }
            };
        }
        let mut tcp = None;
        if share != 0 || ble.is_none() {
            println!(
                "\nWhere should it listen for apps? A port alone (8001) is this computer only; \
                 wlan0:8001 or eth0:8001 is that network."
            );
            let listen = ask_default("Listen on", "8001").await;
            let mut allow = None;
            if reaches_network(&listen) {
                println!(
                    "Anyone who can reach {listen} can transmit through your radio, under your callsign."
                );
                allow = ask_line(
                    "Limit it to an address or network (e.g. 192.168.1.0/24), or Enter for anyone: ",
                )
                .await;
            }
            tcp = Some((listen, allow));
        }
        let always = menu(
            "When should the bridge connect to the TNC?",
            &[
                "While apps are connected (on demand)".into(),
                "All the time".into(),
            ],
            0,
        )
        .await
            == 1;

        // Step 6.
        let bridge = loop {
            let name = ask_default("A name for the bridge", &default_name).await;
            match self
                .change_result(c, |nd| Command::BridgeCreate {
                    definition: json!({"name": name}),
                    non_disruptive: nd,
                })
                .await
            {
                Ok(r) => break r.id.unwrap_or_default(),
                Err(CtlError::Api(e)) if e.code == ErrorCode::Conflict => {
                    self.report(&CtlError::Api(e));
                }
                Err(e) => return Err(e),
            }
        };
        let mut ports = vec![{
            let mut p = tnc_port;
            if always {
                p["connect"] = json!("always");
            }
            p
        }];
        if let Some(a) = ble {
            ports.push(json!({"id": "phones", "type": "ble-server", "adapter": a}));
        }
        if let Some((listen, allow)) = tcp {
            let mut p = json!({"id": "lan", "type": "tcp-server", "listen": listen});
            if let Some(a) = allow {
                p["allow"] = json!([a]);
            }
            ports.push(p);
        }
        for p in ports {
            self.change_result(c, |nd| Command::PortAdd {
                bridge: bridge.clone(),
                definition: p.clone(),
                non_disruptive: nd,
            })
            .await?;
        }
        match self
            .change_result(c, |nd| Command::BridgeEnable {
                bridge: bridge.clone(),
                reset: false,
                non_disruptive: nd,
            })
            .await
        {
            Ok(_) => {}
            Err(CtlError::Api(e))
                if e.code == ErrorCode::ConfigInvalid && e.message.contains("conflicts with") =>
            {
                self.report(&CtlError::Api(e));
                if !confirm("Switch to it, disabling the bridges in its way?", false).await {
                    println!(
                        "{bridge} is set up but disabled; `ktbctl switch {bridge}` turns it on later."
                    );
                    return Ok(());
                }
                self.change_result(c, |nd| Command::BridgeSwitch {
                    bridge: bridge.clone(),
                    non_disruptive: nd,
                })
                .await?;
            }
            Err(e) => return Err(e),
        }
        println!("{bridge} is enabled.");

        // Step 7.
        self.check_tnc(c, &bridge, &tnc_port_id).await;
        Ok(())
    }

    /// Bring the TNC link up, wait for it and then for a first frame (or
    /// 30 s of quiet), say what happened, and let the link follow its
    /// policy again.
    async fn check_tnc(&self, c: &mut Client, bridge: &str, port: &str) {
        println!("\nConnecting to the TNC to check it...");
        let mut events = match Client::connect(&self.socket).await {
            Ok(mut e) => match e.subscribe(vec![Topic::Frames]).await {
                Ok(()) => Some(e),
                Err(_) => None,
            },
            Err(_) => None,
        };
        let connect = Command::PortConnect {
            bridge: bridge.to_string(),
            port: port.to_string(),
        };
        if let Err(e) = c.call(connect).await {
            self.report(&e);
            return;
        }
        let connected = wait_for_link(c, bridge, port, Duration::from_secs(30)).await;
        match connected {
            Ok(()) => {
                println!("The TNC is connected. Listening for a frame (up to 30 s)...");
                let object = format!("{bridge}/{port}");
                let heard = match events.as_mut() {
                    Some(e) => first_frame(e, &object, Duration::from_secs(30)).await,
                    None => None,
                };
                match heard {
                    Some(summary) => println!("Heard: {summary}\nAll set."),
                    None => println!(
                        "Nothing heard in 30 s, which is normal on a quiet frequency. \
                         `ktbctl monitor` shows frames as they come."
                    ),
                }
            }
            Err(why) => println!(
                "The TNC didn't connect: {why}. `ktbctl status` shows the bridge as it tries."
            ),
        }
        let _ = c
            .call(Command::PortRelease {
                bridge: bridge.to_string(),
                port: port.to_string(),
            })
            .await;
    }

    /// The value to use for `adapter` in config: its id, or after offering
    /// one, its MAC.
    async fn adapter_value(
        &self,
        c: &mut Client,
        a: &AdapterStatus,
        s: &Status,
    ) -> Result<String, CtlError> {
        if let Some(id) = &a.id {
            return Ok(id.clone());
        }
        let ids: Vec<&str> = s.adapters.iter().filter_map(|a| a.id.as_deref()).collect();
        let suggested = suggest_adapter_id(a, &ids);
        let what = a.hardware().unwrap_or_else(|| a.mac.clone());
        let id = ask_line(&format!(
            "Give this adapter ({what}) a short id for the config, like {suggested}, \
             or press Enter to use its MAC: "
        ))
        .await;
        let Some(id) = id else {
            return Ok(a.mac.clone());
        };
        let def = json!({"id": id, "mac": a.mac});
        self.change_result(c, |nd| Command::AdapterCreate {
            definition: def.clone(),
            non_disruptive: nd,
        })
        .await?;
        Ok(id)
    }
}

/// The model for a TNC: the radio they picked, or what was recognized, or
/// generic.
fn model_for(radio: Option<&RadioEntry>, known: Option<&str>, detected: Option<&str>) -> String {
    match (radio, detected) {
        (Some(r), _) => r.model.clone(),
        (None, Some(m)) => {
            println!("That's a {}.", known.unwrap_or(m));
            m.to_string()
        }
        (None, None) => "generic".into(),
    }
}

/// `TH-D75` for `Kenwood TH-D75`: what to call it day to day.
fn short_name(name: &str) -> String {
    name.split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or(name)
        .to_string()
}

/// A numbered menu; returns the index chosen.
async fn menu(question: &str, options: &[String], default: usize) -> usize {
    println!("\n{question}");
    for (i, o) in options.iter().enumerate() {
        println!("  {}) {o}", i + 1);
    }
    loop {
        let answer = ask_line(&format!("Choose 1-{} [{}]: ", options.len(), default + 1)).await;
        match answer.map(|a| a.parse::<usize>()) {
            None => return default,
            Some(Ok(n)) if (1..=options.len()).contains(&n) => return n - 1,
            _ => println!("Pick a number from 1 to {}.", options.len()),
        }
    }
}

/// Ask with a default for an empty answer.
async fn ask_default(question: &str, default: &str) -> String {
    ask_line(&format!("{question} [{default}]: "))
        .await
        .unwrap_or_else(|| default.to_string())
}

/// `hci1  AC:A7:F1:29:9C:7F  USB: TP-Link UB500 Adapter  (id dongle)`.
fn describe_adapter(a: &AdapterStatus) -> String {
    let mut d = format!("{}  {}", a.hci.as_deref().unwrap_or("-"), a.mac);
    if let Some(hw) = a.hardware() {
        d.push_str("  ");
        d.push_str(&hw);
    }
    if let Some(id) = &a.id {
        d.push_str(&format!("  (id {id})"));
    }
    d
}

/// How config refers to an adapter: its id if it has one, else its MAC.
fn adapter_ref(a: &AdapterStatus) -> String {
    a.id.clone().unwrap_or_else(|| a.mac.clone())
}

/// The MACs of adapters an enabled bridge already advertises on.
fn ble_adapters_in_use(s: &Status) -> Vec<String> {
    s.bridges
        .iter()
        .filter(|b| b.enabled)
        .flat_map(|b| &b.ports)
        .filter(|p| p.kind == "ble-server")
        .filter_map(|p| p.settings.get("adapter").and_then(|v| v.as_str()))
        // "dongle (AC:A7:F1:29:9C:7F)" or a bare MAC.
        .filter_map(|a| {
            let mac = a.rsplit('(').next()?.trim_end_matches(')');
            (mac.len() == 17).then(|| mac.to_string())
        })
        .collect()
}

/// `d75` for a TH-D75, `tnc` otherwise; numbered if that's taken.
fn suggest_tnc_id(model: &str, taken: &[&str]) -> String {
    let base = model.strip_prefix("th-").unwrap_or("tnc");
    numbered(base, taken)
}

/// `builtin` for the board's own adapter, `dongle` for a USB one.
fn suggest_adapter_id(a: &AdapterStatus, taken: &[&str]) -> String {
    let base = match a.bus.as_deref() {
        Some("board") => "builtin",
        Some(_) => "dongle",
        // Without sysfs to go by, hci0 is usually the board's own.
        None if a.hci.as_deref() == Some("hci0") => "builtin",
        None => "dongle",
    };
    numbered(base, taken)
}

fn numbered(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|c| !taken.contains(&c.as_str()))
        .expect("some number is free")
}

/// Whether a `listen` value is reachable from other hosts: anything but a
/// bare port or a loopback address.
fn reaches_network(listen: &str) -> bool {
    let Some((host, _)) = listen.rsplit_once(':') else {
        return false;
    };
    !(host.starts_with("127.") || host == "localhost" || host == "[::1]" || host == "lo")
}

/// Add `[tnc.kind]`-style `section` to the one TNC in `toml`.
fn add_section(toml: &str, kind: &str, section: &Value) -> Result<String, CtlError> {
    let mut doc: toml_edit::DocumentMut = toml
        .parse()
        .map_err(|e| CtlError::Protocol(format!("the daemon's TOML: {e}")))?;
    let table = doc
        .iter_mut()
        .find_map(|(_, item)| item.as_array_of_tables_mut())
        .and_then(|a| a.iter_mut().next())
        .ok_or_else(|| CtlError::Protocol("the daemon's TOML has no object".into()))?;
    let mut inline = toml_edit::InlineTable::new();
    // In the order a person would write them, not serde_json's sorted one.
    const ORDER: &[&str] = &["address", "adapter", "path", "baud"];
    let mut fields: Vec<(&String, &Value)> = section.as_object().into_iter().flatten().collect();
    fields.sort_by_key(|(k, _)| ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()));
    for (k, v) in fields {
        let value: toml_edit::Value = match v {
            Value::String(s) => s.as_str().into(),
            Value::Number(n) => n.as_i64().unwrap_or_default().into(),
            other => other.to_string().into(),
        };
        inline.insert(k, value);
    }
    table.insert(kind, toml_edit::value(inline));
    Ok(doc.to_string())
}

/// Poll `status` until the bridge's radio port is connected, or say why
/// not.
async fn wait_for_link(
    c: &mut Client,
    bridge: &str,
    port: &str,
    limit: Duration,
) -> Result<(), String> {
    let start = tokio::time::Instant::now();
    let mut last = String::from("no answer");
    while start.elapsed() < limit {
        if let Ok(s) = c.call_as::<Status>(Command::Status).await
            && let Some(p) = s
                .bridges
                .iter()
                .find(|b| b.id == bridge)
                .and_then(|b| b.ports.iter().find(|p| p.id == port))
        {
            if p.state == "connected" {
                return Ok(());
            }
            last = match &p.detail {
                Some(d) => format!("{} ({d})", p.state),
                None => p.state.clone(),
            };
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(last)
}

/// The first frame from `object`'s TNC, within `limit`.
async fn first_frame(e: &mut Client, object: &str, limit: Duration) -> Option<String> {
    tokio::time::timeout(limit, async {
        loop {
            match e.next_message().await {
                Ok(Some(Message::Event(ev))) => {
                    if let Event::Frame {
                        object: o,
                        dir: crate::control::protocol::Dir::Rx,
                        decoded,
                        ..
                    } = ev.event
                        && o == object
                    {
                        return Some(decoded.summary);
                    }
                }
                Ok(Some(Message::Response(_))) => {}
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_models_for_a_new_tnc() {
        assert_eq!(short_name("Kenwood TH-D75"), "TH-D75");
        assert_eq!(short_name("Mobilinkd"), "Mobilinkd");
        let d75: RadioEntry =
            serde_json::from_value(json!({"name": "Kenwood TH-D75", "model": "th-d75"})).unwrap();
        assert_eq!(model_for(Some(&d75), None, Some("th-d74")), "th-d75");
        assert_eq!(
            model_for(None, Some("Kenwood TH-D74"), Some("th-d74")),
            "th-d74"
        );
        assert_eq!(model_for(None, None, None), "generic");
    }

    #[test]
    fn ids_are_suggested_and_numbered() {
        assert_eq!(suggest_tnc_id("th-d75", &[]), "d75");
        assert_eq!(suggest_tnc_id("th-d75", &["d75"]), "d752");
        assert_eq!(suggest_tnc_id("generic", &["tnc", "tnc2"]), "tnc3");
        let adapter = |hci: &str, bus: Option<&str>| -> AdapterStatus {
            serde_json::from_value(json!({"mac": "AC:A7:F1:29:9C:7F", "hci": hci,
                                          "present": true, "bus": bus}))
            .unwrap()
        };
        assert_eq!(
            suggest_adapter_id(&adapter("hci1", Some("board")), &[]),
            "builtin"
        );
        assert_eq!(
            suggest_adapter_id(&adapter("hci0", Some("usb")), &["dongle"]),
            "dongle2"
        );
        assert_eq!(suggest_adapter_id(&adapter("hci0", None), &[]), "builtin");
    }

    #[test]
    fn listen_values_that_reach_the_network() {
        assert!(!reaches_network("8001"));
        assert!(!reaches_network("127.0.0.1:8001"));
        assert!(!reaches_network("lo:8001"));
        assert!(reaches_network("wlan0:8001"));
        assert!(reaches_network("0.0.0.0:8001"));
        assert!(reaches_network("192.168.1.5:8001"));
    }

    #[test]
    fn a_section_is_added_to_the_tnc() {
        let toml = "[[tnc]]\nid = \"d75\"  # mine\nmodel = \"th-d75\"\nspp = { address = \"40:79:12:E4:95:9D\" }\n";
        let t = add_section(
            toml,
            "serial",
            &json!({"path": "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00", "baud": 9600}),
        )
        .unwrap();
        assert!(t.contains("# mine") && t.contains("spp = "), "{t}");
        assert!(
            t.contains(
                "serial = { path = \"/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00\", baud = 9600 }"
            ),
            "{t}"
        );
    }

    #[test]
    fn ble_adapters_in_use_come_from_enabled_bridges() {
        let s: Status = serde_json::from_value(json!({
            "version": "1", "adapters": [], "tncs": [],
            "bridges": [
              {"id": "ktbr0", "name": "a", "enabled": true,
               "public_name": {"configured": "a", "effective": "a"}, "source": "dynamic", "editable": true,
               "ports": [{"id": "phones", "type": "ble-server", "role": "app", "state": "advertising",
                          "settings": {"adapter": "dongle (AC:A7:F1:29:9C:7F)"}, "sessions": []}]},
              {"id": "ktbr1", "name": "b", "enabled": false,
               "public_name": {"configured": "b", "effective": "b"}, "source": "dynamic", "editable": true,
               "ports": [{"id": "phones", "type": "ble-server", "role": "app", "state": "disabled",
                          "settings": {"adapter": "2C:CF:67:6E:2E:BC"}, "sessions": []}]}
            ]
        }))
        .unwrap();
        assert_eq!(ble_adapters_in_use(&s), ["AC:A7:F1:29:9C:7F"]);
    }
}
