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

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;
use tokio_serial::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortBuilderExt, StopBits,
};

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
    // Answers to requests an earlier connection sent, still in the
    // kernel's buffer, aren't for this one.
    let _ = port.clear(ClearBuffer::Input);
    Ok(NoDrain(port))
}

/// A request id no other request has used: the device may still answer
/// requests sent earlier, by this process or another, after they were
/// given up on (a firmware starting answers each `device.info` it was
/// sent while it started). They start from the clock, so another ktbctl's
/// don't collide either.
pub fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let _ = NEXT.compare_exchange(
        0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_millis() as u64 % 1_000_000_000 * 1000 + 1),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
    NEXT.fetch_add(1, Ordering::Relaxed)
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
/// firmware or none, or hasn't started yet.
pub async fn device_info<S>(port: &mut S, wait: Duration) -> Result<Option<DeviceInfo>, CtlError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(value) = request(port, "device.info", wait, true).await? else {
        return Ok(None);
    };
    serde_json::from_value(value)
        .map(Some)
        .map_err(|e| CtlError::Protocol(format!("device.info: {e}")))
}

/// Sends a request with no parameters and waits up to `wait` for its
/// result, sending it again each second if `again`. `None` if nothing
/// answered. The port also carries the firmware's log, so lines that don't
/// start with `{` are skipped, as are events and answers to other requests.
pub async fn request<S>(
    port: &mut S,
    cmd: &str,
    wait: Duration,
    again: bool,
) -> Result<Option<Value>, CtlError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let id = next_id();
    let request = format!("{}\n", json!({"v": VERSION, "id": id, "cmd": cmd}));
    let deadline = Instant::now() + wait;
    let mut next_ask = Some(Instant::now());
    let mut line: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        if next_ask.is_some_and(|t| now >= t) {
            port.write_all(request.as_bytes()).await?;
            next_ask = again.then(|| now + Duration::from_secs(1));
        }
        let until = next_ask.map_or(deadline, |t| t.min(deadline));
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
            if res.id != Some(id) {
                continue;
            }
            return result(res).map(Some);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

    const INFO: &str = r#"{"v":1,"id":ID,"ok":true,"result":{"board":"atom-lite","firmware":{"version":"1.2.0-dev","partition":"ota_0","state":"valid"},"protocol":1,"capabilities":["spp-client"],"fingerprint":"GYB78TWSJVDCF5WG","bluetooth":{"address":"B8:7B:4D:18:F2:72"},"uptime":416,"heap":{"free":119856,"min":112572,"largest":106496},"last_reset":"power_on","max_message":4096,"errors":[]}}"#;

    /// A fake device: reads one request, and returns its id and `cmd`.
    async fn read_request(
        lines: &mut tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    ) -> (u64, String) {
        let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(req["v"], 1);
        (
            req["id"].as_u64().unwrap(),
            req["cmd"].as_str().unwrap().into(),
        )
    }

    fn fake(
        device: DuplexStream,
    ) -> (
        tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        WriteHalf<DuplexStream>,
    ) {
        let (r, w) = tokio::io::split(device);
        (BufReader::new(r).lines(), w)
    }

    #[tokio::test(start_paused = true)]
    async fn the_answer_is_found_among_log_lines_events_and_stale_answers() {
        let (mut ours, device) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            let (mut lines, mut w) = fake(device);
            let (id, cmd) = read_request(&mut lines).await;
            assert_eq!(cmd, "device.info");
            w.write_all(b"I (1234) radio: connecting\n").await.unwrap();
            w.write_all(b"{\"v\":1,\"event\":\"state\",\"what\":\"tnc\"}\n")
                .await
                .unwrap();
            // An answer to a request sent before, by another connection.
            let stale = INFO
                .replace("ID", &(id - 1).to_string())
                .replace("atom-lite", "core2-v1.3");
            w.write_all(format!("{stale}\n").as_bytes()).await.unwrap();
            // The answer in two pieces, as a serial port may deliver it.
            let answer = INFO.replace("ID", &id.to_string());
            let (a, b) = answer.split_at(40);
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
            let mut ids = Vec::new();
            while let Ok(Some(l)) = lines.next_line().await {
                ids.push(
                    serde_json::from_str::<Value>(&l).unwrap()["id"]
                        .as_u64()
                        .unwrap(),
                );
            }
            ids
        });
        let start = Instant::now();
        assert_eq!(device_info(&mut ours, ANSWER_WAIT).await.unwrap(), None);
        assert_eq!(start.elapsed(), ANSWER_WAIT);
        drop(ours);
        let ids = counter.await.unwrap();
        assert_eq!(ids.len(), 3);
        // Asking again is the same request.
        assert!(ids.iter().all(|i| *i == ids[0]));
    }

    #[tokio::test(start_paused = true)]
    async fn each_request_has_its_own_id() {
        assert_ne!(next_id(), next_id());
    }

    #[tokio::test(start_paused = true)]
    async fn junk_without_newlines_doesnt_grow_without_bound() {
        let (mut ours, device) = tokio::io::duplex(1 << 20);
        let fake = tokio::spawn(async move {
            let (mut lines, mut w) = fake(device);
            let (id, _) = read_request(&mut lines).await;
            w.write_all(&vec![0xAAu8; 200_000]).await.unwrap();
            w.write_all(b"\n").await.unwrap();
            let answer = INFO.replace("ID", &id.to_string());
            w.write_all(format!("{answer}\n").as_bytes()).await.unwrap();
            lines
        });
        let info = device_info(&mut ours, ANSWER_WAIT).await.unwrap();
        assert!(info.is_some());
        drop(fake.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_an_api_error() {
        let (mut ours, device) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            let (mut lines, mut w) = fake(device);
            let (id, cmd) = read_request(&mut lines).await;
            assert_eq!(cmd, "auth.code");
            let refusal = format!(
                "{{\"v\":1,\"id\":{id},\"ok\":false,\"error\":{{\"code\":\"unknown_command\",\"message\":\"no command\"}}}}\n"
            );
            w.write_all(refusal.as_bytes()).await.unwrap();
            lines
        });
        match request(&mut ours, "auth.code", ANSWER_WAIT, false).await {
            Err(CtlError::Api(e)) => assert_eq!(e.message, "no command"),
            other => panic!("{other:?}"),
        }
        drop(fake.await.unwrap());
    }
}
