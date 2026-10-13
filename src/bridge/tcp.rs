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

// Used by the Linux-only bridge manager and tests; appears unused elsewhere.
#![allow(dead_code)]

//! The 0.5 TCP KISS client, now a thin wrapper over
//! [`KissConnection`](crate::port::stream::KissConnection). It goes away
//! with the old bridge manager.

use crate::error::Error;
use crate::port::stream::KissConnection;
use tokio::net::TcpStream;

pub struct TcpKissConnection {
    conn: KissConnection<TcpStream>,
}

impl TcpKissConnection {
    pub async fn connect(host: &str, port: u16) -> Result<Self, Error> {
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|e| Error::TcpConnect(format!("{host}:{port}: {e}")))?;
        Ok(Self {
            conn: KissConnection::new(stream),
        })
    }

    /// Read the next complete KISS frame: the raw bytes between FENDs.
    pub async fn read_frame(&mut self) -> Result<Vec<u8>, Error> {
        self.conn.read_frame().await
    }

    /// Send raw KISS frame bytes (already FEND-delimited) to TCP.
    pub async fn write_frame(&mut self, frame_bytes: &[u8]) -> Result<(), Error> {
        self.conn.write_bytes(frame_bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiss::frame::{FEND, KissFrame};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn connect_and_read_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = KissFrame {
                command: 0x00,
                data: vec![0xAA, 0xBB, 0xCC],
            };
            stream.write_all(&frame.encode()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let mut conn = TcpKissConnection::connect("127.0.0.1", addr.port())
            .await
            .unwrap();
        let raw = conn.read_frame().await.unwrap();
        let frame = KissFrame::decode(&raw).unwrap();
        assert_eq!(frame.command, 0x00);
        assert_eq!(frame.data, vec![0xAA, 0xBB, 0xCC]);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_and_write_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let mut conn = TcpKissConnection::connect("127.0.0.1", addr.port())
            .await
            .unwrap();
        let frame = KissFrame {
            command: 0x00,
            data: vec![0x11, 0x22],
        };
        conn.write_frame(&frame.encode()).await.unwrap();
        drop(conn);

        let received = server.await.unwrap();
        assert_eq!(received, vec![FEND, 0x00, 0x11, 0x22, FEND]);
    }

    #[tokio::test]
    async fn read_multiple_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let f1 = KissFrame {
                command: 0x00,
                data: vec![0x01],
            };
            let f2 = KissFrame {
                command: 0x00,
                data: vec![0x02],
            };
            let mut bytes = f1.encode();
            bytes.extend_from_slice(&f2.encode());
            stream.write_all(&bytes).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let mut conn = TcpKissConnection::connect("127.0.0.1", addr.port())
            .await
            .unwrap();
        let r1 = conn.read_frame().await.unwrap();
        let r2 = conn.read_frame().await.unwrap();
        assert_eq!(KissFrame::decode(&r1).unwrap().data, vec![0x01]);
        assert_eq!(KissFrame::decode(&r2).unwrap().data, vec![0x02]);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn detect_closed_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });

        let mut conn = TcpKissConnection::connect("127.0.0.1", addr.port())
            .await
            .unwrap();
        let result = conn.read_frame().await;
        assert!(result.is_err());

        server.await.unwrap();
    }
}
