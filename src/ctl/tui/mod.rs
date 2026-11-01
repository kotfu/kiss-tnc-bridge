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

//! `ktbctl tui`: a full-screen view of the daemon.
//!
//! Two connections: one for requests, one subscribed to state, activity
//! and frames. A state event means "read `status` again"; nothing is
//! polled. Keys come from a thread of their own. If the daemon goes away,
//! the TUI says so and tries again every 2 s.

mod app;
mod ui;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEvent, KeyEventKind};
use tokio::sync::mpsc;

use self::app::{Action, App, Link, Req, Then};
use super::client::{Client, CtlError, Message, result};
use crate::control::protocol::{Command, Response, Status, Topic};

/// How often the screen is redrawn with nothing else happening, for the
/// activity lights to go out and the times to move.
const TICK: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(2);

pub async fn run(socket: &Path) -> Result<(), CtlError> {
    let mut terminal = ratatui::try_init()?;
    let res = run_app(&mut terminal, socket).await;
    ratatui::restore();
    res
}

/// What the terminal sent.
enum Input {
    Key(KeyEvent),
    Resize,
}

/// Read the terminal on a thread of its own: crossterm's reads block.
fn read_input() -> mpsc::UnboundedReceiver<Input> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(200)) {
                Ok(true) => {
                    let input = match event::read() {
                        Ok(TermEvent::Key(k)) if k.kind == KeyEventKind::Press => Input::Key(k),
                        Ok(TermEvent::Resize(..)) => Input::Resize,
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                    if tx.send(input).is_err() {
                        break;
                    }
                }
                Ok(false) if tx.is_closed() => break,
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
    rx
}

/// The two connections to the daemon.
struct Conn {
    requests: Client,
    events: Client,
    /// The `status` request waiting for its answer.
    status_pending: Option<u64>,
    /// State changed again while it was waiting.
    status_stale: bool,
    /// Requests from keys waiting for their answers, by id.
    pending: HashMap<u64, Then>,
}

impl Conn {
    async fn open(socket: &Path) -> Result<Conn, CtlError> {
        let requests = Client::connect(socket).await?;
        let mut events = Client::connect(socket).await?;
        events
            .subscribe(vec![Topic::State, Topic::Activity, Topic::Frames])
            .await?;
        let mut c = Conn {
            requests,
            events,
            status_pending: None,
            status_stale: false,
            pending: HashMap::new(),
        };
        let models = c.requests.send(Command::TncModels).await?;
        c.pending.insert(models, Then::Models);
        c.refresh().await?;
        Ok(c)
    }

    /// Ask for `status`, or if it's already asked for, again when that
    /// answer comes.
    async fn refresh(&mut self) -> Result<(), CtlError> {
        if self.status_pending.is_some() {
            self.status_stale = true;
        } else {
            self.status_pending = Some(self.requests.send(Command::Status).await?);
        }
        Ok(())
    }
}

/// What woke the loop.
enum Woke {
    Input(Option<Input>),
    Request(Result<Option<Message>, CtlError>),
    Event(Result<Option<Message>, CtlError>),
    Tick,
}

async fn next(c: Option<&mut Client>) -> Result<Option<Message>, CtlError> {
    match c {
        Some(c) => c.next_message().await,
        None => std::future::pending().await,
    }
}

async fn run_app(terminal: &mut DefaultTerminal, socket: &Path) -> Result<(), CtlError> {
    let mut app = App::new(hostname());
    let mut input = read_input();
    let mut conn: Option<Conn> = None;
    let mut retry_at = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if conn.is_none() && Instant::now() >= retry_at {
            match Conn::open(socket).await {
                Ok(c) => {
                    conn = Some(c);
                    app.link = Link::Up;
                }
                Err(e) => {
                    app.link = Link::Down(e.to_string());
                    retry_at = Instant::now() + RETRY;
                }
            }
        }
        terminal.draw(|f| ui::draw(f, &app, Instant::now(), SystemTime::now()))?;

        let woke = {
            let (requests, events) = match conn.as_mut() {
                Some(c) => (Some(&mut c.requests), Some(&mut c.events)),
                None => (None, None),
            };
            tokio::select! {
                i = input.recv() => Woke::Input(i),
                m = next(requests) => Woke::Request(m),
                m = next(events) => Woke::Event(m),
                _ = tick.tick() => Woke::Tick,
            }
        };
        let res = match woke {
            Woke::Input(None) => return Ok(()),
            Woke::Input(Some(Input::Key(k))) => {
                let a = app.key(k);
                match act(a, &mut app, &mut conn).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Woke::Input(Some(Input::Resize)) | Woke::Tick => Ok(()),
            Woke::Request(m) => match m {
                Ok(Some(Message::Response(r))) => answered(r, &mut app, &mut conn).await,
                // A pairing's steps and a scan's finds come to the
                // connection that asked.
                Ok(Some(Message::Event(ev))) => {
                    let a = app.on_own_event(ev.event);
                    act(a, &mut app, &mut conn).await.map(|_| ())
                }
                Ok(None) => Err(CtlError::Closed),
                Err(e) => Err(e),
            },
            Woke::Event(m) => match (m, conn.as_mut()) {
                (Ok(Some(Message::Event(ev))), Some(c)) => {
                    if app.on_event(ev.event, Instant::now()) {
                        c.refresh().await
                    } else {
                        Ok(())
                    }
                }
                (Ok(Some(_)), _) => Ok(()),
                (Ok(None), _) => Err(CtlError::Closed),
                (Err(e), _) => Err(e),
            },
        };
        // Any trouble with the daemon: show it, and try again shortly.
        if let Err(e) = res {
            conn = None;
            let why = match e {
                CtlError::Closed => "the daemon closed the connection".to_string(),
                e => e.to_string(),
            };
            app.lost(&why);
            app.link = Link::Down(why);
            retry_at = Instant::now() + RETRY;
        }
    }
}

/// Do what a key or an answer asks. True to quit.
async fn act(action: Action, app: &mut App, conn: &mut Option<Conn>) -> Result<bool, CtlError> {
    match action {
        Action::Quit => Ok(true),
        Action::None => Ok(false),
        Action::Request(req) => {
            let Req { command, then } = *req;
            match conn.as_mut() {
                Some(c) => {
                    let id = c.requests.send(command).await?;
                    app.sent(id, &then);
                    c.pending.insert(id, then);
                }
                None => {
                    app.lost("not connected to the daemon");
                    app.error("not connected to the daemon".into());
                }
            }
            Ok(false)
        }
    }
}

/// An answer on the requests connection.
async fn answered(r: Response, app: &mut App, conn: &mut Option<Conn>) -> Result<(), CtlError> {
    let Some(c) = conn.as_mut() else {
        return Ok(());
    };
    if r.id.is_some() && r.id == c.status_pending {
        c.status_pending = None;
        let s = result(r).and_then(|v| {
            serde_json::from_value::<Status>(v).map_err(|e| CtlError::Protocol(e.to_string()))
        });
        match s {
            Ok(s) => app.set_status(s),
            Err(e) => app.error(e.to_string()),
        }
        return if std::mem::take(&mut c.status_stale) {
            c.refresh().await
        } else {
            Ok(())
        };
    }
    let Some(then) = r.id.and_then(|id| c.pending.remove(&id)) else {
        return Ok(());
    };
    // Not every change sends a state event (removing a disabled bridge
    // stops nothing), so look again.
    let next = app.on_answer(then, result(r));
    c.refresh().await?;
    act(next, app, conn).await.map(|_| ())
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if ok {
        let name = String::from_utf8_lossy(&buf[..end]).to_string();
        // Just the host: aprscc, not aprscc.kotfu.net.
        name.split('.').next().unwrap_or_default().to_string()
    } else {
        "this host".into()
    }
}
