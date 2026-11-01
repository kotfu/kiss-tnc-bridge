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

//! Adding things: the `a` menu, and small forms for a port, an empty
//! bridge, a TNC and a name for an adapter. A bridge for a TNC, step by
//! step, is `ktbctl setup`'s job; the menu hands off to it.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

use super::super::setup::{
    adapter_ref, describe_adapter, numbered, reaches_network, suggest_adapter_id, suggest_tnc_id,
};
use super::app::{Action, App, Req, Then, View, adapter_label, consent, request};
use crate::control::protocol::Command;

/// What `a` can add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Add {
    /// A TNC and a bridge for it, by `ktbctl setup`.
    Setup,
    /// A port on this bridge.
    Port(String),
    Bridge,
    Tnc,
    Adapter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub items: Vec<(String, Add)>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Text(String),
    /// The options, as (shown, written), and the chosen one.
    Choice(Vec<(String, String)>, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub value: FieldValue,
    /// After the value: `(empty: anyone)`.
    pub note: &'static str,
    /// Typed in, so a suggestion doesn't replace it.
    touched: bool,
}

impl Field {
    fn text(key: &'static str, label: &'static str, value: &str, note: &'static str) -> Field {
        Field {
            key,
            label,
            value: FieldValue::Text(value.into()),
            note,
            touched: false,
        }
    }

    fn choice(key: &'static str, label: &'static str, options: Vec<(String, String)>) -> Field {
        Field {
            key,
            label,
            value: FieldValue::Choice(options, 0),
            note: "",
            touched: false,
        }
    }

    /// What goes in the config.
    pub fn value(&self) -> &str {
        match &self.value {
            FieldValue::Text(s) => s.trim(),
            FieldValue::Choice(o, i) => o.get(*i).map_or("", |x| x.1.as_str()),
        }
    }

    /// What's shown.
    pub fn shown(&self) -> &str {
        match &self.value {
            FieldValue::Text(s) => s,
            FieldValue::Choice(o, i) => o.get(*i).map_or("(none)", |x| x.0.as_str()),
        }
    }
}

/// A small form in a box.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub title: String,
    pub what: Add,
    pub fields: Vec<Field>,
    /// The field with the cursor, by index into `fields`.
    pub focus: usize,
    pub error: Option<String>,
    /// Ids already taken, for suggesting one.
    taken: Vec<String>,
    /// For an adapter, the id to suggest for each one.
    suggest: Vec<String>,
}

impl Form {
    fn new(title: String, what: Add, fields: Vec<Field>, taken: Vec<String>) -> Form {
        let mut f = Form {
            title,
            what,
            fields,
            focus: 0,
            error: None,
            taken,
            suggest: Vec::new(),
        };
        f.resuggest();
        f
    }

