//! Phase D spike: `espflash` as a library, for `ktbctl device flash`.
//! See README.md. Throwaway code.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::Segment;
use espflash::target::ProgressCallbacks;
use serialport::{SerialPort, SerialPortType, UsbPortInfo};
use sha2::{Digest, Sha256};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List serial ports with their USB ids
    Ports,
    /// Open the port without resetting the device, and ask for device.info
    Probe {
        port: String,
        /// Seconds to wait for an answer
        #[arg(long, default_value_t = 3)]
        wait: u64,
    },
    /// Open and close the port COUNT times, then probe: does opening reset it?
    OpenClose {
        port: String,
        #[arg(long, default_value_t = 5)]
        count: u32,
    },
    /// Connect to the ROM, print the chip, then reset back into the firmware
    Chip {
        port: String,
        #[arg(long, default_value_t = 115_200)]
        baud: u32,
    },
    /// Write a release manifest's parts
    Flash {
        port: String,
        manifest: PathBuf,
        #[arg(long, default_value_t = 921_600)]
        baud: u32,
        /// Erase the whole flash first
        #[arg(long)]
        clean: bool,
        /// Skip the MD5 check after each part
        #[arg(long)]
        no_verify: bool,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).format_timestamp_millis().init();
    match Cli::parse().cmd {
        Cmd::Ports => ports(),
        Cmd::Probe { port, wait } => {
            match probe(&port, Duration::from_secs(wait))? {
                Some(info) => println!("{}", serde_json::to_string_pretty(&info)?),
                None => println!("no answer: not our firmware, or none"),
            }
            Ok(())
        }
        Cmd::OpenClose { port, count } => open_close(&port, count),
        Cmd::Chip { port, baud } => chip(&port, baud),
        Cmd::Flash { port, manifest, baud, clean, no_verify } => {
            flash(&port, &manifest, baud, clean, !no_verify)
        }
    }
}

fn ports() -> Result<()> {
    for p in serialport::available_ports()? {
        match p.port_type {
            SerialPortType::UsbPort(u) => println!(
                "{}  usb {:04x}:{:04x}  serial={:?} manufacturer={:?} product={:?}",
                p.port_name,
                u.vid,
                u.pid,
                u.serial_number,
                u.manufacturer,
                u.product
            ),
            other => println!("{}  {:?}", p.port_name, other),
        }
    }
    Ok(())
}

/// The port's USB ids. serialport lists ports by their device names
/// (/dev/ttyACM0 on Linux), so a /dev/serial/by-id symlink has to be
/// resolved first.
fn usb_info(port: &str) -> Result<UsbPortInfo> {
    let real = std::fs::canonicalize(port).with_context(|| format!("no port {port}"))?;
    for p in serialport::available_ports()? {
        let listed = std::fs::canonicalize(&p.port_name).unwrap_or_else(|_| p.port_name.clone().into());
        if listed == real {
            if let SerialPortType::UsbPort(u) = p.port_type {
                return Ok(u);
            }
        }
    }
    bail!("{port} isn't a USB serial port")
}

/// Opens the port without resetting the device: the auto-reset circuit
/// pulls EN low while RTS is asserted without DTR. The OS asserts both on
/// open, which does nothing; drop RTS first (DTR alone only holds IO0,
/// which matters only at reset), then DTR.
fn open_quiet(port: &str) -> Result<Box<dyn SerialPort>> {
    let mut s = serialport::new(port, 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("opening {port}"))?;
    s.write_request_to_send(false)?;
    s.write_data_terminal_ready(false)?;
    Ok(s)
}

