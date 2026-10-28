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

//! The control socket: a Unix stream socket, mode 0660, group from
//! `[control] group`, speaking the control protocol as newline-delimited
//! JSON. For management the daemon serves nothing else; remote access goes
//! through `ktb-gateway`, which is a client of this socket too.
//!
//! Commands go to the daemon one at a time, so they see a consistent
//! state. `subscribe` is handled here: the connection streams events from
//! the hub on the topics asked for until it closes.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::events::Hub;
use super::protocol::{
    ApiError, Command, ErrorCode, Event, EventMessage, Request, Response, Topic, VERSION,
};
use crate::supervisor::{DaemonCmd, Requester};

pub const SOCKET_NAME: &str = "control.sock";
/// The longest request line accepted.
const MAX_LINE: usize = 1 << 20;
/// Lines queued for a connection's writer.
const OUTBOX: usize = 256;

/// Connection numbers, to tell connections apart (request ids are only
/// unique within one).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Bind the socket at `path` (replacing a stale one), give it to `group`,
/// and serve until `shutdown`. Binding errors are returned; the daemon runs
/// on without a control socket rather than not at all.
pub async fn serve(
    path: PathBuf,
    group: String,
    daemon: mpsc::Sender<DaemonCmd>,
    hub: Arc<Hub>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let listener = bind(&path, &group)?;
    tracing::info!(socket = %path.display(), group, "control socket ready");
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            res = listener.accept() => match res {
                Ok((stream, _)) => {
                    tokio::spawn(connection(stream, daemon.clone(), hub.clone(), shutdown.clone()));
                }
                Err(e) => {
                    tracing::warn!("control socket accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            },
        }
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn bind(path: &Path, group: &str) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket left by a daemon that died; the instance lock already
    // guarantees no other daemon is using it.
    match std::fs::symlink_metadata(path) {
        Ok(m) if std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()) => {
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "{} exists and isn't a socket",
                path.display()
            )));
        }
        Err(_) => {}
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    match group_id(group) {
        Some(gid) => {
            let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(std::io::Error::other)?;
            // SAFETY: a NUL-terminated path; -1 leaves the owner as is.
            if unsafe { libc::chown(c.as_ptr(), u32::MAX as libc::uid_t, gid) } != 0 {
                let e = std::io::Error::last_os_error();
                tracing::warn!(
                    group,
                    "can't give the control socket to group {group}: {e}; only its owner can use it"
                );
            }
        }
        None => tracing::warn!(
            group,
            "group {group:?} doesn't exist, so only root can use the control socket; create it or set [control] group"
        ),
    }
    Ok(listener)
}

fn group_id(name: &str) -> Option<libc::gid_t> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: getgrnam returns null or a pointer to static storage, read at
    // once. The daemon's other threads don't call getgrnam.
    let g = unsafe { libc::getgrnam(c.as_ptr()) };
    if g.is_null() {
        None
    } else {
        // SAFETY: non-null, from getgrnam.
        Some(unsafe { (*g).gr_gid })
    }
}

