//! Spike: a Linux client for the ATOM Lite update spike
//! (spikes/atom-lite-update). It sends a firmware image over BLE and
//! reports throughput, the way `ktbctl device update` would.
//!
//! Commands are length-prefixed JSON on the management service's write
//! characteristic; replies and events come back as notifications. Image
//! bytes go to the data characteristic as writes without response, each one
//! a 4-byte little-endian offset and then the bytes. The device acknowledges
//! every few KB once they're in flash, and the client keeps at most a window
//! of unacknowledged bytes in flight. A "nak" names the offset to resend
//! from.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, AdapterEvent, Address, Device, DiscoveryFilter, DiscoveryTransport};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::{Uuid, uuid};

const MGMT_SERVICE: Uuid = uuid!("4b544201-9b3e-4a51-8d36-5c3a1f7e0001");
const MGMT_TX: Uuid = uuid!("4b544201-9b3e-4a51-8d36-5c3a1f7e0002");
const MGMT_RX: Uuid = uuid!("4b544201-9b3e-4a51-8d36-5c3a1f7e0003");
const MGMT_DATA: Uuid = uuid!("4b544201-9b3e-4a51-8d36-5c3a1f7e0004");

#[derive(Parser)]
#[command(about = "Firmware update over BLE for the ATOM Lite update spike")]
struct Args {
    /// The ATOM's BLE address.
    device: Address,
    /// Adapter.
    #[arg(short, default_value = "hci0")]
    adapter: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Running and other partition, versions and states.
    Info,
    /// Restart the device.
    Reboot,
    /// Abort an update in progress.
    Abort,
    /// Send an image and switch to it.
    Send {
        image: PathBuf,
        /// Erase the whole image area first ("all") or as writes reach it ("seq").
        #[arg(long, default_value = "all")]
        erase: String,
        /// Bytes in flight; defaults to what the device offers. Larger tests overflow.
        #[arg(long)]
        window: Option<usize>,
        /// Largest image bytes per write; defaults to what the device and MTU allow.
        #[arg(long)]
        chunk: Option<usize>,
        /// Ask for a connection interval, in 1.25 ms units: MIN,MAX.
        #[arg(long, value_delimiter = ',')]
        interval: Option<Vec<u16>>,
        /// Close the radio link during the update.
        #[arg(long)]
        stop_radio: bool,
        /// Reboot into the new image afterwards and show info.
        #[arg(long)]
        reboot: bool,
        /// Stop sending after this many bytes and disconnect, to test an interrupted update.
        #[arg(long)]
        cut_at: Option<usize>,
    },
}

/// `HH:MM:SS.mmm` in UTC.
fn now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let s = d.as_secs() % 86400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, d.subsec_millis())
}

fn note(msg: impl AsRef<str>) {
    println!("{} {}", now(), msg.as_ref());
}

struct Link {
    tx: Characteristic,
    data: Characteristic,
    events: mpsc::UnboundedReceiver<Value>,
}

