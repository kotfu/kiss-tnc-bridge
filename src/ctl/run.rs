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

//! Running `ktbctl` commands against the daemon.
//!
//! **Consent.** A command that changes config is sent with `non_disruptive`
//! first. If the daemon refuses because it would disconnect someone, `ktbctl`
//! shows what it would do and asks, in a terminal; without one it refuses.
//! `--yes` skips the question and `--non-disruptive` refuses without asking.
//! The daemon plans and applies in one step, so what was shown is what
//! happens. `connect`, `disconnect` and `kick` exist to disrupt, and act at
//! once.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::cli::{AdapterCmd, BridgeCmd, Cli, Cmd, PortAdd, PortCmd, SerialCmd, TncAdd, TncCmd};
use super::client::{Client, CtlError, DEFAULT_SOCKET};
use super::render;
use crate::control::protocol::{
    ChangeResult, CheckResult, Command, ConfigText, ErrorCode, ModelEntry, ObjectKind, PlanResult,
    ProblemJson, SerialEntry, Status, Topic,
};

/// How a run ended, for the exit status.
pub enum Outcome {
    Ok,
    /// Something failed or was refused; the reason has been printed.
    Failed,
}

pub struct Ctl {
    socket: PathBuf,
    json: bool,
    yes: bool,
    non_disruptive: bool,
}

