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

//! The TUI's state, and what keys and the daemon's events do to it. Nothing
//! here touches the terminal or the socket, so it can be tested.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::client::CtlError;
use crate::ax25::Decoded;
use crate::control::protocol::{
    BridgeStatus, Command, Dir, ErrorCode, Event, PortStatus, SessionStatus, Status,
};

/// Frames kept for the Frames view.
pub const FRAMES_KEPT: usize = 1000;
/// How long an activity light stays lit after bytes move.
pub const LIGHT: Duration = Duration::from_millis(300);
/// Rows PgUp and PgDn move.
const PAGE: usize = 10;
/// Errors longer than this get a box rather than the message line.
const ERROR_LINE: usize = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Bridges,
    Tncs,
    Frames,
}

/// What the Bridges view's lower pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lower {
    Frames,
    Details,
}

/// The connection to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Up,
    /// Why not, as the CLI would say it.
    Down(String),
}

/// What a key asks of the loop that runs the TUI.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    /// Send this to the daemon.
    Request(Box<Req>),
}

fn request(req: Req) -> Action {
    Action::Request(Box::new(req))
}

/// A request for the daemon, and what to do with its answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Req {
    pub command: Command,
    pub then: Then,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Then {
    /// Say this when it works.
    Done(String),
    /// A change sent with `non_disruptive`. If the daemon says it would
    /// disconnect someone, ask, and on yes send `redo` (the same change
    /// without it).
    Consent {
        title: String,
        yes: String,
        redo: Command,
        done: String,
        /// For an enable: the bridge to offer to switch to if another
        /// bridge is in its way.
        switch: Option<String>,
    },
}

/// A question or a notice over the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<String>,
    /// The yes button's words and what it does; none for a notice.
    pub yes: Option<(String, Req)>,
}

/// A frame from a `frame` event.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRow {
    /// Counts up from 1, so a selection survives older frames going.
    pub seq: u64,
    /// As the daemon sent it: `2026-10-27T14:22:07Z`.
    pub at: String,
    pub dir: Dir,
    /// The TNC port: `ktbr1/radio`.
    pub object: String,
    /// For tx, the session it came from: `ktbr1/phones/40:33:…`.
    pub session: Option<String>,
    pub kiss: String,
    pub decoded: Decoded,
}

impl FrameRow {
    pub fn bridge(&self) -> &str {
        self.object.split('/').next().unwrap_or_default()
    }

    /// The port it's shown on: the TNC port for rx, the app's port for tx.
    pub fn port(&self) -> &str {
        let of = self.session.as_deref().unwrap_or(&self.object);
        of.split('/').nth(1).unwrap_or(of)
    }
}

/// A row of the Bridges tree, by index into `Status::bridges` and its
/// ports and sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Bridge(usize),
    Port(usize, usize),
    Session(usize, usize, usize),
}

/// A row of the TNCs view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TncRow {
    Tnc(usize),
    Adapter(usize),
}

#[derive(Debug, Default)]
pub struct FramesView {
    /// The selected frame's `seq`; `None` follows the newest.
    pub selected: Option<u64>,
    /// While paused, only frames up to this `seq` are shown.
    pub paused: Option<u64>,
    /// Shown only if they contain this (any case).
    pub filter: String,
    /// The filter is being typed.
    pub typing: bool,
    /// Only the bridge selected in the Bridges view.
    pub this_bridge: bool,
    /// The decode pane shows the whole frame in hex.
    pub hex: bool,
}

pub struct App {
    pub view: View,
    /// Where Esc in the Frames view goes back to.
    pub back: View,
    pub link: Link,
    pub host: String,
    pub status: Option<Status>,
    /// The Bridges view's selection, by object id (`ktbr1`, `ktbr1/radio`,
    /// a session id), so it survives a new `status`.
    pub selected: Option<String>,
    /// Where the selection was, for when its object goes away.
    selected_at: usize,
    /// Bridges the user opened or folded; the rest are open while enabled.
    pub folds: HashMap<String, bool>,
    pub lower: Lower,
    /// The TNCs view's selection: `tnc:ID` or `adapter:MAC`.
    pub tnc_selected: Option<String>,
    pub frames: VecDeque<FrameRow>,
    next_seq: u64,
    pub fv: FramesView,
    /// When bytes last moved, by port or session: [rx, tx].
    pub lights: HashMap<String, [Option<Instant>; 2]>,
    /// One line above the keys until the next key: (text, is an error).
    pub message: Option<(String, bool)>,
    pub help: bool,
    pub dialog: Option<Dialog>,
}

impl App {
    pub fn new(host: String) -> App {
        App {
            view: View::Bridges,
            back: View::Bridges,
            link: Link::Connecting,
            host,
            status: None,
            selected: None,
            selected_at: 0,
            folds: HashMap::new(),
            lower: Lower::Frames,
            tnc_selected: None,
            frames: VecDeque::new(),
            next_seq: 1,
            fv: FramesView::default(),
            lights: HashMap::new(),
            message: None,
            help: false,
            dialog: None,
        }
    }

