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

//! `ktbctl` against a real daemon: both binaries as built, a fake TCP TNC,
//! and no terminal, so changes that would disconnect someone are refused
//! unless given `--yes`.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    _dir: tempfile::TempDir,
    socket: PathBuf,
    tnc: TcpListener,
    lan: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn daemon() -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let tnc = TcpListener::bind("127.0.0.1:0").unwrap();
    let lan = free_port();
    let config = dir.path().join("kiss-tnc-bridge.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[[bridge]]
id = "ktbr0"
name = "Test"
  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"
  address = "127.0.0.1:{}"
  connect = "always"
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "127.0.0.1:{lan}"
"#,
            tnc.local_addr().unwrap().port()
        ),
    )
    .unwrap();
    let socket = dir.path().join("run").join("control.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_kiss-tnc-bridge"))
        .arg("-c")
        .arg(&config)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--runtime-dir")
        .arg(dir.path().join("run"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !socket.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no control socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon {
        child,
        _dir: dir,
        socket,
        tnc,
        lan,
    }
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn ktbctl(socket: &Path, args: &[&str]) -> Out {
    let out = Command::new(env!("CARGO_BIN_EXE_ktbctl"))
        .arg("--socket")
        .arg(socket)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// An AX.25 UI frame from N0CALL to APRS, KISS-framed.
fn kiss_ui(text: &str) -> Vec<u8> {
    let mut f = vec![0xC0, 0x00];
    for (call, last) in [("APRS  ", false), ("N0CALL", true)] {
        f.extend(call.bytes().map(|b| b << 1));
        f.push(0x60 | u8::from(last));
    }
    f.extend_from_slice(&[0x03, 0xF0]);
    f.extend_from_slice(text.as_bytes());
    f.push(0xC0);
    f
}

#[test]
fn build_a_bridge_then_enable_it() {
    let d = daemon();
    let s = &d.socket;

    let out = ktbctl(s, &["status"]);
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("ktbr0  Test"), "{}", out.stdout);
    let out = ktbctl(s, &["--json", "status"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["bridges"][0]["id"], "ktbr0");

    let out = ktbctl(s, &["bridge", "add", "--name", "Dyn"]);
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("created ktbr1"), "{}", out.stdout);
    let out = ktbctl(s, &["status", "Dyn"]);
    assert!(
        out.stdout.contains("needs: no TNC port, no app port"),
        "{}",
        out.stdout
    );

    let out = ktbctl(s, &["enable", "Dyn"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("exactly one TNC port"),
        "{}",
        out.stderr
    );

    let tnc = d.tnc.local_addr().unwrap().to_string();
    let lan2 = free_port().to_string();
    for args in [
        vec!["port", "add", "Dyn", "tcp-client", "--address", &tnc],
        vec![
            "port",
            "add",
            "Dyn",
            "tcp-server",
            "--id",
            "lan",
            "--listen",
            &format!("127.0.0.1:{lan2}"),
        ],
    ] {
        let out = ktbctl(s, &args);
        assert!(out.ok, "{args:?}: {}", out.stderr);
    }
    let out = ktbctl(s, &["enable", "Dyn"]);
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("ktbr1 started"), "{}", out.stdout);

    let out = ktbctl(s, &["tnc", "models"]);
    assert!(out.stdout.contains("th-d74, th-d75"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("Kenwood TH-D75 (model = \"th-d75\"): Bluetooth, USB"),
        "{}",
        out.stdout
    );
    let out = ktbctl(s, &["check"]);
    assert!(out.ok && out.stdout.contains("config ok"), "{}", out.stdout);

    // A bad port type, refused by the daemon with its reason.
    let out = ktbctl(s, &["port", "add", "Dyn", "tcp-server", "--tnc", "x"]);
    assert!(!out.ok);
    assert!(out.stderr.contains("tnc"), "{}", out.stderr);
}

#[test]
fn disruptive_changes_need_consent() {
    let d = daemon();
    let s = &d.socket;
    // An app connected to ktbr0 makes disabling it disruptive.
    let app = TcpStream::connect(("127.0.0.1", d.lan)).unwrap();
    let start = Instant::now();
    while !ktbctl(s, &["status"]).stdout.contains("└ ktbr0/lan/") {
        assert!(start.elapsed() < Duration::from_secs(5), "no session");
        std::thread::sleep(Duration::from_millis(50));
    }

    let out = ktbctl(s, &["disable", "ktbr0"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("disconnect session ktbr0/lan/")
            && out.stderr.contains("Run it again with --yes"),
        "{}",
        out.stderr
    );
    let out = ktbctl(s, &["--non-disruptive", "disable", "ktbr0"]);
    assert!(!out.ok && out.stderr.contains("refused"), "{}", out.stderr);
    assert!(ktbctl(s, &["status"]).stdout.contains("ktbr0  Test"));
    assert!(
        ktbctl(s, &["status", "ktbr0"])
            .stdout
            .lines()
            .next()
            .unwrap()
            .contains("enabled")
    );

    let out = ktbctl(s, &["--yes", "disable", "ktbr0"]);
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("ktbr0 stopped"), "{}", out.stdout);
    drop(app);
}

#[test]
fn monitor_prints_frames() {
    let d = daemon();
    let (mut tnc, _) = d.tnc.accept().unwrap();
    let mut mon = Command::new(env!("CARGO_BIN_EXE_ktbctl"))
        .arg("--socket")
        .arg(&d.socket)
        .arg("monitor")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(mon.stdout.take().unwrap()).lines();
    // Keep sending until monitor has subscribed and shows one.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(Ok(l)) = lines.next() {
            let _ = tx.send(l);
        }
    });
    let start = Instant::now();
    let line = loop {
        tnc.write_all(&kiss_ui("hello")).unwrap();
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
            break l;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no frame shown");
    };
    let _ = mon.kill();
    let _ = mon.wait();
    assert!(line.contains("rx ktbr0/tnc  N0CALL>APRS:hello"), "{line}");
}

#[test]
fn no_daemon_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let out = ktbctl(&dir.path().join("control.sock"), &["status"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("Is kiss-tnc-bridge running"),
        "{}",
        out.stderr
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn bluetooth_commands_need_bluez() {
    let d = daemon();
    let out = ktbctl(&d.socket, &["scan", "--seconds", "1"]);
    assert!(!out.ok);
    assert!(out.stderr.contains("BlueZ"), "{}", out.stderr);
}

#[test]
fn setup_needs_a_terminal() {
    let d = daemon();
    let out = ktbctl(&d.socket, &["setup"]);
    assert!(!out.ok);
    assert!(out.stderr.contains("terminal"), "{}", out.stderr);
}

#[test]
fn a_held_link_can_be_released() {
    let d = daemon();
    let s = &d.socket;
    let _tnc = d.tnc.accept().unwrap();
    // connect = "always": held down, it goes idle; released, it reconnects.
    assert!(ktbctl(s, &["disconnect", "ktbr0/tnc"]).ok);
    let start = Instant::now();
    loop {
        let st = ktbctl(s, &["status", "ktbr0"]).stdout;
        if st.contains("held down") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{st}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ktbctl(s, &["release", "ktbr0/tnc"]).ok);
    let _again = d.tnc.accept().unwrap();
    let start = Instant::now();
    loop {
        let st = ktbctl(s, &["status", "ktbr0"]).stdout;
        if st.contains("tnc        tcp-client  connected") && !st.contains("held") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{st}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
