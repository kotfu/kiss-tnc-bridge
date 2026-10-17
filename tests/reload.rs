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

//! Reloads against the running daemon, over TCP only: the config on disk
//! changes, the daemon is asked to reload, and the test checks what
//! happened to the TNC links and the app sessions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kiss_tnc_bridge::config;
use kiss_tnc_bridge::port::stream::KissConnection;
use kiss_tnc_bridge::reconcile::Action;
use kiss_tnc_bridge::supervisor::{self, DaemonCmd, ReloadError, ReloadReport};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(5);

/// A fake KISS TCP server that counts connections and hands each to the test.
struct FakeTnc {
    port: u16,
    accepts: Arc<AtomicUsize>,
    conns: mpsc::UnboundedReceiver<KissConnection<TcpStream>>,
}

async fn fake_tnc() -> FakeTnc {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let (tx, conns) = mpsc::unbounded_channel();
    let n = accepts.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            n.fetch_add(1, Ordering::SeqCst);
            if tx.send(KissConnection::new(s)).is_err() {
                break;
            }
        }
    });
    FakeTnc {
        port,
        accepts,
        conns,
    }
}

impl FakeTnc {
    async fn conn(&mut self) -> KissConnection<TcpStream> {
        timeout(T, self.conns.recv())
            .await
            .expect("the bridge didn't connect to the TNC")
            .unwrap()
    }
}

/// A free TCP port number for a tcp-server to listen on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Daemon {
    dir: tempfile::TempDir,
    cmds: mpsc::Sender<DaemonCmd>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Daemon {
    async fn start(config_text: &str) -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(main_file(dir.path()), config_text).unwrap();
        let paths = supervisor::Paths {
            main_file: main_file(dir.path()),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
        };
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        let sources = config::read_sources(&paths.main_file, Some(&paths.state_dir)).unwrap();
        let (cfg, _) = config::build(&sources, false).unwrap();
        let (cmds, rx) = mpsc::channel(4);
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let task = tokio::spawn(async move {
            supervisor::run(sources, cfg, paths, None, rx, token)
                .await
                .unwrap();
        });
        Daemon {
            dir,
            cmds,
            shutdown,
            task,
        }
    }

    fn write(&self, config_text: &str) {
        std::fs::write(main_file(self.dir.path()), config_text).unwrap();
    }

    async fn reload(&self, non_disruptive: bool) -> Result<ReloadReport, ReloadError> {
        let (tx, rx) = oneshot::channel();
        self.cmds
            .send(DaemonCmd::Reload {
                non_disruptive,
                reply: Some(tx),
            })
            .await
            .unwrap();
        timeout(T, rx).await.expect("no reload reply").unwrap()
    }

    async fn stop(self) {
        self.shutdown.cancel();
        timeout(T, self.task).await.unwrap().unwrap();
    }
}

fn main_file(dir: &Path) -> PathBuf {
    dir.join(config::load::MAIN_FILE_NAME)
}

async fn client(port: u16) -> KissConnection<TcpStream> {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return KissConnection::new(s);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("nothing listening on {port}");
}

/// The connection is closed by the daemon (kicked, or refused).
async fn closed(c: &mut KissConnection<TcpStream>) -> bool {
    matches!(timeout(T, c.read_frame()).await, Ok(Err(_)))
}

fn bridge(id: &str, name: &str, enabled: bool, tnc: u16, listen: u16, allow: &str) -> String {
    format!(
        r#"
[[bridge]]
id = "{id}"
name = "{name}"
enabled = {enabled}
  [[bridge.port]]
  id = "tnc"
  type = "tcp-client"
  address = "127.0.0.1:{tnc}"
  connect = "always"
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "127.0.0.1:{listen}"
  {allow}
"#
    )
}

#[tokio::test]
async fn an_unchanged_config_touches_nothing() {
    let mut tnc = fake_tnc().await;
    let lan = free_port();
    let text = bridge("ktbr0", "A", true, tnc.port, lan, "");
    let d = Daemon::start(&text).await;
    let mut link = tnc.conn().await;
    let mut app = client(lan).await;
    link.write_frame(&[0x00, 0x01]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x01]);

    // Reformatted and commented, but the same resolved config.
    d.write(&format!("# reformatted\n{}", text.replace("  ", "    ")));
    let report = d.reload(true).await.unwrap();
    assert!(report.plan.is_noop(), "{:#?}", report.plan);

    // No reconnect; the session carries on.
    link.write_frame(&[0x00, 0x02]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x02]);
    assert_eq!(tnc.accepts.load(Ordering::SeqCst), 1);
    d.stop().await;
}

