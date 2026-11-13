//! The spike without espflash, for measuring what espflash adds: the same
//! other dependencies (clap, serde_json, serialport, sha2, env_logger) in
//! use.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use clap::Parser;
use sha2::{Digest, Sha256};

#[derive(Parser)]
struct Cli {
    port: String,
    file: Option<String>,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    if let Some(f) = cli.file {
        println!("{}", hex::encode(Sha256::digest(std::fs::read(f)?)));
    }
    for p in serialport::available_ports()? {
        println!("{} {:?}", p.port_name, p.port_type);
    }
    let mut s = serialport::new(&cli.port, 115_200).timeout(Duration::from_secs(1)).open()?;
    s.write_request_to_send(false)?;
    s.write_data_terminal_ready(false)?;
    s.write_all(b"{\"v\":1,\"id\":1,\"cmd\":\"device.info\"}\n")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let v: serde_json::Value = serde_json::from_str(&line)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}
