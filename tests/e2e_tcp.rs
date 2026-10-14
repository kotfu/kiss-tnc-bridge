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

//! End to end over TCP, no Bluetooth: a fake KISS TNC server, a bridge with
//! a tcp-client TNC port and a tcp-server app port, and several clients.
//! This is the data path the daemon runs, minus BlueZ.

use std::net::SocketAddr;
use std::time::Duration;

use kiss_tnc_bridge::bridge::{Bridge, BridgeHandle, TncSettings, TncState};
use kiss_tnc_bridge::config::{AppParams, ConnectPolicy, Listen};
use kiss_tnc_bridge::port::stream::KissConnection;
use kiss_tnc_bridge::port::tcp_client;
use kiss_tnc_bridge::port::tcp_server::{ServerState, TcpLive, TcpServerPort};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(5);

/// A fake TNC: accepts one connection at a time and hands it to the test.
async fn fake_tnc() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

async fn accept_tnc(l: &TcpListener) -> KissConnection<TcpStream> {
    let (s, _) = timeout(T, l.accept())
        .await
        .expect("bridge didn't connect to the TNC")
        .unwrap();
    KissConnection::new(s)
}

struct Setup {
    handle: BridgeHandle,
    server_addr: SocketAddr,
    tnc_listener: TcpListener,
}

