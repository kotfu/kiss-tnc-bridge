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

//! Drawing the TUI from its state. The layouts are in docs/tui-mockups/.

use std::time::{Instant, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::{border, line};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::Value;

use super::add::{FieldValue, Form, Menu};
use super::app::{
    App, Ask, Dialog, FrameRow, Link, Lower, Pairing, Row, SCAN_SECS, Scan, TncRow, View, links_to,
};
use crate::control::protocol::{
    AdapterStatus, BridgeStatus, BtDevice, Dir, PortStatus, SessionStatus, Status, TncStatus,
};
use crate::ctl::render::{ago, parse_rfc3339};

/// A pane's top border joined to the borders of the pane above it.
const JOINED: border::Set = border::Set {
    top_left: line::NORMAL.vertical_right,
    top_right: line::NORMAL.vertical_left,
    ..border::PLAIN
};

/// Rows the lower pane has at least.
const LOWER_MIN: u16 = 8;

pub fn draw(f: &mut Frame, app: &App, now: Instant, wall: SystemTime) {
    match (&app.link, &app.status) {
        (Link::Up, Some(s)) => match app.view {
            View::Bridges => bridges(f, app, s, now, wall),
            View::Tncs => tncs(f, app, s),
            View::Frames => frames(f, app),
        },
        (Link::Down(why), _) => notice(
            f,
            app,
            &[why.as_str(), "", "Trying again every 2 s..."],
            " q quit",
        ),
        _ => notice(f, app, &["Connecting to the daemon..."], " q quit"),
    }
    if app.help {
        help(f, app);
    }
    if let Some(sc) = &app.scan {
        scan(f, sc, now);
    }
    if let Some(p) = app.pairing.as_ref().filter(|p| !p.hidden) {
        pairing(f, p);
    }
    if let Some(m) = &app.menu {
        menu(f, m);
    }
    if let Some(form) = &app.form {
        form_box(f, form);
    }
    if let Some(d) = &app.dialog {
        dialog(f, d);
    }
}

// ── Frame and panes ─────────────────────────────────────────────────────

/// The screen split into a main pane, a lower pane of `lower` rows if
/// any, and a line of keys, sharing borders.
fn panes(area: Rect, lower: Option<u16>) -> (Rect, Option<Rect>, Rect) {
    match lower {
        Some(n) => {
            let [main, low, keys] = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(n + 2),
                Constraint::Length(3),
            ])
            .spacing(-1)
            .areas(area);
            (main, Some(low), keys)
        }
        None => {
            let [main, keys] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)])
                .spacing(-1)
                .areas(area);
            (main, None, keys)
        }
    }
}

fn lower_rows(area: Rect) -> u16 {
    (area.height.saturating_sub(5) / 3).max(LOWER_MIN)
}

/// The outer border, with the title and the views.
fn top_block<'a>(app: &App) -> Block<'a> {
    let version = app
        .status
        .as_ref()
        .map(|s| s.version.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let mut tabs = vec![Span::raw(" ")];
    for (v, name) in [
        (View::Bridges, "1 Bridges"),
        (View::Tncs, "2 TNCs"),
        (View::Frames, "3 Frames"),
    ] {
        if app.view == v {
            tabs.push(Span::styled(
                format!("[{name}]"),
                Style::new().add_modifier(Modifier::BOLD),
            ));
        } else {
            tabs.push(Span::raw(name));
        }
        tabs.push(Span::raw("  "));
    }
    tabs.pop();
    tabs.push(Span::raw(" "));
    Block::bordered()
        .title(Line::from(format!(
            " kiss-tnc-bridge {version} · {} ",
            app.host
        )))
        .title(Line::from(tabs).right_aligned())
}

fn pane<'a>(left: String, right: &str) -> Block<'a> {
    let mut b = Block::bordered()
        .border_set(JOINED)
        .title(Line::from(format!(" {left} ")));
    if !right.is_empty() {
        b = b.title(Line::from(format!(" {right} ")).right_aligned());
    }
    b
}

fn keys(f: &mut Frame, area: Rect, text: &str) {
    let block = Block::bordered().border_set(JOINED);
    f.render_widget(Paragraph::new(format!(" {text}")).block(block), area);
}

/// A message on the lower pane's last line, until the next key.
fn message(f: &mut Frame, app: &App, inner: Rect) {
    let Some((text, error)) = &app.message else {
        return;
    };

    let at = Rect {
        y: inner.y + inner.height.saturating_sub(1),
        height: 1,
        ..inner
    };
    let style = if *error {
        Style::new().fg(Color::Red)
    } else {
        Style::new()
    };
    let mark = if *error { "✗ " } else { "" };
    f.render_widget(Clear, at);
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {mark}{text}"), style)),
        at,
    );
}

/// A whole screen of text: connecting, the daemon away.
fn notice(f: &mut Frame, app: &App, lines: &[&str], keys_text: &str) {
    let (main, _, k) = panes(f.area(), None);
    let width = main.width.saturating_sub(2 + 3 + 1) as usize;
    let mut text = vec![Line::raw(""), Line::raw("")];
    for l in lines {
        text.extend(
            wrap(l, width)
                .into_iter()
                .map(|w| Line::raw(format!("   {w}"))),
        );
    }
    f.render_widget(Paragraph::new(text).block(top_block(app)), main);
    keys(f, k, keys_text);
}

// ── Bridges ─────────────────────────────────────────────────────────────

/// Column widths for the tree, for an inner width `w`.
struct Cols {
    id: usize,
    kind: usize,
    place: usize,
    state: usize,
}

impl Cols {
    fn new(w: usize) -> Cols {
        let (id, kind, state) = (7, 10, 20);
        // 5 indent, the gaps, and "RX TX " over the lights.
        let place = w
            .saturating_sub(5 + id + 1 + kind + 1 + 1 + state + 1 + 6)
            .max(8);
        Cols {
            id,
            kind,
            place,
            state,
        }
    }

    fn heading(&self) -> String {
        format!(
            "     {:<id$} {:<kind$} {:<place$} {:<state$} RX TX ",
            "PORT",
            "TYPE",
            "WHERE",
            "STATE",
            id = self.id,
            kind = self.kind,
            place = self.place,
            state = self.state,
        )
    }
}

