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

//! `ktbctl`'s human output. Everything here takes the daemon's results and
//! returns text, so it can be tested without a daemon.

use std::fmt::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::control::protocol::{
    BridgeStatus, BtDevice, CheckResult, Dir, Event, ModelEntry, PlanResult, PortStatus,
    ProblemJson, SerialEntry, Status,
};
use crate::reconcile::{Action, Plan};

/// `status`, for every bridge or just one (by id or name).
pub fn status(s: &Status, bridge: Option<&str>, now: SystemTime) -> Result<String, String> {
    let mut out = String::new();
    let mut shown = 0;
    for b in &s.bridges {
        if bridge.is_some_and(|w| w != b.id && w != b.name) {
            continue;
        }
        shown += 1;
        bridge_block(&mut out, b, now);
    }
    if let Some(w) = bridge {
        if shown == 0 {
            return Err(format!("there's no bridge {w:?}"));
        }
        return Ok(out);
    }
    if s.bridges.is_empty() {
        out.push_str("no bridges (add one with: ktbctl bridge add --name NAME)\n");
    }
    out.push('\n');
    out.push_str(&tncs(s));
    out.push('\n');
    out.push_str(&adapters(s));
    Ok(out)
}

fn bridge_block(out: &mut String, b: &BridgeStatus, now: SystemTime) {
    let state = match (b.enabled, b.enabled_override) {
        (true, None) => "enabled".to_string(),
        (false, None) => "disabled".to_string(),
        (e, Some(_)) => format!("{} (override)", if e { "enabled" } else { "disabled" }),
    };
    let _ = write!(out, "{}  {:<32} {}", b.id, b.name, state);
    if !b.editable {
        let _ = write!(out, "  [{}]", b.source);
    }
    out.push('\n');
    if !b.incomplete.is_empty() {
        let _ = writeln!(out, "  needs: {}", b.incomplete.join(", "));
    }
    if b.public_name.effective != b.name {
        let _ = write!(out, "  public name \"{}\"", b.public_name.effective);
        if b.public_name.effective != b.public_name.configured {
            let _ = write!(out, " (configured \"{}\")", b.public_name.configured);
        }
        out.push('\n');
    }
    for p in &b.ports {
        port_line(out, p, now);
    }
    if let Some(c) = &b.counters {
        let _ = writeln!(
            out,
            "  frames from the TNC {}, to it {}, dropped {}",
            c.frames_down, c.frames_up, c.dropped
        );
    }
}

fn port_line(out: &mut String, p: &PortStatus, now: SystemTime) {
    let mut state = p.state.clone();
    if let Some(since) = p.since.as_deref().and_then(parse_rfc3339) {
        state = format!("{state} {}", ago(since, now));
    }
    let mut what = Vec::new();
    if let Some(t) = p.settings.get("tnc").and_then(|v| v.as_str()) {
        what.push(format!("tnc {t}"));
    }
    if let Some(a) = p.settings.get("address").and_then(|v| v.as_str()) {
        what.push(a.to_string());
    }
    if let Some(d) = &p.detail {
        what.push(d.clone());
    }
    if p.role == "app" {
        let max = p.settings.get("max_clients").and_then(|v| v.as_u64());
        what.push(match max {
            Some(m) => format!("{}/{m} sessions", p.sessions.len()),
            None => format!("{} sessions", p.sessions.len()),
        });
    }
    let _ = writeln!(
        out,
        "  {:<10} {:<11} {:<26} {}",
        p.id,
        p.kind,
        state,
        what.join("  ")
    );
    for s in &p.sessions {
        let since = parse_rfc3339(&s.since)
            .map(|t| ago(t, now))
            .unwrap_or_default();
        let _ = writeln!(out, "      └ {}  {since}", s.id);
    }
}

