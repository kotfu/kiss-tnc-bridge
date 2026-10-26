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
    dir: tempfile::TempDir,
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
        dir,
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
            .starts_with("ktbr0/lan/127.0.0.1:")
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
            .starts_with("ktbr0/lan/127.0.0.1:")
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
    let main = r.dir.path().join(config::load::MAIN_FILE_NAME);
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

impl Rig {
    fn state_file(&self, name: &str) -> PathBuf {
        self.dir.path().join("state").join(name)
    }
}

fn ok(res: &Value) -> &Value {
    assert_eq!(res["ok"], true, "{res}");
    &res["result"]
}

fn code(res: &Value) -> &str {
    assert_eq!(res["ok"], false, "{res}");
    res["error"]["code"].as_str().unwrap()
}

fn bridge<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
    status["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == id)
}

#[tokio::test]
async fn a_dynamic_bridge_from_creation_to_deletion() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let (tnc_port, lan2) = (r.tnc.local_addr().unwrap().port(), free_port());

    // A new bridge needs only a name, and starts disabled and empty.
    let res = c
        .call(json!({"cmd": "bridge.create", "definition": {"name": "Dyn"}}))
        .await;
    let res = ok(&res);
    assert_eq!(res["id"], "ktbr1");
    assert_eq!(res["applied"], true);
    let text = std::fs::read_to_string(r.state_file("dynamic.toml")).unwrap();
    assert!(text.contains("enabled = false"), "{text}");
    let st = c.call(json!({"cmd": "status"})).await;
    let b = bridge(ok(&st), "ktbr1").unwrap().clone();
    assert_eq!(b["enabled"], false);
    assert_eq!(b["incomplete"], json!(["no TNC port", "no app port"]));

    // It can't be enabled until it's complete.
    let res = c
        .call(json!({"cmd": "bridge.enable", "bridge": "Dyn"}))
        .await;
    assert_eq!(code(&res), "config_invalid");
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exactly one TNC port"),
        "{res}"
    );

    // Ports one at a time, by the bridge's name, then enable it.
    let res = c
        .call(json!({"cmd": "port.add", "bridge": "Dyn", "definition":
            {"type": "tcp-client", "address": format!("127.0.0.1:{tnc_port}")}}))
        .await;
    assert_eq!(ok(&res)["id"], "tcp-client0");
    ok(&c
        .call(json!({"cmd": "port.add", "bridge": "Dyn", "definition":
            {"id": "lan", "type": "tcp-server", "listen": format!("127.0.0.1:{lan2}")}}))
        .await);
    let st = c.call(json!({"cmd": "status"})).await;
    assert!(
        bridge(ok(&st), "ktbr1")
            .unwrap()
            .get("incomplete")
            .is_none()
    );
    let res = c
        .call(json!({"cmd": "bridge.enable", "bridge": "Dyn"}))
        .await;
    let started = ok(&res)["plan"]["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "ktbr1")
        .unwrap()
        .clone();
    assert_eq!(started["action"], "started");

    let st = c.call(json!({"cmd": "status"})).await;
    let b = bridge(ok(&st), "ktbr1").unwrap().clone();
    assert_eq!(
        (b["source"].as_str(), b["editable"].as_bool()),
        (Some("dynamic"), Some(true))
    );
    // Its tcp-server is up: an app can connect.
    TcpStream::connect(("127.0.0.1", lan2)).await.unwrap();

    // A port added to the running bridge, then removed again.
    let lan3 = free_port();
    let res = c
        .call(json!({"cmd": "port.add", "bridge": "Dyn", "definition": {"type": "tcp-server", "listen": format!("127.0.0.1:{lan3}")}}))
        .await;
    assert_eq!(ok(&res)["id"], "tcp-server0");
    TcpStream::connect(("127.0.0.1", lan3)).await.unwrap();
    ok(&c
        .call(json!({"cmd": "port.remove", "bridge": "ktbr1", "port": "tcp-server0"}))
        .await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(("127.0.0.1", lan3)).await.is_err());
    // Removing its last app port would leave an enabled bridge incomplete.
    let res = c
        .call(json!({"cmd": "port.remove", "bridge": "ktbr1", "port": "lan"}))
        .await;
    assert_eq!(code(&res), "config_invalid");
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at least one app port")
    );

    ok(&c
        .call(json!({"cmd": "bridge.disable", "bridge": "ktbr1"}))
        .await);
    let st = c.call(json!({"cmd": "status"})).await;
    assert_eq!(bridge(ok(&st), "ktbr1").unwrap()["enabled"], false);
    ok(&c
        .call(json!({"cmd": "bridge.delete", "bridge": "ktbr1"}))
        .await);
    let st = c.call(json!({"cmd": "status"})).await;
    assert!(bridge(ok(&st), "ktbr1").is_none());
    assert!(
        !std::fs::read_to_string(r.state_file("dynamic.toml"))
            .unwrap()
            .contains("ktbr1")
    );
    r.shutdown.cancel();
}