fn bridges(f: &mut Frame, app: &App, s: &Status, now: Instant, wall: SystemTime) {
    let area = f.area();
    let (main, lower, k) = panes(area, Some(lower_rows(area)));
    let block = top_block(app);
    let inner = block.inner(main);
    f.render_widget(block, main);

    let rows = app.rows();
    let sel = app.selected_index(&rows);
    if s.bridges.is_empty() {
        let text = [
            "",
            "   No bridges yet.",
            "",
            "   A bridge shares one TNC (a radio over Bluetooth or USB, or a KISS TCP",
            "   server) with apps over BLE, TCP or both.",
            "",
            "   Press a to set one up, step by step.",
        ];
        f.render_widget(
            Paragraph::new(text.map(Line::raw).to_vec()).wrap(Wrap { trim: false }),
            inner,
        );
    } else {
        let w = inner.width as usize;
        let cols = Cols::new(w);
        let [head, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        f.render_widget(Paragraph::new(fit(&cols.heading(), w)), head);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|(row, _)| ListItem::new(tree_line(app, s, *row, &cols, w, now, wall)))
            .collect();
        let mut state = ListState::default().with_selected(sel);
        f.render_stateful_widget(
            List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            list,
            &mut state,
        );
    }

    let lower = lower.expect("bridges has a lower pane");
    let bridge = app.selected_bridge();
    let row = sel.map(|i| rows[i].0);
    let (block, lines) = match app.lower {
        Lower::Frames => {
            let title = match bridge {
                Some(b) => format!("Frames: {}", b.id),
                None => "Frames".into(),
            };
            let n = lower.height.saturating_sub(2) as usize;
            let mine: Vec<&FrameRow> = app
                .frames
                .iter()
                .filter(|fr| bridge.is_none_or(|b| fr.bridge() == b.id))
                .collect();
            let w = lower.width.saturating_sub(2) as usize;
            let lines: Vec<Line> = mine[mine.len().saturating_sub(n)..]
                .iter()
                .map(|fr| Line::raw(fit(&frame_text(fr, 7), w)))
                .collect();
            (pane(title, "Tab details · f full screen"), lines)
        }
        Lower::Details => {
            let (title, lines) = match row {
                Some(r) => row_details(app, s, r, wall),
                None => ("Details".into(), Vec::new()),
            };
            (
                pane(title, "Tab frames"),
                lines
                    .into_iter()
                    .map(|l| Line::raw(format!(" {l}")))
                    .collect(),
            )
        }
    };
    let inner_lower = block.inner(lower);
    f.render_widget(Paragraph::new(lines).block(block), lower);
    message(f, app, inner_lower);

    let hint = match row {
        Some(Row::Bridge(_)) => {
            "↑↓ move  e enable  d disable  s switch  a add  - remove  ? help  q quit"
        }
        Some(Row::Port(b, p)) if s.bridges[b].ports[p].role != "app" => {
            "↑↓ move  c connect  x disconnect  r release  a add  - remove  ? help  q quit"
        }
        Some(Row::Port(..)) => "↑↓ move  a add  - remove  E edit  Tab details  ? help  q quit",
        Some(Row::Session(..)) => "↑↓ move  k kick  Tab details  ? help  q quit",
        None => "a add  2 TNCs  ? help  q quit",
    };
    keys(f, k, hint);
}

#[allow(clippy::too_many_arguments)]
fn tree_line(
    app: &App,
    s: &Status,
    row: Row,
    cols: &Cols,
    w: usize,
    now: Instant,
    wall: SystemTime,
) -> Line<'static> {
    match row {
        Row::Bridge(bi) => {
            let b = &s.bridges[bi];
            let running = b.enabled && b.counters.is_some();
            let dot = if running {
                Span::styled("●", Style::new().fg(Color::Green))
            } else {
                Span::raw("○")
            };
            let left = format!(" {}  {}", b.id, b.name);
            let right = bridge_state(b);
            let text = lr(&left, &right, w.saturating_sub(2));
            Line::from(vec![Span::raw(" "), dot, Span::raw(text)])
        }
        Row::Port(bi, pi) => {
            let b = &s.bridges[bi];
            let p = &b.ports[pi];
            let text = format!(
                "     {:<id$} {:<kind$} {:<place$} {:<state$} ",
                fit(&p.id, cols.id),
                fit(&p.kind, cols.kind),
                fit(&place(s, b, p), cols.place),
                fit(&port_state(p, wall), cols.state),
                id = cols.id,
                kind = cols.kind,
                place = cols.place,
                state = cols.state,
            );
            let mut spans = vec![Span::raw(text)];
            spans.extend(lights(app, &format!("{}/{}", b.id, p.id), now));
            Line::from(spans)
        }
        Row::Session(bi, pi, si) => {
            let sess = &s.bridges[bi].ports[pi].sessions[si];
            // Lined up with the ports' STATE column.
            let peer_w = cols.id + cols.kind + cols.place - 2;
            let since = parse_rfc3339(&sess.since)
                .map(|t| format!("since {}", local_hm(t)))
                .unwrap_or_default();
            let text = format!(
                "       └ {:<peer_w$} {:<state$} ",
                fit(peer(&sess.id), peer_w),
                since,
                state = cols.state,
            );
            let mut spans = vec![Span::raw(text)];
            spans.extend(lights(app, &sess.id, now));
            Line::from(spans)
        }
    }
}

