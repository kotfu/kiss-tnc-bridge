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

//! `ktbctl device` and `ktbctl auth`: the commands only a device answers,
//! over its USB serial port.

use serde_json::{Map, Value};

use super::ports::{self, DevicePort};
use super::probe::{self, ANSWER_WAIT};
use super::{DeviceInfo, Found};
use crate::control::protocol::Command;
use crate::ctl::cli::{AuthCmd, DeviceCmd, SettingsCmd};
use crate::ctl::client::{Client, CtlError};
use crate::ctl::render;
use crate::ctl::run::{Ctl, confirm, interactive};

impl Ctl {
    pub(in crate::ctl) async fn device(&self, cmd: DeviceCmd) -> Result<(), CtlError> {
        match cmd {
            DeviceCmd::List => {
                let found = list().await;
                self.print(&found, || render::devices(&found))
            }
            DeviceCmd::Info => {
                let (port, info, _) = self.device_client().await?;
                self.print(&info, || render::device_info(&port, &info))
            }
            DeviceCmd::Flash(a) => self.flash(a).await,
            DeviceCmd::Settings { cmd: None } => {
                let (_, _, mut c) = self.device_client().await?;
                let v = c.call(Command::DeviceSettingsGet).await?;
                self.print(&v, || render::settings(&v))
            }
            DeviceCmd::Settings {
                cmd: Some(SettingsCmd::Set { pairs }),
            } => {
                let settings = key_values(&pairs).map_err(CtlError::Message)?;
                let (_, _, mut c) = self.device_client().await?;
                let v = c.call(Command::DeviceSettingsSet { settings }).await?;
                self.print(&v, || render::settings(&v))
            }
            DeviceCmd::Reboot => {
                self.refuse_if_non_disruptive("restarting the device")?;
                let (port, info, mut c) = self.device_client().await?;
                c.call(Command::DeviceReboot).await?;
                self.done(&format!(
                    "the {} at {} is restarting",
                    super::board_name(&info.board),
                    port.path
                ))
            }
            DeviceCmd::Reset => {
                self.refuse_if_non_disruptive("resetting the device")?;
                let (port, info, mut c) = self.device_client().await?;
                self.ask_first(
                    "Reset clears the device's settings, radio pairing and authorized apps, \
                     then restarts it. Its firmware, key and authorization code stay.",
                )
                .await?;
                c.call(Command::DeviceReset).await?;
                self.done(&format!(
                    "the {} at {} is resetting and restarting",
                    super::board_name(&info.board),
                    port.path
                ))
            }
            DeviceCmd::Identify => {
                let (port, info, mut c) = self.device_client().await?;
                c.call(Command::DeviceIdentify).await?;
                self.done(&format!(
                    "the {} at {} is showing its identify sequence",
                    super::board_name(&info.board),
                    port.path
                ))
            }
        }
    }

    pub(in crate::ctl) async fn auth(&self, cmd: AuthCmd) -> Result<(), CtlError> {
        let (_, _, mut c) = self.device_client().await?;
        match cmd {
            AuthCmd::Info => {
                let v = c.call(Command::AuthInfo).await?;
                self.print(&v, || render::auth_info(&v))
            }
            AuthCmd::Code => {
                let v = c.call(Command::AuthCode).await?;
                self.print(&v, || render::auth_code(&v))
            }
            AuthCmd::RotateCode => {
                self.ask_first(
                    "A new authorization code replaces the old one, which stops working for \
                     authorizing new apps. Apps already authorized keep working.",
                )
                .await?;
                let v = c.call(Command::AuthRotateCode).await?;
                self.print(&v, || render::auth_code(&v))
            }
            AuthCmd::Mode { seconds } => {
                let v = c.call(Command::AuthMode { seconds }).await?;
                self.print(&v, || render::auth_mode(&v))
            }
            AuthCmd::Apps => {
                let v = c.call(Command::AuthApps).await?;
                self.print(&v, || render::auth_apps(&v))
            }
        }
    }

