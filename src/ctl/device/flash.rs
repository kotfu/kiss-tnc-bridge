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

//! `ktbctl device flash`: installs firmware over USB, as the web installer
//! does (docs/web-installer.md, steps 3 to 7; docs/plan-phase-d.md, D1).
//!
//! 1. Ask the firmware for `device.info`: its board, version and
//!    fingerprint, or nothing (other firmware, or none).
//! 2. The firmware: a local manifest, or a GitHub release for the board,
//!    every part checked against its manifest.
//! 3. An update, which keeps NVS, or a clean install, which erases the
//!    whole flash first and is the only way over other firmware. A clean
//!    install asks first.
//! 4. The ROM: check the chip is the firmware's board, erase, write,
//!    reset. At 1500000 baud, then at 115200 if that fails. A failure
//!    resets the device, so it isn't left in its download mode.
//! 5. Wait for the new firmware to answer, and check its version. After a
//!    clean install, show the device's new authorization code.

use std::io::{IsTerminal, Write};
use std::time::Instant;

use espflash::target::ProgressCallbacks;
use serde::Serialize;
use serde_json::json;

use super::manifest::{Firmware, Part, compare_versions};
use super::ports::DevicePort;
use super::probe::{self, ANSWER_WAIT};
use super::rom::{self, ChipInfo, FAST_BAUD, Rom, SLOW_BAUD};
use super::{BOARDS, board_name};
use crate::ctl::cli::FlashArgs;
use crate::ctl::client::CtlError;
use crate::ctl::render;
use crate::ctl::run::{Ctl, confirm, interactive};

/// How long the new firmware has to answer after the reset, as the web
/// installer waits.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// What `device flash` did.
#[derive(Debug, Serialize)]
pub struct Flashed {
    pub port: String,
    pub board: String,
    pub version: String,
    /// The version it ran before, if it was this firmware.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    pub clean: bool,
    pub chip: ChipInfo,
    pub baud: u32,
    pub seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// After a clean install, the device's new authorization code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<String>,
}

