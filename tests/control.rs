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

//! The control socket, end to end: a running daemon (TCP only), its
//! control socket, and a client speaking newline-delimited JSON to it.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use kiss_tnc_bridge::config;
use kiss_tnc_bridge::control::events::Hub;
use kiss_tnc_bridge::control::server;
use kiss_tnc_bridge::port::stream::KissConnection;
use kiss_tnc_bridge::supervisor;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(5);

struct Rig {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    tnc: TcpListener,
    lan: u16,
    shutdown: CancellationToken,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn rig() -> Rig {
    let tnc = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tnc_port = tnc.local_addr().unwrap().port();
    let lan = free_port();
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join(config::load::MAIN_FILE_NAME);
    std::fs::write(
        &main,
        format!(
            r#"
[[bridge]]
id = "ktbr0"
name = "Test"
  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"
  address = "127.0.0.1:{tnc_port}"
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "127.0.0.1:{lan}"
"#
        ),
    )
    .unwrap();
    let paths = supervisor::Paths {
        main_file: main,
        state_dir: dir.path().join("state"),
        runtime_dir: dir.path().join("run"),
    };
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let socket = paths.runtime_dir.join(server::SOCKET_NAME);
    let sources = config::read_sources(&paths.main_file, Some(&paths.state_dir)).unwrap();
    let (cfg, _) = config::build(&sources, false).unwrap();
    let shutdown = CancellationToken::new();
    let hub = Hub::new(shutdown.clone());
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    tokio::spawn(server::serve(
        socket.clone(),
        "nosuchgroup-ktb-test".into(),
        cmd_tx,
        hub.clone(),
        shutdown.clone(),
    ));
    let token = shutdown.clone();
    tokio::spawn(async move {
        supervisor::run(sources, cfg, paths, None, hub, cmd_rx, token)
            .await
            .unwrap();
    });
    Rig {
        _dir: dir,
        socket,
        tnc,
        lan,
        shutdown,
    }
}

struct Client {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    next: u64,
}

impl Client {
    async fn connect(path: &PathBuf) -> Client {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(path).await {
                let (rd, wr) = s.into_split();
                return Client {
                    rd: BufReader::new(rd),
                    wr,
                    next: 1,
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no control socket at {}", path.display());
    }

    async fn send_raw(&mut self, line: &str) {
        self.wr.write_all(line.as_bytes()).await.unwrap();
        self.wr.write_all(b"\n").await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        timeout(T, self.rd.read_line(&mut line))
            .await
            .expect("no message")
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Send a request and return its response, skipping events.
    async fn call(&mut self, mut req: Value) -> Value {
        let id = self.next;
        self.next += 1;
        req["v"] = json!(1);
        req["id"] = json!(id);
        self.send_raw(&req.to_string()).await;
        loop {
            let msg = self.recv().await;
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
        }
    }

    /// The next event matching `want`.
    async fn event(&mut self, want: impl Fn(&Value) -> bool) -> Value {
        loop {
            let msg = self.recv().await;
            if msg.get("event").is_some() && want(&msg) {
                return msg;
            }
        }
    }
}

/// A UI frame from N0CALL to APRS with `text`, as the bridge forwards it.
fn ui(text: &str) -> Vec<u8> {
    let mut data = vec![0x00];
    for (call, last) in [("APRS  ", false), ("N0CALL", true)] {
        data.extend(call.bytes().map(|b| b << 1));
        data.push(0x60 | u8::from(last));
    }
    data.extend_from_slice(&[0x03, 0xF0]);
    data.extend_from_slice(text.as_bytes());
    data
}

#[tokio::test]
async fn status_reports_bridges_ports_and_sessions() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let st = c.call(json!({"cmd": "status"})).await;
    assert_eq!(st["ok"], json!(true), "{st}");
    let b = &st["result"]["bridges"][0];
    assert_eq!(b["id"], "ktbr0");
    assert_eq!(b["editable"], false);
    assert_eq!(
        b["public_name"],
        json!({"configured": "Test", "effective": "Test"})
    );
    assert_eq!(b["ports"][0]["state"], "idle");
    assert_eq!(b["ports"][1]["type"], "tcp-server");

    // An app arrives: a session, and the TNC connects on demand.
    let _app = TcpStream::connect(("127.0.0.1", r.lan)).await.unwrap();
    let (_tnc, _) = timeout(T, r.tnc.accept()).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let st = c.call(json!({"cmd": "status"})).await;
    let b = &st["result"]["bridges"][0];
    assert_eq!(b["ports"][0]["state"], "connected", "{b}");
    assert!(b["ports"][0]["since"].as_str().unwrap().ends_with('Z'));
    assert_eq!(b["ports"][1]["state"], "listening");
    let sessions = b["ports"][1]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0]["id"]
            .as_str()
            .unwrap()
            .starts_with("lan/127.0.0.1:")
    );
    assert_eq!(b["counters"]["tnc_connects"], 1);
    r.shutdown.cancel();
}

#[tokio::test]
async fn events_follow_state_frames_and_activity() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let sub = c
        .call(json!({"cmd": "subscribe", "topics": ["state", "frames", "activity"]}))
        .await;
    assert_eq!(
        sub["result"]["topics"],
        json!(["state", "activity", "frames"])
    );

