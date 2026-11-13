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

//! `ktbctl device`: the commands that talk to a device on USB serial.

use super::ports::{self, DevicePort};
use super::probe::{self, ANSWER_WAIT};
use super::{DeviceInfo, Found};
use crate::ctl::cli::DeviceCmd;
use crate::ctl::client::CtlError;
use crate::ctl::render;
use crate::ctl::run::Ctl;

impl Ctl {
    pub(in crate::ctl) async fn device(&self, cmd: DeviceCmd) -> Result<(), CtlError> {
        match cmd {
            DeviceCmd::List => {
                let found = list().await;
                self.print(&found, || render::devices(&found))
            }
            DeviceCmd::Info => {
                let port = self.device_port()?;
                let info = ask(&port).await?.ok_or_else(|| no_answer(&port))?;
                self.print(&info, || render::device_info(&port, &info))
            }
            DeviceCmd::Flash(a) => self.flash(a).await,
        }
    }

    /// The port this command is for.
    pub(in crate::ctl) fn device_port(&self) -> Result<DevicePort, CtlError> {
        ports::choose(&ports::usb_ports(), self.device.as_deref()).map_err(CtlError::Message)
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
