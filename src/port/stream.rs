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

//! KISS framing over any async byte stream: TCP, RFCOMM, a serial port, or
//! a `tokio::io::duplex` pair in tests.
//!
//! Frames are passed around as the raw bytes between the FEND delimiters,
//! still byte-stuffed: that's what the bridge forwards, so nothing is
//! decoded and re-encoded on the way through.

use std::collections::VecDeque;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::error::Error;
use crate::kiss::buffer::KissReassemblyBuffer;
use crate::kiss::frame::FEND;

/// Default reassembly limit: a frame longer than this is a protocol error.
pub const MAX_FRAME_BYTES: usize = 8192;

/// Reads KISS frames from an async byte stream.
pub struct KissReader<R> {
    read: R,
    reassembly: KissReassemblyBuffer,
    pending: VecDeque<Vec<u8>>,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> KissReader<R> {
    pub fn new(read: R) -> Self {
        Self::with_max_frame(read, MAX_FRAME_BYTES)
    }

    pub fn with_max_frame(read: R, max_frame: usize) -> Self {
        Self {
            read,
            reassembly: KissReassemblyBuffer::new(max_frame),
            pending: VecDeque::new(),
            buf: vec![0u8; 2048],
        }
    }

    /// The next complete frame: the raw bytes between FENDs, still stuffed.
    /// Returns `Error::LinkClosed` when the peer closes the stream.
    pub async fn read_frame(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(frame);
            }
            let n = self.read.read(&mut self.buf).await?;
            if n == 0 {
                return Err(Error::LinkClosed);
            }
            self.reassembly.push(&self.buf[..n])?;
            self.pending.extend(self.reassembly.drain_frames());
        }
    }

    pub fn into_inner(self) -> R {
        self.read
    }
}

/// Writes KISS frames to an async byte stream.
pub struct KissWriter<W> {
    write: W,
}

impl<W: AsyncWrite + Unpin> KissWriter<W> {
    pub fn new(write: W) -> Self {
        Self { write }
    }

    /// Send one frame given as the raw bytes between FENDs; the delimiters
    /// are added here. Everything goes out in one write so frames from
    /// different sources can't interleave on the wire.
    pub async fn write_frame(&mut self, raw: &[u8]) -> Result<(), Error> {
        let mut wire = Vec::with_capacity(raw.len() + 2);
        wire.push(FEND);
        wire.extend_from_slice(raw);
        wire.push(FEND);
        self.write.write_all(&wire).await?;
        self.write.flush().await?;
        Ok(())
    }

    /// Send bytes exactly as given (already delimited, or a KISS command
    /// sequence such as `C0 FF C0`).
    pub async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.write.write_all(bytes).await?;
        self.write.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.write.shutdown().await?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.write
    }
}

/// A KISS connection over one bidirectional stream.
pub struct KissConnection<S> {
    reader: KissReader<ReadHalf<S>>,
    writer: KissWriter<WriteHalf<S>>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> KissConnection<S> {
    pub fn new(stream: S) -> Self {
        let (rd, wr) = tokio::io::split(stream);
        Self {
            reader: KissReader::new(rd),
            writer: KissWriter::new(wr),
        }
    }

    pub async fn read_frame(&mut self) -> Result<Vec<u8>, Error> {
        self.reader.read_frame().await
    }

    pub async fn write_frame(&mut self, raw: &[u8]) -> Result<(), Error> {
        self.writer.write_frame(raw).await
    }

    pub async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.writer.write_bytes(bytes).await
    }