    pub fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .map_or("", |f| f.value())
    }

    /// The fields that apply, given the choices so far.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|&i| self.applies(self.fields[i].key))
            .collect()
    }

    fn applies(&self, key: &str) -> bool {
        match &self.what {
            Add::Port(_) => {
                let t = self.get("type");
                match key {
                    "listen" | "allow" => t == "tcp-server",
                    "address" => t == "tcp-client",
                    "tnc_spp" => t == "spp-client",
                    "tnc_serial" => t == "serial",
                    "adapter" => t == "ble-server",
                    "connect" => matches!(t, "tcp-client" | "spp-client" | "serial"),
                    _ => true,
                }
            }
            Add::Tnc => {
                let bt = self.get("link") == "spp";
                match key {
                    "address" | "adapter" => bt,
                    "path" => !bt,
                    _ => true,
                }
            }
            _ => true,
        }
    }

    /// What to know before adding it.
    pub fn warning(&self) -> Option<String> {
        let listen = self.get("listen");
        (matches!(self.what, Add::Port(_))
            && self.get("type") == "tcp-server"
            && reaches_network(listen)
            && self.get("allow").is_empty())
        .then(|| {
            format!("Anyone who can reach {listen} can transmit through your radio, under your callsign.")
        })
    }

    /// Put a fresh suggestion in the id, unless it's been typed in.
    fn resuggest(&mut self) {
        let taken: Vec<&str> = self.taken.iter().map(String::as_str).collect();
        let id = match &self.what {
            Add::Port(_) => {
                let base = match self.get("type") {
                    "ble-server" => "phones",
                    "tcp-server" => "lan",
                    "tcp-client" => "tnc",
                    _ => "radio",
                };
                numbered(base, &taken)
            }
            Add::Tnc => suggest_tnc_id(self.get("model"), &taken),
            Add::Adapter => match self.fields.iter().find(|f| f.key == "mac") {
                Some(Field {
                    value: FieldValue::Choice(_, i),
                    ..
                }) => self.suggest.get(*i).cloned().unwrap_or_default(),
                _ => return,
            },
            Add::Bridge | Add::Setup => return,
        };
        if let Some(f) = self.fields.iter_mut().find(|f| f.key == "id")
            && !f.touched
        {
            f.value = FieldValue::Text(id);
        }
    }

    /// The definition to send, or what's missing.
    fn definition(&self) -> Result<Value, String> {
        let need = |key: &str, what: &str| match self.get(key) {
            "" => Err(format!("{what} is needed")),
            v => Ok(v.to_string()),
        };
        match &self.what {
            Add::Port(_) => {
                let t = self.get("type");
                let mut d = json!({"id": need("id", "An id")?, "type": t});
                match t {
                    "tcp-server" => {
                        d["listen"] = json!(need("listen", "Where to listen")?);
                        if !self.get("allow").is_empty() {
                            d["allow"] = json!([self.get("allow")]);
                        }
                    }
                    "tcp-client" => d["address"] = json!(need("address", "The TNC's address")?),
                    "ble-server" => {
                        if !self.get("adapter").is_empty() {
                            d["adapter"] = json!(self.get("adapter"));
                        }
                    }
                    "spp-client" => {
                        d["tnc"] = json!(need("tnc_spp", "A TNC with Bluetooth (add one first)")?)
                    }
                    _ => d["tnc"] = json!(need("tnc_serial", "A TNC with USB (add one first)")?),
                }
                if self.applies("connect") && self.get("connect") == "always" {
                    d["connect"] = json!("always");
                }
                Ok(d)
            }
            Add::Bridge => {
                let mut d = json!({"name": need("name", "A name")?});
                if !self.get("id").is_empty() {
                    d["id"] = json!(self.get("id"));
                }
                Ok(d)
            }
            Add::Tnc => {
                let id = need("id", "An id")?;
                let name = match self.get("name") {
                    "" => id.clone(),
                    n => n.to_string(),
                };
                let mut d = json!({"id": id, "name": name, "model": self.get("model")});
                if self.get("link") == "spp" {
                    let mut spp =
                        json!({"address": need("address", "The radio's Bluetooth address")?});
                    if !self.get("adapter").is_empty() {
                        spp["adapter"] = json!(self.get("adapter"));
                    }
                    d["spp"] = spp;
                } else {
                    d["serial"] = json!({"path": need("path", "The device's path")?});
                }
                Ok(d)
            }
            Add::Adapter => {
                Ok(json!({"id": need("id", "An id")?, "mac": need("mac", "An adapter")?}))
            }
            Add::Setup => Err("setup isn't a form".into()),
        }
    }
}

impl App {
    /// `a`: what can be added here.
    pub(super) fn add_menu(&mut self) -> Action {
        if self.pairing.is_some() {
            return self.say("a pairing is under way".into());
        }
        let mut items = vec![(
            "A bridge for a TNC, step by step (ktbctl setup)".to_string(),
            Add::Setup,
        )];
        if self.view == View::Bridges
            && let Some(b) = self.selected_bridge()
            && b.editable
        {
            items.push((format!("A port on {}", b.id), Add::Port(b.id.clone())));
        }
        items.push(("An empty bridge".into(), Add::Bridge));
        items.push(("A TNC".into(), Add::Tnc));
        let unnamed = self
            .status
            .as_ref()
            .is_some_and(|s| s.adapters.iter().any(|a| a.present && a.id.is_none()));
        if unnamed {
            items.push(("A name for an adapter".into(), Add::Adapter));
        }
        let selected = match self.view {
            View::Tncs => items.iter().position(|i| i.1 == Add::Tnc).unwrap_or(0),
            _ => 0,
        };
        self.menu = Some(super::add::Menu { items, selected });
        Action::None
    }