fn lights(app: &App, object: &str, now: Instant) -> [Span<'static>; 3] {
    let light = |dir: Dir, color: Color| {
        if app.lit(object, dir, now) {
            Span::styled("■", Style::new().fg(color).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("□", Style::new().fg(Color::DarkGray))
        }
    };
    [
        light(Dir::Rx, Color::Green),
        Span::raw("  "),
        light(Dir::Tx, Color::Yellow),
    ]
}

/// `static · running`, `disabled · no app port`.
fn bridge_state(b: &BridgeStatus) -> String {
    let mut parts = Vec::new();
    if !b.editable {
        parts.push("static".to_string());
    }
    parts.push(
        match (b.enabled, b.counters.is_some()) {
            (true, true) => "running",
            (true, false) => "enabled",
            (false, _) => "disabled",
        }
        .to_string(),
    );
    parts.extend(b.incomplete.iter().cloned());
    format!("{} ", parts.join(" · "))
}

/// What a port talks to, in few words.
fn place(s: &Status, b: &BridgeStatus, p: &PortStatus) -> String {
    let get = |k: &str| p.settings.get(k).and_then(Value::as_str);
    match p.kind.as_str() {
        "spp-client" => {
            let tnc = get("tnc").unwrap_or("?");
            let on = s
                .tncs
                .iter()
                .find(|t| t.id == tnc)
                .and_then(|t| t.spp.as_ref())
                .map(|spp| format!(" on {}", short_adapter(&spp.adapter)))
                .unwrap_or_default();
            format!("TNC {tnc}{on}")
        }
        "ble-client" => {
            let tnc = get("tnc").unwrap_or("?");
            let on = s
                .tncs
                .iter()
                .find(|t| t.id == tnc)
                .and_then(|t| t.ble.as_ref())
                .map(|ble| format!(" on {}", short_adapter(&ble.adapter)))
                .unwrap_or_default();
            format!("TNC {tnc} (BLE){on}")
        }
        "serial" => format!("TNC {} (serial)", get("tnc").unwrap_or("?")),
        "tcp-client" => get("address").unwrap_or_default().to_string(),
        "tcp-server" => get("listen").unwrap_or_default().to_string(),
        "ble-server" => format!(
            "\"{}\" on {}",
            b.public_name.effective,
            short_adapter(get("adapter").unwrap_or("the default adapter"))
        ),
        other => other.to_string(),
    }
}

/// `builtin (2C:CF:…)` → `builtin`.
fn short_adapter(a: &str) -> &str {
    a.split(" (").next().unwrap_or(a)
}

/// A TNC port's link and how long it's been so; an app port's apps.
fn port_state(p: &PortStatus, wall: SystemTime) -> String {
    if p.role == "app" {
        let n = p.sessions.len();
        if n == 0 {
            return p.state.clone();
        }
        return match p.settings.get("max_clients").and_then(Value::as_u64) {
            Some(m) => format!("{n} of {m} apps"),
            None if n == 1 => "1 app".into(),
            None => format!("{n} apps"),
        };
    }
    match p.since.as_deref().and_then(parse_rfc3339) {
        Some(t) => format!("{} {}", p.state, ago(t, wall)),
        None => p.state.clone(),
    }
}

/// `ktbr1/phones/40:33:E0:1B:DB:76` → `40:33:E0:1B:DB:76`.
fn peer(session: &str) -> &str {
    session.splitn(3, '/').nth(2).unwrap_or(session)
}

/// What the Bridges view's details pane says about a row.
fn row_details(app: &App, s: &Status, row: Row, wall: SystemTime) -> (String, Vec<String>) {
    match row {
        Row::Bridge(bi) => {
            let b = &s.bridges[bi];
            (format!("Details: {}", b.id), bridge_details(b))
        }
        Row::Port(bi, pi) => {
            let b = &s.bridges[bi];
            let p = &b.ports[pi];
            (
                format!("Details: {}/{}", b.id, p.id),
                port_details(s, b, p, wall),
            )
        }
        Row::Session(bi, pi, si) => {
            let b = &s.bridges[bi];
            let p = &b.ports[pi];
            let sess = app.session(bi, pi, si).expect("the row exists");
            (
                format!("Details: {}", sess.id),
                session_details(b, p, sess, wall),
            )
        }
    }
}

/// Where an object is defined, from its `source` (`dynamic`, or
/// `static: PATH`).
fn source_line(source: &str, editable: bool) -> String {
    match source.strip_prefix("static: ") {
        Some(path) if !editable => {
            format!("from {path}: read-only here; edit the file, then reload")
        }
        Some(path) => format!("from {path}"),
        None => "made with ktbctl (in dynamic.toml)".into(),
    }
}

fn bridge_details(b: &BridgeStatus) -> Vec<String> {
    let mut out = vec![format!("Bridge {} \"{}\"", b.id, b.name)];
    if b.public_name.effective != b.name {
        out.push(format!(
            "advertised and announced as \"{}\"",
            b.public_name.effective
        ));
    }
    let mut state = (if b.enabled { "enabled" } else { "disabled" }).to_string();
    if b.enabled_override.is_some() {
        state.push_str(" (by an override in state.toml)");
    }
    if !b.incomplete.is_empty() {
        state.push_str(&format!("; needs: {}", b.incomplete.join(", ")));
    }
    out.push(state);
    if let Some(c) = &b.counters {
        out.push(format!(
            "{} frames from the TNC · {} to it · {} dropped · {} TNC connects",
            c.frames_down, c.frames_up, c.dropped, c.tnc_connects
        ));
    }
    out.push(source_line(&b.source, b.editable));
    out
}

fn port_details(s: &Status, b: &BridgeStatus, p: &PortStatus, wall: SystemTime) -> Vec<String> {
    let role = if p.role == "app" { "app" } else { "TNC" };
    let mut out = vec![format!("{} port {}: {}", role, p.id, p.kind)];
    if let Some(t) = p
        .settings
        .get("tnc")
        .and_then(Value::as_str)
        .and_then(|id| s.tncs.iter().find(|t| t.id == id))
    {
        out.push(tnc_summary(t));
    }
    let mut state = p.state.clone();
    if let Some(t) = p.since.as_deref().and_then(parse_rfc3339) {
        state.push_str(&format!(" since {} ({})", local_hms(t), ago(t, wall)));
    }
    if let Some(d) = &p.detail {
        state.push_str(&format!(": {d}"));
    }
    out.push(state);
    let settings = settings_line(&p.settings);
    if !settings.is_empty() {
        out.push(settings);
    }
    if p.role == "app" {
        out.push(match p.sessions.len() {
            0 => "no apps connected".into(),
            1 => "1 app connected".into(),
            n => format!("{n} apps connected"),
        });
    }
    out.push(source_line(&b.source, b.editable));
    out
}

fn session_details(
    b: &BridgeStatus,
    p: &PortStatus,
    sess: &SessionStatus,
    wall: SystemTime,
) -> Vec<String> {
    let how = match p.kind.as_str() {
        "ble-server" => "BLE",
        "tcp-server" => "TCP",
        k => k,
    };
    let mut out = vec![format!(
        "{how} session on {}/{} from {}",
        b.id,
        p.id,
        peer(&sess.id)
    )];
    if let Some(t) = parse_rfc3339(&sess.since) {
        out.push(format!(
            "connected since {} ({})",
            local_hms(t),
            ago(t, wall)
        ));
    }
    out
}

/// `connect always · idle_timeout 0s`, leaving out what's shown elsewhere.
fn settings_line(v: &Value) -> String {
    let Value::Object(m) = v else {
        return String::new();
    };
    m.iter()
        .filter(|(k, _)| k.as_str() != "tnc")
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Array(a) if a.is_empty() => "anyone".into(),
                Value::Array(a) => a
                    .iter()
                    .map(|x| x.as_str().map(str::to_string).unwrap_or(x.to_string()))
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            format!("{k} {v}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

// ── TNCs ────────────────────────────────────────────────────────────────

fn tncs(f: &mut Frame, app: &App, s: &Status) {
    let (main, lower, k) = panes(f.area(), Some(6));
    let block = top_block(app);
    let inner = block.inner(main);
    f.render_widget(block, main);
    let w = inner.width as usize;

    let rows = app.tnc_rows();
    let sel = app.tnc_selected_index(&rows);
    let mut items = vec![ListItem::new(Line::styled(
        " TNCs",
        Style::new().add_modifier(Modifier::BOLD),
    ))];
    // Where each row's item is in the list.
    let mut at = Vec::new();
    if s.tncs.is_empty() {
        items.push(ListItem::new("   none yet: a adds one"));
    }
    for (row, _) in &rows {
        if let TncRow::Adapter(0) = row {
            items.push(ListItem::new(""));
            items.push(ListItem::new(Line::styled(
                " Adapters",
                Style::new().add_modifier(Modifier::BOLD),
            )));
        }
        at.push(items.len());
        items.push(ListItem::new(match row {
            TncRow::Tnc(i) => tnc_lines(&s.tncs[*i], w),
            TncRow::Adapter(i) => vec![Line::raw(adapter_line(&s.adapters[*i], w))],
            TncRow::Device(a, d) => vec![Line::raw(device_row(&s.adapters[*a].devices[*d], w))],
        }));
    }
    if s.adapters.is_empty() {
        items.push(ListItem::new(""));
        items.push(ListItem::new(" Adapters"));
        items.push(ListItem::new("   none found"));
    }
    let mut state = ListState::default().with_selected(sel.map(|i| at[i]));
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        inner,
        &mut state,
    );

    let lower = lower.expect("tncs has a lower pane");
    let (title, lines) = match sel.map(|i| rows[i].0) {
        Some(TncRow::Tnc(i)) => {
            let t = &s.tncs[i];
            (format!("Details: {}", t.id), tnc_details(s, t))
        }
        Some(TncRow::Adapter(i)) => {
            let a = &s.adapters[i];
            (
                format!("Details: {}", a.id.as_deref().unwrap_or(&a.mac)),
                adapter_details(a),
            )
        }
        Some(TncRow::Device(a, d)) => {
            let (a, d) = (&s.adapters[a], &s.adapters[a].devices[d]);
            (format!("Details: {}", d.address), device_details(a, d))
        }
        None => ("Details".into(), Vec::new()),
    };
    let block = pane(title, "");
    let inner_lower = block.inner(lower);
    let lines: Vec<Line> = lines
        .iter()
        .flat_map(|l| indented(l, " ", inner_lower.width as usize))
        .map(Line::raw)
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), lower);
    message(f, app, inner_lower);
    let hint = match sel.map(|i| rows[i].0) {
        Some(TncRow::Device(..)) => "↑↓ move  a set it up  u unpair  n scan  ? help  q quit",
        _ => "↑↓ move  p pair  u unpair  n scan  a add  - remove  E edit  ? help  q quit",
    };
    keys(f, k, hint);
}