impl Link {
    async fn open(device: &Device) -> Result<Link> {
        let mut tx = None;
        let mut rx = None;
        let mut data = None;
        for service in device.services().await? {
            if service.uuid().await? != MGMT_SERVICE {
                continue;
            }
            for c in service.characteristics().await? {
                match c.uuid().await? {
                    u if u == MGMT_TX => tx = Some(c),
                    u if u == MGMT_RX => rx = Some(c),
                    u if u == MGMT_DATA => data = Some(c),
                    _ => {}
                }
            }
        }
        let (Some(tx), Some(rx), Some(data)) = (tx, rx, data) else {
            bail!("no management service with a data characteristic; is the update spike flashed?");
        };
        let reader = rx.notify_io().await.context("subscribing to management notifications")?;
        note(format!("notify MTU {}", reader.mtu()));
        let (send, events) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            // An empty read is the end: the link closed.
            while let Ok(packet) = reader.recv().await {
                if packet.is_empty() {
                    break;
                }
                buf.extend_from_slice(&packet);
                while buf.len() >= 4 {
                    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                    if buf.len() < 4 + len {
                        break;
                    }
                    let body: Vec<u8> = buf.drain(..4 + len).skip(4).collect();
                    match serde_json::from_slice::<Value>(&body) {
                        Ok(v) => {
                            if send.send(v).is_err() {
                                return;
                            }
                        }
                        Err(e) => note(format!("bad message ({e}): {}", String::from_utf8_lossy(&body))),
                    }
                }
            }
            note("management notifications ended");
        });
        Ok(Link { tx, data, events })
    }

    async fn send(&self, msg: Value) -> Result<()> {
        let body = msg.to_string();
        note(format!("-> {body}"));
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(body.as_bytes());
        // Small messages; 180 bytes fits any MTU the ATOM negotiates.
        for chunk in out.chunks(180) {
            self.tx.write(chunk).await?;
        }
        Ok(())
    }

    /// The next event, printed unless it's an ack.
    async fn next(&mut self, timeout: Duration) -> Result<Value> {
        let v = tokio::time::timeout(timeout, self.events.recv())
            .await
            .context("no reply in time")?
            .context("disconnected")?;
        if v["event"] != "ack" {
            note(format!("<- {v}"));
        }
        if v["event"] == "error" {
            bail!("device error: {v}");
        }
        Ok(v)
    }

    async fn wait_for(&mut self, event: &str, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let v = self.next(left).await?;
            if v["event"] == event {
                return Ok(v);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let session = bluer::Session::new().await?;
    let adapter = session.adapter(&args.adapter)?;
    adapter.set_powered(true).await?;
    let device = find(&adapter, args.device).await?;
    connect(&device).await?;
    let mut link = Link::open(&device).await?;

    let result = match args.cmd {
        Cmd::Info => info(&mut link).await,
        Cmd::Reboot => {
            link.send(json!({"cmd": "reboot"})).await?;
            link.wait_for("rebooting", Duration::from_secs(5)).await.map(|_| ())
        }
        Cmd::Abort => {
            link.send(json!({"cmd": "update.abort"})).await?;
            link.wait_for("aborted", Duration::from_secs(5)).await.map(|_| ())
        }
        Cmd::Send { image, erase, window, chunk, interval, stop_radio, reboot, cut_at } => {
            let image = std::fs::read(&image).with_context(|| format!("reading {}", image.display()))?;
            let opts = SendOpts { erase, window, chunk, interval, stop_radio, cut_at };
            match send(&mut link, &image, &opts).await {
                Ok(true) if reboot => {
                    link.send(json!({"cmd": "reboot"})).await?;
                    link.wait_for("rebooting", Duration::from_secs(5)).await?;
                    let _ = device.disconnect().await;
                    drop(link);
                    note("waiting for it to come back");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let device = find(&adapter, args.device).await?;
                    connect(&device).await?;
                    let mut link = Link::open(&device).await?;
                    info(&mut link).await
                }
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            }
        }
    };
    let _ = device.disconnect().await;
    result
}

async fn info(link: &mut Link) -> Result<()> {
    link.send(json!({"cmd": "info"})).await?;
    let v = link.wait_for("info", Duration::from_secs(5)).await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

struct SendOpts {
    erase: String,
    window: Option<usize>,
    chunk: Option<usize>,
    interval: Option<Vec<u16>>,
    stop_radio: bool,
    cut_at: Option<usize>,
}

/// Sends the image. Ok(true) when the device has it set to boot next.
async fn send(link: &mut Link, image: &[u8], opts: &SendOpts) -> Result<bool> {
    let size = image.len();
    let mut begin = json!({"cmd": "update.begin", "size": size, "erase": opts.erase, "stop_radio": opts.stop_radio});
    if let Some(i) = &opts.interval {
        begin["interval"] = json!(i);
    }
    let t0 = Instant::now();
    link.send(begin).await?;
    let ready = link.wait_for("ready", Duration::from_secs(60)).await?;
    let t_ready = Instant::now();

    let device_window = ready["window"].as_u64().unwrap_or(8192) as usize;
    let window = opts.window.unwrap_or(device_window);
    let writer = link.data.write_io().await.context("acquiring the data characteristic")?;
    let device_chunk = ready["chunk_max"].as_u64().unwrap_or(244) as usize;
    let mtu_chunk = writer.mtu().saturating_sub(4);
    let chunk = opts.chunk.unwrap_or(usize::MAX).min(device_chunk).min(mtu_chunk);
    note(format!(
        "sending {size} bytes: write MTU {}, {chunk} bytes per write, window {window} (device offers {device_window})",
        writer.mtu()
    ));

    let mut sent = 0usize;
    let mut acked = 0usize;
    let mut naks = 0u32;
    let mut writes = 0u64;
    let mut last_report = Instant::now();
    let mut last_report_acked = 0usize;
    let t_data = Instant::now();
    while acked < size {
        // Events that have already arrived.
        while let Ok(v) = link.events.try_recv() {
            handle(&v, &mut sent, &mut acked, &mut naks)?;
        }
        if acked >= size {
            break;
        }
        if let Some(cut) = opts.cut_at {
            if sent >= cut {
                note(format!("cutting the transfer at {sent} bytes, as asked"));
                return Ok(false);
            }
        }
        if sent < size && sent - acked.min(sent) < window {
            let n = chunk.min(size - sent);
            let mut buf = Vec::with_capacity(4 + n);
            buf.extend_from_slice(&(sent as u32).to_le_bytes());
            buf.extend_from_slice(&image[sent..sent + n]);
            writer.send(&buf).await.context("writing image data")?;
            writes += 1;
            sent += n;
        } else {
            let v = tokio::time::timeout(Duration::from_secs(10), link.events.recv())
                .await
                .with_context(|| format!("stalled: sent {sent}, acknowledged {acked}"))?
                .context("disconnected")?;
            handle(&v, &mut sent, &mut acked, &mut naks)?;
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            let rate = (acked - last_report_acked) as f64 / last_report.elapsed().as_secs_f64() / 1024.0;
            note(format!("{:5.1}% acknowledged, {rate:.1} KB/s", acked as f64 * 100.0 / size as f64));
            last_report = Instant::now();
            last_report_acked = acked;
        }
    }
    let data_secs = t_data.elapsed().as_secs_f64();
    link.send(json!({"cmd": "update.finish"})).await?;
    let fin = link.wait_for("finished", Duration::from_secs(60)).await?;
    let total = t0.elapsed().as_secs_f64();
    println!();
    println!("image          {size} bytes, {writes} writes of up to {chunk} bytes, window {window}");
    println!("erase          {:.1} s (device: {} ms)", (t_ready - t0).as_secs_f64(), ready["erase_ms"]);
    println!("transfer       {data_secs:.1} s, {:.1} KB/s", size as f64 / data_secs / 1024.0);
    println!("device         flash writes {} ms, verify {} ms, gaps {}, overflows {}, heap low {}",
        fin["flash_ms"], fin["verify_ms"], fin["gaps"], fin["overflows"], fin["heap_low"]);
    println!("naks           {naks}");
    println!("total          {total:.1} s, {:.1} KB/s overall", size as f64 / total / 1024.0);
    println!("next boot      {} ({})", fin["partition"], fin["version"]);
    Ok(true)
}

fn handle(v: &Value, sent: &mut usize, acked: &mut usize, naks: &mut u32) -> Result<()> {
    match v["event"].as_str() {
        Some("ack") => *acked = v["written"].as_u64().unwrap_or(0) as usize,
        Some("nak") => {
            let expected = v["expected"].as_u64().unwrap_or(0) as usize;
            note(format!("nak: resending from {expected} (had sent {sent})"));
            *sent = expected;
            *naks += 1;
        }
        Some("error") => bail!("device error: {v}"),
        _ => note(format!("<- {v}")),
    }
    Ok(())
}

/// The device, scanning for it if BlueZ doesn't know it.
async fn find(adapter: &Adapter, addr: Address) -> Result<Device> {
    if !adapter.device_addresses().await?.contains(&addr) {
        note(format!("scanning for {addr}"));
        adapter
            .set_discovery_filter(DiscoveryFilter { transport: DiscoveryTransport::Le, ..Default::default() })
            .await?;
        let events = adapter.discover_devices().await?;
        futures::pin_mut!(events);
        let heard = async {
            while let Some(ev) = events.next().await {
                if matches!(ev, AdapterEvent::DeviceAdded(a) if a == addr) {
                    return true;
                }
            }
            false
        };
        if !tokio::time::timeout(Duration::from_secs(30), heard).await.unwrap_or(false) {
            bail!("{addr} isn't advertising");
        }
    }
    Ok(adapter.device(addr)?)
}

async fn connect(device: &Device) -> Result<()> {
    // An LE connect right after a scan often aborts at once
    // (le-connection-abort-by-local); a second try a moment later works.
    let mut tries = 0;
    while !device.is_connected().await? {
        tries += 1;
        note(format!("connecting to {} (try {tries})", device.address()));
        match tokio::time::timeout(Duration::from_secs(20), device.connect()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if tries < 5 => {
                note(format!("connect failed: {e}; trying again"));
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => bail!("no connection after 20 s"),
        }
    }
    for _ in 0..100 {
        if device.is_services_resolved().await? {
            note(format!("connected to {}", device.address()));
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("its services weren't resolved within 20 s")
}