    pub(super) fn menu_key(&mut self, k: KeyEvent) -> Action {
        let Some(m) = &mut self.menu else {
            return Action::None;
        };
        match k.code {
            KeyCode::Up => m.selected = m.selected.saturating_sub(1),
            KeyCode::Down => m.selected = (m.selected + 1).min(m.items.len() - 1),
            KeyCode::Esc => self.menu = None,
            KeyCode::Enter => {
                let what = m.items[m.selected].1.clone();
                self.menu = None;
                return self.open(what);
            }
            _ => {}
        }
        Action::None
    }

    fn open(&mut self, what: Add) -> Action {
        let Some(s) = &self.status else {
            return Action::None;
        };
        // The default adapter, then those present, as config names them.
        let adapters: Vec<(String, String)> =
            std::iter::once(("the default adapter".into(), String::new()))
                .chain(
                    s.adapters
                        .iter()
                        .filter(|a| a.present)
                        .map(|a| (adapter_label(a), adapter_ref(a))),
                )
                .collect();
        let form = match &what {
            Add::Setup => {
                self.new_since = Some(s.bridges.iter().map(|b| b.id.clone()).collect());
                return Action::Run(vec!["setup".into()]);
            }
            Add::Port(bridge) => {
                let taken = s
                    .bridges
                    .iter()
                    .find(|b| b.id == *bridge)
                    .map(|b| b.ports.iter().map(|p| p.id.clone()).collect())
                    .unwrap_or_default();
                let tncs = |has: fn(&crate::control::protocol::TncStatus) -> bool| {
                    s.tncs
                        .iter()
                        .filter(|t| has(t))
                        .map(|t| (format!("{} ({})", t.id, t.name), t.id.clone()))
                        .collect::<Vec<_>>()
                };
                let types = [
                    "ble-server",
                    "tcp-server",
                    "tcp-client",
                    "spp-client",
                    "serial",
                ]
                .map(|t| (t.to_string(), t.to_string()));
                Form::new(
                    format!("Add a port to {bridge}"),
                    what.clone(),
                    vec![
                        Field::choice("type", "Type", types.to_vec()),
                        Field::text("id", "Id", "", ""),
                        Field::text(
                            "listen",
                            "Listen",
                            "8001",
                            "(a port alone: this computer only)",
                        ),
                        Field::text("allow", "Allow", "", "(empty: anyone)"),
                        Field::text(
                            "address",
                            "Address",
                            "127.0.0.1:8001",
                            "(the KISS TCP server)",
                        ),
                        Field::choice("tnc_spp", "TNC", tncs(|t| t.spp.is_some())),
                        Field::choice("tnc_serial", "TNC", tncs(|t| t.serial.is_some())),
                        Field::choice("adapter", "Adapter", adapters),
                        Field::choice(
                            "connect",
                            "Connect",
                            vec![
                                ("while apps are connected".into(), "on_demand".into()),
                                ("all the time".into(), "always".into()),
                            ],
                        ),
                    ],
                    taken,
                )
            }
            Add::Bridge => Form::new(
                "Add an empty bridge".into(),
                what.clone(),
                vec![
                    Field::text("name", "Name", "", ""),
                    Field::text("id", "Id", "", "(empty: the next ktbrN)"),
                ],
                Vec::new(),
            ),
            Add::Tnc => {
                let mut models: Vec<(String, String)> = self
                    .models
                    .iter()
                    .map(|m| (m.names.join(", "), m.names[0].clone()))
                    .collect();
                if models.is_empty() {
                    models.push(("generic".into(), "generic".into()));
                }
                Form::new(
                    "Add a TNC".into(),
                    what.clone(),
                    vec![
                        Field::choice("model", "Model", models),
                        Field::text("id", "Id", "", ""),
                        Field::text("name", "Name", "", "(empty: the id)"),
                        Field::choice(
                            "link",
                            "Reached",
                            vec![
                                ("over Bluetooth".into(), "spp".into()),
                                ("over USB or serial".into(), "serial".into()),
                            ],
                        ),
                        Field::text("address", "Address", "", "(its Bluetooth MAC; n scans)"),
                        Field::choice("adapter", "Adapter", adapters),
                        Field::text("path", "Path", "/dev/serial/by-id/", ""),
                    ],
                    s.tncs.iter().map(|t| t.id.clone()).collect(),
                )
            }
            Add::Adapter => {
                let unnamed: Vec<_> = s
                    .adapters
                    .iter()
                    .filter(|a| a.present && a.id.is_none())
                    .collect();
                let taken: Vec<String> = s.adapters.iter().filter_map(|a| a.id.clone()).collect();
                let refs: Vec<&str> = taken.iter().map(String::as_str).collect();
                let mut f = Form::new(
                    "Name an adapter".into(),
                    what.clone(),
                    vec![
                        Field::choice(
                            "mac",
                            "Adapter",
                            unnamed
                                .iter()
                                .map(|a| (describe_adapter(a), a.mac.clone()))
                                .collect(),
                        ),
                        Field::text("id", "Id", "", "(what config calls it)"),
                    ],
                    taken.clone(),
                );
                f.suggest = unnamed
                    .iter()
                    .map(|a| suggest_adapter_id(a, &refs))
                    .collect();
                f.resuggest();
                f
            }
        };
        self.form = Some(form);
        Action::None
    }

