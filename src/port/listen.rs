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
//! Binding `tcp-server` ports: a TCP port number on loopback, an
//! `address:port`, or an `interface:port` (Linux `SO_BINDTODEVICE`), plus
//! the peer allow list.

use std::net::{IpAddr, SocketAddr};

use ipnet::IpNet;
use tokio::net::{TcpListener, TcpSocket};

use crate::config::Listen;

#[derive(Debug)]
pub enum BindError {
    /// The interface doesn't exist right now; worth retrying.
    NoInterface(String),
    /// Interface binding needs `SO_BINDTODEVICE`, which only Linux has.
    Unsupported(String),
    Io(std::io::Error),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::NoInterface(name) => write!(f, "interface {name:?} doesn't exist"),
            BindError::Unsupported(msg) => f.write_str(msg),
            BindError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BindError {}

/// Bind and listen. Interface bindings use the wildcard address with the
/// socket tied to the device, so they follow the interface's addresses.
pub async fn bind(listen: &Listen) -> Result<TcpListener, BindError> {
    match listen {
        Listen::Addr(addr) => {
            let socket = socket_for(addr)?;
            socket.bind(*addr).map_err(BindError::Io)?;
            socket.listen(64).map_err(BindError::Io)
        }
        Listen::Interface { name, port } => bind_interface(name, *port).await,
    }
}

fn socket_for(addr: &SocketAddr) -> Result<TcpSocket, BindError> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(BindError::Io)?;
    socket.set_reuseaddr(true).map_err(BindError::Io)?;
    Ok(socket)
}

#[cfg(target_os = "linux")]
async fn bind_interface(name: &str, port: u16) -> Result<TcpListener, BindError> {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let socket = socket_for(&addr)?;
    socket.bind_device(Some(name.as_bytes())).map_err(|e| {
        if e.raw_os_error() == Some(libc::ENODEV) {
            BindError::NoInterface(name.to_string())
        } else {
            BindError::Io(e)
        }
    })?;
    socket.bind(addr).map_err(BindError::Io)?;
    socket.listen(64).map_err(BindError::Io)
}

#[cfg(not(target_os = "linux"))]
async fn bind_interface(name: &str, _port: u16) -> Result<TcpListener, BindError> {
    Err(BindError::Unsupported(format!(
        "binding to interface {name:?} needs Linux (SO_BINDTODEVICE); use an address instead"
    )))
}

/// Whether a peer may connect. An empty list allows any peer that can reach
/// the socket. IPv4 peers of a dual-stack listener arrive as mapped IPv6
/// addresses and are matched as IPv4.
pub fn allowed(allow: &[IpNet], peer: IpAddr) -> bool {
    if allow.is_empty() {
        return true;
    }
    let peer = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
        v4 => v4,
    };
    allow.iter().any(|net| net.contains(&peer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_port_only_and_addresses() {
        let l = bind(&"0".parse().unwrap()).await.unwrap();
        assert!(l.local_addr().unwrap().ip().is_loopback());
        let l = bind(&"127.0.0.1:0".parse().unwrap()).await.unwrap();
        assert_eq!(l.local_addr().unwrap().ip(), IpAddr::from([127, 0, 0, 1]));
        if let Ok(l) = bind(&"[::1]:0".parse().unwrap()).await {
            assert!(l.local_addr().unwrap().is_ipv6());
        }
    }

    #[tokio::test]
    async fn missing_interface_is_distinguishable() {
        let err = bind(&"nosuchif9:0".parse().unwrap()).await.unwrap_err();
        if cfg!(target_os = "linux") {
            assert!(matches!(err, BindError::NoInterface(_)), "{err:?}");
        } else {
            assert!(matches!(err, BindError::Unsupported(_)), "{err:?}");
        }
    }

    #[test]
    fn allow_list() {
        let nets: Vec<IpNet> = vec![
            "192.168.4.0/24".parse().unwrap(),
            "10.0.0.5/32".parse().unwrap(),
        ];
        assert!(allowed(&[], "8.8.8.8".parse().unwrap()));
        assert!(allowed(&nets, "192.168.4.20".parse().unwrap()));
        assert!(allowed(&nets, "10.0.0.5".parse().unwrap()));
        assert!(!allowed(&nets, "10.0.0.6".parse().unwrap()));
        assert!(!allowed(&nets, "192.168.5.1".parse().unwrap()));
        assert!(allowed(&nets, "::ffff:192.168.4.7".parse().unwrap()));
        assert!(!allowed(&nets, "fe80::1".parse().unwrap()));
    }
}