    pub fn set_status(&mut self, s: Status) {
        self.status = Some(s);
    }

    /// Take in an event. True if `status` should be read again.
    pub fn on_event(&mut self, ev: Event, now: Instant) -> bool {
        match ev {
            Event::State { .. } => true,
            Event::Activity { object, dir, .. } => {
                let i = match dir {
                    Dir::Rx => 0,
                    Dir::Tx => 1,
                };
                self.lights.entry(object).or_default()[i] = Some(now);
                false
            }
            Event::Frame {
                object,
                dir,
                session,
                at,
                kiss,
                decoded,
            } => {
                self.frames.push_back(FrameRow {
                    seq: self.next_seq,
                    at,
                    dir,
                    object,
                    session,
                    kiss,
                    decoded,
                });
                self.next_seq += 1;
                while self.frames.len() > FRAMES_KEPT {
                    self.frames.pop_front();
                }
                false
            }
            Event::Lagged { .. } | Event::Pairing { .. } | Event::Found { .. } => false,
        }
    }

    /// Whether `object`'s light for `dir` is lit at `now`.
    pub fn lit(&self, object: &str, dir: Dir, now: Instant) -> bool {
        let i = match dir {
            Dir::Rx => 0,
            Dir::Tx => 1,
        };
        self.lights
            .get(object)
            .and_then(|l| l[i])
            .is_some_and(|t| now.duration_since(t) < LIGHT)
    }

    // ── Bridges ─────────────────────────────────────────────────────────

    pub fn folded(&self, b: &BridgeStatus) -> bool {
        self.folds.get(&b.id).copied().unwrap_or(!b.enabled)
    }

    /// The tree's rows, in order, with each row's object id.
    pub fn rows(&self) -> Vec<(Row, String)> {
        let mut out = Vec::new();
        let Some(s) = &self.status else {
            return out;
        };
        for (bi, b) in s.bridges.iter().enumerate() {
            out.push((Row::Bridge(bi), b.id.clone()));
            if self.folded(b) {
                continue;
            }
            for (pi, p) in b.ports.iter().enumerate() {
                out.push((Row::Port(bi, pi), format!("{}/{}", b.id, p.id)));
                for (si, sess) in p.sessions.iter().enumerate() {
                    out.push((Row::Session(bi, pi, si), sess.id.clone()));
                }
            }
        }
        out
    }

    /// The selected row's index in `rows`.
    pub fn selected_index(&self, rows: &[(Row, String)]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        let found = self
            .selected
            .as_ref()
            .and_then(|k| rows.iter().position(|(_, id)| id == k));
        Some(found.unwrap_or(self.selected_at.min(rows.len() - 1)))
    }

