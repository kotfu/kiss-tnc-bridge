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
//! `tcp-client`: the TNC side of a bridge connects out to a KISS TCP
//! server such as Dire Wolf or Graywolf.

use tokio::net::TcpStream;

use crate::bridge::{BoxLink, FnConnector};
use crate::error::Error;

/// A connector for `host:port`. Each connect is a fresh TCP connection
/// with Nagle off, since KISS frames are small and latency matters.
pub fn connector(host: String, port: u16) -> impl crate::bridge::TncConnector {
    let desc = format!("{host}:{port}");
    FnConnector::new(desc, move || {
        let host = host.clone();
        async move {
            let stream = TcpStream::connect((host.as_str(), port))
                .await
                .map_err(|e| Error::TcpConnect(format!("{host}:{port}: {e}")))?;
            let _ = stream.set_nodelay(true);
            Ok(Box::new(stream) as BoxLink)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::TncConnector;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn connects_and_reports_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"hi").await.unwrap();
        });
        let c = connector("127.0.0.1".into(), port);
        assert_eq!(c.describe(), format!("127.0.0.1:{port}"));
        let mut link = c.connect().await.unwrap().link;
        let mut buf = [0u8; 2];
        tokio::io::AsyncReadExt::read_exact(&mut link, &mut buf)
            .await
            .unwrap();
        assert_eq!(&buf, b"hi");
        server.await.unwrap();

        // Nothing listening: a clear error, not a hang.
        let c = connector("127.0.0.1".into(), 1);
        let err = c.connect().await.err().expect("should fail");
        assert!(err.to_string().contains("127.0.0.1:1"), "{err}");
    }
}
