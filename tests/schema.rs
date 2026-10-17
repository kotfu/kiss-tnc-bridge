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

//! The published JSON Schema for the control protocol,
//! `docs/control-api.schema.json`, generated from the protocol's types so
//! it can't drift from them. Apps in other languages (Swift, Kotlin)
//! generate their types from it.
//!
//! This test fails when the file is out of date. Regenerate it with:
//!
//! ```text
//! UPDATE_SCHEMA=1 cargo test --features schema --test schema
//! ```

#![cfg(feature = "schema")]

use kiss_tnc_bridge::control::protocol::{
    ChangeResult, CheckResult, ConfigText, EventMessage, PlanResult, Request, Response,
    SerialEntry, Status,
};
use serde_json::{Value, json};

fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap()
}

fn generate() -> String {
    let doc = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "kiss-tnc-bridge control protocol, version 1",
        "description": "Newline-delimited JSON over the daemon's control socket. Clients send Request; the daemon answers each with a Response, whose result depends on the command (see results), and streams EventMessage on subscribed topics. See docs/control-api.md.",
        "messages": {
            "Request": schema::<Request>(),
            "Response": schema::<Response>(),
            "EventMessage": schema::<EventMessage>(),
        },
        "results": {
            "status": schema::<Status>(),
            "reload": schema::<PlanResult>(),
            "config.check": schema::<CheckResult>(),
            "config.get": schema::<ConfigText>(),
            "serial.list": schema::<Vec<SerialEntry>>(),
            "changes": schema::<ChangeResult>(),
        },
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    text.push('\n');
    text
}

#[test]
fn the_published_schema_is_current() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/control-api.schema.json");
    let want = generate();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date; regenerate it with UPDATE_SCHEMA=1 cargo test --features schema --test schema",
        path.display()
    );
}