async fn setup(app_params: AppParams, max_clients: Option<u32>, allow: Vec<ipnet::IpNet>) -> Setup {
    let (tnc_listener, tnc_port) = fake_tnc().await;
    let connector = tcp_client::connector("127.0.0.1".into(), tnc_port);
    let (bridge, handle) = Bridge::new(
        "ktbr0",
        Box::new(connector),
        TncSettings {
            port_id: "tnc".into(),
            connect: ConnectPolicy::OnDemand,
            idle_timeout: Duration::ZERO,
            app_params,
            exit_kiss_on_disconnect: false,
        },
        tokio_util::sync::CancellationToken::new(),
    );
    tokio::spawn(bridge.run());
    let listen: Listen = "127.0.0.1:0".parse().unwrap();
    let (server, _task) = TcpServerPort {
        bridge_id: "ktbr0".into(),
        port_id: "lan".into(),
        listen,
        live: tokio::sync::watch::channel(TcpLive { max_clients, allow }).1,
        app_tx: handle.app_tx.clone(),
        shutdown: handle.shutdown.clone(),
    }
    .spawn();
    let mut state = server.state.clone();
    let server_addr = timeout(T, async {
        loop {
            if let ServerState::Listening(a) = &*state.borrow() {
                return *a;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("server didn't listen");
    Setup {
        handle,
        server_addr,
        tnc_listener,
    }
}

async fn client(addr: SocketAddr) -> KissConnection<TcpStream> {
    KissConnection::new(TcpStream::connect(addr).await.unwrap())
}

async fn read(c: &mut KissConnection<TcpStream>) -> Vec<u8> {
    timeout(T, c.read_frame()).await.expect("no frame").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_uplink_and_filtering() {
    let s = setup(AppParams::Pass, None, vec![]).await;

    // Two apps connect; the bridge connects to the TNC on the first one.
    let mut a = client(s.server_addr).await;
    let mut tnc = accept_tnc(&s.tnc_listener).await;
    let mut b = client(s.server_addr).await;
    let mut state = s.handle.state.clone();
    timeout(T, async {
        while *state.borrow() != TncState::Connected {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    // TNC -> every app.
    tnc.write_frame(&[0x00, 0x11]).await.unwrap();
    assert_eq!(read(&mut a).await, vec![0x00, 0x11]);
    assert_eq!(read(&mut b).await, vec![0x00, 0x11]);

    // App -> TNC only; the other app hears nothing.
    a.write_frame(&[0x00, 0xAA]).await.unwrap();
    assert_eq!(read(&mut tnc).await, vec![0x00, 0xAA]);
    assert!(
        timeout(Duration::from_millis(200), b.read_frame())
            .await
            .is_err()
    );

    // KISS return from an app never reaches the TNC; the next frame does.
    // Both from one app, so the bridge sees them in order and the drop is
    // counted by the time the frame arrives.
    a.write_bytes(&[0xC0, 0xFF, 0xC0]).await.unwrap();
    a.write_frame(&[0x00, 0xBB]).await.unwrap();
    assert_eq!(read(&mut tnc).await, vec![0x00, 0xBB]);
    assert_eq!(s.handle.counters.dropped(), 1);

    // Concurrent uplink from both apps: 2 x 150 frames, none interleaved
    // (payload bytes stay below 0xC0: frames travel already stuffed).
    let t1 = tokio::spawn(async move {
        for i in 0..150u8 {
            a.write_frame(&[0x00, 0xA0, i, 0xDB, 0xDC, i])
                .await
                .unwrap();
        }
        a
    });
    let t2 = tokio::spawn(async move {
        for i in 0..150u8 {
            b.write_frame(&[0x00, 0xB0, i]).await.unwrap();
        }
        b
    });
    let (mut na, mut nb) = (0u8, 0u8);
    for n in 0..300 {
        let f = timeout(T, tnc.read_frame())
            .await
            .unwrap_or_else(|_| panic!("only {n} of 300 frames arrived"))
            .unwrap();
        match f[1] {
            0xA0 => {
                assert_eq!(f, vec![0x00, 0xA0, na, 0xDB, 0xDC, na]);
                na = na.wrapping_add(1);
            }
            0xB0 => {
                assert_eq!(f, vec![0x00, 0xB0, nb]);
                nb = nb.wrapping_add(1);
            }
            x => panic!("corrupt frame {x:#x}"),
        }
    }
    assert_eq!((na, nb), (150, 150));
    let a = t1.await.unwrap();
    let b = t2.await.unwrap();

    // Last app leaves: the bridge drops the TNC link.
    drop(a);
    drop(b);
    assert!(
        timeout(T, tnc.read_frame())
            .await
            .expect("TNC link should close")
            .is_err()
    );
    timeout(T, async {
        while *state.borrow() != TncState::Idle {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    s.handle.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tnc_restart_is_survived_by_apps() {
    let s = setup(AppParams::Pass, None, vec![]).await;
    let mut a = client(s.server_addr).await;
    let tnc = accept_tnc(&s.tnc_listener).await;
    drop(tnc); // the TNC server dies
    let mut tnc2 = accept_tnc(&s.tnc_listener).await; // ...and the bridge reconnects (1 s backoff)
    tnc2.write_frame(&[0x00, 0x22]).await.unwrap();
    assert_eq!(read(&mut a).await, vec![0x00, 0x22]);
    assert_eq!(s.handle.counters.tnc_connects(), 2);
    s.handle.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_clients_and_allow_list() {
    // max_clients = 1: the second client is dropped at accept.
    let s = setup(AppParams::Pass, Some(1), vec![]).await;
    let mut a = client(s.server_addr).await;
    let mut tnc = accept_tnc(&s.tnc_listener).await;
    tnc.write_frame(&[0x00, 0x01]).await.unwrap();
    assert_eq!(read(&mut a).await, vec![0x00, 0x01]);
    let mut b = client(s.server_addr).await;
    assert!(
        timeout(T, b.read_frame())
            .await
            .expect("rejected client should be closed")
            .is_err()
    );
    tnc.write_frame(&[0x00, 0x02]).await.unwrap();
    assert_eq!(read(&mut a).await, vec![0x00, 0x02]);
    s.handle.shutdown.cancel();

    // An allow list that excludes loopback: every client is refused.
    let s = setup(
        AppParams::Pass,
        None,
        vec!["192.168.4.0/24".parse().unwrap()],
    )
    .await;
    let mut c = client(s.server_addr).await;
    assert!(
        timeout(T, c.read_frame())
            .await
            .expect("disallowed client should be closed")
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(300), s.tnc_listener.accept())
            .await
            .is_err(),
        "no session, so no TNC connect"
    );
    s.handle.shutdown.cancel();
}