#[tokio::test]
async fn static_bridges_only_enable_and_disable() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let res = c
        .call(json!({"cmd": "bridge.update", "bridge": "ktbr0", "definition": {"name": "x"}}))
        .await;
    assert_eq!(code(&res), "static_object_readonly");
    let res = c
        .call(json!({"cmd": "config.get", "kind": "bridge", "object": "Test"}))
        .await;
    assert_eq!(code(&res), "static_object_readonly");

    ok(&c
        .call(json!({"cmd": "bridge.disable", "bridge": "Test"}))
        .await);
    let state = std::fs::read_to_string(r.state_file("state.toml")).unwrap();
    assert!(state.contains("enabled = false"), "{state}");
    let st = c.call(json!({"cmd": "status"})).await;
    let b = bridge(ok(&st), "ktbr0").unwrap();
    assert_eq!(
        (b["enabled"].as_bool(), b["enabled_override"].as_bool()),
        (Some(false), Some(false))
    );
    assert_eq!(b["ports"][1]["state"], "disabled");

    ok(&c
        .call(json!({"cmd": "bridge.enable", "bridge": "ktbr0", "reset": true}))
        .await);
    let st = c.call(json!({"cmd": "status"})).await;
    let b = bridge(ok(&st), "ktbr0").unwrap();
    assert_eq!(b["enabled"], true);
    assert!(b.get("enabled_override").is_none());
    r.shutdown.cancel();
}

#[tokio::test]
async fn switch_takes_over_a_listen_socket() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    let tnc_port = r.tnc.local_addr().unwrap().port();
    // A second bridge on ktbr0's socket can't be enabled alongside it...
    let def = json!({
        "name": "Other",
        "port": [
            {"type": "tcp-client", "address": format!("127.0.0.1:{tnc_port}")},
            {"type": "tcp-server", "listen": format!("127.0.0.1:{}", r.lan)}
        ]
    });
    let mut enabled = def.clone();
    enabled["enabled"] = json!(true);
    let res = c
        .call(json!({"cmd": "bridge.create", "definition": enabled}))
        .await;
    assert_eq!(code(&res), "config_invalid");
    // New bridges start disabled.
    ok(&c
        .call(json!({"cmd": "bridge.create", "definition": def}))
        .await);
    let res = c
        .call(json!({"cmd": "bridge.enable", "bridge": "Other"}))
        .await;
    assert_eq!(code(&res), "config_invalid");
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("conflicts with enabled bridge ktbr0")
    );
    // ... but switch moves it over.
    let res = c
        .call(json!({"cmd": "bridge.switch", "bridge": "Other"}))
        .await;
    let actions: Vec<(String, String)> = ok(&res)["plan"]["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["id"].as_str().unwrap().into(),
                b["action"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            ("ktbr0".into(), "stopped".into()),
            ("ktbr1".into(), "started".into())
        ]
    );
    let st = c.call(json!({"cmd": "status"})).await;
    assert_eq!(bridge(ok(&st), "ktbr0").unwrap()["enabled_override"], false);
    assert_eq!(bridge(ok(&st), "ktbr1").unwrap()["enabled"], true);
    r.shutdown.cancel();
}