    pub(super) fn form_key(&mut self, k: KeyEvent) -> Action {
        let Some(f) = &mut self.form else {
            return Action::None;
        };
        let vis = f.visible();
        let at = vis.iter().position(|&i| i == f.focus).unwrap_or(0);
        match k.code {
            KeyCode::Esc => self.form = None,
            KeyCode::Up | KeyCode::BackTab => f.focus = vis[at.saturating_sub(1)],
            KeyCode::Down | KeyCode::Tab => f.focus = vis[(at + 1).min(vis.len() - 1)],
            KeyCode::Left | KeyCode::Right => {
                if let FieldValue::Choice(o, i) = &mut f.fields[f.focus].value
                    && !o.is_empty()
                {
                    *i = if k.code == KeyCode::Left {
                        (*i + o.len() - 1) % o.len()
                    } else {
                        (*i + 1) % o.len()
                    };
                    f.resuggest();
                }
            }
            KeyCode::Char(_) | KeyCode::Backspace => {
                let field = &mut f.fields[f.focus];
                if let FieldValue::Text(t) = &mut field.value {
                    match k.code {
                        KeyCode::Char(c) => t.push(c),
                        _ => {
                            t.pop();
                        }
                    }
                    field.touched = true;
                }
            }
            KeyCode::Enter => return self.submit(),
            _ => {}
        }
        Action::None
    }

