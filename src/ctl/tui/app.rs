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
use super::super::render::join_and;
use super::add::{Form, Menu};
use crate::ax25::Decoded;
use crate::control::protocol::{
    AdapterStatus, BridgeStatus, BtDevice, Command, Dir, ErrorCode, Event, ModelEntry, PairResult,
    PairingStep, PortStatus, SessionStatus, Status, TncStatus,
};

/// Frames kept for the Frames view.
pub const FRAMES_KEPT: usize = 1000;
/// How long a scan looks, as in `setup`.
pub const SCAN_SECS: u64 = 60;
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
    /// Step aside for `ktbctl ARGS` (setup, edit), and come back.
    Run(Vec<String>),
}

pub(super) fn request(req: Req) -> Action {
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
    /// Nothing to say when it works: a pairing reply, stopping a scan.
    Ack,
    /// The TNC models, for how to put each radio in pairing mode.
    Models,
    /// The answer ends the pairing in `App::pairing`.
    Pair,
    /// The answer ends the scan in `App::scan`.
    Scan,
    /// Sent from the form in `App::form`: a mistake goes back to it, to be
    /// put right; otherwise as `Then`.
    Form(Box<Then>),
}

/// A question or a notice over the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<String>,
    /// The yes button's words and what it does; none for a notice.
    pub yes: Option<(String, Req)>,
}

/// What a pairing waits for from the person at the radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Does the radio show this passkey?
    Confirm(String),
    /// May the device pair?
    Authorize,
    Pin,
    Passkey,
}

/// A pairing under way, in a box of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Pairing {
    pub title: String,
    /// The `bluetooth.pair` request, once sent; replies name it.
    pub request: Option<u64>,
    /// What's happened so far, a line each.
    pub steps: Vec<String>,
    pub ask: Option<Ask>,
    /// A PIN or passkey being typed.
    pub typed: String,
    /// How it ended, and whether it worked.
    pub end: Option<(String, bool)>,
    /// The bridge using the TNC, to say it can connect now.
    pub bridge: Option<String>,
    /// Esc put it out of the way. It goes on, comes back if it needs an
    /// answer, and says how it ended on the message line.
    pub hidden: bool,
}

/// A Bluetooth scan, in a box of its own, filling in as devices are found.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    /// `Scan on builtin (hci0)`.
    pub title: String,
    /// What to tell the person at the radio, above the list.
    pub hint: Vec<String>,
    /// Pairing a TNC: look for its radio, and pair as soon as it turns up.
    pub target: Option<TncStatus>,
    /// What `bluetooth.scan` was given; none for the default adapter.
    pub adapter: Option<String>,
    /// The `bluetooth.scan` request, once sent; stopping it names it.
    pub request: Option<u64>,
    pub started: Instant,
    /// Likely TNCs first, then as found.
    pub devices: Vec<BtDevice>,
    /// The selected device's address; none for the first.
    pub selected: Option<String>,
    /// Time ran out, or it was stopped.
    pub over: bool,
    pub error: Option<String>,
    /// Pair with this once the scan lets go of the adapter.
    pub then_pair: Option<BtDevice>,
}

impl Scan {
    fn new(title: String, adapter: Option<String>) -> Scan {
        Scan {
            title,
            hint: vec!["Radios have to be in pairing mode to be found.".into()],
            target: None,
            adapter,
            request: None,
            started: Instant::now(),
            devices: Vec::new(),
            selected: None,
            over: false,
            error: None,
            then_pair: None,
        }
    }

    /// The same scan, from the start.
    fn again(&self) -> Scan {
        Scan {
            hint: self.hint.clone(),
            target: self.target.clone(),
            ..Scan::new(self.title.clone(), self.adapter.clone())
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        if self.devices.is_empty() {
            return None;
        }
        Some(
            self.selected
                .as_ref()
                .and_then(|a| self.devices.iter().position(|d| d.address == *a))
                .unwrap_or(0),
        )
    }

    fn command(&self) -> Req {
        Req {
            command: Command::BluetoothScan {
                adapter: self.adapter.clone(),
                seconds: Some(SCAN_SECS),
            },
            then: Then::Scan,
        }
    }
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
    /// The TNC models, for how to put a radio in pairing mode.
    pub models: Vec<ModelEntry>,
    pub pairing: Option<Pairing>,
    pub scan: Option<Scan>,
    /// `a`'s menu.
    pub menu: Option<Menu>,
    pub form: Option<Form>,
    /// The bridges there were before something that adds one; the first
    /// new one gets selected.
    pub new_since: Option<Vec<String>>,
}

impl App {
    pub fn new(host: String) -> App {
        App {
            view: View::Bridges,
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
            models: Vec::new(),
            pairing: None,
            scan: None,
            menu: None,
            form: None,
            new_since: None,
        }
    }

    pub fn set_status(&mut self, s: Status) {
        if let Some(before) = &self.new_since
            && let Some(b) = s.bridges.iter().find(|b| !before.contains(&b.id))
        {
            self.selected = Some(b.id.clone());
            self.view = View::Bridges;
            self.new_since = None;
        }
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
            // These come on the requests connection: `on_own_event`.
            Event::Pairing { .. } | Event::Found { .. } | Event::Lagged { .. } => false,
        }
    }

