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

//! A client for the daemon's control socket: one request at a time, and
//! events for the topics it subscribes to.

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::control::protocol::{
    ApiError, Command, EventMessage, Request, Response, Topic, VERSION,
};

/// Where the daemon's control socket is, unless `--socket` or
/// `KTBCTL_SOCKET` says otherwise.
pub const DEFAULT_SOCKET: &str = "/run/kiss-tnc-bridge/control.sock";

#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    #[error("{0}")]
    Connect(String),
    #[error("the daemon closed the connection")]
    Closed,
    #[error("talking to the daemon: {0}")]
    Io(#[from] io::Error),
    #[error("the daemon sent something unexpected: {0}")]
    Protocol(String),
    /// The daemon refused or failed the request.
    #[error("{}", .0.message)]
    Api(ApiError),
    /// Something to tell the user, from ktbctl itself.
    #[error("{0}")]
    Message(String),
    /// A failure whose details have already been printed.
    #[error("")]
    Reported,
}

/// Something the daemon sent.
pub enum Message {
    Response(Response),
    Event(EventMessage),
}

/// A response's result, or its error.
pub fn result(res: Response) -> Result<Value, CtlError> {
    match (res.ok, res.result, res.error) {
        (true, result, _) => Ok(result.unwrap_or(Value::Null)),
        (false, _, Some(e)) => Err(CtlError::Api(e)),
        (false, _, None) => Err(CtlError::Protocol("an error with no detail".into())),
    }
}

pub struct Client {
    path: PathBuf,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    /// A line partly read when a read was cancelled.
    line: Vec<u8>,
}

impl Client {
    pub async fn connect(path: &Path) -> Result<Client, CtlError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|e| CtlError::Connect(connect_hint(path, &e)))?;
        let (r, w) = stream.into_split();
        Ok(Client {
            path: path.to_path_buf(),
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            line: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Send `command` and return its result, skipping any events that
    /// arrive first.
    pub async fn call(&mut self, command: Command) -> Result<Value, CtlError> {
        let id = self.send(command).await?;
        loop {
            match self.next_message().await?.ok_or(CtlError::Closed)? {
                Message::Response(res) if res.id == Some(id) => return result(res),
                _ => continue,
            }
        }
    }

    /// Send `command` without waiting for its response, and return its id.
    /// The daemon answers requests by id, so several can be in flight.
    pub async fn send(&mut self, command: Command) -> Result<u64, CtlError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&Request {
            v: VERSION,
            id,
            command,
        })
        .map_err(|e| CtlError::Protocol(e.to_string()))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        Ok(id)
    }

    /// The next response or event; `None` when the daemon goes away.
    /// Cancel-safe: a message partly read is finished by the next call.
    pub async fn next_message(&mut self) -> Result<Option<Message>, CtlError> {
        let Some(msg) = self.read_line().await? else {
            return Ok(None);
        };
        let protocol = |e: serde_json::Error| CtlError::Protocol(e.to_string());
        if msg.get("event").is_some() {
            serde_json::from_value(msg)
                .map(|e| Some(Message::Event(e)))
                .map_err(protocol)
        } else {
            serde_json::from_value(msg)
                .map(|r| Some(Message::Response(r)))
                .map_err(protocol)
        }
    }

    /// [`call`](Self::call), with the result read as `T`.
    pub async fn call_as<T: DeserializeOwned>(&mut self, command: Command) -> Result<T, CtlError> {
        let v = self.call(command).await?;
        serde_json::from_value(v).map_err(|e| CtlError::Protocol(e.to_string()))
    }

    pub async fn subscribe(&mut self, topics: Vec<Topic>) -> Result<(), CtlError> {
        self.call(Command::Subscribe { topics }).await.map(|_| ())
    }

    /// The next event on this connection; `None` when the daemon goes away.
    pub async fn next_event(&mut self) -> Result<Option<EventMessage>, CtlError> {
        loop {
            let Some(msg) = self.read_line().await? else {
                return Ok(None);
            };
            if msg.get("event").is_none() {
                continue;
            }
            return serde_json::from_value(msg)
                .map(Some)
                .map_err(|e| CtlError::Protocol(e.to_string()));
        }
    }

    async fn read_line(&mut self) -> Result<Option<Value>, CtlError> {
        // read_until keeps what it has read in `self.line` if it's
        // cancelled (the setup wizard races it against the keyboard).
        if self.reader.read_until(b'\n', &mut self.line).await? == 0 && self.line.is_empty() {
            return Ok(None);
        }
        let line = std::mem::take(&mut self.line);
        serde_json::from_slice(&line)
            .map(Some)
            .map_err(|e| CtlError::Protocol(e.to_string()))
    }
}

/// What to tell someone who can't reach the socket.
fn connect_hint(path: &Path, e: &io::Error) -> String {
    let p = path.display();
    match e.kind() {
        io::ErrorKind::NotFound => format!(
            "can't reach the daemon: {p} doesn't exist. Is kiss-tnc-bridge running? (systemctl status kiss-tnc-bridge)"
        ),
        io::ErrorKind::ConnectionRefused => {
            format!(
                "can't reach the daemon: nothing is listening on {p}. Is kiss-tnc-bridge running?"
            )
        }
        io::ErrorKind::PermissionDenied => format!(
            "can't reach the daemon: permission denied on {p}. Run ktbctl as root, or join the kiss-tnc-bridge group (sudo usermod -aG kiss-tnc-bridge $USER, then log in again)"
        ),
        _ => format!("can't reach the daemon at {p}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failures_say_what_to_do() {
        let p = Path::new("/run/x/control.sock");
        let msg = connect_hint(p, &io::Error::from(io::ErrorKind::NotFound));
        assert!(msg.contains("Is kiss-tnc-bridge running"), "{msg}");
        let msg = connect_hint(p, &io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(msg.contains("kiss-tnc-bridge group"), "{msg}");
    }

    #[tokio::test]
    async fn a_cancelled_read_loses_nothing() {
        let (ours, mut daemon) = UnixStream::pair().unwrap();
        let (r, w) = ours.into_split();
        let mut c = Client {
            path: PathBuf::new(),
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            line: Vec::new(),
        };
        let line = r#"{"v":1,"event":"lagged","missed":3}"#;
        let (head, tail) = line.split_at(12);
        daemon.write_all(head.as_bytes()).await.unwrap();
        // Half a message, then the read is given up on, as the setup
        // wizard does when a key is pressed first.
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_millis(50), c.next_message()).await;
        assert!(cancelled.is_err());
        daemon.write_all(tail.as_bytes()).await.unwrap();
        daemon.write_all(b"\n").await.unwrap();
        match c.next_message().await.unwrap() {
            Some(Message::Event(ev)) => assert_eq!(
                ev.event,
                crate::control::protocol::Event::Lagged { missed: 3 }
            ),
            _ => panic!("expected the lagged event"),
        }
    }
}