/// The TNCs, as in `status` and `tnc list`.
pub fn tncs(s: &Status) -> String {
    let mut out = String::from("TNCs\n");
    if s.tncs.is_empty() {
        out.push_str("  none (add one with: ktbctl tnc add ID ...)\n");
    }
    for t in &s.tncs {
        let mut parts = vec![format!("{:<10} {:<20} {}", t.id, t.name, t.model)];
        if let Some(spp) = &t.spp {
            let paired = match spp.paired {
                Some(true) => "paired",
                Some(false) => "NOT PAIRED",
                None => "pairing unknown",
            };
            parts.push(format!(
                "bluetooth {} on {}: {paired}",
                spp.address, spp.adapter
            ));
        }
        if let Some(ser) = &t.serial {
            parts.push(format!(
                "serial {}: {}",
                ser.path,
                if ser.present {
                    "present"
                } else {
                    "not present"
                }
            ));
        }
        if let Some(b) = &t.in_use_by {
            parts.push(format!("used by {b}"));
        }
        let _ = writeln!(out, "  {}", parts.join("  "));
    }
    out
}

/// The adapters, as in `status` and `adapter list`.
pub fn adapters(s: &Status) -> String {
    let mut out = String::from("Adapters\n");
    if s.adapters.is_empty() {
        out.push_str("  none found\n");
    }
    for a in &s.adapters {
        let state = match (a.present, a.powered) {
            (false, _) => "not present",
            (true, Some(true)) => "powered",
            (true, Some(false)) => "off",
            (true, None) => "present",
        };
        let _ = writeln!(
            out,
            "  {:<10} {:<6} {}  {state}",
            a.id.as_deref().unwrap_or("-"),
            a.hci.as_deref().unwrap_or("-"),
            a.mac
        );
    }
    out
}

pub fn serial(list: &[SerialEntry]) -> String {
    if list.is_empty() {
        return "no serial devices found\n".into();
    }
    let mut out = String::new();
    for e in list {
        let _ = write!(out, "{}\n    {}", e.path, e.device);
        if let Some(d) = &e.description {
            let _ = write!(out, "  {d}");
        }
        if let Some(u) = &e.usb_id {
            let _ = write!(out, "  ({u})");
        }
        out.push('\n');
        if let Some(k) = &e.known_tnc {
            let _ = write!(out, "    a {k}");
            if let Some(m) = &e.model {
                let _ = write!(out, ": use model = \"{m}\"");
            }
            out.push('\n');
        }
        if let Some(t) = &e.tnc {
            let _ = writeln!(out, "    used by tnc {t}");
        }
        for a in &e.also {
            let _ = writeln!(out, "    also {a}");
        }
    }
    out
}

/// `scan`: likely TNCs first, as the daemon sorted them.
pub fn bt_devices(list: &[BtDevice]) -> String {
    if list.is_empty() {
        return "nothing found. Is the radio in pairing mode, with Bluetooth on?\n".into();
    }
    let mut out = String::new();
    for d in list {
        let what = match (&d.known_tnc, &d.model) {
            (Some(k), Some(m)) => format!("{k} (model {m})"),
            (Some(k), None) => k.clone(),
            _ => d.icon.clone().unwrap_or_default(),
        };
        let mut extra = Vec::new();
        if let Some(r) = d.rssi {
            extra.push(format!("{r} dBm"));
        }
        if d.paired {
            extra.push("paired".into());
        }
        if let Some(t) = &d.tnc {
            extra.push(format!("tnc {t}"));
        }
        let _ = writeln!(
            out,
            "{}  {:<20} {:<32} {}",
            d.address,
            d.name.as_deref().unwrap_or("(no name)"),
            what,
            extra.join("  ")
        );
    }
    out
}

pub fn models(list: &[ModelEntry]) -> String {
    let mut out = String::new();
    for m in list {
        let _ = writeln!(out, "{:<16} {}", m.names.join(", "), m.summary);
        for r in &m.radios {
            let mut how = Vec::new();
            if r.bluetooth.is_some() {
                how.push("Bluetooth");
            }
            if r.usb.is_some() {
                how.push("USB");
            }
            let _ = writeln!(
                out,
                "{:<16}   {} (model = \"{}\"): {}",
                "",
                r.name,
                r.model,
                how.join(", ")
            );
        }
    }
    out
}

/// What a change did: anything it created, then each bridge and port it
/// touched, then config warnings.
pub fn change(created: Option<&str>, r: &PlanResult) -> String {
    let mut out = String::new();
    if let Some(id) = created {
        let _ = writeln!(out, "created {id}");
    }
    let lines = plan_lines(&r.plan);
    if lines.is_empty() && created.is_none() {
        out.push_str("done; no running bridge changed\n");
    }
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
    for w in &r.warnings {
        let _ = writeln!(out, "warning: {}", problem(w));
    }
    out
}