#[tokio::test]
async fn config_get_and_put_with_revisions() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    ok(&c
        .call(json!({"cmd": "tnc.create", "definition": {"id": "nino", "serial": {"path": "/dev/serial/by-id/nino"}}}))
        .await);
    let got = c
        .call(json!({"cmd": "config.get", "kind": "tnc", "object": "nino"}))
        .await;
    let got = ok(&got).clone();
    let toml = got["toml"].as_str().unwrap();
    assert!(toml.starts_with("[[tnc]]"), "{toml}");

    let edited = toml.replace("/dev/serial/by-id/nino", "/dev/serial/by-id/nino2");
    let put = json!({"cmd": "config.put", "kind": "tnc", "object": "nino", "toml": edited, "revision": got["revision"]});
    ok(&c.call(put.clone()).await);
    // The same revision again is stale now.
    assert_eq!(code(&c.call(put).await), "revision_conflict");

    // The whole file, and a hand edit behind the daemon's back.
    let all = c.call(json!({"cmd": "config.get"})).await;
    assert!(ok(&all)["toml"].as_str().unwrap().contains("nino2"));
    std::fs::write(r.state_file("dynamic.toml"), "# hand edited\n").unwrap();
    let other = json!({"cmd": "tnc.create", "definition": {"id": "other", "serial": {"path": "/dev/serial/by-id/o"}}});
    assert_eq!(code(&c.call(other.clone()).await), "changed_on_disk");
    assert_eq!(
        std::fs::read_to_string(r.state_file("dynamic.toml")).unwrap(),
        "# hand edited\n"
    );
    // A reload takes the hand edit, and changes work again.
    ok(&c.call(json!({"cmd": "reload"})).await);
    ok(&c.call(other).await);
    r.shutdown.cancel();
}

#[tokio::test]
async fn tncs_in_use_stay() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    ok(&c
        .call(json!({"cmd": "tnc.create", "definition": {"id": "nino", "name": "NinoTNC", "serial": {"path": "/dev/serial/by-id/nino"}}}))
        .await);
    ok(&c
        .call(
            json!({"cmd": "bridge.create", "definition": {"name": "Nino", "enabled": false, "port": [
                {"type": "serial", "tnc": "nino"},
                {"type": "tcp-server", "listen": format!("127.0.0.1:{}", free_port())}
            ]}}),
        )
        .await);
    // By name or id, it's in use.
    let res = c.call(json!({"cmd": "tnc.delete", "tnc": "NinoTNC"})).await;
    assert_eq!(code(&res), "in_use");
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bridge ktbr1 port serial0")
    );
    ok(&c
        .call(json!({"cmd": "bridge.delete", "bridge": "Nino"}))
        .await);
    ok(&c.call(json!({"cmd": "tnc.delete", "tnc": "nino"})).await);
    r.shutdown.cancel();
}

#[tokio::test]
async fn links_are_held_and_sessions_kicked() {
    let r = rig().await;
    let mut c = Client::connect(&r.socket).await;
    ok(&c
        .call(json!({"cmd": "port.connect", "bridge": "ktbr0", "port": "tnc"}))
        .await);
    let (tnc, _) = timeout(T, r.tnc.accept()).await.unwrap().unwrap();
    let mut tnc = KissConnection::new(tnc);
    let res = c
        .call(json!({"cmd": "port.connect", "bridge": "ktbr0", "port": "lan"}))
        .await;
    assert_eq!(code(&res), "bad_request");

    let app = TcpStream::connect(("127.0.0.1", r.lan)).await.unwrap();
    let mut app = KissConnection::new(app);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let st = c.call(json!({"cmd": "status"})).await;
    let session = bridge(ok(&st), "ktbr0").unwrap()["ports"][1]["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(&c
        .call(json!({"cmd": "session.kick", "session": session}))
        .await);
    assert!(matches!(timeout(T, app.read_frame()).await, Ok(Err(_))));
    let res = c
        .call(json!({"cmd": "session.kick", "session": "ktbr0/lan/1.2.3.4:5"}))
        .await;
    assert_eq!(code(&res), "not_found");

    ok(&c
        .call(json!({"cmd": "port.disconnect", "bridge": "ktbr0", "port": "tnc"}))
        .await);
    assert!(matches!(timeout(T, tnc.read_frame()).await, Ok(Err(_))));
    let res = c.call(json!({"cmd": "serial.list"})).await;
    assert!(ok(&res).is_array());
    r.shutdown.cancel();
}
