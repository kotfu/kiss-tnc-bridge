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

//! The daemon binary stops on SIGTERM, even when nobody is reading its
//! log any more. A dead stderr once made the signal task panic while
//! logging, and the daemon ignored every signal after that.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CONFIG: &str = r#"
[[bridge]]
id = "ktbr0"
name = "Signal test"
  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:1"
  [[bridge.port]]
  type = "tcp-server"
  listen = "0"
"#;

fn wait_exit(child: &mut std::process::Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn sigterm_stops_the_daemon_with_a_dead_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("kiss-tnc-bridge.toml");
    std::fs::write(&config, CONFIG).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kiss-tnc-bridge"))
        .arg("-c")
        .arg(&config)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--runtime-dir")
        .arg(dir.path().join("run"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Let it start and log a bit, then close the only reader of its log.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the daemon exited early"
    );
    drop(child.stderr.take());

    // SAFETY: signalling our own child by pid.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0);
    match wait_exit(&mut child, Duration::from_secs(10)) {
        Some(status) => assert!(status.success(), "{status}"),
        None => {
            let _ = child.kill();
            panic!("the daemon ignored SIGTERM");
        }
    }
}