/// One line per bridge or port that changes: `ktbr0 started`,
/// `ktbr0/lan restarted (listen)`.
pub fn plan_lines(plan: &Plan) -> Vec<String> {
    let mut out: Vec<String> = plan.global.iter().map(|g| format!("{g} updated")).collect();
    for b in &plan.bridges {
        if b.action != Action::Unchanged {
            out.push(with_changes(format!("{} {}", b.id, b.action), &b.changes));
        }
        if b.action == Action::Updated {
            for p in &b.ports {
                if p.action != Action::Unchanged {
                    out.push(with_changes(
                        format!("{}/{} {}", b.id, p.id, p.action),
                        &p.changes,
                    ));
                }
            }
        }
    }
    out
}

fn with_changes(line: String, changes: &[String]) -> String {
    if changes.is_empty() {
        line
    } else {
        format!("{line} ({})", changes.join(", "))
    }
}

pub fn check(r: &CheckResult) -> String {
    let mut out = String::new();
    for e in &r.errors {
        let _ = writeln!(out, "error: {}", problem(e));
    }
    for w in &r.warnings {
        let _ = writeln!(out, "warning: {}", problem(w));
    }
    if !r.valid {
        let _ = writeln!(out, "the config has errors; a reload would be refused");
        return out;
    }
    out.push_str("config ok\n");
    match r.plan.as_ref().map(plan_lines) {
        Some(lines) if !lines.is_empty() => {
            out.push_str("a reload would:\n");
            for l in lines {
                let _ = writeln!(out, "  {l}");
            }
            if r.disruptive {
                let _ = writeln!(out, "and would {}", join_and(&r.disruptions));
            }
        }
        _ => out.push_str("a reload would change nothing\n"),
    }
    out
}

pub fn problem(p: &ProblemJson) -> String {
    let mut s = String::new();
    if let Some(src) = &p.source {
        let _ = write!(s, "{src}: ");
    }
    if !p.object.is_empty() {
        let _ = write!(s, "{}: ", p.object);
    }
    s.push_str(&p.message);
    s
}

/// One line of `monitor`: `14:22:07Z rx ktbr0/radio  W1AW-9>APDR16:...`.
pub fn frame_line(ev: &Event) -> Option<String> {
    let Event::Frame {
        object,
        dir,
        session,
        at,
        decoded,
        ..
    } = ev
    else {
        return None;
    };
    let time = at.get(11..19).map(|t| format!("{t}Z")).unwrap_or_default();
    let dir = match dir {
        Dir::Rx => "rx",
        Dir::Tx => "tx",
    };
    let mut line = format!("{time} {dir} {object}  {}", decoded.summary);
    if let Some(s) = session {
        let _ = write!(line, "   (from {s})");
    }
    Some(line)
}

/// "a", "a and b", "a, b and c".
pub fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `2026-10-16T02:26:54Z` (as the daemon writes them), as a time.
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days since 1970-01-01, by Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