    pub fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        self.selected_index(&rows).map(|i| rows[i].0)
    }

    /// The bridge the selection is in.
    pub fn selected_bridge(&self) -> Option<&BridgeStatus> {
        let s = self.status.as_ref()?;
        match self.selected_row()? {
            Row::Bridge(b) | Row::Port(b, _) | Row::Session(b, _, _) => s.bridges.get(b),
        }
    }

    pub fn port(&self, b: usize, p: usize) -> Option<(&BridgeStatus, &PortStatus)> {
        let br = self.status.as_ref()?.bridges.get(b)?;
        Some((br, br.ports.get(p)?))
    }

    pub fn session(&self, b: usize, p: usize, s: usize) -> Option<&SessionStatus> {
        self.port(b, p)?.1.sessions.get(s)
    }

    fn select_row(&mut self, rows: &[(Row, String)], i: usize) {
        if let Some((_, id)) = rows.get(i) {
            self.selected = Some(id.clone());
            self.selected_at = i;
        }
    }

    fn move_bridges(&mut self, delta: isize) {
        let rows = self.rows();
        let Some(at) = self.selected_index(&rows) else {
            return;
        };
        let to = at.saturating_add_signed(delta).min(rows.len() - 1);
        self.select_row(&rows, to);
    }

    /// → opens a bridge; ← folds it, or from a port or session goes up to
    /// its bridge.
    fn fold(&mut self, fold: bool) {
        let rows = self.rows();
        let Some(i) = self.selected_index(&rows) else {
            return;
        };
        let Some(s) = &self.status else {
            return;
        };
        match rows[i].0 {
            Row::Bridge(b) => {
                let id = s.bridges[b].id.clone();
                self.folds.insert(id, fold);
            }
            Row::Port(b, _) | Row::Session(b, _, _) if fold => {
                let id = s.bridges[b].id.clone();
                let at = rows.iter().position(|(_, k)| *k == id).unwrap_or(0);
                self.select_row(&rows, at);
            }
            _ => {}
        }
    }

    // ── TNCs ────────────────────────────────────────────────────────────

    pub fn tnc_rows(&self) -> Vec<(TncRow, String)> {
        let Some(s) = &self.status else {
            return Vec::new();
        };
        let tncs = s
            .tncs
            .iter()
            .enumerate()
            .map(|(i, t)| (TncRow::Tnc(i), format!("tnc:{}", t.id)));
        let adapters = s
            .adapters
            .iter()
            .enumerate()
            .map(|(i, a)| (TncRow::Adapter(i), format!("adapter:{}", a.mac)));
        tncs.chain(adapters).collect()
    }

    pub fn tnc_selected_index(&self, rows: &[(TncRow, String)]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        Some(
            self.tnc_selected
                .as_ref()
                .and_then(|k| rows.iter().position(|(_, id)| id == k))
                .unwrap_or(0),
        )
    }

    fn move_tncs(&mut self, delta: isize) {
        let rows = self.tnc_rows();
        let Some(at) = self.tnc_selected_index(&rows) else {
            return;
        };
        let to = at.saturating_add_signed(delta).min(rows.len() - 1);
        self.tnc_selected = Some(rows[to].1.clone());
    }

    // ── Frames ──────────────────────────────────────────────────────────

    /// The frames the Frames view shows, oldest first.
    pub fn visible_frames(&self) -> Vec<&FrameRow> {
        let bridge = self
            .fv
            .this_bridge
            .then(|| self.selected_bridge().map(|b| b.id.clone()))
            .flatten();
        let filter = self.fv.filter.to_lowercase();
        self.frames
            .iter()
            .filter(|f| self.fv.paused.is_none_or(|p| f.seq <= p))
            .filter(|f| bridge.as_deref().is_none_or(|b| f.bridge() == b))
            .filter(|f| {
                filter.is_empty()
                    || f.decoded.summary.to_lowercase().contains(&filter)
                    || f.object.to_lowercase().contains(&filter)
            })
            .collect()
    }

    /// The selected frame's index in `visible`, and whether it's following
    /// the newest.
    pub fn selected_frame(&self, visible: &[&FrameRow]) -> Option<usize> {
        if visible.is_empty() {
            return None;
        }
        let last = visible.len() - 1;
        Some(match self.fv.selected {
            None => last,
            // The first one still here at or after it, if it's gone.
            Some(seq) => visible.iter().position(|f| f.seq >= seq).unwrap_or(last),
        })
    }

    fn move_frames(&mut self, delta: isize) {
        let visible = self.visible_frames();
        let Some(at) = self.selected_frame(&visible) else {
            return;
        };
        let last = visible.len() - 1;
        let to = at.saturating_add_signed(delta).min(last);
        self.fv.selected = (to != last).then(|| visible[to].seq);
    }

    fn frames_key(&mut self, k: KeyEvent) {
        if self.fv.typing {
            match k.code {
                KeyCode::Enter => self.fv.typing = false,
                KeyCode::Esc => {
                    self.fv.typing = false;
                    self.fv.filter.clear();
                }
                KeyCode::Backspace => {
                    self.fv.filter.pop();
                }
                KeyCode::Char(c) => self.fv.filter.push(c),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Up => self.move_frames(-1),
            KeyCode::Down => self.move_frames(1),
            KeyCode::PageUp => self.move_frames(-(PAGE as isize)),
            KeyCode::PageDown => self.move_frames(PAGE as isize),
            KeyCode::Home | KeyCode::Char('g') => self.move_frames(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.fv.selected = None,
            KeyCode::Char(' ') => {
                self.fv.paused = match self.fv.paused {
                    Some(_) => None,
                    None => Some(self.next_seq - 1),
                }
            }
            KeyCode::Char('/') => {
                self.fv.typing = true;
                self.fv.filter.clear();
            }
            KeyCode::Char('b') => self.fv.this_bridge = !self.fv.this_bridge,
            KeyCode::Char('h') => self.fv.hex = !self.fv.hex,
            KeyCode::Esc => self.view = self.back,
            _ => {}
        }
    }

    // ── Keys ────────────────────────────────────────────────────────────

    pub fn key(&mut self, k: KeyEvent) -> Action {
        self.message = None;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if let Some(d) = &self.dialog {
            return match (k.code, &d.yes) {
                (KeyCode::Char('y') | KeyCode::Enter, Some((_, req))) => {
                    let req = req.clone();
                    self.dialog = None;
                    request(req)
                }
                (KeyCode::Char('n') | KeyCode::Esc, Some(_))
                | (KeyCode::Enter | KeyCode::Esc, None) => {
                    self.dialog = None;
                    Action::None
                }
                _ => Action::None,
            };
        }
        if self.help {
            if matches!(
                k.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.help = false;
            }
            return Action::None;
        }
        // Typing a filter takes every other key.
        if self.view == View::Frames && self.fv.typing {
            self.frames_key(k);
            return Action::None;
        }
        match k.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('1') => self.view = View::Bridges,
            KeyCode::Char('2') => self.view = View::Tncs,
            KeyCode::Char('3') | KeyCode::Char('f') if self.view != View::Frames => {
                self.back = self.view;
                self.view = View::Frames;
            }
            KeyCode::Char('R') if self.view != View::Frames => {
                return request(consent(
                    |nd| Command::Reload { non_disruptive: nd },
                    "Reload the config?",
                    "Reload",
                    "config reloaded",
                ));
            }
            _ => match self.view {
                View::Bridges => match k.code {
                    KeyCode::Up => self.move_bridges(-1),
                    KeyCode::Down => self.move_bridges(1),
                    KeyCode::PageUp => self.move_bridges(-(PAGE as isize)),
                    KeyCode::PageDown => self.move_bridges(PAGE as isize),
                    KeyCode::Home => self.move_bridges(isize::MIN),
                    KeyCode::End => self.move_bridges(isize::MAX),
                    KeyCode::Left => self.fold(true),
                    KeyCode::Right => self.fold(false),
                    KeyCode::Tab => {
                        self.lower = match self.lower {
                            Lower::Frames => Lower::Details,
                            Lower::Details => Lower::Frames,
                        }
                    }
                    _ => return self.bridges_action(k.code),
                },
                View::Tncs => match k.code {
                    KeyCode::Up => self.move_tncs(-1),
                    KeyCode::Down => self.move_tncs(1),
                    KeyCode::Home => self.move_tncs(isize::MIN),
                    KeyCode::End => self.move_tncs(isize::MAX),
                    KeyCode::Char('-') | KeyCode::Delete => return self.remove_tnc_row(),
                    _ => {}
                },
                View::Frames => self.frames_key(k),
            },
        }
        Action::None
    }

    /// The keys that act on the selected row of the Bridges view.
    fn bridges_action(&mut self, code: KeyCode) -> Action {
        let Some(row) = self.selected_row() else {
            return Action::None;
        };
        let Some(s) = &self.status else {
            return Action::None;
        };
        match row {
            Row::Bridge(bi) => {
                let b = &s.bridges[bi];
                let (id, label) = (b.id.clone(), format!("{} ({})", b.id, b.name));
                match code {
                    KeyCode::Char('e') if b.enabled => self.say(format!("{id} is already enabled")),
                    KeyCode::Char('e') => {
                        let id2 = id.clone();
                        let mut req = consent(
                            move |nd| Command::BridgeEnable {
                                bridge: id2.clone(),
                                reset: false,
                                non_disruptive: nd,
                            },
                            &format!("Enable {label}?"),
                            "Enable it",
                            &format!("{id} enabled"),
                        );
                        if let Then::Consent { switch, .. } = &mut req.then {
                            *switch = Some(id);
                        }
                        request(req)
                    }
                    KeyCode::Char('d') if !b.enabled => {
                        self.say(format!("{id} is already disabled"))
                    }
                    KeyCode::Char('d') => request(consent(
                        move |nd| Command::BridgeDisable {
                            bridge: id.clone(),
                            non_disruptive: nd,
                        },
                        &format!("Disable {label}?"),
                        "Disable it",
                        &format!("{} disabled", b.id),
                    )),
                    KeyCode::Char('s') if b.enabled => self.say(format!("{id} is already enabled")),
                    KeyCode::Char('s') => request(switch_to(&id, &label)),
                    KeyCode::Char('-') | KeyCode::Delete if !b.editable => {
                        self.dialog = Some(static_notice(
                            &id,
                            &b.source,
                            "only enabling and disabling it works",
                        ));
                        Action::None
                    }
                    KeyCode::Char('-') | KeyCode::Delete => {
                        let id2 = id.clone();
                        self.dialog = Some(Dialog {
                            title: format!("Remove bridge {label}?"),
                            lines: vec!["It's removed from dynamic.toml, with its ports.".into()],
                            yes: Some((
                                "Remove it".into(),
                                consent(
                                    move |nd| Command::BridgeDelete {
                                        bridge: id2.clone(),
                                        non_disruptive: nd,
                                    },
                                    &format!("Remove bridge {label}?"),
                                    "Remove it",
                                    &format!("{id} removed"),
                                ),
                            )),
                        });
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            Row::Port(bi, pi) => {
                let b = &s.bridges[bi];
                let p = &b.ports[pi];
                let (bridge, port) = (b.id.clone(), p.id.clone());
                let object = format!("{bridge}/{port}");
                let tnc = p.role != "app";
                let act = |command, done: String| {
                    request(Req {
                        command,
                        then: Then::Done(done),
                    })
                };
                match code {
                    KeyCode::Char('c') if tnc => act(
                        Command::PortConnect { bridge, port },
                        format!("{object}: connecting, and staying up until x or r"),
                    ),
                    KeyCode::Char('x') if tnc => act(
                        Command::PortDisconnect { bridge, port },
                        format!("{object}: disconnected, and staying down until c or r"),
                    ),
                    KeyCode::Char('r') if tnc => act(
                        Command::PortRelease { bridge, port },
                        format!("{object}: released to its connect policy"),
                    ),
                    KeyCode::Char('-') | KeyCode::Delete if !b.editable => {
                        self.dialog = Some(static_notice(
                            &bridge,
                            &b.source,
                            "only enabling and disabling it works",
                        ));
                        Action::None
                    }
                    KeyCode::Char('-') | KeyCode::Delete => {
                        let (b2, p2) = (bridge.clone(), port.clone());
                        let title = format!("Remove port {port} from {bridge}?");
                        self.dialog = Some(Dialog {
                            title: title.clone(),
                            lines: vec!["It's removed from dynamic.toml.".into()],
                            yes: Some((
                                "Remove it".into(),
                                consent(
                                    move |nd| Command::PortRemove {
                                        bridge: b2.clone(),
                                        port: p2.clone(),
                                        non_disruptive: nd,
                                    },
                                    &title,
                                    "Remove it",
                                    &format!("{object} removed"),
                                ),
                            )),
                        });
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            Row::Session(bi, pi, si) => {
                let sess = &s.bridges[bi].ports[pi].sessions[si];
                match code {
                    KeyCode::Char('k') => request(Req {
                        command: Command::SessionKick {
                            session: sess.id.clone(),
                        },
                        then: Then::Done(format!("kicked {}", sess.id)),
                    }),
                    _ => Action::None,
                }
            }
        }
    }

    /// `-` in the TNCs view.
    fn remove_tnc_row(&mut self) -> Action {
        let rows = self.tnc_rows();
        let (Some(i), Some(s)) = (self.tnc_selected_index(&rows), &self.status) else {
            return Action::None;
        };
        match rows[i].0 {
            TncRow::Tnc(t) => {
                let t = &s.tncs[t];
                if !t.editable {
                    self.dialog = Some(static_notice(&t.id, &t.source, "it can't be changed"));
                    return Action::None;
                }
                let id = t.id.clone();
                let mut lines = vec!["It's removed from dynamic.toml.".to_string()];
                if let Some(b) = &t.in_use_by {
                    lines.push(format!(
                        "Bridge {b} uses it, so it can't go until {b} doesn't."
                    ));
                }
                let title = format!("Remove TNC {} ({})?", t.id, t.name);
                self.dialog = Some(Dialog {
                    title: title.clone(),
                    lines,
                    yes: Some((
                        "Remove it".into(),
                        consent(
                            move |nd| Command::TncDelete {
                                tnc: id.clone(),
                                non_disruptive: nd,
                            },
                            &title,
                            "Remove it",
                            &format!("TNC {} removed", t.id),
                        ),
                    )),
                });
            }
            TncRow::Adapter(a) => {
                let a = &s.adapters[a];
                let Some(id) = a.id.clone() else {
                    return self.say(format!("{} has no id to remove", a.mac));
                };
                if a.source.as_deref().is_some_and(|src| src != "dynamic") {
                    let src = a.source.clone().unwrap_or_default();
                    self.dialog = Some(static_notice(&id, &src, "it can't be changed"));
                    return Action::None;
                }
                let title = format!("Remove the adapter id {id}?");
                let id2 = id.clone();
                self.dialog = Some(Dialog {
                    title: title.clone(),
                    lines: vec![format!(
                        "{} stays; the config just stops calling it {id}.",
                        a.mac
                    )],
                    yes: Some((
                        "Remove it".into(),
                        consent(
                            move |nd| Command::AdapterDelete {
                                adapter: id2.clone(),
                                non_disruptive: nd,
                            },
                            &title,
                            "Remove it",
                            &format!("adapter id {id} removed"),
                        ),
                    )),
                });
            }
        }
        Action::None
    }

    /// An error: on the message line, or in a box if it won't fit there.
    pub fn error(&mut self, text: String) {
        if text.chars().count() <= ERROR_LINE {
            self.message = Some((text, true));
        } else {
            self.dialog = Some(Dialog {
                title: "That didn't work".into(),
                lines: vec![text],
                yes: None,
            });
        }
    }

    fn say(&mut self, text: String) -> Action {
        self.message = Some((text, false));
        Action::None
    }

    /// What the daemon answered to a request sent with `then`.
    pub fn on_answer(&mut self, then: Then, res: Result<Value, CtlError>) {
        match (then, res) {
            (Then::Done(text), Ok(_)) | (Then::Consent { done: text, .. }, Ok(_)) => {
                self.message = Some((text, false));
            }
            (
                Then::Consent {
                    title,
                    yes,
                    redo,
                    done,
                    ..
                },
                Err(CtlError::Api(e)),
            ) if e.code == ErrorCode::ReloadWouldDisrupt => {
                let would: Vec<String> = e
                    .data
                    .as_ref()
                    .and_then(|d| d.get("disruptions"))
                    .and_then(|d| serde_json::from_value(d.clone()).ok())
                    .unwrap_or_else(|| vec![e.message.clone()]);
                let mut lines = vec!["This would:".to_string()];
                lines.extend(would.into_iter().map(|w| format!("  • {w}")));
                self.dialog = Some(Dialog {
                    title,
                    lines,
                    yes: Some((
                        yes,
                        Req {
                            command: redo,
                            then: Then::Done(done),
                        },
                    )),
                });
            }
            (
                Then::Consent {
                    switch: Some(bridge),
                    ..
                },
                Err(CtlError::Api(e)),
            ) if e.code == ErrorCode::ConfigInvalid && e.message.contains("conflicts with") => {
                let label = self
                    .status
                    .as_ref()
                    .and_then(|s| s.bridges.iter().find(|b| b.id == bridge))
                    .map(|b| format!("{} ({})", b.id, b.name))
                    .unwrap_or_else(|| bridge.clone());
                self.dialog = Some(Dialog {
                    title: format!("Switch to {label}?"),
                    lines: vec![
                        e.message.clone(),
                        String::new(),
                        "Switching enables it and disables the bridges in its way.".into(),
                    ],
                    yes: Some(("Switch".into(), switch_to(&bridge, &label))),
                });
            }
            (_, Err(e)) => self.error(e.to_string()),
        }
    }
}

/// A change to send with `non_disruptive`, asking before it disconnects
/// anyone.
fn consent(make: impl Fn(bool) -> Command, title: &str, yes: &str, done: &str) -> Req {
    Req {
        command: make(true),
        then: Then::Consent {
            title: title.into(),
            yes: yes.into(),
            redo: make(false),
            done: done.into(),
            switch: None,
        },
    }
}

fn switch_to(bridge: &str, label: &str) -> Req {
    let id = bridge.to_string();
    consent(
        move |nd| Command::BridgeSwitch {
            bridge: id.clone(),
            non_disruptive: nd,
        },
        &format!("Switch to {label}?"),
        "Switch",
        &format!("switched to {bridge}"),
    )
}

/// Why a static object can't be changed here.
fn static_notice(id: &str, source: &str, what_works: &str) -> Dialog {
    let path = source.strip_prefix("static: ").unwrap_or(source);
    Dialog {
        title: format!("{id} is in the config file"),
        lines: vec![
            format!("{id} comes from {path}, so {what_works} from here."),
            "To change it, edit that file, then press R to reload.".into(),
        ],
        yes: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::control::protocol::{
        AdapterStatus, BridgeStatus, Counters, PublicName, SessionStatus, SppStatus, TncStatus,
    };
    use serde_json::json;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    pub(crate) fn port(
        id: &str,
        kind: &str,
        role: &str,
        state: &str,
        settings: serde_json::Value,
    ) -> PortStatus {
        PortStatus {
            id: id.into(),
            kind: kind.into(),
            role: role.into(),
            state: state.into(),
            detail: None,
            since: None,
            settings,
            sessions: Vec::new(),
        }
    }

    pub(crate) fn bridge(
        id: &str,
        name: &str,
        enabled: bool,
        ports: Vec<PortStatus>,
    ) -> BridgeStatus {
        BridgeStatus {
            id: id.into(),
            name: name.into(),
            enabled,
            enabled_override: None,
            public_name: PublicName {
                configured: name.into(),
                effective: name.into(),
            },
            source: "dynamic".into(),
            editable: true,
            incomplete: Vec::new(),
            ports,
            counters: enabled.then_some(Counters {
                frames_up: 37,
                frames_down: 412,
                dropped: 0,
                tnc_connects: 3,
            }),
        }
    }

    /// Two bridges like the dev Pi's: a TH-D75 shared over BLE and TCP
    /// with a phone connected, and a disabled one.
    pub(crate) fn status() -> Status {
        let mut phones = port(
            "phones",
            "ble-server",
            "app",
            "advertising",
            json!({"adapter": "dongle (AC:A7:F1:29:9C:7F)", "max_clients": 2}),
        );
        phones.sessions.push(SessionStatus {
            id: "ktbr1/phones/40:33:E0:1B:DB:76".into(),
            since: "2026-10-27T14:01:33Z".into(),
        });
        let mut radio = port(
            "radio",
            "spp-client",
            "tnc",
            "connected",
            json!({"tnc": "d75", "connect": "always"}),
        );
        radio.since = Some("2026-10-27T12:08:11Z".into());
        Status {
            version: "0.6.0".into(),
            adapters: vec![AdapterStatus {
                id: Some("builtin".into()),
                mac: "2C:CF:67:6E:2E:BC".into(),
                hci: Some("hci0".into()),
                present: true,
                powered: Some(true),
                source: None,
                bus: Some("board".into()),
                product: None,
                usb_id: None,
            }],
            tncs: vec![TncStatus {
                id: "d75".into(),
                name: "TH-D75".into(),
                model: "th-d75".into(),
                spp: Some(SppStatus {
                    address: "40:79:12:E4:95:9D".into(),
                    adapter: "builtin (2C:CF:67:6E:2E:BC)".into(),
                    paired: Some(true),
                }),
                serial: None,
                in_use_by: Some("ktbr1".into()),
                source: "dynamic".into(),
                editable: true,
            }],
            bridges: vec![
                bridge(
                    "ktbr1",
                    "TH-D75",
                    true,
                    vec![
                        radio,
                        phones,
                        port(
                            "lan",
                            "tcp-server",
                            "app",
                            "listening",
                            json!({"listen": "wlan0:8001", "max_clients": 4}),
                        ),
                    ],
                ),
                bridge("ktbr2", "Shack relay", false, Vec::new()),
            ],
        }
    }

    pub(crate) fn frame(object: &str, session: Option<&str>, summary: &str) -> Event {
        Event::Frame {
            object: object.into(),
            dir: if session.is_some() { Dir::Tx } else { Dir::Rx },
            session: session.map(Into::into),
            at: "2026-10-27T14:22:07Z".into(),
            kiss: "00".into(),
            decoded: Decoded {
                channel: 0,
                command: 0,
                ax25: None,
                summary: summary.into(),
            },
        }
    }

    #[test]
    fn the_tree_folds_disabled_bridges_and_keeps_the_selection() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        let ids: Vec<String> = app.rows().into_iter().map(|r| r.1).collect();
        assert_eq!(
            ids,
            [
                "ktbr1",
                "ktbr1/radio",
                "ktbr1/phones",
                "ktbr1/phones/40:33:E0:1B:DB:76",
                "ktbr1/lan",
                "ktbr2"
            ]
        );
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected.as_deref(), Some("ktbr1/phones"));
        // A new status with the session gone keeps the port selected.
        let mut s = status();
        s.bridges[0].ports[1].sessions.clear();
        app.set_status(s);
        assert_eq!(app.selected_row(), Some(Row::Port(0, 1)));
        // ← from a port goes to its bridge, and ← again folds it.
        app.key(key(KeyCode::Left));
        assert_eq!(app.selected.as_deref(), Some("ktbr1"));
        app.key(key(KeyCode::Left));
        assert_eq!(app.rows().len(), 2);
        app.key(key(KeyCode::Right));
        assert_eq!(app.rows().len(), 5);
    }

    #[test]
    fn frames_follow_until_moved_and_filter() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        let now = Instant::now();
        for (i, s) in ["W1AW>APRS:one", "KD2ABC>APRS:two", "W1AW>APRS:three"]
            .iter()
            .enumerate()
        {
            let session = (i == 1).then_some("ktbr1/phones/40:33:E0:1B:DB:76");
            app.on_event(frame("ktbr1/radio", session, s), now);
        }
        let v = app.visible_frames();
        assert_eq!(app.selected_frame(&v), Some(2));
        assert_eq!(v[1].port(), "phones");
        assert_eq!(v[0].port(), "radio");

        app.view = View::Frames;
        app.key(key(KeyCode::Up));
        assert_eq!(app.fv.selected, Some(2));
        // New frames don't move a held selection.
        app.on_event(frame("ktbr1/radio", None, "N1MIE>APRS:four"), now);
        let v = app.visible_frames();
        assert_eq!(v[app.selected_frame(&v).unwrap()].seq, 2);
        app.key(key(KeyCode::End));
        assert_eq!(app.fv.selected, None);

        app.key(key(KeyCode::Char('/')));
        for c in "w1aw".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(app.visible_frames().len(), 2);
        // q while typing a filter is a letter; after, it quits.
        app.key(key(KeyCode::Char('/')));
        assert_eq!(app.key(key(KeyCode::Char('q'))), Action::None);
        app.key(key(KeyCode::Esc));
        assert_eq!(app.visible_frames().len(), 4);

        app.key(key(KeyCode::Char(' ')));
        app.on_event(frame("ktbr1/radio", None, "five"), now);
        assert_eq!(app.visible_frames().len(), 4);
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(app.visible_frames().len(), 5);
        assert_eq!(app.key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn lights_go_out() {
        let mut app = App::new("pi".into());
        let t = Instant::now();
        let ev = Event::Activity {
            object: "ktbr1/radio".into(),
            dir: Dir::Tx,
            bytes: 40,
        };
        assert!(!app.on_event(ev, t));
        assert!(app.lit("ktbr1/radio", Dir::Tx, t));
        assert!(!app.lit("ktbr1/radio", Dir::Rx, t));
        assert!(!app.lit("ktbr1/radio", Dir::Tx, t + LIGHT));
        let state = Event::State {
            what: "session".into(),
            object: "ktbr1/lan/1.2.3.4:5".into(),
            state: "opened".into(),
        };
        assert!(app.on_event(state, t));
    }

    fn select(app: &mut App, id: &str) {
        app.selected = Some(id.into());
    }

    #[test]
    fn disabling_asks_before_disconnecting_anyone() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        select(&mut app, "ktbr1");
        let Action::Request(req) = app.key(key(KeyCode::Char('d'))) else {
            panic!("d sends a request");
        };
        assert_eq!(
            req.command,
            Command::BridgeDisable {
                bridge: "ktbr1".into(),
                non_disruptive: true
            }
        );
        // The daemon says it would disrupt: the dialog lists how.
        let refused = crate::control::protocol::ApiError::new(
            ErrorCode::ReloadWouldDisrupt,
            "this would disrupt",
        )
        .data(json!({"disruptions": ["disconnect session ktbr1/phones/40:33:E0:1B:DB:76"]}));
        app.on_answer(req.then, Err(CtlError::Api(refused)));
        let d = app.dialog.clone().expect("a dialog");
        assert_eq!(d.title, "Disable ktbr1 (TH-D75)?");
        assert_eq!(
            d.lines,
            [
                "This would:",
                "  • disconnect session ktbr1/phones/40:33:E0:1B:DB:76"
            ]
        );
        // n changes nothing; y sends it again without non_disruptive.
        let mut no = App::new("pi".into());
        no.dialog = Some(d);
        assert_eq!(no.key(key(KeyCode::Char('n'))), Action::None);
        assert!(no.dialog.is_none());
        let Action::Request(again) = app.key(key(KeyCode::Char('y'))) else {
            panic!("y sends it again");
        };
        assert_eq!(
            again.command,
            Command::BridgeDisable {
                bridge: "ktbr1".into(),
                non_disruptive: false
            }
        );
        app.on_answer(again.then, Ok(json!({})));
        assert_eq!(app.message, Some(("ktbr1 disabled".into(), false)));
    }

    #[test]
    fn enabling_offers_to_switch_when_a_bridge_is_in_the_way() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        select(&mut app, "ktbr2");
        assert_eq!(app.key(key(KeyCode::Char('d'))), Action::None);
        assert_eq!(
            app.message,
            Some(("ktbr2 is already disabled".into(), false))
        );
        let Action::Request(req) = app.key(key(KeyCode::Char('e'))) else {
            panic!("e sends a request");
        };
        let conflict = crate::control::protocol::ApiError::new(
            ErrorCode::ConfigInvalid,
            "bridge ktbr2: TNC d75 conflicts with bridge ktbr1",
        );
        app.on_answer(req.then, Err(CtlError::Api(conflict)));
        assert_eq!(
            app.dialog.as_ref().map(|d| d.title.as_str()),
            Some("Switch to ktbr2 (Shack relay)?")
        );
        let Action::Request(switch) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter switches");
        };
        assert_eq!(
            switch.command,
            Command::BridgeSwitch {
                bridge: "ktbr2".into(),
                non_disruptive: true
            }
        );
        // Any other failure is said on the message line.
        app.on_answer(
            switch.then,
            Err(CtlError::Api(crate::control::protocol::ApiError::new(
                ErrorCode::ConfigInvalid,
                "bridge ktbr2 has no app port",
            ))),
        );
        assert_eq!(
            app.message,
            Some(("bridge ktbr2 has no app port".into(), true))
        );
    }

    #[test]
    fn ports_sessions_and_static_bridges() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        // TNC keys do nothing on an app port.
        select(&mut app, "ktbr1/lan");
        assert_eq!(app.key(key(KeyCode::Char('c'))), Action::None);
        select(&mut app, "ktbr1/radio");
        let Action::Request(r) = app.key(key(KeyCode::Char('x'))) else {
            panic!("x disconnects");
        };
        assert_eq!(
            r.command,
            Command::PortDisconnect {
                bridge: "ktbr1".into(),
                port: "radio".into()
            }
        );
        select(&mut app, "ktbr1/phones/40:33:E0:1B:DB:76");
        let Action::Request(r) = app.key(key(KeyCode::Char('k'))) else {
            panic!("k kicks");
        };
        assert_eq!(
            r.command,
            Command::SessionKick {
                session: "ktbr1/phones/40:33:E0:1B:DB:76".into()
            }
        );
        // Removing asks first, even when nobody would notice.
        select(&mut app, "ktbr1/lan");
        assert_eq!(app.key(key(KeyCode::Char('-'))), Action::None);
        let Action::Request(r) = app.key(key(KeyCode::Char('y'))) else {
            panic!("y removes");
        };
        assert_eq!(
            r.command,
            Command::PortRemove {
                bridge: "ktbr1".into(),
                port: "lan".into(),
                non_disruptive: true
            }
        );
        // A static bridge says why it can't be removed.
        let mut s = status();
        s.bridges[0].editable = false;
        s.bridges[0].source = "static: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml".into();
        app.set_status(s);
        select(&mut app, "ktbr1");
        app.key(key(KeyCode::Char('-')));
        let d = app.dialog.clone().expect("a notice");
        assert_eq!(d.title, "ktbr1 is in the config file");
        assert!(d.yes.is_none());
        assert!(d.lines[0].contains("/etc/kiss-tnc-bridge/kiss-tnc-bridge.toml"));
        assert_eq!(app.key(key(KeyCode::Char('y'))), Action::None);
        assert!(app.dialog.is_some());
        app.key(key(KeyCode::Enter));
        assert!(app.dialog.is_none());
    }
}
