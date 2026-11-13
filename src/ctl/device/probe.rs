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

//! Opening a device's USB serial port without resetting it, and asking
//! the firmware for `device.info` (docs/control-api.md, USB serial).

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;
use tokio_serial::{DataBits, FlowControl, Parity, SerialPort, SerialPortBuilderExt, StopBits};

use super::DeviceInfo;
use crate::control::protocol::{Response, VERSION};
use crate::ctl::client::{CtlError, result};
use crate::port::serial::NoDrain;

/// The firmware's control API runs at this rate.
pub const BAUD: u32 = 115_200;

/// How long to wait for the firmware to answer `device.info`, asking again
/// each second: as long as the web installer waits.
pub const ANSWER_WAIT: Duration = Duration::from_secs(3);

/// A request line longer than this from the device is junk: noise from the
/// ROM bootloader at another baud rate, or other firmware.
const MAX_LINE: usize = 64 * 1024;

/// Opens a device's port at the control API's rate, taken exclusively,
/// without resetting the device.
///
/// RTS and DTR drive the ESP32's EN and IO0 through the boards'
/// auto-reset circuit, which pulls EN low (a reset) while RTS is asserted
/// without DTR. Opening asserts both, which does nothing; dropping DTR
/// first would pass through "RTS alone" and reset the device. So RTS
/// goes first: DTR alone only holds IO0, which matters only at a reset.
pub fn open_quiet(path: &str) -> Result<NoDrain, CtlError> {
    let mut port = tokio_serial::new(path, BAUD)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .exclusive(true)
        .open_native_async()
        .map_err(|e| open_error(path, &e))?;
    port.write_request_to_send(false)
        .and_then(|()| port.write_data_terminal_ready(false))
        .map_err(|e| CtlError::Message(format!("can't set {path}'s control lines: {e}")))?;
    Ok(NoDrain(port))
}

fn open_error(path: &str, e: &tokio_serial::Error) -> CtlError {
    use std::io::ErrorKind as Io;
    use tokio_serial::ErrorKind;
    CtlError::Message(match e.kind {
        ErrorKind::Io(Io::NotFound) | ErrorKind::NoDevice
            if !std::path::Path::new(path).exists() =>
        {
            format!("no serial port {path}: is the device plugged in?")
        }
        // serialport's name for EBUSY: someone else holds it exclusively.
        ErrorKind::NoDevice => format!(
            "{path} is in use by another program: close serial monitors, the web \
             installer and other ktbctl commands that might be using it"
        ),
        ErrorKind::Io(Io::PermissionDenied) if cfg!(target_os = "linux") => format!(
            "no permission to open {path}: add yourself to the dialout group ('uucp' on \
             Arch), then log out and back in"
        ),
        _ => format!("can't open {path}: {e}"),
    })
}