impl Ctl {
    pub fn new(cli: &Cli) -> Ctl {
        let socket = cli
            .socket
            .clone()
            .or_else(|| std::env::var_os("KTBCTL_SOCKET").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
        Ctl {
            socket,
            json: cli.json,
            yes: cli.yes,
            non_disruptive: cli.non_disruptive,
        }
    }

    pub async fn run(&self, cmd: Option<Cmd>) -> Outcome {
        let res = match cmd {
            // The TUI will live here; until then, the overview.
            None => self.status(None).await,
            Some(Cmd::Monitor { bridge }) => self.monitor(bridge).await,
            Some(cmd) => match Client::connect(&self.socket).await {
                Ok(mut c) => self.dispatch(&mut c, cmd).await,
                Err(e) => Err(e),
            },
        };
        match res {
            Ok(()) => Outcome::Ok,
            Err(e) => {
                self.report(&e);
                Outcome::Failed
            }
        }
    }

    async fn dispatch(&self, c: &mut Client, cmd: Cmd) -> Result<(), CtlError> {
        match cmd {
            Cmd::Status { bridge } => self.status_with(c, bridge).await,
            Cmd::Adapter(AdapterCmd::List) => {
                let s: Status = c.call_as(Command::Status).await?;
                self.print(&s.adapters, || render::adapters(&s))
            }
            Cmd::Adapter(AdapterCmd::Add { id, mac }) => {
                let def = json!({"id": id, "mac": mac});
                self.change(c, |nd| Command::AdapterCreate {
                    definition: def.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Adapter(AdapterCmd::Rm { adapter }) => {
                self.change(c, |nd| Command::AdapterDelete {
                    adapter: adapter.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Tnc(TncCmd::List) => {
                let s: Status = c.call_as(Command::Status).await?;
                self.print(&s.tncs, || render::tncs(&s))
            }
            Cmd::Tnc(TncCmd::Models) => {
                let m: Vec<ModelEntry> = c.call_as(Command::TncModels).await?;
                self.print(&m, || render::models(&m))
            }
            Cmd::Tnc(TncCmd::Add(a)) => {
                let def = tnc_definition(&a);
                self.change(c, |nd| Command::TncCreate {
                    definition: def.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Tnc(TncCmd::Rm { tnc }) => {
                self.change(c, |nd| Command::TncDelete {
                    tnc: tnc.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Serial(SerialCmd::List) => {
                let l: Vec<SerialEntry> = c.call_as(Command::SerialList).await?;
                self.print(&l, || render::serial(&l))
            }
            Cmd::Bridge(b) => self.bridge(c, b).await,
            Cmd::Enable { bridge, reset } => {
                self.change(c, |nd| Command::BridgeEnable {
                    bridge: bridge.clone(),
                    reset,
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Disable { bridge } => {
                self.change(c, |nd| Command::BridgeDisable {
                    bridge: bridge.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Switch { bridge } => {
                self.change(c, |nd| Command::BridgeSwitch {
                    bridge: bridge.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Port(PortCmd::Add(a)) => {
                let def = port_definition(&a);
                self.change(c, |nd| Command::PortAdd {
                    bridge: a.bridge.clone(),
                    definition: def.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Port(PortCmd::Rm { port }) => {
                let (bridge, port) = split_port(&port)?;
                self.change(c, |nd| Command::PortRemove {
                    bridge: bridge.clone(),
                    port: port.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            Cmd::Connect { port } => {
                let (bridge, port) = split_port(&port)?;
                self.act(c, Command::PortConnect { bridge, port }).await
            }
            Cmd::Disconnect { port } => {
                let (bridge, port) = split_port(&port)?;
                self.act(c, Command::PortDisconnect { bridge, port }).await
            }
            Cmd::Kick { session } => self.act(c, Command::SessionKick { session }).await,
            Cmd::Edit { what } => self.edit(c, &what).await,
            Cmd::Check => {
                let r: CheckResult = c.call_as(Command::ConfigCheck { dynamic: None }).await?;
                self.print(&r, || render::check(&r))?;
                if r.valid {
                    Ok(())
                } else {
                    Err(CtlError::Reported)
                }
            }
            Cmd::Reload => {
                self.change(c, |nd| Command::Reload { non_disruptive: nd })
                    .await
            }
            Cmd::Monitor { .. } => unreachable!("handled in run"),
        }
    }

    async fn bridge(&self, c: &mut Client, cmd: BridgeCmd) -> Result<(), CtlError> {
        match cmd {
            BridgeCmd::Add { id, name } => {
                let mut def = json!({"name": name});
                if let Some(id) = id {
                    def["id"] = json!(id);
                }
                self.change(c, |nd| Command::BridgeCreate {
                    definition: def.clone(),
                    non_disruptive: nd,
                })
                .await?;
                if !self.json {
                    println!(
                        "add its ports with: ktbctl port add BRIDGE TYPE ..., then: ktbctl enable BRIDGE"
                    );
                }
                Ok(())
            }
            BridgeCmd::Clone {
                bridge,
                new_id,
                name,
            } => {
                self.change(c, |nd| Command::BridgeClone {
                    bridge: bridge.clone(),
                    new_id: new_id.clone(),
                    name: name.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            BridgeCmd::Rm { bridge } => {
                self.change(c, |nd| Command::BridgeDelete {
                    bridge: bridge.clone(),
                    non_disruptive: nd,
                })
                .await
            }
            BridgeCmd::Name { bridge, name } => {
                self.edit_key(c, ObjectKind::Bridge, &bridge, "name", Some(name))
                    .await
            }
            BridgeCmd::PublicName {
                bridge,
                name,
                default,
                retry,
            } => {
                if retry {
                    return self.act(c, Command::BridgeRetryPublicName { bridge }).await;
                }
                if default {
                    return self
                        .edit_key(c, ObjectKind::Bridge, &bridge, "public_name", None)
                        .await;
                }
                if let Some(name) = name {
                    return self
                        .edit_key(c, ObjectKind::Bridge, &bridge, "public_name", Some(name))
                        .await;
                }
                let s: Status = c.call_as(Command::Status).await?;
                let b = s
                    .bridges
                    .iter()
                    .find(|b| b.id == bridge || b.name == bridge)
                    .ok_or_else(|| CtlError::Message(format!("there's no bridge {bridge:?}")))?;
                self.print(&b.public_name, || {
                    let p = &b.public_name;
                    if p.effective == p.configured {
                        format!("{}\n", p.effective)
                    } else {
                        format!(
                            "{} (configured {:?}, adjusted after a name collision; \
                             ktbctl bridge public-name {} --retry tries it again)\n",
                            p.effective, p.configured, b.id
                        )
                    }
                })
            }
        }
    }

    async fn status(&self, bridge: Option<String>) -> Result<(), CtlError> {
        let mut c = Client::connect(&self.socket).await?;
        self.status_with(&mut c, bridge).await
    }

    async fn status_with(&self, c: &mut Client, bridge: Option<String>) -> Result<(), CtlError> {
        let s: Status = c.call_as(Command::Status).await?;
        if self.json {
            return self.print(&s, String::new);
        }
        let text =
            render::status(&s, bridge.as_deref(), SystemTime::now()).map_err(CtlError::Message)?;
        print!("{text}");
        Ok(())
    }

    /// A command that changes config, under the consent rule.
    async fn change(&self, c: &mut Client, make: impl Fn(bool) -> Command) -> Result<(), CtlError> {
        let v = match c.call(make(!self.yes)).await {
            Err(CtlError::Api(e)) if e.code == ErrorCode::ReloadWouldDisrupt => {
                let would = e
                    .data
                    .as_ref()
                    .and_then(|d| serde_json::from_value::<PlanResult>(d.clone()).ok())
                    .map(|r| r.plan.describe_disruptions())
                    .unwrap_or_else(|| e.message.clone());
                if self.non_disruptive {
                    return Err(CtlError::Message(format!(
                        "refused: this would {would}; nothing was changed"
                    )));
                }
                if !interactive() {
                    return Err(CtlError::Message(format!(
                        "this would {would}; nothing was changed. Run it again with --yes to go ahead"
                    )));
                }
                if !confirm(&format!("This would {would}. Go ahead?"), false).await {
                    return Err(CtlError::Message("nothing was changed".into()));
                }
                c.call(make(false)).await?
            }
            other => other?,
        };
        if self.json {
            return self.print(&v, String::new);
        }
        let r: ChangeResult =
            serde_json::from_value(v).map_err(|e| CtlError::Protocol(e.to_string()))?;
        print!("{}", render::change(r.id.as_deref(), &r.plan));
        Ok(())
    }

    /// A command that acts at once, with no plan: connect, disconnect, kick.
    async fn act(&self, c: &mut Client, cmd: Command) -> Result<(), CtlError> {
        let v = c.call(cmd).await?;
        if self.json {
            self.print(&v, String::new)
        } else {
            Ok(())
        }
    }

    /// Set or remove one key of a dynamic object, through `config.get` and
    /// `config.put`, so the change is checked against what's there now.
    async fn edit_key(
        &self,
        c: &mut Client,
        kind: ObjectKind,
        object: &str,
        key: &str,
        value: Option<String>,
    ) -> Result<(), CtlError> {
        let text: ConfigText = c
            .call_as(Command::ConfigGet {
                kind: Some(kind),
                object: Some(object.to_string()),
            })
            .await?;
        let toml = set_key(&text.toml, key, value.as_deref())?;
        self.change(c, |nd| Command::ConfigPut {
            kind: Some(kind),
            object: Some(object.to_string()),
            toml: toml.clone(),
            revision: text.revision.clone(),
            non_disruptive: nd,
        })
        .await
    }

    /// `edit`: dynamic config in `$EDITOR`, checked by the daemon when it's
    /// saved, and reopened if it has errors.
    async fn edit(&self, c: &mut Client, what: &[String]) -> Result<(), CtlError> {
        let (kind, object) = match what {
            [] => (None, None),
            [k, id] if k == "tnc" => (Some(ObjectKind::Tnc), Some(id.clone())),
            [k, id] if k == "adapter" => (Some(ObjectKind::Adapter), Some(id.clone())),
            [k, id] if k == "bridge" => (Some(ObjectKind::Bridge), Some(id.clone())),
            [id] => (Some(ObjectKind::Bridge), Some(id.clone())),
            _ => {
                return Err(CtlError::Message(
                    "edit what? ktbctl edit [BRIDGE | tnc TNC | adapter ADAPTER]".into(),
                ));
            }
        };
        if !interactive() {
            return Err(CtlError::Message("edit needs a terminal".into()));
        }
        let text: ConfigText = c
            .call_as(Command::ConfigGet {
                kind,
                object: object.clone(),
            })
            .await?;
        let path = std::env::temp_dir().join(format!(
            "ktbctl-{}-{}.toml",
            object.as_deref().unwrap_or("dynamic"),
            std::process::id()
        ));
        std::fs::write(&path, &text.toml)?;
        loop {
            run_editor(&path)?;
            let edited = std::fs::read_to_string(&path)?;
            if edited == text.toml {
                println!("no changes");
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            if let Some(o) = &object
                && edited.trim().is_empty()
                && !self.yes
                && !confirm(
                    &format!("The file is empty, which deletes {o}. Delete it?"),
                    false,
                )
                .await
            {
                println!("nothing was changed; your edits are in {}", path.display());
                return Ok(());
            }
            let res = self
                .change(c, |nd| Command::ConfigPut {
                    kind,
                    object: object.clone(),
                    toml: edited.clone(),
                    revision: text.revision.clone(),
                    non_disruptive: nd,
                })
                .await;
            match res {
                Ok(()) => {
                    let _ = std::fs::remove_file(&path);
                    return Ok(());
                }
                Err(CtlError::Api(e))
                    if matches!(e.code, ErrorCode::ConfigInvalid | ErrorCode::BadRequest) =>
                {
                    self.report(&CtlError::Api(e));
                    if !confirm("Edit it again?", true).await {
                        println!("nothing was changed; your edits are in {}", path.display());
                        return Err(CtlError::Reported);
                    }
                }
                Err(e) => {
                    eprintln!("your edits are in {}", path.display());
                    return Err(e);
                }
            }
        }
    }

    /// `monitor`: frames as they go by, until Ctrl-C. When the daemon goes
    /// away it waits for it to come back and carries on.
    async fn monitor(&self, bridge: Option<String>) -> Result<(), CtlError> {
        let prefix = bridge.map(|b| format!("{b}/"));
        let mut waiting = false;
        loop {
            let session = async {
                let mut c = Client::connect(&self.socket).await?;
                c.subscribe(vec![Topic::Frames]).await?;
                if waiting {
                    eprintln!("-- connected again");
                }
                while let Some(ev) = c.next_event().await? {
                    if let crate::control::protocol::Event::Frame { object, .. } = &ev.event
                        && prefix
                            .as_ref()
                            .is_some_and(|p| !object.starts_with(p.as_str()))
                    {
                        continue;
                    }
                    if self.json {
                        println!("{}", serde_json::to_string(&ev).unwrap_or_default());
                    } else if let Some(line) = render::frame_line(&ev.event) {
                        println!("{line}");
                    }
                }
                Ok::<(), CtlError>(())
            };
            let res = tokio::select! {
                r = session => r,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            match res {
                // The daemon went away, or isn't there yet.
                Ok(()) | Err(CtlError::Closed | CtlError::Connect(_) | CtlError::Io(_)) => {
                    if !waiting {
                        eprintln!("-- the daemon isn't answering; waiting for it");
                        waiting = true;
                    }
                }
                Err(e) => return Err(e),
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }

    /// Print `value` as JSON with `--json`, else `human()`.
    fn print<T: serde::Serialize>(
        &self,
        value: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), CtlError> {
        if self.json {
            let s = serde_json::to_string_pretty(value)
                .map_err(|e| CtlError::Protocol(e.to_string()))?;
            println!("{s}");
        } else {
            print!("{}", human());
        }
        Ok(())
    }

    fn report(&self, e: &CtlError) {
        match e {
            CtlError::Reported => {}
            CtlError::Api(api) if self.json => {
                eprintln!("{}", serde_json::to_string(api).unwrap_or_default());
            }
            CtlError::Api(api) => {
                eprintln!("ktbctl: {}", api.message);
                if api.code == ErrorCode::ConfigInvalid
                    && let Some(ps) = api
                        .data
                        .as_ref()
                        .and_then(|d| serde_json::from_value::<Vec<ProblemJson>>(d.clone()).ok())
                {
                    for p in ps {
                        eprintln!("  {}", render::problem(&p));
                    }
                }
            }
            other => eprintln!("ktbctl: {other}"),
        }
    }
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Ask a yes/no question on the terminal.
async fn confirm(question: &str, default_yes: bool) -> bool {
    let prompt = format!("{question} [{}] ", if default_yes { "Y/n" } else { "y/N" });
    tokio::task::spawn_blocking(move || {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" => default_yes,
            a => a == "y" || a == "yes",
        }
    })
    .await
    .unwrap_or(false)
}

fn run_editor(path: &std::path::Path) -> Result<(), CtlError> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    // $EDITOR may carry arguments ("code --wait"), so let the shell split it.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("ktbctl")
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(CtlError::Message(format!(
            "{editor} exited with {status}; your edits are in {}",
            path.display()
        )))
    }
}

/// `BRIDGE/PORT` → (bridge, port).
fn split_port(s: &str) -> Result<(String, String), CtlError> {
    match s.split_once('/') {
        Some((b, p)) if !b.is_empty() && !p.is_empty() && !p.contains('/') => {
            Ok((b.to_string(), p.to_string()))
        }
        _ => Err(CtlError::Message(format!(
            "{s:?} isn't BRIDGE/PORT (e.g. ktbr0/radio)"
        ))),
    }
}

fn tnc_definition(a: &TncAdd) -> Value {
    let mut def = Map::new();
    def.insert("id".into(), json!(a.id));
    if let Some(n) = &a.name {
        def.insert("name".into(), json!(n));
    }
    if let Some(m) = &a.model {
        def.insert("model".into(), json!(m));
    }
    if let Some(addr) = &a.spp_address {
        let mut spp = json!({"address": addr});
        if let Some(ad) = &a.adapter {
            spp["adapter"] = json!(ad);
        }
        def.insert("spp".into(), spp);
    }
    if let Some(p) = &a.serial {
        let mut ser = json!({"path": p});
        if let Some(b) = a.baud {
            ser["baud"] = json!(b);
        }
        def.insert("serial".into(), ser);
    }
    Value::Object(def)
}

/// A port definition from `port add`'s flags. The daemon checks that they
/// suit the port type.
fn port_definition(a: &PortAdd) -> Value {
    let mut def = Map::new();
    def.insert("type".into(), json!(a.kind));
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            def.insert(k.into(), v);
        }
    };
    put("id", a.id.as_ref().map(|v| json!(v)));
    put("role", a.role.as_ref().map(|v| json!(v)));
    put("tnc", a.tnc.as_ref().map(|v| json!(v)));
    put("address", a.address.as_ref().map(|v| json!(v)));
    put("connect", a.connect.as_ref().map(|v| json!(v)));
    put("idle_timeout", a.idle_timeout.as_ref().map(|v| json!(v)));
    put(
        "exit_kiss_on_disconnect",
        a.exit_kiss_on_disconnect.then_some(json!(true)),
    );
    put("adapter", a.adapter.as_ref().map(|v| json!(v)));
    put("max_clients", a.max_clients.map(|v| json!(v)));
    put("listen", a.listen.as_ref().map(|v| json!(v)));
    put("allow", (!a.allow.is_empty()).then(|| json!(a.allow)));
    put("announce", a.no_announce.then_some(json!(false)));
    Value::Object(def)
}

/// Set (or with `None`, remove) a top-level key of the one object in
/// `toml`, keeping everything else as it was.
fn set_key(toml: &str, key: &str, value: Option<&str>) -> Result<String, CtlError> {
    let mut doc: toml_edit::DocumentMut = toml
        .parse()
        .map_err(|e| CtlError::Protocol(format!("the daemon's TOML: {e}")))?;
    let table = doc
        .iter_mut()
        .find_map(|(_, item)| item.as_array_of_tables_mut())
        .and_then(|a| a.iter_mut().next())
        .ok_or_else(|| CtlError::Protocol("the daemon's TOML has no object".into()))?;
    match value {
        Some(v) => {
            table.insert(key, toml_edit::value(v));
        }
        None => {
            table.remove(key);
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn port_add(args: &[&str]) -> Box<PortAdd> {
        let mut all = vec!["ktbctl", "port", "add"];
        all.extend_from_slice(args);
        match Cli::parse_from(all).command {
            Some(Cmd::Port(PortCmd::Add(a))) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn port_flags_become_a_definition() {
        let d = port_definition(&port_add(&[
            "ktbr0",
            "tcp-server",
            "--listen",
            "wlan0:8001",
            "--allow",
            "192.168.1.0/24",
            "--allow",
            "10.0.0.5",
            "--no-announce",
            "--max-clients",
            "4",
        ]));
        assert_eq!(
            d,
            json!({"type": "tcp-server", "listen": "wlan0:8001",
                   "allow": ["192.168.1.0/24", "10.0.0.5"], "announce": false, "max_clients": 4})
        );
        let d = port_definition(&port_add(&[
            "ktbr0",
            "spp-client",
            "--tnc",
            "d75",
            "--idle-timeout",
            "90s",
            "--exit-kiss-on-disconnect",
        ]));
        assert_eq!(
            d,
            json!({"type": "spp-client", "tnc": "d75", "idle_timeout": "90s",
                   "exit_kiss_on_disconnect": true})
        );
    }

    #[test]
    fn tnc_flags_become_a_definition() {
        let a = match Cli::parse_from([
            "ktbctl",
            "tnc",
            "add",
            "d75",
            "--model",
            "th-d75",
            "--spp-address",
            "40:79:12:E4:95:9D",
            "--adapter",
            "builtin",
            "--serial",
            "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00",
        ])
        .command
        {
            Some(Cmd::Tnc(TncCmd::Add(a))) => a,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            tnc_definition(&a),
            json!({"id": "d75", "model": "th-d75",
                   "spp": {"address": "40:79:12:E4:95:9D", "adapter": "builtin"},
                   "serial": {"path": "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00"}})
        );
    }

    #[test]
    fn keys_are_set_and_removed_in_place() {
        let toml = "# mine\n[[bridge]]\nid = \"ktbr2\"\nname = \"Old\"  # keep\npublic_name = \"P\"\n  [[bridge.port]]\n  type = \"ble-server\"\n";
        let t = set_key(toml, "name", Some("New")).unwrap();
        assert!(t.contains("name = \"New\"") && t.contains("# mine"), "{t}");
        assert!(t.contains("type = \"ble-server\""), "{t}");
        let t = set_key(toml, "public_name", None).unwrap();
        assert!(!t.contains("public_name"), "{t}");
    }

    #[test]
    fn ports_are_bridge_slash_port() {
        assert_eq!(
            split_port("ktbr0/radio").unwrap(),
            ("ktbr0".into(), "radio".into())
        );
        assert!(split_port("ktbr0").is_err());
        assert!(split_port("ktbr0/lan/1.2.3.4:5").is_err());
    }
}