/// A device paired on the adapter above, not a TNC yet.
fn device_row(d: &BtDevice, w: usize) -> String {
    let what = match (&d.name, &d.known_tnc) {
        (Some(n), Some(k)) if n != k => format!("  {n} ({k})"),
        (Some(n), _) => format!("  {n}"),
        (None, Some(k)) => format!("  {k}"),
        (None, None) => String::new(),
    };
    lr(
        &format!("          paired  {}{what}", d.address),
        "not a TNC yet ",
        w,
    )
}

fn device_details(a: &AdapterStatus, d: &BtDevice) -> Vec<String> {
    let mut out = vec![format!(
        "{}{}, paired on {}",
        d.address,
        d.name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default(),
        super::app::adapter_label(a)
    )];
    out.push(match &d.known_tnc {
        Some(k) => format!("A {k}, not set up as a TNC yet: a sets it up, with a bridge."),
        None => "Not set up as a TNC: a sets it up, with a bridge.".into(),
    });
    out.push("u unpairs it.".into());
    out
}

fn tnc_lines(t: &TncStatus, w: usize) -> Vec<Line<'static>> {
    let used = match &t.in_use_by {
        Some(b) => format!("used by {b} "),
        None => "not in use ".into(),
    };
    let mut out = vec![Line::raw(lr(
        &format!(" {:<8} {} ({})", t.id, t.name, t.model),
        &used,
        w,
    ))];
    if let Some(spp) = &t.spp {
        let paired = match spp.paired {
            Some(true) => "paired ",
            Some(false) => "not paired ",
            None => "pairing unknown ",
        };
        out.push(Line::raw(lr(
            &format!(
                "          Bluetooth  {} on {}",
                spp.address,
                short_adapter(&spp.adapter)
            ),
            paired,
            w,
        )));
    }
    if let Some(ble) = &t.ble {
        out.push(Line::raw(format!(
            "          BLE        {} on {}",
            ble.address,
            short_adapter(&ble.adapter)
        )));
    }
    if let Some(ser) = &t.serial {
        let present = if ser.present {
            "plugged in "
        } else {
            "not plugged in "
        };
        out.push(Line::raw(lr(
            &format!("          Serial     {}", ser.path),
            present,
            w,
        )));
    }
    out
}

fn adapter_line(a: &AdapterStatus, w: usize) -> String {
    let state = match (a.present, a.powered) {
        (false, _) => "not present ",
        (true, Some(true)) => "on ",
        (true, Some(false)) => "off ",
        (true, None) => "present ",
    };
    let mut left = format!(
        " {:<8} {:<5} {}",
        a.id.as_deref().unwrap_or("-"),
        a.hci.as_deref().unwrap_or("-"),
        a.mac
    );
    if let Some(hw) = a.hardware() {
        left.push_str(&format!("  {hw}"));
    } else if let Some(src) = &a.source {
        left.push_str(&format!("  named in {src}"));
    }
    lr(&left, state, w)
}

/// `TNC d75: TH-D75 (th-d75), Bluetooth 40:79:… on builtin, paired`.
fn tnc_summary(t: &TncStatus) -> String {
    let mut out = format!("TNC {}: {} ({})", t.id, t.name, t.model);
    if let Some(spp) = &t.spp {
        let paired = match spp.paired {
            Some(true) => ", paired",
            Some(false) => ", not paired",
            None => "",
        };
        out.push_str(&format!(
            ", Bluetooth {} on {}{paired}",
            spp.address,
            short_adapter(&spp.adapter)
        ));
    }
    if let Some(ble) = &t.ble {
        out.push_str(&format!(
            ", BLE {} on {}",
            ble.address,
            short_adapter(&ble.adapter)
        ));
    }
    if let Some(ser) = &t.serial {
        out.push_str(&format!(
            ", serial {}{}",
            ser.path,
            if ser.present { "" } else { " (not plugged in)" }
        ));
    }
    out
}

fn tnc_details(s: &Status, t: &TncStatus) -> Vec<String> {
    let mut out = vec![tnc_summary(t)];
    out.push(match &t.in_use_by {
        Some(b) => format!("Bridge {b} uses it."),
        None => "No enabled bridge uses it.".into(),
    });
    let links = links_to(s, &t.id);
    if t.spp.is_some() && !links.is_empty() {
        out.push(format!(
            "Unpairing would drop the link on {}.",
            crate::ctl::render::join_and(&links)
        ));
    }
    out.push(source_line(&t.source, t.editable));
    out
}

fn adapter_details(a: &AdapterStatus) -> Vec<String> {
    let mut out = vec![format!(
        "Adapter {}{}{}",
        a.mac,
        a.hci
            .as_deref()
            .map(|h| format!(", {h}"))
            .unwrap_or_default(),
        a.hardware().map(|h| format!(", {h}")).unwrap_or_default()
    )];
    out.push(match &a.id {
        Some(id) => format!("config calls it {id}"),
        None => "no id in the config: ports name it by MAC".into(),
    });
    if !a.present {
        out.push("not present on this host".into());
    }
    if let Some(src) = &a.source {
        out.push(format!("defined in {src}"));
    }
    out
}

// ── Frames ──────────────────────────────────────────────────────────────

fn frames(f: &mut Frame, app: &App) {
    let (main, lower, k) = panes(f.area(), Some(4));
    let block = top_block(app);
    let inner = block.inner(main);
    f.render_widget(block, main);
    let w = inner.width as usize;

    let visible = app.visible_frames();
    let sel = app.selected_frame(&visible);
    let mut about = vec![match app.fv.this_bridge {
        true => match app.selected_bridge() {
            Some(b) => format!("{} only", b.id),
            None => "All bridges".into(),
        },
        false => "All bridges".into(),
    }];
    if !app.fv.filter.is_empty() {
        about.push(format!("matching \"{}\"", app.fv.filter));
    }
    about.push(
        if app.fv.paused.is_some() {
            "paused"
        } else if app.fv.selected.is_some() {
            "held (End follows the newest)"
        } else {
            "following the newest"
        }
        .into(),
    );
    about.push(format!("{} frames", visible.len()));
    let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(inner);
    f.render_widget(
        Paragraph::new(fit(&format!(" {}", about.join(" · ")), w)),
        head,
    );
    let items: Vec<ListItem> = visible
        .iter()
        .map(|fr| {
            let label = format!("{}/{}", fr.bridge(), fr.port());
            ListItem::new(fit(&frame_text_labeled(fr, &label, 12), w))
        })
        .collect();
    let mut state = ListState::default().with_selected(sel);
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        list,
        &mut state,
    );

    let lower = lower.expect("frames has a lower pane");
    let chosen = sel.map(|i| visible[i]);
    let title = match chosen {
        Some(fr) => format!(
            "Frame {} {} {}",
            time_of(&fr.at),
            dir_name(fr.dir),
            fr.object
        ),
        None => "Frame".into(),
    };
    let block = pane(title, "");
    let inner_lower = block.inner(lower);
    let lines = chosen
        .map(|fr| decode_lines(fr, app.fv.hex))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        lower,
    );
    message(f, app, inner_lower);
    if app.fv.typing {
        keys(
            f,
            k,
            &format!("Filter: {}▏   ⏎ done  Esc clear", app.fv.filter),
        );
    } else {
        keys(
            f,
            k,
            "↑↓ move  space pause  / filter  b this bridge  h hex  1 2 views  q quit",
        );
    }
}