/// Asks for `device.info` and waits up to `wait` for the answer, asking
/// again each second. `None` if nothing answered: the device runs other
/// firmware or none, or hasn't started yet. The port also carries the
/// firmware's log, so lines that don't start with `{` are skipped, as are
/// events and answers to other requests.
pub async fn device_info<S>(port: &mut S, wait: Duration) -> Result<Option<DeviceInfo>, CtlError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    const ID: u64 = 1;
    let request = format!(
        "{}\n",
        json!({"v": VERSION, "id": ID, "cmd": "device.info"})
    );
    let deadline = Instant::now() + wait;
    let mut next_ask = Instant::now();
    let mut line: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        if now >= next_ask {
            port.write_all(request.as_bytes()).await?;
            next_ask = now + Duration::from_secs(1);
        }
        let until = next_ask.min(deadline);
        let n = match tokio::time::timeout_at(until, port.read(&mut buf)).await {
            Err(_) => continue,
            Ok(Ok(0)) => return Err(CtlError::Closed),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
        };
        for &b in &buf[..n] {
            if b != b'\n' {
                if line.len() < MAX_LINE {
                    line.push(b);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&line).trim().to_string();
            line.clear();
            if !text.starts_with('{') {
                continue;
            }
            let Ok(res) = serde_json::from_str::<Response>(&text) else {
                continue; // an event, or not ours
            };
            if res.id != Some(ID) {
                continue;
            }
            let value = result(res)?;
            return serde_json::from_value(value)
                .map(Some)
                .map_err(|e| CtlError::Protocol(format!("device.info: {e}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    const INFO: &str = r#"{"v":1,"id":1,"ok":true,"result":{"board":"atom-lite","firmware":{"version":"1.2.0-dev","partition":"ota_0","state":"valid"},"protocol":1,"capabilities":["spp-client"],"fingerprint":"GYB78TWSJVDCF5WG","bluetooth":{"address":"B8:7B:4D:18:F2:72"},"uptime":416,"heap":{"free":119856,"min":112572,"largest":106496},"last_reset":"power_on","max_message":4096,"errors":[]}}"#;

    #[tokio::test(start_paused = true)]
    async fn the_answer_is_found_among_log_lines_and_events() {
        let (mut ours, device) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(device);
            let mut lines = BufReader::new(r).lines();
            let req = lines.next_line().await.unwrap().unwrap();
            assert_eq!(req, r#"{"cmd":"device.info","id":1,"v":1}"#);
            w.write_all(b"I (1234) radio: connecting\n").await.unwrap();
            w.write_all(b"{\"v\":1,\"event\":\"state\",\"what\":\"tnc\"}\n")
                .await
                .unwrap();
            w.write_all(b"{\"v\":1,\"id\":7,\"ok\":true,\"result\":{}}\n")
                .await
                .unwrap();
            // The answer in two pieces, as a serial port may deliver it.
            let (a, b) = INFO.split_at(40);
            w.write_all(a.as_bytes()).await.unwrap();
            w.write_all(format!("{b}\r\n").as_bytes()).await.unwrap();
        });
        let info = device_info(&mut ours, ANSWER_WAIT).await.unwrap().unwrap();
        fake.await.unwrap();
        assert_eq!(info.board, "atom-lite");
        assert_eq!(info.firmware.version, "1.2.0-dev");
        assert_eq!(info.fingerprint.as_deref(), Some("GYB78TWSJVDCF5WG"));
        assert_eq!(info.heap.as_ref().map(|h| h.free), Some(119_856));
        // Fields this version doesn't know are kept.
        assert_eq!(info.other.get("max_message"), Some(&json!(4096)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_device_is_asked_each_second_then_given_up_on() {
        let (mut ours, device) = tokio::io::duplex(4096);
        let counter = tokio::spawn(async move {
            let mut lines = BufReader::new(device).lines();
            let mut asked = 0;
            while let Ok(Some(_)) = lines.next_line().await {
                asked += 1;
            }
            asked
        });
        let start = Instant::now();
        assert_eq!(device_info(&mut ours, ANSWER_WAIT).await.unwrap(), None);
        assert_eq!(start.elapsed(), ANSWER_WAIT);
        drop(ours);
        assert_eq!(counter.await.unwrap(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn junk_without_newlines_doesnt_grow_without_bound() {
        let (mut ours, mut device) = tokio::io::duplex(1 << 20);
        device.write_all(&vec![0xAAu8; 200_000]).await.unwrap();
        device.write_all(b"\n").await.unwrap();
        device.write_all(INFO.as_bytes()).await.unwrap();
        device.write_all(b"\n").await.unwrap();
        let info = device_info(&mut ours, ANSWER_WAIT).await.unwrap();
        assert!(info.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_an_api_error() {
        let (mut ours, mut device) = tokio::io::duplex(4096);
        device
            .write_all(
                b"{\"v\":1,\"id\":1,\"ok\":false,\"error\":{\"code\":\"unknown_command\",\"message\":\"no command\"}}\n",
            )
            .await
            .unwrap();
        match device_info(&mut ours, ANSWER_WAIT).await {
            Err(CtlError::Api(e)) => assert_eq!(e.message, "no command"),
            other => panic!("{other:?}"),
        }
    }
}