    /// Separate halves, so one task can read while another writes.
    pub fn into_split(self) -> (KissReader<ReadHalf<S>>, KissWriter<WriteHalf<S>>) {
        (self.reader, self.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiss::frame::KissFrame;
    use tokio::io::duplex;

    fn frame(data: &[u8]) -> Vec<u8> {
        let mut raw = vec![0x00];
        raw.extend_from_slice(data);
        raw
    }

    #[tokio::test]
    async fn read_one_frame() {
        let (ours, mut theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        theirs
            .write_all(&[FEND, 0x00, 0xAA, 0xBB, FEND])
            .await
            .unwrap();
        assert_eq!(conn.read_frame().await.unwrap(), frame(&[0xAA, 0xBB]));
    }

    #[tokio::test]
    async fn read_frames_split_and_batched() {
        let (ours, mut theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        // Two frames in one write, then one frame over two writes.
        theirs
            .write_all(&[FEND, 0x00, 0x01, FEND, FEND, 0x00, 0x02, FEND, FEND, 0x00])
            .await
            .unwrap();
        assert_eq!(conn.read_frame().await.unwrap(), frame(&[0x01]));
        assert_eq!(conn.read_frame().await.unwrap(), frame(&[0x02]));
        theirs.write_all(&[0x03, FEND]).await.unwrap();
        assert_eq!(conn.read_frame().await.unwrap(), frame(&[0x03]));
    }

    #[tokio::test]
    async fn stuffed_bytes_pass_through_untouched() {
        let (ours, mut theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        let f = KissFrame {
            command: 0x00,
            data: vec![FEND, 0x42, 0xDB],
        };
        theirs.write_all(&f.encode()).await.unwrap();
        let raw = conn.read_frame().await.unwrap();
        assert_eq!(raw, vec![0x00, 0xDB, 0xDC, 0x42, 0xDB, 0xDD]);
        assert_eq!(KissFrame::decode(&raw).unwrap(), f);
    }

    #[tokio::test]
    async fn write_frame_adds_delimiters() {
        let (ours, mut theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        conn.write_frame(&[0x00, 0x11, 0x22]).await.unwrap();
        let mut got = vec![0u8; 5];
        theirs.read_exact(&mut got).await.unwrap();
        assert_eq!(got, vec![FEND, 0x00, 0x11, 0x22, FEND]);
    }

    #[tokio::test]
    async fn write_bytes_is_verbatim() {
        let (ours, mut theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        conn.write_bytes(&[FEND, 0xFF, FEND]).await.unwrap();
        let mut got = vec![0u8; 3];
        theirs.read_exact(&mut got).await.unwrap();
        assert_eq!(got, vec![FEND, 0xFF, FEND]);
    }

    #[tokio::test]
    async fn closed_link_is_reported() {
        let (ours, theirs) = duplex(256);
        let mut conn = KissConnection::new(ours);
        drop(theirs);
        assert!(matches!(conn.read_frame().await, Err(Error::LinkClosed)));
    }

    #[tokio::test]
    async fn oversized_frame_is_an_error_and_recovers() {
        let (ours, mut theirs) = duplex(256);
        let mut reader = KissReader::with_max_frame(ours, 16);
        let mut junk = vec![FEND, 0x00];
        junk.extend(std::iter::repeat_n(0x55, 20));
        theirs.write_all(&junk).await.unwrap();
        assert!(matches!(
            reader.read_frame().await,
            Err(Error::KissFrame(_))
        ));
        theirs.write_all(&[FEND, 0x00, 0x01, FEND]).await.unwrap();
        assert_eq!(reader.read_frame().await.unwrap(), frame(&[0x01]));
    }

    #[tokio::test]
    async fn split_halves_work_concurrently() {
        let (ours, theirs) = duplex(256);
        let (mut rd, mut wr) = KissConnection::new(ours).into_split();
        let (mut their_rd, mut their_wr) = KissConnection::new(theirs).into_split();
        let echo = tokio::spawn(async move {
            let f = their_rd.read_frame().await.unwrap();
            their_wr.write_frame(&f).await.unwrap();
        });
        wr.write_frame(&[0x00, 0x99]).await.unwrap();
        assert_eq!(rd.read_frame().await.unwrap(), frame(&[0x99]));
        echo.await.unwrap();
    }
}