async fn connection(
    stream: UnixStream,
    daemon: mpsc::Sender<DaemonCmd>,
    hub: Arc<Hub>,
    shutdown: CancellationToken,
) {
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let (rd, mut wr) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(OUTBOX);
    let done = shutdown.child_token();
    let writer = {
        let done = done.clone();
        tokio::spawn(async move {
            loop {
                let line = tokio::select! {
                    _ = done.cancelled() => break,
                    line = out_rx.recv() => match line { Some(l) => l, None => break },
                };
                if wr.write_all(line.as_bytes()).await.is_err()
                    || wr.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
            done.cancel();
        })
    };

    let mut reader = BufReader::new(rd);
    let mut buf = Vec::new();
    let mut subscribed: HashSet<Topic> = HashSet::new();
    loop {
        buf.clear();
        let mut limited = (&mut reader).take(MAX_LINE as u64 + 1);
        let n = tokio::select! {
            _ = done.cancelled() => break,
            n = limited.read_until(b'\n', &mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            },
        };
        if n > MAX_LINE {
            let _ = send(
                &out_tx,
                &Response::err(
                    None,
                    ApiError::new(ErrorCode::BadRequest, "request too long"),
                ),
            )
            .await;
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        let response = match parse(&line) {
            Err(resp) => *resp,
            Ok(req) => match req.command {
                Command::Subscribe { topics } => {
                    for t in topics.iter().copied().filter(|t| subscribed.insert(*t)) {
                        tokio::spawn(forward(hub.subscribe(t), out_tx.clone(), done.clone()));
                    }
                    let mut all: Vec<Topic> = subscribed.iter().copied().collect();
                    all.sort_by_key(|t| *t as u8);
                    Response::ok(req.id, json!({ "topics": all }))
                }
                command => {
                    // Handed to the daemon in the order they arrive, then
                    // answered by id as each finishes, so a long request
                    // (a pairing waiting on someone) doesn't hold up the
                    // ones after it.
                    let (tx, rx) = oneshot::channel();
                    let from = Requester {
                        conn,
                        request: req.id,
                        events: out_tx.clone(),
                    };
                    let sent = daemon
                        .send(DaemonCmd::Control {
                            command,
                            reply: tx,
                            from,
                        })
                        .await
                        .is_ok();
                    let out = out_tx.clone();
                    tokio::spawn(async move {
                        let response = match (sent, rx.await) {
                            (true, Ok(Ok(result))) => Response::ok(req.id, result),
                            (true, Ok(Err(e))) => Response::err(Some(req.id), e),
                            _ => Response::err(
                                Some(req.id),
                                ApiError::new(ErrorCode::Internal, "the daemon is shutting down"),
                            ),
                        };
                        let _ = send(&out, &response).await;
                    });
                    continue;
                }
            },
        };
        if send(&out_tx, &response).await.is_err() {
            break;
        }
    }
    done.cancel();
    let _ = writer.await;
}

/// A request, or the error response for a line that isn't one.
fn parse(line: &str) -> Result<Request, Box<Response>> {
    let bad =
        |id, msg: String| Box::new(Response::err(id, ApiError::new(ErrorCode::BadRequest, msg)));
    let value: Value =
        serde_json::from_str(line).map_err(|e| bad(None, format!("not JSON: {e}")))?;
    let id = value.get("id").and_then(Value::as_u64);
    match value.get("v").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        Some(v) => {
            return Err(Box::new(Response::err(
                id,
                ApiError::new(
                    ErrorCode::UnsupportedVersion,
                    format!("protocol version {v} isn't supported; this daemon speaks {VERSION}"),
                ),
            )));
        }
        None => return Err(bad(id, "a request needs \"v\": 1".into())),
    }
    if id.is_none() {
        return Err(bad(None, "a request needs a numeric \"id\"".into()));
    }
    let cmd = value
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !Command::NAMES.contains(&cmd.as_str()) {
        return Err(Box::new(Response::err(
            id,
            ApiError::new(
                ErrorCode::UnknownCommand,
                format!(
                    "no command {cmd:?}; the commands are {}",
                    Command::NAMES.join(", ")
                ),
            ),
        )));
    }
    serde_json::from_value(value).map_err(|e| bad(id, format!("{cmd}: {e}")))
}

async fn send(out: &mpsc::Sender<String>, msg: &impl serde::Serialize) -> Result<(), ()> {
    let line = serde_json::to_string(msg).map_err(|_| ())?;
    out.send(line).await.map_err(|_| ())
}

/// Stream one topic's events to a connection.
async fn forward(
    mut rx: broadcast::Receiver<Event>,
    out: mpsc::Sender<String>,
    done: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            _ = done.cancelled() => return,
            ev = rx.recv() => match ev {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(missed)) => Event::Lagged { missed },
                Err(broadcast::error::RecvError::Closed) => return,
            },
        };
        let msg = EventMessage { v: VERSION, event };
        // A client that doesn't read its events loses them rather than
        // holding up the daemon.
        let Ok(line) = serde_json::to_string(&msg) else {
            continue;
        };
        match out.try_send(line) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(r: &Response) -> Option<ErrorCode> {
        r.error.as_ref().map(|e| e.code)
    }

    #[test]
    fn requests_are_checked_in_order() {
        assert_eq!(
            code(&parse("nope").unwrap_err()),
            Some(ErrorCode::BadRequest)
        );
        let r = parse(r#"{"v":2,"id":1,"cmd":"status"}"#).unwrap_err();
        assert_eq!(code(&r), Some(ErrorCode::UnsupportedVersion));
        assert_eq!(r.id, Some(1));
        assert_eq!(
            code(&parse(r#"{"id":1,"cmd":"status"}"#).unwrap_err()),
            Some(ErrorCode::BadRequest)
        );
        assert_eq!(
            code(&parse(r#"{"v":1,"cmd":"status"}"#).unwrap_err()),
            Some(ErrorCode::BadRequest)
        );
        let r = parse(r#"{"v":1,"id":2,"cmd":"frobnicate"}"#).unwrap_err();
        assert_eq!(code(&r), Some(ErrorCode::UnknownCommand));
        assert!(r.error.unwrap().message.contains("status"));
        let r = parse(r#"{"v":1,"id":3,"cmd":"subscribe","topics":["gossip"]}"#).unwrap_err();
        assert_eq!(code(&r), Some(ErrorCode::BadRequest));
        assert_eq!(
            parse(r#"{"v":1,"id":4,"cmd":"status"}"#).unwrap().command,
            Command::Status
        );
    }
}