fn decode_lines(fr: &FrameRow, hex: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let d = &fr.decoded;
    if let Some(h) = &d.ax25 {
        let via = if h.path.is_empty() {
            String::new()
        } else {
            format!(" via {}", h.path.join(","))
        };
        out.push(format!(" {} > {}{via}", h.source, h.destination));
        if let Some((_, info)) = d.summary.split_once(':') {
            out.push(format!(" {info}"));
        }
    } else {
        out.push(format!(" {}", d.summary));
    }
    if let Some(s) = &fr.session {
        out.push(format!(" from {s}"));
    }
    let bytes: Vec<String> = fr
        .kiss
        .as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect();
    let hex_line = format!(" KISS {}", bytes.join(" "));
    if hex {
        out.push(hex_line);
    } else {
        out.push(hex_line.chars().take(78).collect());
    }
    out.into_iter().map(Line::raw).collect()
}

/// ` 14:22:07 rx radio   W1AW-9>APDR16,…`.
fn frame_text(fr: &FrameRow, port_w: usize) -> String {
    frame_text_labeled(fr, fr.port(), port_w)
}

fn frame_text_labeled(fr: &FrameRow, label: &str, w: usize) -> String {
    format!(
        " {} {} {:<w$} {}",
        time_of(&fr.at),
        dir_name(fr.dir),
        label,
        fr.decoded.summary
    )
}

fn dir_name(d: Dir) -> &'static str {
    match d {
        Dir::Rx => "rx",
        Dir::Tx => "tx",
    }
}

/// A daemon timestamp as local `HH:MM:SS`.
fn time_of(at: &str) -> String {
    parse_rfc3339(at)
        .map(local_hms)
        .unwrap_or_else(|| at.get(11..19).unwrap_or(at).to_string())
}

// ── Help ────────────────────────────────────────────────────────────────

fn help(f: &mut Frame, app: &App) {
    let mut lines: Vec<(&str, &str)> = vec![
        ("1 2 3", "Bridges, TNCs, Frames"),
        ("↑ ↓ PgUp PgDn", "move"),
        ("Home End", "first, last"),
    ];
    match app.view {
        View::Bridges => lines.extend([
            ("→ ←", "open and fold a bridge"),
            ("Tab", "frames or details below"),
            ("f", "frames, full screen"),
            ("e d s", "enable, disable, switch to a bridge"),
            ("c x r", "connect, disconnect, release a TNC"),
            ("k", "kick an app's session"),
            ("a", "add a bridge, port, TNC or adapter name"),
            ("-", "remove a bridge or port"),
            ("E", "edit this in $EDITOR (ktbctl edit)"),
            ("C", "edit all of dynamic.toml in $EDITOR"),
            ("R", "reload the config file"),
        ]),
        View::Tncs => lines.extend([
            ("p u", "pair, unpair a TNC's radio"),
            ("n", "scan for Bluetooth devices"),
            ("a", "add a bridge, TNC or adapter name"),
            ("-", "remove a TNC, or an adapter's id"),
            ("E", "edit this in $EDITOR (ktbctl edit)"),
            ("C", "edit all of dynamic.toml in $EDITOR"),
            ("R", "reload the config file"),
        ]),
        View::Frames => lines.extend([
            ("space", "pause"),
            ("/", "filter by text; Esc clears it"),
            ("b", "all bridges, or the selected one"),
            ("h", "the whole frame in hex"),
            ("End", "follow the newest again"),
        ]),
    }
    lines.extend([("?", "this help"), ("q  Esc  Ctrl-C", "quit")]);
    let area = f.area();
    let h = (lines.len() as u16 + 4).min(area.height);
    let box_area = Rect {
        x: area.x + area.width.saturating_sub(60) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: 60.min(area.width),
        height: h,
    };
    let text: Vec<Line> = std::iter::once(Line::raw(""))
        .chain(
            lines
                .into_iter()
                .map(|(k, what)| Line::raw(format!("  {k:<16} {what}"))),
        )
        .collect();
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(" Keys ")
                .title(Line::from(" Esc close ").right_aligned()),
        ),
        box_area,
    );
}

// ── Dialogs ─────────────────────────────────────────────────────────────

/// A question with yes and no, or a notice to close, in the middle of the
/// screen.
fn dialog(f: &mut Frame, d: &Dialog) {
    let area = f.area();
    let width = 60.min(area.width);
    let text_w = width.saturating_sub(6) as usize;
    let mut text = vec![Line::raw("")];
    for l in &d.lines {
        // A bullet's text wraps under its text, not under the bullet.
        let lead: String = l.chars().take_while(|c| *c == ' ' || *c == '•').collect();
        let under = " ".repeat(lead.chars().count());
        let body = &l[lead.len()..];
        let w = text_w.saturating_sub(under.len()).max(10);
        for (i, part) in wrap(body, w).into_iter().enumerate() {
            let first = if i == 0 {
                lead.as_str()
            } else {
                under.as_str()
            };
            text.push(Line::raw(format!("  {first}{part}")));
        }
    }
    text.push(Line::raw(""));
    let footer = match &d.yes {
        Some((yes, _)) => {
            text.push(Line::from(vec![
                Span::raw("        "),
                Span::styled(
                    format!("[ y  {yes} ]"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("     [ n  Cancel ]"),
            ]));
            text.push(Line::raw(""));
            " Esc cancel "
        }
        None => " ⏎ close ",
    };
    let h = (text.len() as u16 + 2).min(area.height);
    let at = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width,
        height: h,
    };
    f.render_widget(Clear, at);
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(Line::from(format!(" {} ", d.title)))
                .title_bottom(Line::from(footer).right_aligned()),
        ),
        at,
    );
}

// ── Pairing and scanning ────────────────────────────────────────────────

/// A box `width` by `height` in the middle of `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (w, h) = (width.min(area.width), height.min(area.height));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// `text` wrapped to `w`, each line indented by `indent`.
fn indented(text: &str, indent: &str, w: usize) -> Vec<String> {
    wrap(text, w.saturating_sub(indent.len()))
        .into_iter()
        .map(|l| format!("{indent}{l}"))
        .collect()
}

fn scan(f: &mut Frame, sc: &Scan, now: Instant) {
    let area = f.area();
    let rows = sc.devices.len().max(1) as u16;
    let hint: Vec<String> = sc.hint.iter().flat_map(|h| indented(h, " ", 66)).collect();
    let errors = sc
        .error
        .as_ref()
        .map_or(0, |e| indented(e, "   ", 66).len()) as u16;
    let at = centered(area, 70, rows + errors + hint.len() as u16 + 5);
    let right = if sc.over {
        "done".to_string()
    } else if sc.then_pair.is_some() {
        "found it".to_string()
    } else {
        let gone = now.saturating_duration_since(sc.started).as_secs();
        format!("{} s left", SCAN_SECS.saturating_sub(gone))
    };
    let block = Block::bordered()
        .title(Line::from(format!(" {} ", sc.title)))
        .title(Line::from(format!(" {right} ")).right_aligned());
    let inner = block.inner(at);
    f.render_widget(Clear, at);
    f.render_widget(block, at);
    let [top, list, bottom] = Layout::vertical([
        Constraint::Length(hint.len() as u16 + 1),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(hint.into_iter().map(Line::raw).collect::<Vec<_>>()),
        top,
    );
    let w = list.width as usize;
    let error: Vec<ListItem> = match &sc.error {
        Some(e) => wrap(e, w.saturating_sub(4))
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let lead = if i == 0 { "✗ " } else { "  " };
                ListItem::new(Line::styled(
                    format!(" {lead}{l}"),
                    Style::new().fg(Color::Red),
                ))
            })
            .collect(),
        None => Vec::new(),
    };
    if sc.devices.is_empty() && !error.is_empty() {
        f.render_widget(List::new(error), list);
    } else if sc.devices.is_empty() {
        let text = if sc.over {
            " Nothing found. Is the radio in pairing mode, with Bluetooth on?"
        } else {
            " Looking..."
        };
        f.render_widget(Paragraph::new(text), list);
    } else {
        let items: Vec<ListItem> = sc
            .devices
            .iter()
            .map(|d| ListItem::new(device_line(d, w)))
            .chain(error)
            .collect();
        let mut state = ListState::default().with_selected(sc.selected_index());
        f.render_stateful_widget(
            List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            list,
            &mut state,
        );
    }
    let keys_text = match (sc.over, sc.target.is_some()) {
        (true, true) => " n look again   Esc close",
        (true, false) => " ⏎ pair it   a set it up   n scan again   Esc close",
        (false, true) => " Esc stop",
        (false, false) => " ⏎ pair it   a set it up as a new TNC   Esc stop",
    };
    f.render_widget(
        Paragraph::new(vec![Line::raw(""), Line::raw(keys_text)]),
        bottom,
    );
}