impl Ctl {
    pub(in crate::ctl) async fn flash(&self, a: FlashArgs) -> Result<(), CtlError> {
        let say = Reporter::new(self.json);
        if self.non_disruptive {
            return Err(msg(
                "installing firmware restarts the device, which drops its radio link and \
                 phones; --non-disruptive refuses it",
            ));
        }
        if let Some(b) = &a.board
            && !BOARDS.contains(&b.as_str())
        {
            return Err(msg(format!(
                "no board {b:?}; the boards are {}",
                BOARDS.join(", ")
            )));
        }
        let port = self.device_port()?;
        let start = Instant::now();

        // 1. What it runs now.
        let running = {
            let mut serial = probe::open_quiet(&port.path)?;
            probe::device_info(&mut serial, ANSWER_WAIT).await?
        };
        match &running {
            Some(i) => say.step(
                "found",
                &format!(
                    "{} on {}, running firmware {}",
                    board_name(&i.board),
                    port.path,
                    i.firmware.version
                ),
            ),
            None => say.step(
                "found",
                &format!(
                    "{} isn't running kiss-tnc-bridge firmware (or hasn't started)",
                    port.path
                ),
            ),
        }
        if let (Some(i), Some(b)) = (&running, &a.board)
            && i.board != *b
        {
            return Err(msg(format!(
                "the device says it's {}, not {}",
                board_name(&i.board),
                board_name(b)
            )));
        }

        // 2. The firmware.
        let known_board = running
            .as_ref()
            .map(|i| i.board.clone())
            .or(a.board.clone());
        let fw = match &a.manifest {
            Some(m) => Firmware::load(m).map_err(CtlError::Message)?,
            None => {
                let board = match known_board.clone() {
                    Some(b) => b,
                    None => self.board_from_chip(&port, &say).await?,
                };
                let (version, prerelease) = (a.version.clone(), a.prerelease);
                let say2 = say.clone();
                blocking(move || {
                    super::release::fetch(&board, version.as_deref(), prerelease, &say2)
                })
                .await?
            }
        };
        say.step(
            "firmware",
            &format!(
                "firmware {} for the {}, from {}",
                fw.version,
                board_name(&fw.board),
                fw.source.display()
            ),
        );
        if let Some(b) = known_board.as_ref()
            && *b != fw.board
        {
            return Err(msg(format!(
                "the firmware is for the {}, and this is a{} {}",
                board_name(&fw.board),
                if b.starts_with(['a', 'e', 'i', 'o', 'u']) {
                    "n"
                } else {
                    ""
                },
                board_name(b)
            )));
        }

        // 3. Update or clean install.
        let clean = a.clean || running.is_none();
        if let Some(i) = &running
            && compare_versions(&fw.version, &i.firmware.version) == Some(std::cmp::Ordering::Less)
        {
            say.warn(&format!(
                "firmware {} is older than the {} it runs. Settings a newer version \
                 wrote may not all be understood; a clean install (--clean) starts afresh",
                fw.version, i.firmware.version
            ));
        }
        if clean && !self.yes {
            let what = if running.is_none() {
                "It isn't running kiss-tnc-bridge firmware, so this is a clean install: \
                 everything on the device is erased first."
            } else {
                "A clean install erases everything on the device first: its settings, radio \
                 pairing and authorized apps, and its device key and authorization code. \
                 Management apps will need its new code."
            };
            if !interactive() {
                return Err(msg(format!("{what} Add --yes to go ahead.")));
            }
            eprintln!("{what}");
            if !confirm("Go ahead?", false).await {
                return Err(msg("nothing was written"));
            }
        }
        say.step(
            "installing",
            &if clean {
                format!("Clean install of firmware {}", fw.version)
            } else {
                format!(
                    "Updating to firmware {}: its settings, radio pairing and \
                     authorization code are kept",
                    fw.version
                )
            },
        );

        // 4. The ROM.
        let job = Job {
            port: port.clone(),
            parts: fw.parts.clone(),
            board: fw.board.clone(),
            board_known: known_board.is_some(),
            clean,
            baud: a.baud.unwrap_or(FAST_BAUD),
        };
        let say2 = say.clone();
        let (chip, baud) = blocking(move || install(job, &say2)).await?;

        // 5. The new firmware.
        say.step("waiting", "Waiting for the firmware to start...");
        let info = {
            let mut serial = probe::open_quiet(&port.path)?;
            probe::device_info(&mut serial, START_WAIT).await?
        };
        let Some(info) = info else {
            return Err(msg(format!(
                "firmware {} was written, but it didn't answer within {} s. Unplug the \
                 device and plug it in, then 'ktbctl device info'; if it still doesn't \
                 answer, install it again with --clean",
                fw.version,
                START_WAIT.as_secs()
            )));
        };
        if info.firmware.version != fw.version {
            return Err(msg(format!(
                "the device runs firmware {} after writing {}: install again",
                info.firmware.version, fw.version
            )));
        }
        let authorization_code = if clean {
            match authorization_code(&port).await {
                Ok(code) => Some(code),
                Err(e) => {
                    say.warn(&format!(
                        "couldn't get its new authorization code ({e}); 'ktbctl device \
                         info' shows whether it's running, and the code comes with the \
                         auth commands"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let done = Flashed {
            port: port.path.clone(),
            board: info.board.clone(),
            version: info.firmware.version.clone(),
            previous: running.map(|i| i.firmware.version),
            clean,
            chip,
            baud,
            seconds: start.elapsed().as_secs(),
            fingerprint: info.fingerprint.clone(),
            authorization_code,
        };
        self.print(&done, || render::flashed(&done))
    }

    /// The board, from the chip, for a device that can't say: about 8 s in
    /// its bootloader.
    async fn board_from_chip(&self, port: &DevicePort, say: &Reporter) -> Result<String, CtlError> {
        say.step(
            "identifying",
            "Asking its bootloader which chip it has (about 8 s)...",
        );
        let p = port.clone();
        let chip = blocking(move || {
            let rom = match Rom::connect(&p, SLOW_BAUD) {
                Ok(rom) => rom,
                Err(e) => {
                    let _ = rom::hard_reset(&p.path);
                    return Err(e);
                }
            };
            let chip = rom.chip.clone();
            if rom.reset().is_err() {
                let _ = rom::hard_reset(&p.path);
            }
            Ok(chip)
        })
        .await?;
        match chip.boards() {
            [] => Err(msg(format!(
                "its chip is {}, which isn't on a board ktbctl knows",
                chip.describe()
            ))),
            [one] => Ok((*one).to_string()),
            many => Err(ambiguous(&chip, many)),
        }
    }
}

/// What to write, and where.
struct Job {
    port: DevicePort,
    parts: Vec<Part>,
    board: String,
    /// The firmware or `--board` said which board this is.
    board_known: bool,
    clean: bool,
    baud: u32,
}

/// Writes the firmware: at the job's baud rate, then at 115200 if that
/// fails, resetting the device after any failure. Returns the chip and the
/// rate that worked. Blocking.
fn install(job: Job, say: &Reporter) -> Result<(ChipInfo, u32), String> {
    let mut rates = vec![job.baud];
    if job.baud > SLOW_BAUD {
        rates.push(SLOW_BAUD);
    }
    let mut last = String::new();
    for (i, &baud) in rates.iter().enumerate() {
        if i > 0 {
            say.warn(&format!("{last}; trying again at {baud} baud"));
        }
        match install_at(&job, baud, say) {
            Ok(chip) => return Ok((chip, baud)),
            Err(Failure::Final(e)) => return Err(e),
            Err(Failure::Retry(e)) => {
                let _ = rom::hard_reset(&job.port.path);
                last = e;
            }
        }
    }
    Err(format!(
        "{last}. The device was restarted; nothing is lost that another install \
         can't fix"
    ))
}

enum Failure {
    /// Not worth trying again: the wrong board.
    Final(String),
    /// Possibly the baud rate.
    Retry(String),
}

fn install_at(job: &Job, baud: u32, say: &Reporter) -> Result<ChipInfo, Failure> {
    say.step("connecting", "Connecting to its bootloader (about 8 s)...");
    let mut rom = Rom::connect(&job.port, baud).map_err(Failure::Retry)?;
    let boards = rom.chip.boards();
    let refuse = |rom: Rom, e: String| {
        if rom.reset().is_err() {
            let _ = rom::hard_reset(&job.port.path);
        }
        Failure::Final(e)
    };
    if !boards.contains(&job.board.as_str()) {
        let e = format!(
            "its chip is {}, which isn't the {}'s; nothing was written",
            rom.chip.describe(),
            board_name(&job.board)
        );
        return Err(refuse(rom, e));
    }
    if boards.len() > 1 && !job.board_known {
        let e = ambiguous(&rom.chip, boards).to_string();
        return Err(refuse(rom, e));
    }
    if job.clean {
        say.step("erasing", "Erasing...");
        rom.erase().map_err(Failure::Retry)?;
    }
    let mut progress = Progress::new(say.clone(), &job.parts);
    rom.write(&job.parts, &mut progress)
        .map_err(Failure::Retry)?;
    progress.done();
    let chip = rom.chip.clone();
    say.step("restarting", "Restarting it...");
    rom.reset().map_err(Failure::Retry)?;
    Ok(chip)
}

fn ambiguous(chip: &ChipInfo, boards: &[&str]) -> CtlError {
    let names: Vec<String> = boards.iter().map(|b| board_name(b).to_string()).collect();
    let flags: Vec<String> = boards.iter().map(|b| format!("--board {b}")).collect();
    msg(format!(
        "its chip, {}, is on the {}, and the device isn't running kiss-tnc-bridge \
         firmware to say which it is. Say with {}",
        chip.describe(),
        render::join_and(&names),
        flags.join(" or ")
    ))
}

/// After a clean install, the device's new authorization code, asked
/// again each second while it settles after making its new key.
async fn authorization_code(port: &DevicePort) -> Result<String, String> {
    let mut serial = probe::open_quiet(&port.path).map_err(|e| e.to_string())?;
    let v = probe::request(&mut serial, "auth.code", START_WAIT, true)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("the device didn't answer")?;
    v.get("code")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("its answer had no code: {v}"))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, CtlError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| msg(format!("internal: {e}")))?
        .map_err(CtlError::Message)
}

fn msg(s: impl Into<String>) -> CtlError {
    CtlError::Message(s.into())
}

/// Progress on stderr: steps as lines, and the write as a percentage, on
/// one line in a terminal. With `--json`, each as a JSON line, so stdout
/// has only the result.
#[derive(Clone)]
pub struct Reporter {
    json: bool,
    tty: bool,
}

impl Reporter {
    pub fn new(json: bool) -> Reporter {
        Reporter {
            json,
            tty: std::io::stderr().is_terminal(),
        }
    }

    pub fn step(&self, step: &str, text: &str) {
        if self.json {
            eprintln!("{}", json!({"step": step}));
        } else {
            eprintln!("{text}");
        }
    }

    pub fn warn(&self, text: &str) {
        if self.json {
            eprintln!("{}", json!({"warning": text}));
        } else {
            eprintln!("ktbctl: {text}");
        }
    }

    fn percent(&self, step: &str, pct: u64, done: usize, total: usize) {
        if self.json {
            eprintln!("{}", json!({"step": step, "percent": pct}));
        } else if self.tty {
            eprint!(
                "\r{step} {pct:>3}%  ({} of {} KB)",
                done / 1024,
                total / 1024
            );
            let _ = std::io::stderr().flush();
        } else {
            eprintln!("{step} {pct}%");
        }
    }
}

/// espflash reports each part's progress in compressed chunks; this turns
/// it into a percentage of the whole write, by size.
struct Progress {
    say: Reporter,
    /// Each part's offset, the bytes before it, and its size.
    parts: Vec<(u32, usize, usize)>,
    total: usize,
    current: (usize, usize),
    chunks: usize,
    shown: Option<u64>,
}

impl Progress {
    fn new(say: Reporter, parts: &[Part]) -> Progress {
        let mut before = 0;
        let parts = parts
            .iter()
            .map(|p| {
                let entry = (p.offset, before, p.data.len());
                before += p.data.len();
                entry
            })
            .collect();
        Progress {
            say,
            parts,
            total: before,
            current: (0, 0),
            chunks: 1,
            shown: None,
        }
    }

    fn done(&mut self) {
        if self.shown != Some(100) {
            self.say.percent("writing", 100, self.total, self.total);
        }
        if self.say.tty && !self.say.json {
            eprintln!();
        }
    }
}

impl ProgressCallbacks for Progress {
    fn init(&mut self, addr: u32, total: usize) {
        if let Some(&(_, before, size)) = self.parts.iter().find(|p| p.0 == addr) {
            self.current = (before, size);
        }
        self.chunks = total.max(1);
    }

    fn update(&mut self, current: usize) {
        let (before, size) = self.current;
        let done = before + size * current.min(self.chunks) / self.chunks;
        let pct = (done * 100 / self.total.max(1)) as u64;
        // A line each 10% when it can't be redrawn in place.
        let step = if self.say.tty && !self.say.json {
            1
        } else {
            10
        };
        if self.shown.is_none_or(|s| pct >= s + step) {
            self.shown = Some(pct - pct % step);
            self.say.percent("writing", pct, done, self.total);
        }
    }

    fn verifying(&mut self) {}

    fn finish(&mut self, _skipped: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_a_share_of_the_whole_write() {
        let part = |offset, len| Part {
            name: String::new(),
            offset,
            data: vec![0; len],
        };
        let mut p = Progress::new(
            Reporter {
                json: true,
                tty: false,
            },
            &[part(0x1000, 100), part(0x10000, 900)],
        );
        p.init(0x1000, 2);
        p.update(2);
        assert_eq!(p.shown, Some(10));
        p.init(0x10000, 9);
        p.update(4);
        assert_eq!(p.shown, Some(50));
        p.update(9);
        assert_eq!(p.shown, Some(100));
    }

    #[test]
    fn board_takes_every_board() {
        use clap::CommandFactory;
        let cmd = crate::ctl::cli::Cli::command();
        let flash = cmd
            .find_subcommand("device")
            .and_then(|d| d.find_subcommand("flash"))
            .unwrap();
        let board = flash
            .get_arguments()
            .find(|a| a.get_id() == "board")
            .unwrap();
        let values: Vec<String> = board
            .get_possible_values()
            .iter()
            .map(|v| v.get_name().to_string())
            .collect();
        assert_eq!(values, BOARDS);
    }
}
