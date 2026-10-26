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

//! `ktbctl`: manage kiss-tnc-bridge through its control socket.

use std::process::ExitCode;

use clap::Parser;
use kiss_tnc_bridge::ctl::cli::Cli;
use kiss_tnc_bridge::ctl::run::{Ctl, Outcome};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    let ctl = Ctl::new(&cli);
    match rt.block_on(ctl.run(cli.command)) {
        Outcome::Ok => ExitCode::SUCCESS,
        Outcome::Failed => ExitCode::FAILURE,
    }
}