#[tokio::test]
async fn tightening_allow_kicks_who_is_no_longer_allowed() {
    let mut tnc = fake_tnc().await;
    let lan = free_port();
    let text = bridge(
        "ktbr0",
        "A",
        true,
        tnc.port,
        lan,
        "allow = [\"127.0.0.1/32\"]",
    );
    let d = Daemon::start(&text).await;
    let _link = tnc.conn().await;
    let mut app = client(lan).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // non_disruptive refuses, and nothing changes.
    let tight = text.replace("127.0.0.1/32", "10.0.0.0/8");
    d.write(&tight);
    match d.reload(true).await {
        Err(ReloadError::WouldDisrupt(plan)) => {
            assert_eq!(plan.disconnects().len(), 1, "{plan:#?}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        timeout(Duration::from_millis(200), app.read_frame())
            .await
            .is_err()
    );

    // Without it, the client goes, and new ones are refused.
    let report = d.reload(false).await.unwrap();
    assert!(report.plan.disruptive());
    assert!(closed(&mut app).await);
    let mut again = client(lan).await;
    assert!(closed(&mut again).await);
    assert_eq!(
        tnc.accepts.load(Ordering::SeqCst),
        1,
        "the TNC link stays up"
    );
    d.stop().await;
}

#[tokio::test]
async fn a_new_tnc_address_reconnects_and_apps_stay() {
    let mut a = fake_tnc().await;
    let mut b = fake_tnc().await;
    let lan = free_port();
    let text = bridge("ktbr0", "A", true, a.port, lan, "");
    let d = Daemon::start(&text).await;
    let mut link_a = a.conn().await;
    let mut app = client(lan).await;

    d.write(&text.replace(&format!(":{}\"", a.port), &format!(":{}\"", b.port)));
    let report = d.reload(false).await.unwrap();
    let port = &report.plan.bridges[0].ports[0];
    assert_eq!((port.id.as_str(), port.action), ("tnc", Action::Restarted));
    assert!(port.disconnects.is_empty());

    assert!(matches!(timeout(T, link_a.read_frame()).await, Ok(Err(_))));
    let mut link_b = b.conn().await;
    link_b.write_frame(&[0x00, 0x42]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x42]);
    app.write_frame(&[0x00, 0x43]).await.unwrap();
    assert_eq!(link_b.read_frame().await.unwrap(), vec![0x00, 0x43]);
    d.stop().await;
}

#[tokio::test]
async fn switching_bridges_moves_a_listen_socket_in_one_reload() {
    let mut a = fake_tnc().await;
    let mut b = fake_tnc().await;
    let lan = free_port();
    // Both listen on the same socket; only one may be enabled.
    let (a_port, b_port) = (a.port, b.port);
    let text = |a_on: bool| {
        format!(
            "{}{}",
            bridge("ktbr0", "A", a_on, a_port, lan, ""),
            bridge("ktbr1", "B", !a_on, b_port, lan, "")
        )
    };
    let d = Daemon::start(&text(true)).await;
    let _link_a = a.conn().await;
    let mut app = client(lan).await;

    d.write(&text(false));
    let report = d.reload(false).await.unwrap();
    let actions: Vec<_> = report
        .plan
        .bridges
        .iter()
        .map(|b| (b.id.as_str(), b.action))
        .collect();
    assert_eq!(
        actions,
        vec![("ktbr0", Action::Stopped), ("ktbr1", Action::Started)]
    );
    assert!(closed(&mut app).await);

    // The same socket, now served by ktbr1 and its TNC.
    let mut link_b = b.conn().await;
    let mut app = client(lan).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    link_b.write_frame(&[0x00, 0x09]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x09]);
    d.stop().await;
}

#[tokio::test]
async fn an_invalid_config_is_rejected_whole() {
    let mut tnc = fake_tnc().await;
    let lan = free_port();
    let text = bridge("ktbr0", "A", true, tnc.port, lan, "");
    let d = Daemon::start(&text).await;
    let mut link = tnc.conn().await;
    let mut app = client(lan).await;

    d.write(&format!(
        "{text}\n[[bridge]]\nid = \"BAD ID\"\nname = \"x\"\n"
    ));
    match d.reload(false).await {
        Err(ReloadError::Invalid(problems)) => assert!(!problems.is_empty()),
        other => panic!("expected the config to be rejected, got {other:?}"),
    }
    // Everything carries on as before.
    link.write_frame(&[0x00, 0x05]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x05]);
    d.stop().await;
}

#[tokio::test]
async fn live_updates_dont_restart_anything() {
    let mut tnc = fake_tnc().await;
    let lan = free_port();
    let text = bridge("ktbr0", "A", true, tnc.port, lan, "");
    let d = Daemon::start(&text).await;
    let mut link = tnc.conn().await;
    let mut app = client(lan).await;

    // A new name, max_clients raised, a new idle timeout: all live.
    let edited = text
        .replace("name = \"A\"", "name = \"A2\"")
        .replace(
            "connect = \"always\"",
            "connect = \"always\"\n  idle_timeout = \"30s\"",
        )
        .replace("listen = ", "max_clients = 5\n  listen = ");
    d.write(&edited);
    let report = d.reload(true).await.unwrap();
    assert!(!report.plan.is_noop());
    assert!(!report.plan.disruptive(), "{:?}", report.plan.disruptions());
    link.write_frame(&[0x00, 0x06]).await.unwrap();
    assert_eq!(app.read_frame().await.unwrap(), vec![0x00, 0x06]);
    assert_eq!(tnc.accepts.load(Ordering::SeqCst), 1);
    d.stop().await;
}