    /// The port this command is for.
    pub(in crate::ctl) fn device_port(&self) -> Result<DevicePort, CtlError> {
        ports::choose(&ports::usb_ports(), self.device.as_deref()).map_err(CtlError::Message)
    }

    /// A client on the device's port, once its firmware has answered
    /// `device.info`, so a board running something else fails at once
    /// instead of leaving a request waiting forever.
    pub(in crate::ctl) async fn device_client(
        &self,
    ) -> Result<(DevicePort, DeviceInfo, Client), CtlError> {
        let port = self.device_port()?;
        let mut serial = probe::open_quiet(&port.path)?;
        let info = probe::device_info(&mut serial, ANSWER_WAIT)
            .await?
            .ok_or_else(|| no_answer(&port))?;
        let client = Client::over_serial(&port.path, serial);
        Ok((port, info, client))
    }

    fn refuse_if_non_disruptive(&self, what: &str) -> Result<(), CtlError> {
        if self.non_disruptive {
            return Err(CtlError::Message(format!(
                "{what} drops its radio link and phones; --non-disruptive refuses it"
            )));
        }
        Ok(())
    }

    /// Asks before something that can't be undone, unless `--yes`.
    async fn ask_first(&self, what: &str) -> Result<(), CtlError> {
        if self.yes {
            return Ok(());
        }
        if !interactive() {
            return Err(CtlError::Message(format!("{what} Add --yes to go ahead.")));
        }
        eprintln!("{what}");
        if confirm("Go ahead?", false).await {
            Ok(())
        } else {
            Err(CtlError::Message("nothing was changed".into()))
        }
    }

    fn done(&self, text: &str) -> Result<(), CtlError> {
        self.print(&serde_json::json!({}), || format!("{text}\n"))
    }
}

/// `KEY=VALUE` pairs as JSON: `true` and `false` as booleans, whole
/// numbers as numbers, anything else (or anything in quotes) as a string.
pub fn key_values(pairs: &[String]) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .filter(|(k, _)| !k.is_empty())
            .ok_or_else(|| format!("{pair:?} isn't KEY=VALUE"))?;
        out.insert(k.to_string(), value(v));
    }
    Ok(out)
}

fn value(v: &str) -> Value {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return Value::String(v[1..v.len() - 1].to_string());
        }
    }
    match v {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => v
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(v.to_string())),
    }
}

/// Every port with a board's USB serial chip, each asked at once.
async fn list() -> Vec<Found> {
    let likely = ports::usb_ports()
        .into_iter()
        .filter(|p| p.looks_like.is_some());
    futures::future::join_all(likely.map(|port| async move {
        match ask(&port).await {
            Ok(info) => Found {
                port,
                info,
                error: None,
            },
            Err(e) => Found {
                error: Some(e.to_string()),
                port,
                info: None,
            },
        }
    }))
    .await
}

async fn ask(port: &DevicePort) -> Result<Option<DeviceInfo>, CtlError> {
    let mut serial = probe::open_quiet(&port.path)?;
    probe::device_info(&mut serial, ANSWER_WAIT).await
}

fn no_answer(port: &DevicePort) -> CtlError {
    CtlError::Message(format!(
        "no answer from {}: it isn't running kiss-tnc-bridge firmware, or hasn't started yet",
        port.path
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_are_typed_from_their_text() {
        let pairs: Vec<String> = [
            "led_normal=dim",
            "cpu_mhz=80",
            "modem_sleep=false",
            "public_name=TH-D75 bridge",
            "public_name2='1234'",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = key_values(&pairs).unwrap();
        assert_eq!(m["led_normal"], json!("dim"));
        assert_eq!(m["cpu_mhz"], json!(80));
        assert_eq!(m["modem_sleep"], json!(false));
        assert_eq!(m["public_name"], json!("TH-D75 bridge"));
        assert_eq!(m["public_name2"], json!("1234"));
        assert!(key_values(&["nothing".into()]).is_err());
        assert!(key_values(&["=5".into()]).is_err());
    }
}