    fn submit(&mut self) -> Action {
        let Some(f) = &mut self.form else {
            return Action::None;
        };
        let d = match f.definition() {
            Ok(d) => d,
            Err(e) => {
                f.error = Some(e);
                return Action::None;
            }
        };
        f.error = None;
        let id = d["id"].as_str().unwrap_or_default().to_string();
        let req = match &f.what {
            Add::Port(bridge) => {
                let b = bridge.clone();
                consent(
                    move |nd| Command::PortAdd {
                        bridge: b.clone(),
                        definition: d.clone(),
                        non_disruptive: nd,
                    },
                    &format!("Add port {id} to {bridge}?"),
                    "Add it",
                    &format!("{bridge}/{id} added"),
                )
            }
            Add::Bridge => {
                let name = d["name"].as_str().unwrap_or_default().to_string();
                self.new_since = self
                    .status
                    .as_ref()
                    .map(|s| s.bridges.iter().map(|b| b.id.clone()).collect());
                consent(
                    move |nd| Command::BridgeCreate {
                        definition: d.clone(),
                        non_disruptive: nd,
                    },
                    "Add the bridge?",
                    "Add it",
                    &format!("bridge {name} added, disabled until it has ports and e"),
                )
            }
            Add::Tnc => consent(
                move |nd| Command::TncCreate {
                    definition: d.clone(),
                    non_disruptive: nd,
                },
                "Add the TNC?",
                "Add it",
                &format!("TNC {id} added"),
            ),
            Add::Adapter => {
                let mac = d["mac"].as_str().unwrap_or_default().to_string();
                consent(
                    move |nd| Command::AdapterCreate {
                        definition: d.clone(),
                        non_disruptive: nd,
                    },
                    "Name the adapter?",
                    "Name it",
                    &format!("{mac} is {id} now"),
                )
            }
            Add::Setup => return Action::None,
        };
        request(Req {
            command: req.command,
            then: Then::Form(Box::new(req.then)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::app::tests::status;
    use super::*;
    use crate::control::protocol::{ApiError, ErrorCode, ModelEntry};
    use crate::ctl::client::CtlError;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.key(key(KeyCode::Char(c)));
        }
    }

    fn app() -> App {
        let mut app = App::new("pi".into());
        app.set_status(status());
        app.selected = Some("ktbr1".into());
        app
    }

    #[test]
    fn the_menu_offers_what_fits_here_and_setup_hands_off() {
        let mut app = app();
        app.key(key(KeyCode::Char('a')));
        let m = app.menu.clone().expect("a menu");
        let items: Vec<&str> = m.items.iter().map(|i| i.0.as_str()).collect();
        assert_eq!(
            items,
            [
                "A bridge for a TNC, step by step (ktbctl setup)",
                "A port on ktbr1",
                "An empty bridge",
                "A TNC"
            ]
        );
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Action::Run(vec!["setup".into()])
        );
        // When it's back, the bridge it made is selected.
        let mut s = status();
        s.bridges.push(s.bridges[1].clone());
        s.bridges[2].id = "ktbr3".into();
        app.view = View::Tncs;
        app.set_status(s);
        assert_eq!(app.selected.as_deref(), Some("ktbr3"));
        assert_eq!(app.view, View::Bridges);

        // From the TNCs view it starts on a TNC, and an unnamed adapter can
        // be named.
        let mut s = status();
        s.adapters[0].id = None;
        app.set_status(s);
        app.view = View::Tncs;
        app.key(key(KeyCode::Char('a')));
        let m = app.menu.clone().unwrap();
        assert_eq!(m.items[m.selected].1, Add::Tnc);
        assert_eq!(m.items.last().unwrap().1, Add::Adapter);
        app.key(key(KeyCode::Esc));
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_port_form_follows_its_type() {
        let mut app = app();
        app.key(key(KeyCode::Char('a')));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        let f = app.form.clone().expect("a form");
        assert_eq!(f.title, "Add a port to ktbr1");
        assert_eq!(f.get("type"), "ble-server");
        // phones is taken.
        assert_eq!(f.get("id"), "phones2");
        let keys =
            |f: &Form| -> Vec<&str> { f.visible().iter().map(|&i| f.fields[i].key).collect() };
        assert_eq!(keys(&f), ["type", "id", "adapter"]);
        app.key(key(KeyCode::Right));
        let f = app.form.clone().unwrap();
        assert_eq!(keys(&f), ["type", "id", "listen", "allow"]);
        assert_eq!(f.get("id"), "lan2");
        assert_eq!(f.warning(), None);
        // To the listen field, and somewhere other hosts can reach.
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        for _ in 0..4 {
            app.key(key(KeyCode::Backspace));
        }
        typed(&mut app, "wlan0:8002");
        let f = app.form.clone().unwrap();
        assert!(
            f.warning()
                .unwrap()
                .starts_with("Anyone who can reach wlan0:8002")
        );
        let Action::Request(req) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter adds it");
        };
        assert_eq!(
            req.command,
            Command::PortAdd {
                bridge: "ktbr1".into(),
                definition: json!({"id": "lan2", "type": "tcp-server", "listen": "wlan0:8002"}),
                non_disruptive: true
            }
        );
        // A mistake comes back to the form; success closes it.
        app.on_answer(
            req.then.clone(),
            Err(CtlError::Api(ApiError::new(
                ErrorCode::ConfigInvalid,
                "port lan2: no interface wlan0",
            ))),
        );
        assert_eq!(
            app.form.as_ref().unwrap().error.as_deref(),
            Some("port lan2: no interface wlan0")
        );
        app.on_answer(req.then, Ok(json!({})));
        assert!(app.form.is_none());
        assert_eq!(app.message, Some(("ktbr1/lan2 added".into(), false)));
    }

    #[test]
    fn a_tnc_form_says_what_it_needs() {
        let mut app = app();
        app.models = serde_json::from_value::<Vec<ModelEntry>>(json!([
            {"names": ["generic"], "summary": "Any KISS TNC"},
            {"names": ["th-d75", "th-d74"], "summary": "Kenwood"}
        ]))
        .unwrap();
        app.view = View::Tncs;
        app.key(key(KeyCode::Char('a')));
        app.key(key(KeyCode::Enter));
        assert_eq!(app.form.as_ref().unwrap().get("id"), "tnc");
        // A TH-D75, and d75 is taken.
        app.key(key(KeyCode::Right));
        assert_eq!(app.form.as_ref().unwrap().get("id"), "d752");
        assert_eq!(app.key(key(KeyCode::Enter)), Action::None);
        assert_eq!(
            app.form.as_ref().unwrap().error.as_deref(),
            Some("The radio's Bluetooth address is needed")
        );
        // Over USB instead: a path.
        for _ in 0..3 {
            app.key(key(KeyCode::Down));
        }
        app.key(key(KeyCode::Right));
        app.key(key(KeyCode::Down));
        typed(&mut app, "usb-JVC");
        let Action::Request(req) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter adds it");
        };
        assert_eq!(
            req.command,
            Command::TncCreate {
                definition: json!({
                    "id": "d752", "name": "d752", "model": "th-d75",
                    "serial": {"path": "/dev/serial/by-id/usb-JVC"}
                }),
                non_disruptive: true
            }
        );
    }