/// `40:79:12:E4:95:9D  TH-D75  Kenwood TH-D75  -52 dBm`.
fn device_line(d: &BtDevice, w: usize) -> String {
    let mut what = d
        .known_tnc
        .clone()
        .or_else(|| d.icon.clone())
        .unwrap_or_else(|| "-".into());
    if let Some(t) = &d.tnc {
        what.push_str(&format!(", tnc {t}"));
    } else if d.paired {
        what.push_str(", paired");
    }
    let rssi = d.rssi.map(|r| format!("{r} dBm")).unwrap_or_default();
    let left = format!(
        " {}  {:<12} {}",
        d.address,
        fit(d.name.as_deref().unwrap_or("-"), 12),
        what
    );
    lr(&left, &format!("{rssi:>8} "), w)
}

fn pairing(f: &mut Frame, p: &Pairing) {
    let area = f.area();
    let width = 60u16.min(area.width);
    let w = width.saturating_sub(4) as usize;
    let mut text = vec![Line::raw("")];
    for (i, s) in p.steps.iter().enumerate() {
        // What to do at the radio, then what's happened.
        if i == 1 {
            text.push(Line::raw(""));
        }
        text.extend(indented(s, "  ", w).into_iter().map(Line::raw));
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let buttons = |yes: &str| {
        Line::from(vec![
            Span::raw("        "),
            Span::styled(format!("[ y  {yes} ]"), bold),
            Span::raw("     [ n  No ]"),
        ])
    };
    match &p.ask {
        Some(Ask::Confirm(passkey)) => {
            let spaced: Vec<String> = passkey.chars().map(String::from).collect();
            text.push(Line::raw(""));
            text.push(Line::from(vec![
                Span::raw("  The radio should show   "),
                Span::styled(spaced.join(" "), bold),
            ]));
            text.push(Line::raw("  Does it?"));
            text.push(Line::raw(""));
            text.push(buttons("Yes, it matches"));
        }
        Some(Ask::Authorize) => {
            text.push(Line::raw(""));
            text.push(Line::raw("  Allow the device to pair?"));
            text.push(Line::raw(""));
            text.push(buttons("Allow it"));
        }
        Some(ask @ (Ask::Pin | Ask::Passkey)) => {
            let label = if *ask == Ask::Pin {
                "The device's PIN:"
            } else {
                "The passkey the device shows:"
            };
            text.push(Line::raw(""));
            text.push(Line::from(vec![
                Span::raw(format!("  {label} ")),
                Span::styled(format!("{}_", p.typed), bold),
            ]));
        }
        None => {}
    }
    if let Some((said, ok)) = &p.end {
        let (mark, style) = if *ok {
            ("✓ ", Style::new().fg(Color::Green))
        } else {
            ("✗ ", Style::new().fg(Color::Red))
        };
        text.push(Line::raw(""));
        for (i, l) in wrap(said, w.saturating_sub(4)).into_iter().enumerate() {
            let lead = if i == 0 { mark } else { "  " };
            text.push(Line::styled(format!("  {lead}{l}"), style));
        }
    }
    text.push(Line::raw(""));
    let footer = match (&p.end, &p.ask) {
        (Some(_), _) => " ⏎ close ",
        (None, Some(Ask::Pin | Ask::Passkey)) => " ⏎ send  Esc cancel ",
        (None, Some(_)) => " Esc no ",
        (None, None) => " Esc hide ",
    };
    let at = centered(area, width, text.len() as u16 + 2);
    f.render_widget(Clear, at);
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(Line::from(format!(" {} ", p.title)))
                .title_bottom(Line::from(footer).right_aligned()),
        ),
        at,
    );
}

// ── Adding ──────────────────────────────────────────────────────────────

fn menu(f: &mut Frame, m: &Menu) {
    let area = f.area();
    let at = centered(area, 60, m.items.len() as u16 + 4);
    let block = Block::bordered()
        .title(" Add ")
        .title_bottom(Line::from(" ⏎ choose  Esc cancel ").right_aligned());
    let inner = block.inner(at);
    f.render_widget(Clear, at);
    f.render_widget(block, at);
    let list = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let items: Vec<ListItem> = m
        .items
        .iter()
        .map(|(text, _)| ListItem::new(format!("  {text}")))
        .collect();
    let mut state = ListState::default().with_selected(Some(m.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        list,
        &mut state,
    );
}

fn form_box(f: &mut Frame, form: &Form) {
    let area = f.area();
    let width = 64u16.min(area.width);
    let w = width.saturating_sub(4) as usize;
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut text = vec![Line::raw("")];
    for i in form.visible() {
        let field = &form.fields[i];
        let focused = i == form.focus;
        let label = Span::styled(
            format!("  {:<9} ", field.label),
            if focused { bold } else { Style::new() },
        );
        let value = match (&field.value, focused) {
            (FieldValue::Choice(..), true) => format!("◂ {} ▸", field.shown()),
            (FieldValue::Choice(..), false) => format!("  {}", field.shown()),
            (FieldValue::Text(t), true) => format!("  {t}_"),
            (FieldValue::Text(t), false) => format!("  {t}"),
        };
        let room = w.saturating_sub(12);
        let mut spans = vec![label, Span::raw(fit(&value, room))];
        let used = value.chars().count();
        if !field.note.is_empty() && used + 2 + field.note.chars().count() <= room {
            spans.push(Span::styled(format!("  {}", field.note), dim));
        }
        text.push(Line::from(spans));
    }
    if let Some(warn) = form.warning() {
        text.push(Line::raw(""));
        text.extend(indented(&warn, "  ", w).into_iter().map(Line::raw));
    }
    if let Some(e) = &form.error {
        text.push(Line::raw(""));
        for (i, l) in wrap(e, w.saturating_sub(4)).into_iter().enumerate() {
            let lead = if i == 0 { "✗ " } else { "  " };
            text.push(Line::styled(
                format!("  {lead}{l}"),
                Style::new().fg(Color::Red),
            ));
        }
    }
    text.push(Line::raw(""));
    text.push(Line::from(vec![
        Span::raw("        "),
        Span::styled("[ ⏎ Add ]", bold),
        Span::raw("     [ Esc Cancel ]"),
    ]));
    text.push(Line::raw(""));
    let at = centered(area, width, text.len() as u16 + 2);
    f.render_widget(Clear, at);
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(Line::from(format!(" {} ", form.title)))
                .title_bottom(Line::from(" ↑↓ field  ←→ choose ").right_aligned()),
        ),
        at,
    );
}

