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

//! KISS init by TNC model: what to send a TNC when its link comes up so
//! it's in KISS mode, and what to send when the bridge lets it go.
//!
//! The TH-D74 and TH-D75 share one command set (the same one B.B. Link
//! uses), over Bluetooth and USB alike. Commands are `XX…\r` and replies
//! `XX …\r`. The radio comes up in whatever mode it was left in, so init
//! runs on every connect:
//!
//! 1. `BT`: answered in command mode, ignored in KISS mode. Silence means
//!    the TNC is already in KISS mode, so send KISS "return" (`C0 FF C0`)
//!    to get back to the command parser, and ask again.
//! 2. `ID`, for the log.
//! 3. `TN`: the TNC mode (0 off, 1 APRS, 2 KISS) and band (0 A, 1 B).
//! 4. `TN 2,<band>`: KISS mode on the band it was already using.
//!
//! KISS frames can arrive while probing (the radio was in KISS mode and
//! heard something), so replies are read from the stream with frames
//! stripped out, and the frames are kept for the bridge.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::config::TncModel;
use crate::error::Error;
use crate::kiss::frame::FEND;

/// KISS "return": leave KISS mode.
pub const KISS_RETURN: [u8; 3] = [FEND, 0xFF, FEND];
/// How long to wait for a reply to a radio command.
const REPLY_WAIT: Duration = Duration::from_secs(1);
/// The most init may take: a TNC that stops answering or stops taking
/// bytes fails the connect, and the next attempt opens the link afresh.
/// The full TH-D7x sequence from KISS mode takes about 1.4 s.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the radio needs after leaving KISS mode before it takes commands.
const AFTER_KISS_RETURN: Duration = Duration::from_millis(300);
/// `?` means the radio is busy; ask again this many times, this far apart.
const BUSY_RETRIES: u32 = 3;
const BUSY_GAP: Duration = Duration::from_millis(200);
/// Command replies are short; anything longer isn't one.
const MAX_LINE: usize = 128;
const MAX_FRAME: usize = 8192;

/// What init found and what it leaves behind.
#[derive(Debug, Default)]
pub struct InitOutcome {
    /// The radio's model from `ID`, e.g. `TH-D75`.
    pub radio: Option<String>,
    /// Frames that arrived during init, raw (between FENDs, still stuffed),
    /// for the bridge to forward.
    pub frames: Vec<Vec<u8>>,
    /// What to send when the bridge closes the link on purpose.
    pub teardown: Vec<Vec<u8>>,
}

/// Put the TNC on `stream` into KISS mode. With `exit_kiss_on_disconnect`,
/// the returned teardown takes it back out, restoring the TNC mode it was
/// found in where the model allows.
pub async fn init<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    model: TncModel,
    exit_kiss_on_disconnect: bool,
) -> Result<InitOutcome, Error> {
    match model {
        TncModel::None => Ok(InitOutcome {
            teardown: if exit_kiss_on_disconnect {
                vec![KISS_RETURN.to_vec()]
            } else {
                Vec::new()
            },
            ..Default::default()
        }),
        TncModel::ThD7x => tokio::time::timeout(
            INIT_TIMEOUT,
            init_th_d7x(stream, exit_kiss_on_disconnect),
        )
        .await
        .map_err(|_| {
            Error::Connect(format!(
                "the TNC didn't finish KISS init in {}s (not answering, or not taking data)",
                INIT_TIMEOUT.as_secs()
            ))
        })?,
    }
}

async fn init_th_d7x<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    exit_kiss_on_disconnect: bool,
) -> Result<InitOutcome, Error> {
    let t0 = Instant::now();
    let mut radio = Radio::new(stream);

    let mut was_kiss = false;
    if radio.command("BT").await?.is_none() {
        // Silent: already in KISS mode. Leave it so init can run.
        was_kiss = true;
        radio.send_raw(&KISS_RETURN).await?;
        tokio::time::sleep(AFTER_KISS_RETURN).await;
        if radio.command("BT").await?.is_none() {
            // Nothing else will get an answer either. A TH-D75 just plugged
            // back in over USB can be like this until the port is reopened.
            return Err(Error::Connect(
                "the TNC doesn't answer TH-D7x commands, in or out of KISS mode".into(),
            ));
        }
    }
    let id = radio.command("ID").await?;
    let tn = radio.command("TN").await?;
    let (mode, band) = tn.as_deref().and_then(parse_tn).unzip();
    let band = band.unwrap_or(0);
    // The mode to restore: after a KISS exit `TN` reports 0 (off), so what
    // the probe saw wins.
    let prev_mode = if was_kiss { Some(2) } else { mode };
    match radio.command(&format!("TN 2,{band}")).await? {
        Some(reply) if reply.starts_with("TN") => {}
        other => {
            return Err(Error::Connect(format!(
                "the TNC didn't accept KISS mode (TN 2,{band}): {}",
                other.as_deref().unwrap_or("no reply")
            )));
        }
    }

    let radio_id = id.map(|s| s.strip_prefix("ID ").unwrap_or(&s).trim().to_string());
    tracing::info!(
        radio = radio_id.as_deref().unwrap_or("?"),
        was_kiss,
        prev_mode = ?prev_mode,
        band,
        "TNC in KISS mode after {:.1?}",
        t0.elapsed()
    );

    let mut teardown = Vec::new();
    if exit_kiss_on_disconnect {
        teardown.push(KISS_RETURN.to_vec());
        // Leaving KISS turns the TNC off; put back APRS if that's what it was.
        if let Some(m @ 1) = prev_mode {
            teardown.push(format!("TN {m},{band}\r").into_bytes());
        }
    }
    Ok(InitOutcome {
        radio: radio_id,
        frames: radio.frames,
        teardown,
    })
}