/// How long ago `then` was: `40s`, `12m`, `2h14m`, `3d4h`.
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(then).unwrap_or_default().as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h{}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d{}h", s / 86_400, s % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::protocol::rfc3339;

    #[test]
    fn times_round_trip() {
        let t = UNIX_EPOCH + Duration::from_secs(1_791_813_731);
        assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
        assert_eq!(parse_rfc3339("2026-10-16T02:26:54"), None);
        assert_eq!(parse_rfc3339("not a time at all!!!"), None);
    }

    #[test]
    fn ages_read_naturally() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago_s = |s: u64| ago(now - Duration::from_secs(s), now);
        assert_eq!(ago_s(5), "5s");
        assert_eq!(ago_s(125), "2m");
        assert_eq!(ago_s(2 * 3600 + 14 * 60), "2h14m");
        assert_eq!(ago_s(3 * 86_400 + 4 * 3600), "3d4h");
    }

    #[test]
    fn status_shows_ports_sessions_and_what_a_bridge_needs() {
        let s: Status = serde_json::from_value(serde_json::json!({
            "version": "1.0.0",
            "adapters": [{"id": "dongle", "mac": "AC:A7:F1:29:9C:7F", "hci": "hci1", "present": true, "powered": true}],
            "tncs": [{"id": "d75", "name": "TH-D75", "model": "th-d74/th-d75",
                      "spp": {"address": "40:79:12:E4:95:9D", "adapter": "builtin", "paired": true},
                      "in_use_by": "ktbr0", "source": "dynamic", "editable": true}],
            "bridges": [
              {"id": "ktbr0", "name": "Portable", "enabled": true,
               "public_name": {"configured": "Portable", "effective": "Portable (2)"},
               "source": "dynamic", "editable": true,
               "ports": [
                 {"id": "radio", "type": "spp-client", "role": "tnc", "state": "connected",
                  "since": "2026-10-16T02:00:00Z", "settings": {"tnc": "d75"}, "sessions": []},
                 {"id": "phones", "type": "ble-server", "role": "app", "state": "advertising",
                  "detail": "hci1", "settings": {"max_clients": 2},
                  "sessions": [{"id": "ktbr0/phones/7C:2A:00:11:22:33", "since": "2026-10-16T02:10:00Z"}]}],
               "counters": {"frames_up": 2, "frames_down": 18, "dropped": 0, "tnc_connects": 1}},
              {"id": "ktbr1", "name": "Spare", "enabled": false,
               "public_name": {"configured": "Spare", "effective": "Spare"},
               "source": "dynamic", "editable": true, "incomplete": ["no TNC port", "no app port"],
               "ports": []}
            ]
        }))
        .unwrap();
        let now = parse_rfc3339("2026-10-16T02:30:00Z").unwrap();
        let out = status(&s, None, now).unwrap();
        assert!(out.contains("ktbr0  Portable"), "{out}");
        assert!(out.contains("connected 30m"), "{out}");
        assert!(out.contains("1/2 sessions"), "{out}");
        assert!(
            out.contains("└ ktbr0/phones/7C:2A:00:11:22:33  20m"),
            "{out}"
        );
        assert!(
            out.contains("public name \"Portable (2)\" (configured \"Portable\")"),
            "{out}"
        );
        assert!(out.contains("needs: no TNC port, no app port"), "{out}");
        assert!(out.contains("from the TNC 18, to it 2"), "{out}");
        assert!(out.contains("used by ktbr0"), "{out}");
        let one = status(&s, Some("Spare"), now).unwrap();
        assert!(one.contains("ktbr1") && !one.contains("ktbr0"), "{one}");
        assert!(status(&s, Some("nope"), now).is_err());
    }

    #[test]
    fn scans_show_what_each_device_is() {
        let l: Vec<BtDevice> = serde_json::from_value(serde_json::json!([
            {"address": "40:79:12:E4:95:9D", "name": "TH-D75", "rssi": -52, "paired": true,
             "known_tnc": "Kenwood TH-D75", "model": "th-d75", "tnc": "d75"},
            {"address": "50:3B:70:E1:1C:11", "name": "jPhone", "icon": "phone", "paired": false}
        ]))
        .unwrap();
        let out = bt_devices(&l);
        let mut lines = out.lines();
        let d75 = lines.next().unwrap();
        assert!(d75.contains("Kenwood TH-D75 (model th-d75)"), "{d75}");
        assert!(d75.contains("-52 dBm  paired  tnc d75"), "{d75}");
        assert!(lines.next().unwrap().contains("phone"));
        assert!(bt_devices(&[]).contains("pairing mode"));
    }

    #[test]
    fn plans_list_what_changes() {
        let plan: Plan = serde_json::from_value(serde_json::json!({
            "global": [],
            "bridges": [
              {"id": "ktbr0", "action": "updated", "changes": [], "stops_in_use": false,
               "ports": [{"id": "lan", "action": "restarted", "changes": ["listen"], "disconnects": [], "drops_tnc_link": false},
                         {"id": "radio", "action": "unchanged", "changes": [], "disconnects": [], "drops_tnc_link": false}]},
              {"id": "ktbr1", "action": "started", "changes": [], "stops_in_use": false, "ports": []},
              {"id": "ktbr2", "action": "unchanged", "changes": [], "stops_in_use": false, "ports": []}
            ]
        }))
        .unwrap();
        assert_eq!(
            plan_lines(&plan),
            [
                "ktbr0 updated",
                "ktbr0/lan restarted (listen)",
                "ktbr1 started"
            ]
        );
    }
}