    let app = TcpStream::connect(("127.0.0.1", r.lan)).await.unwrap();
    let mut app = KissConnection::new(app);
    let ev = c
        .event(|e| e["event"] == "state" && e["what"] == "session")
        .await;
    assert_eq!(ev["state"], "opened");
    let (tnc, _) = timeout(T, r.tnc.accept()).await.unwrap().unwrap();
    let mut tnc = KissConnection::new(tnc);
    let ev = c
        .event(|e| e["event"] == "state" && e["what"] == "tnc" && e["state"] == "connected")
        .await;
    assert_eq!(ev["object"], "ktbr0/tnc");

    // From the TNC: a frame event, decoded, and activity on the TNC port.
    tnc.write_frame(&ui("hello")).await.unwrap();
    let ev = c.event(|e| e["event"] == "frame").await;
    assert_eq!(ev["dir"], "rx");
    assert_eq!(ev["decoded"]["summary"], "N0CALL>APRS:hello");
    let ev = c
        .event(|e| e["event"] == "activity" && e["object"] == "ktbr0/tnc")
        .await;
    assert_eq!(ev["dir"], "rx");
    assert_eq!(app.read_frame().await.unwrap(), ui("hello"));

    // From the app: a tx frame event that names the session.
    app.write_frame(&ui("back")).await.unwrap();
    let ev = c.event(|e| e["event"] == "frame" && e["dir"] == "tx").await;
    assert!(
        ev["session"]
            .as_str()
            .unwrap()
            .starts_with("lan/127.0.0.1:")
    );
    assert_eq!(tnc.read_frame().await.unwrap(), ui("back"));
    r.shutdown.cancel();
}

#[tokio::test]
async fn config_check_and_reload() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;

    // A candidate dynamic.toml adding a bridge: valid, and it would start.
    let candidate = format!(
        "[[bridge]]\nid = \"ktbr1\"\nname = \"More\"\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"127.0.0.1:1\"\n[[bridge.port]]\ntype = \"tcp-server\"\nlisten = \"127.0.0.1:{}\"\n",
        free_port()
    );
    let res = c
        .call(json!({"cmd": "config.check", "dynamic": candidate}))
        .await;
    let check = &res["result"];
    assert_eq!(check["valid"], true, "{res}");
    let bridges = check["plan"]["bridges"].as_array().unwrap();
    assert!(
        bridges
            .iter()
            .any(|b| b["id"] == "ktbr1" && b["action"] == "started")
    );
    assert_eq!(check["disruptive"], false);

    // An invalid one: the errors, not an API error.
    let res = c
        .call(json!({"cmd": "config.check", "dynamic": "[[bridge]]\nid = \"BAD\"\nname = \"x\"\n"}))
        .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["result"]["valid"], false);
    assert!(!res["result"]["errors"].as_array().unwrap().is_empty());

    // Nothing changed on disk: a reload is a no-op.
    let res = c
        .call(json!({"cmd": "reload", "non_disruptive": true}))
        .await;
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["result"]["applied"], true);
    assert_eq!(res["result"]["disruptive"], false);
    r.shutdown.cancel();
}

#[tokio::test]
async fn a_disruptive_reload_is_refused_with_its_plan() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let _app = TcpStream::connect(("127.0.0.1", r.lan)).await.unwrap();
    let (_tnc, _) = timeout(T, r.tnc.accept()).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Disable the bridge on disk; the app would be disconnected.
    let main = r
        .socket
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(config::load::MAIN_FILE_NAME);
    let text = std::fs::read_to_string(&main).unwrap();
    std::fs::write(
        &main,
        text.replace("name = \"Test\"", "name = \"Test\"\nenabled = false"),
    )
    .unwrap();
    let res = c
        .call(json!({"cmd": "reload", "non_disruptive": true}))
        .await;
    assert_eq!(res["ok"], false, "{res}");
    assert_eq!(res["error"]["code"], "reload_would_disrupt");
    assert_eq!(res["error"]["data"]["applied"], false);
    assert!(
        !res["error"]["data"]["disruptions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Still running.
    let st = c.call(json!({"cmd": "status"})).await;
    assert_eq!(st["result"]["bridges"][0]["ports"][0]["state"], "connected");
    r.shutdown.cancel();
}

#[tokio::test]
async fn bad_requests_get_error_codes() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    c.send_raw("this isn't json").await;
    let res = c.recv().await;
    assert_eq!(res["error"]["code"], "bad_request");
    assert_eq!(res["id"], Value::Null);
    let res = c.call(json!({"cmd": "frobnicate"})).await;
    assert_eq!(res["error"]["code"], "unknown_command");
    c.send_raw(r#"{"v":2,"id":99,"cmd":"status"}"#).await;
    let res = c.recv().await;
    assert_eq!(res["error"]["code"], "unsupported_version");
    assert_eq!(res["id"], 99);
    // The connection still works.
    let res = c.call(json!({"cmd": "status"})).await;
    assert_eq!(res["ok"], true);
    r.shutdown.cancel();
}