/// `TN 1,0` → (1, 0).
fn parse_tn(reply: &str) -> Option<(u8, u8)> {
    let (mode, band) = reply.strip_prefix("TN ")?.split_once(',')?;
    Some((mode.trim().parse().ok()?, band.trim().parse().ok()?))
}

/// Command/reply over a stream that may also carry KISS frames.
struct Radio<'a, S> {
    stream: &'a mut S,
    /// Text outside frames since the last reply.
    line: Vec<u8>,
    frame: Vec<u8>,
    state: Scan,
    replies: std::collections::VecDeque<String>,
    frames: Vec<Vec<u8>>,
}

impl<'a, S: AsyncRead + AsyncWrite + Unpin> Radio<'a, S> {
    fn new(stream: &'a mut S) -> Self {
        Self {
            stream,
            line: Vec::new(),
            frame: Vec::new(),
            state: Scan::Text,
            replies: Default::default(),
            frames: Vec::new(),
        }
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.stream.write_all(bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Send `cmd` and return its reply, or `None` if there's none within
    /// [`REPLY_WAIT`]. A busy reply (`?`) is retried.
    async fn command(&mut self, cmd: &str) -> Result<Option<String>, Error> {
        for attempt in 0..=BUSY_RETRIES {
            self.replies.clear();
            self.send_raw(format!("{cmd}\r").as_bytes()).await?;
            let reply = self.reply().await?;
            tracing::debug!(cmd, reply = ?reply, "TNC command");
            match reply {
                Some(r) if r == "?" && attempt < BUSY_RETRIES => {
                    tokio::time::sleep(BUSY_GAP).await;
                }
                other => return Ok(other),
            }
        }
        Ok(None)
    }

    async fn reply(&mut self) -> Result<Option<String>, Error> {
        let deadline = tokio::time::Instant::now() + REPLY_WAIT;
        let mut buf = [0u8; 512];
        loop {
            if let Some(r) = self.replies.pop_front() {
                return Ok(Some(r));
            }
            // Checked here too: a read that's always ready (a stream of
            // bytes that are never a reply) would otherwise win every time.
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            let n = match tokio::time::timeout_at(deadline, self.stream.read(&mut buf)).await {
                Err(_) => return Ok(None),
                Ok(Ok(0)) => return Err(Error::LinkClosed),
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e.into()),
            };
            self.push(&buf[..n]);
        }
    }

    /// Split incoming bytes into KISS frames and `\r`-terminated lines.
    ///
    /// A FEND that closes a frame may also open the next one (KISS allows a
    /// shared delimiter), or be followed by a text reply once the radio has
    /// left KISS mode. The next byte tells them apart: a frame starts with
    /// a KISS command byte (below 0x20 for data on the first two KISS
    /// channels, which is all a radio sends), a reply with a letter or `?`.
    fn push(&mut self, data: &[u8]) {
        for &b in data {
            match (b, self.state) {
                (FEND, Scan::Frame) if !self.frame.is_empty() => {
                    self.frames.push(std::mem::take(&mut self.frame));
                    self.state = Scan::AfterFrame;
                }
                (FEND, _) => {
                    self.frame.clear();
                    self.state = Scan::Frame;
                }
                (_, Scan::AfterFrame) if b < 0x20 => {
                    self.frame.push(b);
                    self.state = Scan::Frame;
                }
                (_, Scan::Frame) => {
                    if self.frame.len() < MAX_FRAME {
                        self.frame.push(b);
                    } else {
                        self.frame.clear();
                        self.state = Scan::Text;
                    }
                }
                (b'\r' | b'\n', _) => {
                    self.state = Scan::Text;
                    if !self.line.is_empty() {
                        let text = String::from_utf8_lossy(&self.line).trim().to_string();
                        self.line.clear();
                        self.replies.push_back(text);
                    }
                }
                (_, _) => {
                    self.state = Scan::Text;
                    if self.line.len() < MAX_LINE {
                        self.line.push(b);
                    } else {
                        self.line.clear();
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Text,
    Frame,
    AfterFrame,
}

/// A link with bytes to hand out before its own: the frames init read off
/// the stream, put back for the bridge's KISS reader.
pub struct Prefixed<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S> Prefixed<S> {
    /// `frames` are raw (between FENDs); they're delimited again here.
    pub fn new(inner: S, frames: &[Vec<u8>]) -> Self {
        let mut prefix = Vec::new();
        for f in frames {
            prefix.push(FEND);
            prefix.extend_from_slice(f);
            prefix.push(FEND);
        }
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.prefix[start..start + n]);
            self.pos += n;
            if self.pos == self.prefix.len() {
                self.prefix = Vec::new();
                self.pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A fake TH-D75 command port, close enough to the real one (Phase 0
    //! findings) to exercise init: it answers commands in command mode,
    //! ignores them in KISS mode, and leaves KISS mode on `C0 FF C0`.

    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use crate::kiss::frame::FEND;

    #[derive(Debug, Default)]
    pub struct RadioState {
        /// 0 off, 1 APRS, 2 KISS.
        pub mode: u8,
        pub band: u8,
        /// Everything the radio was sent, as text where it is text.
        pub log: Vec<String>,
    }

    /// Run the radio on `io` until the other end closes. In KISS mode it
    /// sends `heard` (a raw frame) the first time it ignores a command.
    pub async fn radio(io: DuplexStream, state: Arc<Mutex<RadioState>>, heard: Option<Vec<u8>>) {
        let (mut rd, mut wr) = tokio::io::split(io);
        let mut heard = heard;
        let mut buf = [0u8; 256];
        let mut line = Vec::new();
        let mut kiss = Vec::new();
        loop {
            let n = match rd.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for &b in &buf[..n] {
                let mode = state.lock().unwrap().mode;
                if mode == 2 {
                    // KISS mode: collect frames; text is ignored.
                    kiss.push(b);
                    if kiss.ends_with(&[FEND, 0xFF, FEND]) {
                        let mut st = state.lock().unwrap();
                        st.mode = 0;
                        st.log.push("KISS return".into());
                        kiss.clear();
                    } else if b == b'\r'
                        && let Some(f) = heard.take()
                    {
                        let mut wire = vec![FEND];
                        wire.extend_from_slice(&f);
                        wire.push(FEND);
                        let _ = wr.write_all(&wire).await;
                    }
                    continue;
                }
                if b == FEND {
                    continue;
                }
                if b != b'\r' {
                    line.push(b);
                    continue;
                }
                let cmd = String::from_utf8_lossy(&line).to_string();
                line.clear();
                let reply = {
                    let mut st = state.lock().unwrap();
                    st.log.push(cmd.clone());
                    match cmd.as_str() {
                        "BT" => "BT 1".to_string(),
                        "ID" => "ID TH-D75".to_string(),
                        "TN" => format!("TN {},{}", st.mode, st.band),
                        c if c.starts_with("TN ") => {
                            let (m, bd) = c[3..].split_once(',').unwrap();
                            st.mode = m.parse().unwrap();
                            st.band = bd.parse().unwrap();
                            c.to_string()
                        }
                        _ => "?".to_string(),
                    }
                };
                let _ = wr.write_all(format!("{reply}\r").as_bytes()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{RadioState, radio};
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn run(
        mode: u8,
        model: TncModel,
        exit_kiss: bool,
        heard: Option<Vec<u8>>,
    ) -> (InitOutcome, Arc<Mutex<RadioState>>) {
        let (mut ours, theirs) = tokio::io::duplex(1024);
        let state = Arc::new(Mutex::new(RadioState {
            mode,
            band: 1,
            log: Vec::new(),
        }));
        tokio::spawn(radio(theirs, state.clone(), heard));
        let out = init(&mut ours, model, exit_kiss).await.unwrap();
        (out, state)
    }

    #[tokio::test(start_paused = true)]
    async fn from_kiss_mode() {
        let heard = vec![0x00, 0x82, 0xA0];
        let (out, state) = run(2, TncModel::ThD7x, false, Some(heard.clone())).await;
        let st = state.lock().unwrap();
        assert_eq!(st.mode, 2);
        assert_eq!(st.band, 1);
        assert_eq!(
            st.log,
            vec!["KISS return", "BT", "ID", "TN", "TN 2,1"],
            "{:?}",
            st.log
        );
        assert_eq!(out.radio.as_deref(), Some("TH-D75"));
        // The frame the radio sent while still in KISS mode is kept.
        assert_eq!(out.frames, vec![heard]);
        assert!(out.teardown.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn from_aprs_mode_restores_it_on_teardown() {
        let (out, state) = run(1, TncModel::ThD7x, true, None).await;
        let st = state.lock().unwrap();
        assert_eq!(st.log, vec!["BT", "ID", "TN", "TN 2,1"]);
        assert_eq!(st.mode, 2);
        assert_eq!(
            out.teardown,
            vec![KISS_RETURN.to_vec(), b"TN 1,1\r".to_vec()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn found_in_kiss_mode_teardown_just_leaves_it() {
        let (out, _) = run(2, TncModel::ThD7x, true, None).await;
        assert_eq!(out.teardown, vec![KISS_RETURN.to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn model_none_sends_nothing() {
        let (out, state) = run(1, TncModel::None, false, None).await;
        assert!(state.lock().unwrap().log.is_empty());
        assert!(out.teardown.is_empty());
        let (out, _) = run(1, TncModel::None, true, None).await;
        assert_eq!(out.teardown, vec![KISS_RETURN.to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_tnc_fails_init_fast() {
        let (mut ours, mut theirs) = tokio::io::duplex(1024);
        let t0 = tokio::time::Instant::now();
        let err = init(&mut ours, TncModel::ThD7x, false).await.unwrap_err();
        assert!(err.to_string().contains("doesn't answer"), "{err}");
        // Probe, KISS return, probe again; nothing more.
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        drop(ours);
        let mut sent = Vec::new();
        theirs.read_to_end(&mut sent).await.unwrap();
        assert_eq!(sent, b"BT\r\xC0\xFF\xC0BT\r".to_vec());
    }

    #[test]
    fn frames_and_replies_are_told_apart() {
        let mut io = tokio::io::empty();
        let mut r = Radio::new(&mut io);
        // A frame, a frame sharing its FEND, then a reply after a frame.
        r.push(&[FEND, 0x00, 0x41, FEND, 0x00, 0x42, FEND]);
        r.push(b"BT 1\r");
        r.push(&[FEND, FEND, 0x10, 0x43, FEND, b'?', b'\r']);
        assert_eq!(
            r.frames,
            vec![vec![0x00, 0x41], vec![0x00, 0x42], vec![0x10, 0x43]]
        );
        assert_eq!(r.replies, ["BT 1", "?"]);
    }

    /// Takes no bytes at all, like a TNC whose USB input has stalled.
    struct Stalled;

    impl AsyncRead for Stalled {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for Stalled {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_tnc_fails_init_in_time() {
        let t0 = tokio::time::Instant::now();
        let err = init(&mut Stalled, TncModel::ThD7x, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("didn't finish KISS init"), "{err}");
        assert_eq!(t0.elapsed(), INIT_TIMEOUT);
    }

    /// Always has another byte, none of them a reply.
    struct Chatty;

    impl AsyncRead for Chatty {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            buf.put_slice(b"x");
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for Chatty {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            b: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    // Real time: a read that's always ready never lets a paused clock move.
    #[tokio::test]
    async fn a_reply_wait_ends_even_while_bytes_keep_coming() {
        let mut io = Chatty;
        let mut r = Radio::new(&mut io);
        let t0 = std::time::Instant::now();
        assert_eq!(r.command("BT").await.unwrap(), None);
        assert!(t0.elapsed() < REPLY_WAIT * 3, "{:?}", t0.elapsed());
    }

    #[test]
    fn tn_replies() {
        assert_eq!(parse_tn("TN 1,0"), Some((1, 0)));
        assert_eq!(parse_tn("TN 2,1"), Some((2, 1)));
        assert_eq!(parse_tn("TN"), None);
        assert_eq!(parse_tn("?"), None);
    }

    #[tokio::test]
    async fn prefixed_reads_the_prefix_first() {
        let (ours, mut theirs) = tokio::io::duplex(64);
        let mut p = Prefixed::new(ours, &[vec![0x00, 0x01]]);
        theirs.write_all(&[FEND, 0x00, 0x02, FEND]).await.unwrap();
        let mut got = [0u8; 8];
        p.read_exact(&mut got).await.unwrap();
        assert_eq!(got, [FEND, 0x00, 0x01, FEND, FEND, 0x00, 0x02, FEND]);
        p.write_all(b"x").await.unwrap();
        let mut one = [0u8; 1];
        theirs.read_exact(&mut one).await.unwrap();
        assert_eq!(&one, b"x");
    }
}