fn probe(port: &str, wait: Duration) -> Result<Option<serde_json::Value>> {
    let s = open_quiet(port)?;
    let mut w = s.try_clone()?;
    let mut r = BufReader::new(s);
    let start = Instant::now();
    let mut next_send = start;
    let mut line = Vec::new();
    while start.elapsed() < wait {
        if Instant::now() >= next_send {
            w.write_all(b"{\"v\":1,\"id\":1,\"cmd\":\"device.info\"}\n")?;
            next_send += Duration::from_secs(1);
        }
        let mut buf = Vec::new();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) => {}
            Ok(_) => {
                line.extend_from_slice(&buf);
                if line.ends_with(b"\n") {
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    line.clear();
                    if text.starts_with('{') {
                        let v: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if v.get("id") == Some(&serde_json::json!(1)) {
                            return Ok(v.get("result").cloned());
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                line.extend_from_slice(&buf);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

fn open_close(port: &str, count: u32) -> Result<()> {
    let before = probe(port, Duration::from_secs(3))?.context("no device.info before")?;
    for _ in 0..count {
        drop(open_quiet(port)?);
        std::thread::sleep(Duration::from_millis(300));
    }
    let after = probe(port, Duration::from_secs(3))?.context("no device.info after")?;
    let up = |v: &serde_json::Value| v["uptime"].as_u64().unwrap_or(0);
    println!(
        "uptime before {} s, after {} s, last_reset {} -> {}",
        up(&before),
        up(&after),
        before["last_reset"],
        after["last_reset"]
    );
    if up(&after) < up(&before) {
        println!("RESET: opening the port rebooted the device");
    } else {
        println!("no reset");
    }
    Ok(())
}

fn connect(port: &str, baud: u32, verify: bool) -> Result<Flasher> {
    let info = usb_info(port)?;
    let serial = serialport::new(port, 115_200)
        .timeout(Duration::from_secs(3))
        .open_native()
        .with_context(|| format!("opening {port}"))?;
    let conn = Connection::new(
        serial,
        info,
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        115_200,
    );
    let baud = (baud > 115_200).then_some(baud);
    Ok(Flasher::connect(conn, true, verify, false, None, baud)?)
}

/// esptool's names for the ESP32's package, from eFuse block 0 word 3.
fn esp32_package(flasher: &mut Flasher, rev_major: u32) -> Result<String> {
    let word3 = flasher.connection().read_reg(0x3FF5_A00C)?;
    let pkg = ((word3 >> 9) & 0x7) | (((word3 >> 2) & 0x1) << 3);
    let v3 = rev_major >= 3;
    Ok(match pkg {
        0 if v3 => "ESP32-D0WDQ6-V3".into(),
        0 => "ESP32-D0WDQ6".into(),
        1 if v3 => "ESP32-D0WD-V3".into(),
        1 => "ESP32-D0WD".into(),
        2 => "ESP32-D2WD".into(),
        4 => "ESP32-U4WDH".into(),
        5 if v3 => "ESP32-PICO-V3".into(),
        5 => "ESP32-PICO-D4".into(),
        6 => "ESP32-PICO-V3-02".into(),
        7 => "ESP32-D0WDR2-V3".into(),
        n => format!("unknown package {n}"),
    })
}

fn chip(port: &str, baud: u32) -> Result<()> {
    let t = Instant::now();
    let mut f = connect(port, baud, true)?;
    let connected = t.elapsed();
    let info = f.device_info()?;
    let rev = info.revision.unwrap_or((0, 0));
    let package = esp32_package(&mut f, rev.0)?;
    println!("connected in {:.2} s", connected.as_secs_f64());
    println!("chip      {:?} rev {}.{}", info.chip, rev.0, rev.1);
    println!("package   {package}");
    println!("flash     {}", info.flash_size);
    println!("crystal   {}", info.crystal_frequency);
    println!("mac       {}", info.mac_address.unwrap_or_default());
    println!("features  {}", info.features.join(", "));
    let chip = f.chip();
    f.connection().reset_after(true, chip)?;
    println!("reset; {:.2} s in all", t.elapsed().as_secs_f64());
    Ok(())
}

#[derive(serde::Deserialize)]
struct Manifest {
    version: String,
    board: String,
    builds: Vec<Build>,
}

#[derive(serde::Deserialize)]
struct Build {
    #[serde(rename = "chipFamily")]
    chip_family: String,
    parts: Vec<Part>,
}

#[derive(serde::Deserialize)]
struct Part {
    path: String,
    offset: u32,
    sha256: String,
    size: usize,
}

struct Progress {
    name: String,
    total: usize,
    last: usize,
    started: Instant,
}

impl ProgressCallbacks for Progress {
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total;
        self.last = 0;
        self.started = Instant::now();
        eprint!("  {} at 0x{addr:x}: ", self.name);
    }
    fn update(&mut self, current: usize) {
        let pct = current * 100 / self.total.max(1);
        if pct >= self.last + 10 {
            self.last = pct - pct % 10;
            eprint!("{}% ", self.last);
        }
    }
    fn verifying(&mut self) {
        eprint!("verifying ");
    }
    fn finish(&mut self, skipped: bool) {
        eprintln!(
            "{} ({:.2} s)",
            if skipped { "skipped" } else { "done" },
            self.started.elapsed().as_secs_f64()
        );
    }
}

fn flash(port: &str, manifest_path: &Path, baud: u32, clean: bool, verify: bool) -> Result<()> {
    let dir = manifest_path.parent().unwrap_or(Path::new("."));
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    let build = m.builds.first().ok_or_else(|| anyhow!("no builds"))?;
    if build.chip_family != "ESP32" {
        bail!("chipFamily {}", build.chip_family);
    }
    let mut parts = Vec::new();
    for p in &build.parts {
        let data = std::fs::read(dir.join(&p.path)).with_context(|| p.path.clone())?;
        if data.len() != p.size {
            bail!("{}: {} bytes, the manifest says {}", p.path, data.len(), p.size);
        }
        if hex::encode(Sha256::digest(&data)) != p.sha256 {
            bail!("{}: SHA-256 doesn't match the manifest", p.path);
        }
        parts.push((p, data));
    }
    println!("{} {}: {} parts checked", m.board, m.version, parts.len());

    match probe(port, Duration::from_secs(3))? {
        Some(info) => println!(
            "running: {} {}",
            info["board"].as_str().unwrap_or("?"),
            info["firmware"]["version"].as_str().unwrap_or("?")
        ),
        None => println!("running: not our firmware"),
    }

    let t = Instant::now();
    let mut f = connect(port, baud, verify)?;
    println!("ROM and stub at {baud} baud in {:.2} s", t.elapsed().as_secs_f64());
    if clean {
        let e = Instant::now();
        f.erase_flash()?;
        println!("erased in {:.2} s", e.elapsed().as_secs_f64());
    }
    // One call for every part: espflash's CLI does the same, and a second
    // call after the first hung at the next part (seen on the ATOM Lite).
    let w = Instant::now();
    let segments: Vec<Segment<'_>> = parts.iter().map(|(p, d)| Segment::new(p.offset, d)).collect();
    let mut progress = Progress {
        name: "part".into(),
        total: 0,
        last: 0,
        started: Instant::now(),
    };
    f.write_bins_to_flash(&segments, &mut progress)?;
    println!("written in {:.2} s", w.elapsed().as_secs_f64());
    let chip = f.chip();
    f.connection().reset_after(true, chip)?;
    drop(f);
    println!("reset; {:.2} s since connecting", t.elapsed().as_secs_f64());

    let b = Instant::now();
    match probe(port, Duration::from_secs(15))? {
        Some(info) => println!(
            "firmware answered after {:.2} s: {} {}",
            b.elapsed().as_secs_f64(),
            info["board"].as_str().unwrap_or("?"),
            info["firmware"]["version"].as_str().unwrap_or("?")
        ),
        None => println!("no answer from the firmware within 15 s"),
    }
    Ok(())
}