// ── Text ────────────────────────────────────────────────────────────────

/// `s`, cut to `w` characters with an ellipsis if it's longer.
fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `text` in lines of at most `w` characters, broken at spaces.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for word in text.split(' ') {
        let line = out.last_mut().expect("never empty");
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
            out.push(word.to_string());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    out
}

/// `left` and `right` on one line `w` wide, `right` against the edge.
fn lr(left: &str, right: &str, w: usize) -> String {
    let rw = right.chars().count();
    let left = fit(left, w.saturating_sub(rw + 1));
    let gap = w.saturating_sub(left.chars().count() + rw);
    format!("{left}{}{right}", " ".repeat(gap))
}

/// Local `HH:MM:SS`.
pub fn local_hms(t: SystemTime) -> String {
    let (h, m, s) = local(t);
    format!("{h:02}:{m:02}:{s:02}")
}

fn local_hm(t: SystemTime) -> String {
    let (h, m, _) = local(t);
    format!("{h:02}:{m:02}")
}

fn local(t: SystemTime) -> (i32, i32, i32) {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r only writes the tm it's given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok {
        (tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        // time_t is i64 on most targets but i32 on 32-bit ARM Linux, where
        // the cast does nothing.
        #[allow(clippy::unnecessary_cast)]
        let s = secs.rem_euclid(86_400) as i32;
        (s / 3600, s % 3600 / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::super::app::tests::{frame, status};
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn screen(app: &App) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let wall = parse_rfc3339("2026-10-27T14:22:20Z").unwrap();
        t.draw(|f| draw(f, app, Instant::now(), wall)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..24)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    fn app() -> App {
        let mut app = App::new("aprscc".into());
        app.link = Link::Up;
        app.set_status(status());
        let now = Instant::now();
        app.on_event(
            frame(
                "ktbr1/radio",
                None,
                "W1AW-9>APDR16,WIDE1-1::KD2ABC-7 :ack12",
            ),
            now,
        );
        app
    }

    fn has(screen: &[String], text: &str) -> bool {
        screen.iter().any(|l| l.contains(text))
    }

    #[test]
    fn the_bridges_view_fits_80_by_24() {
        let app = app();
        let s = screen(&app);
        let all = s.join("\n");
        assert!(
            s[0].starts_with("┌ kiss-tnc-bridge 0.6.0 · aprscc"),
            "{all}"
        );
        assert!(s[0].contains("[1 Bridges]  2 TNCs  3 Frames"), "{all}");
        assert!(has(&s, "ktbr1  TH-D75"), "{all}");
        assert!(has(&s, "running"), "{all}");
        assert!(has(&s, "TNC d75 on builtin"), "{all}");
        assert!(has(&s, "\"TH-D75\" on dongle"), "{all}");
        assert!(has(&s, "1 of 2 apps"), "{all}");
        // The session's columns line up with its port's.
        let col = |needle: &str, of: &str| {
            let l = s.iter().find(|l| l.contains(needle)).unwrap();
            let chars: Vec<char> = l.chars().collect();
            let of: Vec<char> = of.chars().collect();
            chars
                .windows(of.len())
                .position(|w| w == of.as_slice())
                .unwrap()
        };
        assert_eq!(col("└ 40:33", "since"), col("phones", "1 of 2"), "{all}");
        assert_eq!(col("└ 40:33", "□"), col("phones", "□"), "{all}");
        assert_eq!(col("PORT", "RX"), col("phones", "□"), "{all}");
        assert!(s[1].ends_with("RX TX │"), "{all}");
        assert!(has(&s, "└ 40:33:E0:1B:DB:76"), "{all}");
        assert!(has(&s, "ktbr2  Shack relay"), "{all}");
        assert!(has(&s, "├ Frames: ktbr1"), "{all}");
        assert!(has(&s, " rx radio   W1AW-9>APDR16"), "{all}");
        // The panes share their borders.
        assert!(
            s.iter().filter(|l| l.starts_with('├')).count() == 2,
            "{all}"
        );
        assert!(s[23].starts_with('└'), "{all}");
    }

    #[test]
    fn details_follow_the_selection() {
        let mut app = app();
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.key(k(KeyCode::Down));
        app.key(k(KeyCode::Tab));
        let s = screen(&app);
        let all = s.join("\n");
        assert!(has(&s, "Details: ktbr1/radio"), "{all}");
        assert!(
            has(
                &s,
                "TNC d75: TH-D75 (th-d75), Bluetooth 40:79:12:E4:95:9D on builtin, paired"
            ),
            "{all}"
        );
        assert!(has(&s, "connect always"), "{all}");
    }

    #[test]
    fn the_tncs_and_frames_views() {
        let mut app = app();
        app.view = View::Tncs;
        let s = screen(&app);
        let all = s.join("\n");
        assert!(has(&s, "d75      TH-D75 (th-d75)"), "{all}");
        assert!(has(&s, "Bluetooth  40:79:12:E4:95:9D on builtin"), "{all}");
        assert!(has(&s, "paired"), "{all}");
        assert!(
            has(&s, "builtin  hci0  2C:CF:67:6E:2E:BC  built into the board"),
            "{all}"
        );
        assert!(has(&s, "Bridge ktbr1 uses it."), "{all}");

        // A paired radio that isn't a TNC, under its adapter.
        let mut st = status();
        st.adapters[0].devices = vec![crate::control::protocol::BtDevice {
            address: "40:79:12:00:00:01".into(),
            name: Some("TH-D75".into()),
            rssi: None,
            icon: None,
            paired: true,
            known_tnc: Some("Kenwood TH-D75".into()),
            model: Some("th-d75".into()),
            tnc: None,
        }];
        app.set_status(st);
        app.tnc_selected = Some("device:2C:CF:67:6E:2E:BC/40:79:12:00:00:01".into());
        let s = screen(&app);
        let all = s.join("\n");
        assert!(
            has(&s, "paired  40:79:12:00:00:01  TH-D75 (Kenwood TH-D75)"),
            "{all}"
        );
        assert!(has(&s, "not a TNC yet"), "{all}");
        assert!(has(&s, "not set up as a TNC yet: a sets it up"), "{all}");
        assert!(has(&s, "a set it up  u unpair  n scan"), "{all}");
        app.set_status(status());

        app.view = View::Frames;
        let s = screen(&app);
        let all = s.join("\n");
        assert!(
            has(&s, "All bridges · following the newest · 1 frames"),
            "{all}"
        );
        assert!(has(&s, "rx ktbr1/radio  W1AW-9>APDR16"), "{all}");
    }

    #[test]
    fn dialogs_and_keys_for_the_row() {
        let mut app = app();
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let s = screen(&app);
        assert!(has(&s, "e enable  d disable  s switch"), "{}", s.join("\n"));
        app.key(k(KeyCode::Down));
        let s = screen(&app);
        assert!(
            has(&s, "c connect  x disconnect  r release"),
            "{}",
            s.join("\n")
        );
        app.dialog = Some(Dialog {
            title: "Disable ktbr1 (TH-D75)?".into(),
            lines: vec![
                "This would:".into(),
                "  • disconnect session ktbr1/phones/40:33:E0:1B:DB:76, and a long tail that wraps"
                    .into(),
            ],
            yes: None,
        });
        let s = screen(&app);
        let all = s.join("\n");
        assert!(has(&s, "┌ Disable ktbr1 (TH-D75)? "), "{all}");
        assert!(has(&s, "  • disconnect session"), "{all}");
        // The wrapped part lines up under the bullet's text.
        let i = s.iter().position(|l| l.contains("• disconnect")).unwrap();
        let col = |l: &str, c: char| l.chars().position(|x| x == c);
        let text_at = col(&s[i], 'd');
        let next = &s[i + 1];
        let next_at = next
            .chars()
            .enumerate()
            .skip(12)
            .find(|(_, c)| *c != ' ')
            .map(|(i, _)| i);
        assert_eq!(text_at, next_at, "{all}");
        assert!(has(&s, "⏎ close"), "{all}");
    }

    #[test]
    fn the_daemon_away() {
        let mut app = App::new("aprscc".into());
        app.link = Link::Down("can't reach the daemon: nothing is listening".into());
        let s = screen(&app);
        assert!(has(&s, "can't reach the daemon"));
        assert!(has(&s, "Trying again every 2 s"));
    }

    #[test]
    fn text_fits() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(lr("left", "right", 12), "left   right");
        assert_eq!(lr("a long left side", "r", 8), "a lon… r");
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("", 7), [""]);
        assert_eq!(
            source_line("static: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml", false),
            "from /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml: read-only here; edit the file, then reload"
        );
    }
    #[test]
    fn the_scan_and_pairing_boxes() {
        use super::super::app::{Pairing, Scan};
        use crate::control::protocol::BtDevice;
        let mut app = app();
        app.view = View::Tncs;
        let s = screen(&app);
        let all = s.join("\n");
        assert!(has(&s, "p pair  u unpair  n scan"), "{all}");
        assert!(
            has(&s, "Unpairing would drop the link on ktbr1/radio."),
            "{all}"
        );
        let d = |address: &str, name: Option<&str>, known: Option<&str>, rssi| BtDevice {
            address: address.into(),
            name: name.map(Into::into),
            rssi: Some(rssi),
            icon: Some("phone".into()),
            paired: false,
            known_tnc: known.map(Into::into),
            model: None,
            tnc: None,
        };
        app.scan = Some(Scan {
            title: "Scan on builtin (hci0)".into(),
            hint: vec!["Radios have to be in pairing mode to be found.".into()],
            target: None,
            adapter: None,
            request: Some(1),
            started: Instant::now(),
            devices: vec![
                d(
                    "40:79:12:E4:95:9D",
                    Some("TH-D75"),
                    Some("Kenwood TH-D75"),
                    -52,
                ),
                d("50:3B:70:E1:1C:11", None, None, -64),
            ],
            selected: None,
            over: false,
            error: None,
            then_pair: None,
        });
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(has(&s, "┌ Scan on builtin (hci0) "), "{all}");
        assert!(has(&s, " s left ┐"), "{all}");
        assert!(
            has(&s, "40:79:12:E4:95:9D  TH-D75       Kenwood TH-D75"),
            "{all}"
        );
        assert!(has(&s, "-52 dBm"), "{all}");
        assert!(has(&s, "50:3B:70:E1:1C:11  -            phone"), "{all}");
        assert!(
            has(&s, "⏎ pair it   a set it up as a new TNC   Esc stop"),
            "{all}"
        );

        // p: looking for one TNC's radio, which hasn't turned up.
        let sc = app.scan.as_mut().unwrap();
        sc.title = "Pair d75 (TH-D75): looking on builtin (hci0)".into();
        sc.hint = vec![
            "Put the radio in pairing mode: Menu > Bluetooth > Pairing Mode".into(),
            "Pairing starts when 40:79:12:E4:95:9D turns up.".into(),
        ];
        sc.target = app.status.as_ref().map(|s| s.tncs[0].clone());
        sc.devices.remove(0);
        sc.over = true;
        sc.error = Some(
            "40:79:12:E4:95:9D didn't turn up in 60 s. Is the radio on, nearby, and in pairing mode?"
                .into(),
        );
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(
            has(&s, "Pairing starts when 40:79:12:E4:95:9D turns up."),
            "{all}"
        );
        assert!(has(&s, "50:3B:70:E1:1C:11"), "{all}");
        assert!(
            has(&s, "✗ 40:79:12:E4:95:9D didn't turn up in 60 s."),
            "{all}"
        );
        assert!(has(&s, "n look again   Esc close"), "{all}");

        app.scan = None;
        app.pairing = Some(Pairing {
            title: "Pair d75 (TH-D75)".into(),
            request: Some(1),
            steps: vec![
                "Put the radio in pairing mode: Menu > Bluetooth > Pairing Mode".into(),
                "Pairing with TH-D75...".into(),
            ],
            ask: Some(Ask::Confirm("774523".into())),
            typed: String::new(),
            end: None,
            bridge: None,
            hidden: false,
        });
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(has(&s, "┌ Pair d75 (TH-D75) "), "{all}");
        assert!(
            has(&s, "Put the radio in pairing mode: Menu > Bluetooth >"),
            "{all}"
        );
        assert!(has(&s, "The radio should show   7 7 4 5 2 3"), "{all}");
        assert!(has(&s, "[ y  Yes, it matches ]     [ n  No ]"), "{all}");
        assert!(has(&s, " Esc no ┘"), "{all}");

        let p = app.pairing.as_mut().unwrap();
        p.ask = None;
        p.end = Some((
            "Paired and trusted on hci0 (2C:CF:67:6E:2E:BC). Bridge ktbr1 can connect to it now."
                .into(),
            true,
        ));
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(has(&s, "✓ Paired and trusted on hci0"), "{all}");
        assert!(has(&s, " ⏎ close ┘"), "{all}");
    }

    #[test]
    fn the_add_menu_forms_and_no_bridges() {
        let mut app = app();
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.selected = Some("ktbr1".into());
        app.key(k(KeyCode::Char('a')));
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(has(&s, "┌ Add "), "{all}");
        assert!(
            has(&s, "A bridge for a TNC, step by step (ktbctl setup)"),
            "{all}"
        );
        assert!(has(&s, "A port on ktbr1"), "{all}");

        app.key(k(KeyCode::Down));
        app.key(k(KeyCode::Enter));
        app.key(k(KeyCode::Right));
        app.key(k(KeyCode::Down));
        app.key(k(KeyCode::Down));
        for _ in 0..4 {
            app.key(k(KeyCode::Backspace));
        }
        for c in "wlan0:8002".chars() {
            app.key(k(KeyCode::Char(c)));
        }
        let s = screen(&app);
        let all = s.join("\n");
        println!("{all}");
        assert!(has(&s, "┌ Add a port to ktbr1 "), "{all}");
        assert!(has(&s, "Type        tcp-server"), "{all}");
        assert!(has(&s, "Id          lan2"), "{all}");
        assert!(has(&s, "Listen      wlan0:8002_"), "{all}");
        assert!(has(&s, "(empty: anyone)"), "{all}");
        assert!(
            has(&s, "Anyone who can reach wlan0:8002 can transmit"),
            "{all}"
        );
        assert!(has(&s, "[ ⏎ Add ]     [ Esc Cancel ]"), "{all}");

        let mut none = App::new("aprscc".into());
        none.link = Link::Up;
        let mut st = status();
        st.bridges.clear();
        none.set_status(st);
        let s = screen(&none);
        let all = s.join("\n");
        assert!(has(&s, "No bridges yet."), "{all}");
        assert!(has(&s, "Press a to set one up, step by step."), "{all}");
        assert!(has(&s, "a add  2 TNCs  ? help  q quit"), "{all}");
    }
}