    #[test]
    fn an_empty_bridge_is_selected_once_made() {
        let mut app = app();
        app.key(key(KeyCode::Char('a')));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        typed(&mut app, "Field day");
        let Action::Request(req) = app.key(key(KeyCode::Enter)) else {
            panic!("Enter adds it");
        };
        assert_eq!(
            req.command,
            Command::BridgeCreate {
                definition: json!({"name": "Field day"}),
                non_disruptive: true
            }
        );
        let mut s = status();
        s.bridges.push(s.bridges[1].clone());
        s.bridges[2].id = "ktbr3".into();
        app.set_status(s);
        assert_eq!(app.selected.as_deref(), Some("ktbr3"));
    }

    #[test]
    fn e_edits_what_is_selected() {
        let mut app = app();
        assert_eq!(
            app.key(key(KeyCode::Char('E'))),
            Action::Run(vec!["edit".into(), "ktbr1".into()])
        );
        app.view = View::Tncs;
        assert_eq!(
            app.key(key(KeyCode::Char('E'))),
            Action::Run(vec!["edit".into(), "tnc".into(), "d75".into()])
        );
        // A static bridge says why not.
        let mut s = status();
        s.bridges[0].editable = false;
        s.bridges[0].source = "static: /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml".into();
        app.set_status(s);
        app.view = View::Bridges;
        assert_eq!(app.key(key(KeyCode::Char('E'))), Action::None);
        assert_eq!(
            app.dialog.as_ref().unwrap().title,
            "ktbr1 is in the config file"
        );
    }
}