    /// An event sent only to the connection that asked for it: a pairing's
    /// steps and a scan's finds. It may send something back.
    pub fn on_own_event(&mut self, ev: Event) -> Action {
        match ev {
            Event::Pairing { request, step } => {
                self.pairing_step(request, step);
                Action::None
            }
            Event::Found { request, device } => self.found(request, device),
            _ => Action::None,
        }
    }

    /// A request went to the daemon as `id`.
    pub fn sent(&mut self, id: u64, then: &Then) {
        match then {
            Then::Pair => {
                if let Some(p) = &mut self.pairing {
                    p.request = Some(id);
                }
            }
            Then::Scan => {
                if let Some(s) = &mut self.scan {
                    s.request = Some(id);
                }
            }
            _ => {}
        }
    }

    /// The connection to the daemon went, taking a pairing or scan with it.
    pub fn lost(&mut self, why: &str) {
        if let Some(p) = &mut self.pairing
            && p.end.is_none()
        {
            p.ask = None;
            p.hidden = false;
            p.end = Some((why.to_string(), false));
        }
        if let Some(s) = &mut self.scan
            && !s.over
        {
            s.over = true;
            s.then_pair = None;
            s.error = Some(why.to_string());
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
        if self.menu.is_some() {
            return self.menu_key(k);
        }
        if self.form.is_some() {
            return self.form_key(k);
        }
        if self.pairing.as_ref().is_some_and(|p| !p.hidden) {
            return self.pairing_key(k);
        }
        if self.scan.is_some() {
            return self.scan_key(k);
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
            // Esc that isn't closing something quits, as q does.
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('1') => self.view = View::Bridges,
            KeyCode::Char('2') => self.view = View::Tncs,
            KeyCode::Char('3') | KeyCode::Char('f') => self.view = View::Frames,
            KeyCode::Char('a') if self.view != View::Frames => return self.add_menu(),
            KeyCode::Char('E') if self.view != View::Frames => return self.edit(),
            // All of dynamic.toml, whatever's selected.
            KeyCode::Char('C') if self.view != View::Frames => {
                return Action::Run(vec!["edit".into()]);
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
                    KeyCode::Char('p') => return self.pair_tnc(),
                    KeyCode::Char('u') => return self.unpair_tnc(),
                    KeyCode::Char('n') => return self.start_scan(),
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

    // ── Pairing and scanning ────────────────────────────────────────────

    /// The TNC selected in the TNCs view.
    pub fn selected_tnc(&self) -> Option<&TncStatus> {
        let rows = self.tnc_rows();
        match rows.get(self.tnc_selected_index(&rows)?)?.0 {
            TncRow::Tnc(i) => self.status.as_ref()?.tncs.get(i),
            TncRow::Adapter(_) => None,
        }
    }

    /// `p` in the TNCs view.
    fn pair_tnc(&mut self) -> Action {
        let Some(t) = self.selected_tnc().cloned() else {
            return self.say("select a TNC to pair with it".into());
        };
        if self.pairing.is_some() {
            return self.say("a pairing is under way".into());
        }
        let Some(spp) = &t.spp else {
            return self.say(format!("{} has no Bluetooth to pair", t.id));
        };
        if spp.paired == Some(true) {
            return self.say(format!("{} is already paired; u unpairs it", t.id));
        }
        // Look for it in the open, as setup does, and pair when it turns up.
        let (adapter, on) = match self.scan_target() {
            Ok(x) => x,
            Err(why) => return self.say(why),
        };
        let mut scan = Scan::new(
            format!("Pair {} ({}): looking on {on}", t.id, t.name),
            adapter,
        );
        scan.hint = vec![
            self.pairing_mode(&t.model),
            format!("Pairing starts when {} turns up.", spp.address),
        ];
        scan.target = Some(t);
        let req = scan.command();
        self.scan = Some(scan);
        request(req)
    }

    /// Pair TNC `t`, its radio found.
    fn pair_found_tnc(&mut self, t: TncStatus) -> Action {
        self.start_pairing(
            format!("Pair {} ({})", t.id, t.name),
            Some(&t.model),
            t.in_use_by.clone(),
            Command::BluetoothPair {
                tnc: Some(t.id.clone()),
                address: None,
                adapter: None,
            },
        )
    }

    /// How to put a radio of `model` in pairing mode.
    fn pairing_mode(&self, model: &str) -> String {
        let how = self
            .models
            .iter()
            .flat_map(|e| &e.radios)
            .find(|r| r.model == model)
            .and_then(|r| r.bluetooth.as_ref());
        match how {
            Some(b) => format!("Put the radio in pairing mode: {}", b.pairing_mode),
            None => "Put the radio in pairing mode (discoverable) if it isn't.".into(),
        }
    }

    fn start_pairing(
        &mut self,
        title: String,
        model: Option<&str>,
        bridge: Option<String>,
        command: Command,
    ) -> Action {
        let first = match model {
            Some(m) => self.pairing_mode(m),
            None => "Put the radio in pairing mode (discoverable) if it isn't.".into(),
        };
        self.pairing = Some(Pairing {
            title,
            request: None,
            steps: vec![first],
            ask: None,
            typed: String::new(),
            end: None,
            bridge,
            hidden: false,
        });
        request(Req {
            command,
            then: Then::Pair,
        })
    }

    fn pairing_step(&mut self, request: u64, step: PairingStep) {
        let Some(p) = &mut self.pairing else {
            return;
        };
        if p.request != Some(request) {
            return;
        }
        let line = match &step {
            PairingStep::Searching => Some("Looking for it...".to_string()),
            PairingStep::Pairing { name } => Some(format!(
                "Pairing with {}...",
                name.as_deref().unwrap_or("it")
            )),
            PairingStep::ShowPin { pin } => Some(format!("Enter {pin} on the device.")),
            PairingStep::ShowPasskey { passkey } => Some(format!("Enter {passkey} on the device.")),
            _ => None,
        };
        p.steps.extend(line);
        let ask = match step {
            PairingStep::Confirm { passkey } => Some(Ask::Confirm(passkey)),
            PairingStep::Authorize => Some(Ask::Authorize),
            PairingStep::EnterPin => Some(Ask::Pin),
            PairingStep::EnterPasskey => Some(Ask::Passkey),
            _ => None,
        };
        if ask.is_some() {
            p.ask = ask;
            p.typed.clear();
            p.hidden = false;
        }
    }

    fn pairing_key(&mut self, k: KeyEvent) -> Action {
        let Some(p) = &mut self.pairing else {
            return Action::None;
        };
        if p.end.is_some() {
            if matches!(k.code, KeyCode::Enter | KeyCode::Esc) {
                self.pairing = None;
            }
            return Action::None;
        }
        let Some(ask) = p.ask.clone() else {
            if k.code == KeyCode::Esc {
                p.hidden = true;
                return self.say("the pairing goes on; how it ends shows here".into());
            }
            return Action::None;
        };
        let Some(id) = p.request else {
            return Action::None;
        };
        let reply = |accept, pin, passkey| {
            request(Req {
                command: Command::BluetoothPairReply {
                    request: id,
                    accept,
                    pin,
                    passkey,
                },
                then: Then::Ack,
            })
        };
        match (ask, k.code) {
            (Ask::Confirm(_) | Ask::Authorize, KeyCode::Char('y') | KeyCode::Enter) => {
                if let Some(Ask::Confirm(_)) = p.ask {
                    p.steps.push("Now confirm it on the radio too.".into());
                }
                p.ask = None;
                reply(Some(true), None, None)
            }
            (Ask::Confirm(_) | Ask::Authorize, KeyCode::Char('n') | KeyCode::Esc) => {
                p.ask = None;
                reply(Some(false), None, None)
            }
            (Ask::Pin | Ask::Passkey, KeyCode::Esc) => {
                p.ask = None;
                reply(None, None, None)
            }
            (Ask::Pin | Ask::Passkey, KeyCode::Backspace) => {
                p.typed.pop();
                Action::None
            }
            (Ask::Pin, KeyCode::Char(c)) => {
                p.typed.push(c);
                Action::None
            }
            (Ask::Passkey, KeyCode::Char(c)) if c.is_ascii_digit() => {
                p.typed.push(c);
                Action::None
            }
            (Ask::Pin, KeyCode::Enter) => {
                p.ask = None;
                reply(None, Some(std::mem::take(&mut p.typed)), None)
            }
            (Ask::Passkey, KeyCode::Enter) => match p.typed.parse::<u32>() {
                Ok(n) => {
                    p.ask = None;
                    p.typed.clear();
                    reply(None, None, Some(n))
                }
                Err(_) => Action::None,
            },
            _ => Action::None,
        }
    }

    /// `u` in the TNCs view. Always asks: pairing again needs someone at
    /// the radio.
    fn unpair_tnc(&mut self) -> Action {
        let Some(t) = self.selected_tnc().cloned() else {
            return self.say("select a TNC to unpair it".into());
        };
        let Some(spp) = &t.spp else {
            return self.say(format!("{} has no Bluetooth to unpair", t.id));
        };
        if spp.paired == Some(false) {
            return self.say(format!("{} isn't paired", t.id));
        }
        let links = self
            .status
            .as_ref()
            .map(|s| links_to(s, &t.id))
            .unwrap_or_default();
        let mut lines = Vec::new();
        if !links.is_empty() {
            lines.push(format!("This drops the link on {}.", join_and(&links)));
        } else if let Some(b) = &t.in_use_by {
            lines.push(format!(
                "Bridge {b} uses it, and can't connect to it until it's paired again."
            ));
        }
        lines.push("Pairing again needs the radio in pairing mode.".into());
        self.dialog = Some(Dialog {
            title: format!("Unpair {} ({})?", t.id, t.name),
            lines,
            yes: Some((
                "Unpair it".into(),
                Req {
                    command: Command::BluetoothUnpair {
                        tnc: Some(t.id.clone()),
                        address: None,
                        adapter: None,
                    },
                    then: Then::Done(format!("{} unpaired", t.id)),
                },
            )),
        });
        Action::None
    }

    /// `n` in the TNCs view: scan on the selected adapter, or the selected
    /// TNC's.
    fn start_scan(&mut self) -> Action {
        if self.pairing.is_some() {
            return self.say("a pairing is under way".into());
        }
        let (adapter, on) = match self.scan_target() {
            Ok(t) => t,
            Err(why) => return self.say(why),
        };
        let scan = Scan::new(format!("Scan on {on}"), adapter);
        let req = scan.command();
        self.scan = Some(scan);
        request(req)
    }

    /// The adapter to scan on, as `bluetooth.scan` takes it, and as people
    /// know it.
    fn scan_target(&self) -> Result<(Option<String>, String), String> {
        let Some(s) = &self.status else {
            return Err("not connected to the daemon".into());
        };
        let rows = self.tnc_rows();
        let row = self.tnc_selected_index(&rows).map(|i| rows[i].0);
        let present: Vec<&AdapterStatus> = s.adapters.iter().filter(|a| a.present).collect();
        match row {
            Some(TncRow::Adapter(i)) => {
                let a = &s.adapters[i];
                if !a.present {
                    return Err(format!("{} isn't present", adapter_label(a)));
                }
                Ok((Some(a.mac.clone()), adapter_label(a)))
            }
            Some(TncRow::Tnc(i)) if s.tncs[i].spp.is_some() => {
                let conf = &s.tncs[i].spp.as_ref().expect("checked").adapter;
                // `builtin (2C:CF:…)`, a MAC, `hci0`, or `default adapter`.
                let sel = match conf.rsplit_once(" (") {
                    Some((_, mac)) => Some(mac.trim_end_matches(')').to_string()),
                    None if conf == "default adapter" => None,
                    None => Some(conf.clone()),
                };
                let a = sel.as_ref().and_then(|x| {
                    s.adapters
                        .iter()
                        .find(|a| a.mac == *x || a.hci.as_deref() == Some(x))
                });
                match (sel, a) {
                    (Some(_), Some(a)) if !a.present => {
                        Err(format!("{} isn't present", adapter_label(a)))
                    }
                    (Some(sel), Some(a)) => Ok((Some(sel), adapter_label(a))),
                    (Some(sel), None) => Ok((Some(sel.clone()), sel)),
                    (None, _) => Ok((None, "the default adapter".into())),
                }
            }
            _ => match present.as_slice() {
                [] => Err("no Bluetooth adapter is present".into()),
                [a] => Ok((Some(a.mac.clone()), adapter_label(a))),
                _ => Ok((None, "the default adapter".into())),
            },
        }
    }

    fn found(&mut self, request: u64, device: BtDevice) -> Action {
        let Some(s) = &mut self.scan else {
            return Action::None;
        };
        if s.request != Some(request) {
            return Action::None;
        }
        let wanted = s
            .target
            .as_ref()
            .and_then(|t| t.spp.as_ref())
            .is_some_and(|spp| spp.address.eq_ignore_ascii_case(&device.address));
        if wanted {
            s.selected = Some(device.address.clone());
        }
        match s.devices.iter_mut().find(|d| d.address == device.address) {
            Some(d) => *d = device.clone(),
            None => s.devices.push(device.clone()),
        }
        s.devices.sort_by_key(|d| d.known_tnc.is_none());
        if wanted && !s.over && s.then_pair.is_none() {
            // The scan holds the adapter: stop it, and pair when it's
            // answered.
            s.then_pair = Some(device);
            return stop_scan(s);
        }
        Action::None
    }

    fn scan_key(&mut self, k: KeyEvent) -> Action {
        let Some(s) = &mut self.scan else {
            return Action::None;
        };
        let at = s.selected_index();
        let stop = stop_scan;
        match k.code {
            KeyCode::Up | KeyCode::Down => {
                if let Some(at) = at {
                    let to = if k.code == KeyCode::Up {
                        at.saturating_sub(1)
                    } else {
                        (at + 1).min(s.devices.len() - 1)
                    };
                    s.selected = Some(s.devices[to].address.clone());
                }
                Action::None
            }
            // Looking for a TNC's radio, pairing starts on its own.
            KeyCode::Enter if s.target.is_some() => Action::None,
            KeyCode::Enter => {
                let Some(d) = at.map(|i| s.devices[i].clone()) else {
                    return Action::None;
                };
                if s.over {
                    let adapter = s.adapter.clone();
                    self.scan = None;
                    self.pair_device(d, adapter)
                } else if s.then_pair.is_none() {
                    // The scan holds the adapter until it ends, and pairing
                    // needs it: stop it, and pair when it's answered.
                    s.then_pair = Some(d);
                    stop(s)
                } else {
                    Action::None
                }
            }
            KeyCode::Esc => {
                let a = if s.over { Action::None } else { stop(s) };
                self.scan = None;
                a
            }
            // Hand it to setup, as a new TNC. Closing the connection on
            // the way out stops the scan, and lets go of the adapter.
            KeyCode::Char('a') if s.target.is_none() => {
                let Some(d) = at.map(|i| s.devices[i].clone()) else {
                    return Action::None;
                };
                let mut args = vec!["setup".to_string(), "--address".into(), d.address];
                if let Some(a) = &s.adapter {
                    args.extend(["--adapter".into(), a.clone()]);
                }
                if let Some(n) = d.name {
                    args.extend(["--name".into(), n]);
                }
                if let Some(m) = d.model {
                    args.extend(["--model".into(), m]);
                }
                if d.paired {
                    args.push("--paired".into());
                }
                self.scan = None;
                self.new_since = self
                    .status
                    .as_ref()
                    .map(|s| s.bridges.iter().map(|b| b.id.clone()).collect());
                Action::Run(args)
            }
            KeyCode::Char('n') if s.over => {
                let scan = s.again();
                let req = scan.command();
                *s = scan;
                request(req)
            }
            _ => Action::None,
        }
    }

    /// Pair with a device a scan found.
    fn pair_device(&mut self, d: BtDevice, adapter: Option<String>) -> Action {
        let bridge = d.tnc.as_ref().and_then(|t| {
            self.status
                .as_ref()?
                .tncs
                .iter()
                .find(|x| x.id == *t)?
                .in_use_by
                .clone()
        });
        let title = match &d.name {
            Some(n) => format!("Pair {n} ({})", d.address),
            None => format!("Pair {}", d.address),
        };
        self.start_pairing(
            title,
            d.model.as_deref(),
            bridge,
            Command::BluetoothPair {
                tnc: None,
                address: Some(d.address.clone()),
                adapter,
            },
        )
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

    /// `E`: the selected bridge, TNC or adapter in `$EDITOR`, by
    /// `ktbctl edit`; all of dynamic.toml with nothing selected.
    fn edit(&mut self) -> Action {
        let Some(s) = &self.status else {
            return Action::None;
        };
        let (args, source, editable) = match self.view {
            View::Tncs => {
                let rows = self.tnc_rows();
                match self.tnc_selected_index(&rows).map(|i| rows[i].0) {
                    Some(TncRow::Tnc(i)) => {
                        let t = &s.tncs[i];
                        (
                            vec!["tnc".into(), t.id.clone()],
                            t.source.clone(),
                            t.editable,
                        )
                    }
                    Some(TncRow::Adapter(i)) => {
                        let a = &s.adapters[i];
                        let Some(id) = a.id.clone() else {
                            return self.say(format!("{} has no id to edit; a names it", a.mac));
                        };
                        let source = a.source.clone().unwrap_or_default();
                        let editable = source == "dynamic";
                        (vec!["adapter".into(), id], source, editable)
                    }
                    None => (Vec::new(), String::new(), true),
                }
            }
            _ => match self.selected_bridge() {
                Some(b) => (vec![b.id.clone()], b.source.clone(), b.editable),
                None => (Vec::new(), String::new(), true),
            },
        };
        if !editable {
            let id = args.last().cloned().unwrap_or_default();
            self.dialog = Some(static_notice(&id, &source, "it can't be edited"));
            return Action::None;
        }
        Action::Run(std::iter::once("edit".to_string()).chain(args).collect())
    }

    pub(super) fn say(&mut self, text: String) -> Action {
        self.message = Some((text, false));
        Action::None
    }

    /// What the daemon answered to a request sent with `then`, and anything
    /// that sends next.
    pub fn on_answer(&mut self, then: Then, res: Result<Value, CtlError>) -> Action {
        match (then, res) {
            (Then::Done(text), Ok(_)) | (Then::Consent { done: text, .. }, Ok(_)) => {
                self.message = Some((text, false));
            }
            (Then::Ack, Ok(_)) => {}
            (Then::Models, Ok(v)) => self.models = serde_json::from_value(v).unwrap_or_default(),
            // Without them, pairing just says less.
            (Then::Models, Err(_)) => {}
            (Then::Pair, res) => self.paired(res),
            // A mistake in a form goes back to it, to be put right.
            (Then::Form(_), Err(CtlError::Api(e)))
                if e.code != ErrorCode::ReloadWouldDisrupt && self.form.is_some() =>
            {
                if let Some(f) = &mut self.form {
                    f.error = Some(e.message.clone());
                }
            }
            (Then::Form(then), res) => {
                self.form = None;
                return self.on_answer(*then, res);
            }
            (Then::Scan, res) => return self.scanned(res),
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
        Action::None
    }

    /// The answer to `bluetooth.pair`.
    fn paired(&mut self, res: Result<Value, CtlError>) {
        let Some(p) = &mut self.pairing else {
            return;
        };
        let end = match res {
            Ok(v) => {
                let on = serde_json::from_value::<PairResult>(v)
                    .map(|r| format!(" on {}", r.adapter))
                    .unwrap_or_default();
                let mut text = format!("Paired and trusted{on}.");
                if let Some(b) = &p.bridge {
                    text.push_str(&format!(" Bridge {b} can connect to it now."));
                }
                (text, true)
            }
            Err(e) => (e.to_string(), false),
        };
        p.ask = None;
        if p.hidden {
            self.pairing = None;
            self.message = Some((end.0, !end.1));
        } else {
            p.end = Some(end);
        }
    }

    /// The answer to `bluetooth.scan`: the whole list, likely TNCs first.
    fn scanned(&mut self, res: Result<Value, CtlError>) -> Action {
        let Some(s) = &mut self.scan else {
            return Action::None;
        };
        s.over = true;
        match res.and_then(|v| {
            serde_json::from_value::<Vec<BtDevice>>(v)
                .map_err(|e| CtlError::Protocol(e.to_string()))
        }) {
            Ok(list) => s.devices = list,
            Err(e) => {
                s.then_pair = None;
                s.error = Some(e.to_string());
            }
        }
        match (s.then_pair.take(), s.target.clone()) {
            (Some(_), Some(t)) => {
                self.scan = None;
                self.pair_found_tnc(t)
            }
            (Some(d), None) => {
                let adapter = s.adapter.clone();
                self.scan = None;
                self.pair_device(d, adapter)
            }
            (None, Some(t)) => {
                if s.error.is_none() {
                    let addr = t.spp.map(|spp| spp.address).unwrap_or_default();
                    s.error = Some(format!(
                        "{addr} didn't turn up in {SCAN_SECS} s. Is the radio on, nearby, and in pairing mode?"
                    ));
                }
                Action::None
            }
            (None, None) => Action::None,
        }
    }
}

/// Stop scan `s`, if it's been sent.
fn stop_scan(s: &Scan) -> Action {
    match s.request {
        Some(id) => request(Req {
            command: Command::BluetoothScanStop { request: id },
            then: Then::Ack,
        }),
        None => Action::None,
    }
}

/// The ports with a link up to TNC `tnc`: `ktbr1/radio`.
pub fn links_to(s: &Status, tnc: &str) -> Vec<String> {
    s.bridges
        .iter()
        .flat_map(|b| b.ports.iter().map(move |p| (b, p)))
        .filter(|(_, p)| {
            p.state == "connected" && p.settings.get("tnc").and_then(|v| v.as_str()) == Some(tnc)
        })
        .map(|(b, p)| format!("{}/{}", b.id, p.id))
        .collect()
}

/// An adapter as people know it: `builtin (hci0)`.
pub fn adapter_label(a: &AdapterStatus) -> String {
    match (&a.id, &a.hci) {
        (Some(id), Some(hci)) => format!("{id} ({hci})"),
        (Some(id), None) => id.clone(),
        (None, Some(hci)) => format!("{hci} ({})", a.mac),
        (None, None) => a.mac.clone(),
    }
}

/// A change to send with `non_disruptive`, asking before it disconnects
/// anyone.
pub(super) fn consent(make: impl Fn(bool) -> Command, title: &str, yes: &str, done: &str) -> Req {
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
    fn d75_models() -> Vec<ModelEntry> {
        serde_json::from_value(json!([{
            "names": ["th-d75"],
            "summary": "Kenwood TH-D74 and TH-D75",
            "radios": [{
                "name": "Kenwood TH-D75",
                "model": "th-d75",
                "bluetooth": {
                    "name_prefix": "TH-D75",
                    "setup": "Turn Bluetooth on.",
                    "pairing_mode": "Menu > Bluetooth > Pairing Mode",
                    "pairing": "Check the number."
                }
            }]
        }]))
        .unwrap()
    }

    fn device(address: &str, name: &str, known: Option<&str>) -> BtDevice {
        BtDevice {
            address: address.into(),
            name: Some(name.into()),
            rssi: Some(-60),
            icon: Some("phone".into()),
            paired: false,
            known_tnc: known.map(Into::into),
            model: known.map(|_| "th-d75".into()),
            tnc: None,
        }
    }

    /// An app with d75 unpaired, in the TNCs view.
    fn unpaired() -> App {
        let mut app = App::new("pi".into());
        let mut s = status();
        s.tncs[0].spp.as_mut().unwrap().paired = Some(false);
        app.set_status(s);
        app.view = View::Tncs;
        app
    }

    /// Press p, and find the radio: the request that pairs it, sent as `id`.
    fn pair_d75(app: &mut App, id: u64) -> Req {
        let Action::Request(look) = app.key(key(KeyCode::Char('p'))) else {
            panic!("p looks for the radio");
        };
        assert_eq!(
            look.command,
            Command::BluetoothScan {
                adapter: Some("2C:CF:67:6E:2E:BC".into()),
                seconds: Some(SCAN_SECS)
            }
        );
        app.sent(id - 1, &look.then);
        let found = |device| Event::Found {
            request: id - 1,
            device,
        };
        // Something else turns up first: it's listed, and that's all.
        assert_eq!(
            app.on_own_event(found(device("50:3B:70:E1:1C:11", "jPhone", None))),
            Action::None
        );
        // ⏎ doesn't pick another device: it's looking for d75's radio.
        assert_eq!(app.key(key(KeyCode::Enter)), Action::None);
        let Action::Request(stop) = app.on_own_event(found(device(
            "40:79:12:E4:95:9D",
            "TH-D75",
            Some("Kenwood TH-D75"),
        ))) else {
            panic!("finding it stops the scan");
        };
        assert_eq!(stop.command, Command::BluetoothScanStop { request: id - 1 });
        let Action::Request(pair) = app.on_answer(Then::Scan, Ok(json!([]))) else {
            panic!("the scan's end starts the pairing");
        };
        assert_eq!(
            pair.command,
            Command::BluetoothPair {
                tnc: Some("d75".into()),
                address: None,
                adapter: None
            }
        );
        assert!(app.scan.is_none());
        app.sent(id, &pair.then);
        *pair
    }

    #[test]
    fn p_waits_for_the_radio_to_turn_up() {
        let mut app = unpaired();
        app.on_answer(
            Then::Models,
            Ok(serde_json::to_value(d75_models()).unwrap()),
        );
        app.key(key(KeyCode::Char('p')));
        let sc = app.scan.clone().expect("a scan");
        assert_eq!(sc.title, "Pair d75 (TH-D75): looking on builtin (hci0)");
        assert_eq!(
            sc.hint,
            [
                "Put the radio in pairing mode: Menu > Bluetooth > Pairing Mode",
                "Pairing starts when 40:79:12:E4:95:9D turns up."
            ]
        );
        app.sent(4, &Then::Scan);
        // Time runs out: it says so, and n looks again.
        assert_eq!(app.on_answer(Then::Scan, Ok(json!([]))), Action::None);
        let sc = app.scan.clone().unwrap();
        assert!(sc.over);
        assert!(
            sc.error
                .unwrap()
                .starts_with("40:79:12:E4:95:9D didn't turn up")
        );
        let Action::Request(again) = app.key(key(KeyCode::Char('n'))) else {
            panic!("n looks again");
        };
        assert!(matches!(again.command, Command::BluetoothScan { .. }));
        assert!(app.scan.as_ref().unwrap().target.is_some());
    }

    #[test]
    fn pairing_walks_through_the_steps() {
        let mut app = unpaired();
        app.on_answer(
            Then::Models,
            Ok(serde_json::to_value(d75_models()).unwrap()),
        );
        let req = pair_d75(&mut app, 7);
        let step = |step| Event::Pairing { request: 7, step };
        app.on_own_event(step(PairingStep::Pairing {
            name: Some("TH-D75".into()),
        }));
        // Another connection's pairing isn't this one's.
        app.on_own_event(Event::Pairing {
            request: 8,
            step: PairingStep::Searching,
        });
        let p = app.pairing.clone().unwrap();
        assert_eq!(p.title, "Pair d75 (TH-D75)");
        assert_eq!(
            p.steps,
            [
                "Put the radio in pairing mode: Menu > Bluetooth > Pairing Mode",
                "Pairing with TH-D75..."
            ]
        );
        // Esc while it works puts the box away; a question brings it back.
        assert_eq!(app.key(key(KeyCode::Esc)), Action::None);
        assert!(app.pairing.as_ref().unwrap().hidden);
        app.on_own_event(step(PairingStep::Confirm {
            passkey: "774523".into(),
        }));
        assert!(!app.pairing.as_ref().unwrap().hidden);
        // Keys answer the box, not the view behind it.
        assert_eq!(app.key(key(KeyCode::Char('u'))), Action::None);
        let Action::Request(reply) = app.key(key(KeyCode::Char('y'))) else {
            panic!("y answers");
        };
        assert_eq!(
            reply.command,
            Command::BluetoothPairReply {
                request: 7,
                accept: Some(true),
                pin: None,
                passkey: None
            }
        );
        app.on_answer(
            req.then,
            Ok(json!({"address": "40:79:12:E4:95:9D", "adapter": "hci0 (2C:CF:67:6E:2E:BC)", "paired": true})),
        );
        let p = app.pairing.clone().unwrap();
        assert_eq!(
            p.end,
            Some((
                "Paired and trusted on hci0 (2C:CF:67:6E:2E:BC). Bridge ktbr1 can connect to it now."
                    .into(),
                true
            ))
        );
        app.key(key(KeyCode::Enter));
        assert!(app.pairing.is_none());
        // A paired TNC says so.
        app.set_status(status());
        assert_eq!(app.key(key(KeyCode::Char('p'))), Action::None);
        assert_eq!(
            app.message,
            Some(("d75 is already paired; u unpairs it".into(), false))
        );
    }

    #[test]
    fn a_passkey_is_typed_and_a_lost_daemon_ends_the_pairing() {
        let mut app = unpaired();
        pair_d75(&mut app, 3);
        // Without the models, it says what it can.
        assert_eq!(
            app.pairing.as_ref().unwrap().steps[0],
            "Put the radio in pairing mode (discoverable) if it isn't."
        );
        app.on_own_event(Event::Pairing {
            request: 3,
            step: PairingStep::EnterPasskey,
        });
        for c in ['1', 'x', '2', '3'] {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Backspace));
        assert_eq!(app.pairing.as_ref().unwrap().typed, "12");
        let Action::Request(reply) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter sends it");
        };
        assert_eq!(
            reply.command,
            Command::BluetoothPairReply {
                request: 3,
                accept: None,
                pin: None,
                passkey: Some(12)
            }
        );
        app.lost("the daemon closed the connection");
        assert_eq!(
            app.pairing.as_ref().unwrap().end,
            Some(("the daemon closed the connection".into(), false))
        );
    }

    #[test]
    fn a_scan_fills_in_and_pairs_once_stopped() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        app.on_answer(
            Then::Models,
            Ok(serde_json::to_value(d75_models()).unwrap()),
        );
        app.view = View::Tncs;
        // The adapter row: scan on it.
        app.key(key(KeyCode::Down));
        let Action::Request(req) = app.key(key(KeyCode::Char('n'))) else {
            panic!("n scans");
        };
        assert_eq!(
            req.command,
            Command::BluetoothScan {
                adapter: Some("2C:CF:67:6E:2E:BC".into()),
                seconds: Some(SCAN_SECS)
            }
        );
        assert_eq!(app.scan.as_ref().unwrap().title, "Scan on builtin (hci0)");
        app.sent(9, &req.then);
        let found = |device| Event::Found { request: 9, device };
        app.on_own_event(found(device("50:3B:70:E1:1C:11", "jPhone", None)));
        app.on_own_event(found(device(
            "40:79:12:E4:95:9D",
            "TH-D75",
            Some("Kenwood TH-D75"),
        )));
        // Likely TNCs first, and the first is selected.
        let sc = app.scan.clone().unwrap();
        assert_eq!(sc.devices[0].address, "40:79:12:E4:95:9D");
        assert_eq!(sc.selected_index(), Some(0));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        assert_eq!(
            app.scan.as_ref().unwrap().selected.as_deref(),
            Some("50:3B:70:E1:1C:11")
        );
        app.key(key(KeyCode::Up));
        // ⏎ stops the scan first: pairing needs the adapter.
        let Action::Request(stop) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter stops the scan");
        };
        assert_eq!(stop.command, Command::BluetoothScanStop { request: 9 });
        let list = vec![device(
            "40:79:12:E4:95:9D",
            "TH-D75",
            Some("Kenwood TH-D75"),
        )];
        let Action::Request(pair) =
            app.on_answer(Then::Scan, Ok(serde_json::to_value(list).unwrap()))
        else {
            panic!("the scan's end starts the pairing");
        };
        assert_eq!(
            pair.command,
            Command::BluetoothPair {
                tnc: None,
                address: Some("40:79:12:E4:95:9D".into()),
                adapter: Some("2C:CF:67:6E:2E:BC".into())
            }
        );
        assert!(app.scan.is_none());
        let p = app.pairing.clone().unwrap();
        assert_eq!(p.title, "Pair TH-D75 (40:79:12:E4:95:9D)");
        assert_eq!(
            p.steps[0],
            "Put the radio in pairing mode: Menu > Bluetooth > Pairing Mode"
        );

        // a hands the selected device to setup, as a new TNC.
        app.pairing = None;
        let Action::Request(req) = app.key(key(KeyCode::Char('n'))) else {
            panic!("n scans");
        };
        app.sent(10, &req.then);
        let mut d75 = device("40:79:12:E4:95:9D", "TH-D75", Some("Kenwood TH-D75"));
        d75.paired = true;
        app.on_own_event(Event::Found {
            request: 10,
            device: d75,
        });
        assert_eq!(
            app.key(key(KeyCode::Char('a'))),
            Action::Run(
                [
                    "setup",
                    "--address",
                    "40:79:12:E4:95:9D",
                    "--adapter",
                    "2C:CF:67:6E:2E:BC",
                    "--name",
                    "TH-D75",
                    "--model",
                    "th-d75",
                    "--paired"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        assert!(app.scan.is_none());
        assert!(app.new_since.is_some());

        // Esc stops a scan and closes it.
        let Action::Request(req) = app.key(key(KeyCode::Char('n'))) else {
            panic!("n scans");
        };
        app.sent(11, &req.then);
        let Action::Request(stop) = app.key(key(KeyCode::Esc)) else {
            panic!("Esc stops it");
        };
        assert_eq!(stop.command, Command::BluetoothScanStop { request: 11 });
        assert!(app.scan.is_none());
    }

    #[test]
    fn unpairing_says_what_it_drops() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        app.view = View::Tncs;
        assert_eq!(app.key(key(KeyCode::Char('u'))), Action::None);
        let d = app.dialog.clone().expect("a question");
        assert_eq!(d.title, "Unpair d75 (TH-D75)?");
        assert_eq!(d.lines[0], "This drops the link on ktbr1/radio.");
        let Action::Request(r) = app.key(key(KeyCode::Char('y'))) else {
            panic!("y unpairs");
        };
        assert_eq!(
            r.command,
            Command::BluetoothUnpair {
                tnc: Some("d75".into()),
                address: None,
                adapter: None
            }
        );
        app.on_answer(r.then, Ok(json!({})));
        assert_eq!(app.message, Some(("d75 unpaired".into(), false)));
    }
    #[test]
    fn esc_quits_once_nothing_is_open() {
        let mut app = App::new("pi".into());
        app.set_status(status());
        // It closes the help first, then quits, in every view.
        app.key(key(KeyCode::Char('?')));
        assert_eq!(app.key(key(KeyCode::Esc)), Action::None);
        assert!(!app.help);
        assert_eq!(app.key(key(KeyCode::Esc)), Action::Quit);
        app.view = View::Frames;
        // In the Frames view, Esc ends typing a filter before it quits.
        app.key(key(KeyCode::Char('/')));
        assert_eq!(app.key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.key(key(KeyCode::Esc)), Action::Quit);
        app.view = View::Tncs;
        app.key(key(KeyCode::Char('u')));
        assert!(app.dialog.is_some());
        assert_eq!(app.key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.key(key(KeyCode::Esc)), Action::Quit);
    }
}
