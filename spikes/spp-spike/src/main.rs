//! Phase 0 spike: talk to a Kenwood TH-D75's KISS TNC from Linux over
//! Bluetooth SPP (through a BlueZ profile) or USB serial, log everything
//! with timings, and expose the link as a KISS TCP server so the 0.5 daemon
//! can sit in front of it for the BLE coexistence test.
//!
//! Throwaway code. Nothing here is meant to survive into the daemon; it
//! exists to answer the Phase 0 questions in docs/plan-1.0.md.

use anyhow::{Context, Result, anyhow, bail};
use bluer::agent::{
    Agent, AuthorizeService, DisplayPasskey, DisplayPinCode, RequestAuthorization,
    RequestConfirmation, RequestPasskey, RequestPinCode,
};
use bluer::rfcomm::{Profile, ProfileHandle, ReqError, Role, SocketAddr as RfcommAddr, Stream as RfcommStream};
use bluer::{Adapter, AdapterEvent, Address, Device, DiscoveryFilter, DiscoveryTransport, Session};
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STOP: std::sync::LazyLock<tokio::sync::Notify> = std::sync::LazyLock::new(tokio::sync::Notify::new);

/// Resolves once Ctrl-C has been pressed (immediately if it already was).
async fn stopped() {
    use std::sync::atomic::Ordering;
    loop {
        let notified = STOP.notified();
        if STOPPED.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

fn install_ctrl_c() {
    tokio::spawn(async {
        use std::sync::atomic::Ordering;
        let _ = tokio::signal::ctrl_c().await;
        info!("Ctrl-C: stopping (press again to exit immediately)");
        STOPPED.store(true, Ordering::SeqCst);
        STOP.notify_waiters();
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(130);
    });
}

const SPP_UUID: Uuid = Uuid::from_u128(0x00001101_0000_1000_8000_00805f9b34fb);
/// Class of device bb-link uses to recognise Kenwood handsets (TH-D74/D75).
const KENWOOD_HANDSET_COD: u32 = 0x0062_0204;

const FEND: u8 = 0xC0;
const FESC: u8 = 0xDB;
const TFEND: u8 = 0xDC;
const TFESC: u8 = 0xDD;

// ── CLI ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(version, about = "Phase 0 spike: TH-D75 KISS TNC over Bluetooth SPP / USB serial")]
struct Cli {
    /// Bluetooth adapter (hci0, hci1). Default: BlueZ's default adapter.
    #[arg(short, long, global = true)]
    adapter: Option<String>,
    /// More logging (-v: raw bytes, -vv: trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

fn parse_addr(s: &str) -> Result<Address, String> {
    s.parse().map_err(|_| format!("invalid Bluetooth address {s:?}"))
}

#[derive(Subcommand)]
enum Cmd {
    /// List Bluetooth adapters and their rfkill state
    Adapters,
    /// Bluetooth Classic discovery; flags Kenwood handsets and SPP devices
    Scan {
        /// Seconds to scan
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// BLE scan: what do nearby advertisements (e.g. our own daemon on the
    /// other adapter) look like? Prints name, RSSI and service UUIDs.
    Lescan {
        /// Seconds to scan
        #[arg(long, default_value_t = 15)]
        timeout: u64,
        /// Only report this address
        #[arg(long, value_parser = parse_addr)]
        only: Option<Address>,
    },
    /// Show what BlueZ knows about a device
    Info {
        #[arg(value_parser = parse_addr)]
        address: Address,
    },
    /// Pair with a device, logging every agent callback BlueZ makes
    Pair {
        #[arg(value_parser = parse_addr)]
        address: Address,
        /// PIN to answer a RequestPinCode with (legacy pairing)
        #[arg(long, default_value = "0000")]
        pin: String,
        /// Seconds to wait for pairing
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Seconds to discover the device first if BlueZ doesn't know it
        #[arg(long, default_value_t = 30)]
        discover_timeout: u64,
        /// Agent capability to pair with: keyboard-display (logs and answers
        /// every callback) or no-input-no-output (Just Works, what the 0.5
        /// daemon registers)
        #[arg(long, default_value = "keyboard-display")]
        agent: AgentKind,
    },
    /// Forget a device (removes the pairing on this adapter)
    Unpair {
        #[arg(value_parser = parse_addr)]
        address: Address,
    },
    /// Connect to a TNC over SPP, run KISS init, and bridge it to TCP
    Connect {
        #[arg(value_parser = parse_addr)]
        address: Address,
        /// Raw RFCOMM connect to this channel instead of a BlueZ profile (no SDP)
        #[arg(long)]
        channel: Option<u8>,
        /// Register the profile without RequireAuthentication
        #[arg(long)]
        no_auth: bool,
        #[command(flatten)]
        link: LinkOpts,
    },
    /// Open a serial device, run KISS init, and bridge it to TCP
    Serial {
        path: String,
        #[arg(long, default_value_t = 9600)]
        baud: u32,
        #[command(flatten)]
        link: LinkOpts,
    },
}

#[derive(Args, Clone)]
struct LinkOpts {
    /// KISS init sequence: th-d7x (bb-link's BT/ID/TN probe) or none
    #[arg(long, default_value = "th-d7x")]
    model: Model,
    /// Serve a KISS TCP server for apps here, e.g. 127.0.0.1:8002
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Keep reconnecting when the link drops, logging every attempt's timing
    #[arg(long)]
    forever: bool,
    /// Seconds between reconnect attempts
    #[arg(long, default_value_t = 5)]
    retry: u64,
    /// Run KISS init, report, then disconnect without bridging
    #[arg(long)]
    init_only: bool,
    /// On exit: send C0 FF C0 and put the TNC back in the mode seen at init
    #[arg(long)]
    restore_on_exit: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum AgentKind {
    KeyboardDisplay,
    NoInputNoOutput,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    #[value(name = "th-d7x")]
    ThD7x,
    None,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_target(false)
        .init();
    install_ctrl_c();

    let adapter = cli.adapter.as_deref();
    match cli.cmd {
        Cmd::Adapters => adapters().await,
        Cmd::Scan { timeout } => scan(adapter, timeout).await,
        Cmd::Lescan { timeout, only } => lescan(adapter, timeout, only).await,
        Cmd::Info { address } => info_cmd(adapter, address).await,
        Cmd::Pair { address, pin, timeout, discover_timeout, agent } => {
            pair(adapter, address, pin, timeout, discover_timeout, agent).await
        }
        Cmd::Unpair { address } => unpair(adapter, address).await,
        Cmd::Connect { address, channel, no_auth, link } => {
            connect(adapter, address, channel, no_auth, link).await
        }
        Cmd::Serial { path, baud, link } => serial(path, baud, link).await,
    }
}

// ── Adapters and devices ────────────────────────────────────────────────

async fn open_adapter(session: &Session, name: Option<&str>) -> Result<Adapter> {
    let adapter = match name {
        Some(n) => session.adapter(n).with_context(|| format!("adapter {n}"))?,
        None => session.default_adapter().await.context("default adapter")?,
    };
    if !adapter.is_powered().await? {
        info!(adapter = adapter.name(), "powering on");
        adapter.set_powered(true).await.with_context(|| {
            format!(
                "powering on {} (rfkill blocked? check `adapters`): {}",
                adapter.name(),
                rfkill_state(adapter.name()).unwrap_or_else(|| "no rfkill entry".into())
            )
        })?;
    }
    Ok(adapter)
}

fn rfkill_state(hci: &str) -> Option<String> {
    let dir = std::fs::read_dir("/sys/class/rfkill").ok()?;
    for entry in dir.flatten() {
        let p = entry.path();
        let Ok(name) = std::fs::read_to_string(p.join("name")) else { continue };
        if name.trim() != hci {
            continue;
        }
        let soft = std::fs::read_to_string(p.join("soft")).unwrap_or_default();
        let hard = std::fs::read_to_string(p.join("hard")).unwrap_or_default();
        return Some(format!("{}: soft={} hard={}", p.display(), soft.trim(), hard.trim()));
    }
    None
}

async fn adapters() -> Result<()> {
    let session = Session::new().await?;
    for name in session.adapter_names().await? {
        let a = session.adapter(&name)?;
        println!(
            "{name}  {}  powered={} pairable={} discoverable={} alias={:?}  rfkill {}",
            a.address().await?,
            a.is_powered().await?,
            a.is_pairable().await?,
            a.is_discoverable().await?,
            a.alias().await?,
            rfkill_state(&name).unwrap_or_else(|| "-".into())
        );
        for addr in a.device_addresses().await? {
            println!("    {}", describe_device(&a, addr).await);
        }
    }
    Ok(())
}

async fn describe_device(adapter: &Adapter, addr: Address) -> String {
    let Ok(dev) = adapter.device(addr) else { return addr.to_string() };
    let name = dev.name().await.ok().flatten().unwrap_or_default();
    let class = dev.class().await.ok().flatten();
    let rssi = dev.rssi().await.ok().flatten();
    let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
    let mut tags = Vec::new();
    if class == Some(KENWOOD_HANDSET_COD) {
        tags.push("kenwood-handset");
    }
    if uuids.contains(&SPP_UUID) {
        tags.push("spp");
    }
    if dev.is_paired().await.unwrap_or(false) {
        tags.push("paired");
    }
    if dev.is_connected().await.unwrap_or(false) {
        tags.push("connected");
    }
    format!(
        "{addr}  {name:<16} class={} rssi={} uuids={} {}",
        class.map(|c| format!("0x{c:06X}")).unwrap_or_else(|| "-".into()),
        rssi.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
        uuids.len(),
        tags.join(",")
    )
}

async fn scan(adapter: Option<&str>, secs: u64) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    info!(adapter = adapter.name(), addr = %adapter.address().await?, "known devices:");
    for addr in adapter.device_addresses().await? {
        println!("  {}", describe_device(&adapter, addr).await);
    }
    adapter
        .set_discovery_filter(DiscoveryFilter { transport: DiscoveryTransport::BrEdr, ..Default::default() })
        .await?;
    let t0 = Instant::now();
    let mut events = adapter.discover_devices_with_changes().await?;
    info!("scanning Bluetooth Classic for {secs}s");
    let deadline = sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(ev) = events.next() => match ev {
                AdapterEvent::DeviceAdded(addr) => {
                    println!("  +{:>5.1}s {}", t0.elapsed().as_secs_f32(), describe_device(&adapter, addr).await)
                }
                AdapterEvent::DeviceRemoved(addr) => println!("  +{:>5.1}s gone {addr}", t0.elapsed().as_secs_f32()),
                _ => {}
            },
            _ = &mut deadline => break,
        }
    }
    Ok(())
}

async fn lescan(adapter: Option<&str>, secs: u64, only: Option<Address>) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            ..Default::default()
        })
        .await?;
    let t0 = Instant::now();
    let mut events = adapter.discover_devices_with_changes().await?;
    info!(adapter = adapter.name(), "scanning LE for {secs}s");
    let deadline = sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(ev) = events.next() => {
                if let AdapterEvent::DeviceAdded(addr) = ev {
                    if only.is_some_and(|o| o != addr) {
                        continue;
                    }
                    let Ok(dev) = adapter.device(addr) else { continue };
                    let name = dev.name().await.ok().flatten();
                    let alias = dev.alias().await.ok();
                    let rssi = dev.rssi().await.ok().flatten();
                    let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
                    let sd = dev.service_data().await.ok().flatten().map(|m| m.len()).unwrap_or(0);
                    let md = dev.manufacturer_data().await.ok().flatten().map(|m| m.len()).unwrap_or(0);
                    println!(
                        "  +{:>5.1}s {addr} name={name:?} alias={alias:?} rssi={rssi:?} uuids={:?} service_data={sd} mfr_data={md}",
                        t0.elapsed().as_secs_f32(),
                        uuids.iter().map(|u| u.to_string()).collect::<Vec<_>>()
                    );
                }
            },
            _ = &mut deadline => break,
        }
    }
    Ok(())
}

async fn info_cmd(adapter: Option<&str>, addr: Address) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    let dev = adapter.device(addr)?;
    println!("{}", describe_device(&adapter, addr).await);
    println!(
        "  alias={:?} paired={} trusted={} connected={}",
        dev.alias().await?,
        dev.is_paired().await?,
        dev.is_trusted().await?,
        dev.is_connected().await?
    );
    if let Some(uuids) = dev.uuids().await? {
        for u in uuids {
            println!("  uuid {u}");
        }
    }
    Ok(())
}

// ── Pairing ─────────────────────────────────────────────────────────────

/// An agent that answers everything and logs which callbacks BlueZ makes.
/// Having every handler set makes BlueZ advertise us as KeyboardDisplay.
fn logging_agent(pin: String) -> Agent {
    let passkey: u32 = pin.parse().unwrap_or(0);
    Agent {
        request_default: false,
        request_pin_code: Some(Box::new(move |req: RequestPinCode| {
            let pin = pin.clone();
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, "agent: RequestPinCode -> answering {pin:?}");
                Ok(pin)
            })
        })),
        display_pin_code: Some(Box::new(|req: DisplayPinCode| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, pincode = %req.pincode, "agent: DisplayPinCode");
                Ok(())
            })
        })),
        request_passkey: Some(Box::new(move |req: RequestPasskey| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, "agent: RequestPasskey -> answering {passkey}");
                Ok(passkey)
            })
        })),
        display_passkey: Some(Box::new(|req: DisplayPasskey| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, passkey = req.passkey, entered = req.entered, "agent: DisplayPasskey");
                Ok(())
            })
        })),
        request_confirmation: Some(Box::new(|req: RequestConfirmation| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, passkey = req.passkey, "agent: RequestConfirmation -> confirming");
                Ok(())
            })
        })),
        request_authorization: Some(Box::new(|req: RequestAuthorization| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, "agent: RequestAuthorization -> authorizing");
                Ok(())
            })
        })),
        authorize_service: Some(Box::new(|req: AuthorizeService| {
            Box::pin(async move {
                info!(adapter = %req.adapter, device = %req.device, service = %req.service, "agent: AuthorizeService -> authorizing");
                Ok(())
            })
        })),
        ..Default::default()
    }
}

async fn ensure_known(adapter: &Adapter, addr: Address, secs: u64) -> Result<()> {
    if adapter.device_addresses().await?.contains(&addr) {
        return Ok(());
    }
    info!("BlueZ doesn't know {addr} yet; discovering (Classic) for up to {secs}s");
    adapter
        .set_discovery_filter(DiscoveryFilter { transport: DiscoveryTransport::BrEdr, ..Default::default() })
        .await?;
    let mut events = adapter.discover_devices().await?;
    let found = timeout(Duration::from_secs(secs), async {
        while let Some(ev) = events.next().await {
            if let AdapterEvent::DeviceAdded(a) = ev {
                info!("  found {}", describe_device(adapter, a).await);
                if a == addr {
                    return true;
                }
            }
        }
        false
    })
    .await;
    match found {
        Ok(true) => Ok(()),
        _ => bail!("{addr} not found; is the radio in pairing mode (Menu > Bluetooth > Pairing Mode)?"),
    }
}

async fn pair(adapter: Option<&str>, addr: Address, pin: String, secs: u64, discover_secs: u64, kind: AgentKind) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    let _agent = match kind {
        AgentKind::KeyboardDisplay => {
            let h = session.register_agent(logging_agent(pin)).await?;
            info!(adapter = adapter.name(), "registered logging agent (all callbacks, KeyboardDisplay)");
            h
        }
        AgentKind::NoInputNoOutput => {
            let h = session.register_agent(Agent::default()).await?;
            info!(adapter = adapter.name(), "registered NoInputNoOutput agent (Just Works); BlueZ makes no callbacks to it");
            h
        }
    };
    adapter.set_pairable(true).await?;
    ensure_known(&adapter, addr, discover_secs).await?;
    let dev = adapter.device(addr)?;
    println!("{}", describe_device(&adapter, addr).await);
    if dev.is_paired().await? {
        warn!("already paired on this adapter; `unpair` first to record the callbacks");
    }
    info!("pairing with {addr}; accept the prompt on the radio if it shows one");
    let t0 = Instant::now();
    tokio::select! {
        res = timeout(Duration::from_secs(secs), dev.pair()) => match res {
            Ok(Ok(())) => info!("paired in {:.1?}", t0.elapsed()),
            Ok(Err(e)) => bail!("pairing failed after {:.1?}: {e}", t0.elapsed()),
            Err(_) => bail!("pairing timed out after {secs}s"),
        },
        _ = stopped() => bail!("stopped by user after {:.1?}", t0.elapsed()),
    }
    dev.set_trusted(true).await?;
    info!(
        paired = dev.is_paired().await?,
        trusted = dev.is_trusted().await?,
        connected = dev.is_connected().await?,
        "state after pairing"
    );
    if let Some(uuids) = dev.uuids().await? {
        for u in uuids {
            info!("  uuid {u}");
        }
    }
    sleep(Duration::from_secs(3)).await;
    info!(connected = dev.is_connected().await?, "3s later (did BlueZ auto-connect anything?)");
    Ok(())
}

async fn unpair(adapter: Option<&str>, addr: Address) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    adapter.remove_device(addr).await?;
    info!("removed {addr} from {}", adapter.name());
    Ok(())
}

// ── KISS framing and display ────────────────────────────────────────────

/// Splits a byte stream into KISS frames. Bytes outside frames (the radio's
/// text replies when it isn't in KISS mode) are collected as noise.
#[derive(Default)]
struct Reasm {
    buf: Vec<u8>,
    in_frame: bool,
    noise: Vec<u8>,
}

impl Reasm {
    fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for &b in data {
            if b == FEND {
                if self.in_frame && !self.buf.is_empty() {
                    frames.push(std::mem::take(&mut self.buf));
                }
                self.buf.clear();
                self.in_frame = true;
            } else if self.in_frame {
                self.buf.push(b);
                if self.buf.len() > 4096 {
                    warn!("frame over 4096 bytes without FEND; dropping");
                    self.buf.clear();
                    self.in_frame = false;
                }
            } else {
                self.noise.push(b);
            }
        }
        frames
    }

    fn take_noise(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.noise)
    }
}

fn wrap(raw: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(raw.len() + 2);
    v.push(FEND);
    v.extend_from_slice(raw);
    v.push(FEND);
    v
}

fn unescape(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == FESC && i + 1 < raw.len() {
            out.push(match raw[i + 1] {
                TFEND => FEND,
                TFESC => FESC,
                other => other,
            });
            i += 2;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

fn printable(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x20..=0x7E => c as char,
            b'\r' => '\u{23CE}',
            b'\n' => '\u{21B5}',
            _ => '.',
        })
        .collect()
}

fn ax25_addr(b: &[u8]) -> (String, bool) {
    let call: String = b[..6].iter().map(|c| (c >> 1) as char).collect::<String>().trim_end().to_string();
    let ssid = (b[6] >> 1) & 0x0F;
    let last = b[6] & 0x01 == 1;
    let repeated = b[6] & 0x80 != 0;
    let s = if ssid == 0 { call } else { format!("{call}-{ssid}") };
    (if repeated { format!("{s}*") } else { s }, last)
}

fn describe_frame(raw: &[u8]) -> String {
    let f = unescape(raw);
    let Some((&cmd, data)) = f.split_first() else {
        return "empty".into();
    };
    if cmd & 0x0F != 0 || data.len() < 16 {
        return format!("cmd=0x{cmd:02X} {} bytes: {}", data.len(), hex(data));
    }
    let (dst, dlast) = ax25_addr(&data[0..7]);
    let (src, slast) = ax25_addr(&data[7..14]);
    let mut last = dlast || slast;
    let mut path = Vec::new();
    let mut i = 14;
    while !last && i + 7 <= data.len() {
        let (d, l) = ax25_addr(&data[i..i + 7]);
        path.push(d);
        last = l;
        i += 7;
    }
    let rest = &data[i..];
    let (ctl, info) = match rest {
        [c, _pid, info @ ..] => (*c, info),
        [c] => (*c, &[][..]),
        [] => (0, &[][..]),
    };
    let via = if path.is_empty() { String::new() } else { format!(",{}", path.join(",")) };
    format!("[{}B ch{}] {src}>{dst}{via} ctl=0x{ctl:02X}: {}", data.len(), cmd >> 4, printable(info))
}

// ── KISS TCP server for apps ────────────────────────────────────────────

type Clients = Arc<Mutex<Vec<(SocketAddr, mpsc::UnboundedSender<Vec<u8>>)>>>;

struct Hub {
    clients: Clients,
    _uplink_tx: mpsc::Sender<Vec<u8>>,
    uplink_rx: mpsc::Receiver<Vec<u8>>,
}

impl Hub {
    async fn new(listen: Option<SocketAddr>) -> Result<Self> {
        let (uplink_tx, uplink_rx) = mpsc::channel(64);
        let clients: Clients = Arc::default();
        if let Some(addr) = listen {
            let listener = TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;
            info!("KISS TCP server listening on {addr}");
            tokio::spawn(accept_loop(listener, clients.clone(), uplink_tx.clone()));
        }
        Ok(Self { clients, _uplink_tx: uplink_tx, uplink_rx })
    }
}

fn broadcast(clients: &Clients, wire: &[u8]) {
    clients.lock().unwrap().retain(|(_, tx)| tx.send(wire.to_vec()).is_ok());
}

fn client_count(clients: &Clients) -> usize {
    clients.lock().unwrap().len()
}

async fn accept_loop(listener: TcpListener, clients: Clients, uplink: mpsc::Sender<Vec<u8>>) {
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                error!("accept: {e}");
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        info!("tcp client {peer} connected");
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        clients.lock().unwrap().push((peer, tx));
        let (mut rd, mut wr) = sock.into_split();
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if wr.write_all(&f).await.is_err() {
                    break;
                }
            }
        });
        let clients = clients.clone();
        let uplink = uplink.clone();
        tokio::spawn(async move {
            let mut reasm = Reasm::default();
            let mut buf = [0u8; 2048];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for raw in reasm.push(&buf[..n]) {
                            if raw.first() == Some(&0xFF) {
                                warn!("tx <- {peer}: KISS exit (C0 FF C0) from an app; dropping it");
                                continue;
                            }
                            info!("tx <- {peer}: {}", describe_frame(&raw));
                            if uplink.send(wrap(&raw)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            info!("tcp client {peer} disconnected");
            clients.lock().unwrap().retain(|(p, _)| *p != peer);
        });
    }
}

// ── KISS init (bb-link's THD7x sequence) ────────────────────────────────

#[derive(Debug, Default)]
struct InitReport {
    /// `BT` got no reply, so the TNC was already in KISS mode when we connected
    already_kiss: Option<bool>,
    /// `ID` reply, e.g. "TH-D75"
    radio_id: Option<String>,
    /// `TN` p2: band 0 = A, 1 = B
    band: Option<u8>,
    /// `TN` p1: 0 off, 1 APRS, 2 KISS
    prev_mode: Option<u8>,
    /// We sent `TN 2,<band>`
    entered_kiss: bool,
}

/// Write `cmd` + CR and wait up to `wait` for a CR-terminated reply.
/// Bytes that aren't the reply stay in `pending`.
async fn radio_cmd<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    rd: &mut R,
    wr: &mut W,
    pending: &mut Vec<u8>,
    cmd: &str,
    wait: Duration,
) -> Result<Option<String>> {
    let t0 = Instant::now();
    wr.write_all(format!("{cmd}\r").as_bytes()).await?;
    wr.flush().await?;
    info!("> {cmd:?}");
    let mut buf = [0u8; 256];
    loop {
        if let Some(pos) = pending.iter().position(|&b| b == b'\r') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            let text = printable(&line[..line.len() - 1]);
            info!("< {text:?} after {:.0?} (raw {})", t0.elapsed(), hex(&line));
            return Ok(Some(text));
        }
        let remaining = wait.saturating_sub(t0.elapsed());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, rd.read(&mut buf)).await {
            Ok(Ok(0)) => bail!("link closed while waiting for a reply to {cmd:?}"),
            Ok(Ok(n)) => {
                debug!("  raw {}", hex(&buf[..n]));
                pending.extend_from_slice(&buf[..n]);
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break,
        }
    }
    info!("< no reply to {cmd:?} within {wait:?} ({} stray bytes: {})", pending.len(), hex(pending));
    Ok(None)
}

async fn kiss_exit<W: AsyncWrite + Unpin>(wr: &mut W) -> Result<()> {
    info!("> KISS exit C0 FF C0");
    wr.write_all(&[FEND, 0xFF, FEND]).await?;
    wr.flush().await?;
    sleep(Duration::from_millis(300)).await;
    Ok(())
}

async fn kiss_init<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    rd: &mut R,
    wr: &mut W,
    pending: &mut Vec<u8>,
    model: Model,
) -> Result<InitReport> {
    let mut rep = InitReport::default();
    if model == Model::None {
        info!("model=none: skipping KISS init");
        return Ok(rep);
    }
    let t0 = Instant::now();
    let one = Duration::from_secs(1);
    // 1. bb-link's probe: `BT` is answered in command mode, ignored in KISS mode.
    let bt = radio_cmd(rd, wr, pending, "BT", one).await?;
    rep.already_kiss = Some(bt.is_none());
    if bt.is_none() {
        info!("no reply to BT: TNC is presumably already in KISS mode; leaving it to run init");
        kiss_exit(wr).await?;
        if radio_cmd(rd, wr, pending, "BT", one).await?.is_none() {
            warn!("still silent after KISS exit: not a TH-D7x in KISS mode, or the link is dead");
        }
    }
    // 2. Identify the radio.
    if let Some(id) = radio_cmd(rd, wr, pending, "ID", one).await? {
        rep.radio_id = Some(id.strip_prefix("ID ").unwrap_or(&id).to_string());
    }
    // 3. Current TNC mode and band: `TN p1,p2`.
    if let Some(tn) = radio_cmd(rd, wr, pending, "TN", one).await? {
        if let Some(rest) = tn.strip_prefix("TN ") {
            let mut it = rest.split(',');
            rep.prev_mode = it.next().and_then(|s| s.trim().parse().ok());
            rep.band = it.next().and_then(|s| s.trim().parse().ok());
        }
    }
    // 4. Enter KISS mode on that band, as bb-link does (`TN 2,<band>`).
    let band = rep.band.unwrap_or(0);
    let reply = radio_cmd(rd, wr, pending, &format!("TN 2,{band}"), one).await?;
    rep.entered_kiss = true;
    info!(?rep, ?reply, "KISS init done in {:.1?}", t0.elapsed());
    Ok(rep)
}

async fn restore<W: AsyncWrite + Unpin>(wr: &mut W, rep: &InitReport) -> Result<()> {
    if !rep.entered_kiss {
        return Ok(());
    }
    kiss_exit(wr).await?;
    if let (Some(mode), Some(band)) = (rep.prev_mode, rep.band) {
        if mode != 2 {
            let cmd = format!("TN {mode},{band}\r");
            info!("> {:?} (restoring the TNC mode seen at init)", cmd.trim_end());
            wr.write_all(cmd.as_bytes()).await?;
            wr.flush().await?;
            sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

// ── The link loop ───────────────────────────────────────────────────────

enum Stop {
    User,
    Done,
}

async fn run_link<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    link: &LinkOpts,
    clients: &Clients,
    uplink_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<Stop> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let mut pending = Vec::new();
    let rep = kiss_init(&mut rd, &mut wr, &mut pending, link.model).await?;
    if link.init_only {
        if link.restore_on_exit {
            restore(&mut wr, &rep).await?;
        }
        return Ok(Stop::Done);
    }
    info!("bridging; {} tcp client(s) attached; Ctrl-C to stop", client_count(clients));
    let mut reasm = Reasm::default();
    let mut buf = vec![0u8; 4096];
    let (mut rx_frames, mut tx_frames, mut rx_bytes) = (0u64, 0u64, 0u64);
    let t0 = Instant::now();
    let mut last_rx = Instant::now();
    let mut stats = tokio::time::interval(Duration::from_secs(60));
    stats.tick().await;
    for raw in reasm.push(&pending) {
        info!("rx (arrived during init) {}", describe_frame(&raw));
        broadcast(clients, &wrap(&raw));
    }
    let outcome: Result<Stop> = loop {
        tokio::select! {
            r = rd.read(&mut buf) => match r {
                Ok(0) => break Err(anyhow!("link closed by peer after {:.1?}", t0.elapsed())),
                Err(e) => break Err(anyhow!("read error after {:.1?}: {e}", t0.elapsed())),
                Ok(n) => {
                    rx_bytes += n as u64;
                    debug!("rx raw {}", hex(&buf[..n]));
                    for raw in reasm.push(&buf[..n]) {
                        rx_frames += 1;
                        last_rx = Instant::now();
                        info!("rx {}", describe_frame(&raw));
                        broadcast(clients, &wrap(&raw));
                    }
                    let noise = reasm.take_noise();
                    if !noise.is_empty() {
                        info!("non-KISS bytes from the TNC: {:?} ({})", printable(&noise), hex(&noise));
                    }
                }
            },
            Some(wire) = uplink_rx.recv() => {
                if let Err(e) = wr.write_all(&wire).await {
                    break Err(anyhow!("write error after {:.1?}: {e}", t0.elapsed()));
                }
                tx_frames += 1;
            },
            _ = stats.tick() => info!(
                up = %format!("{:.0?}", t0.elapsed()), rx_frames, tx_frames, rx_bytes,
                clients = client_count(clients), since_rx = %format!("{:.0?}", last_rx.elapsed()), "stats"
            ),
            _ = stopped() => break Ok(Stop::User),
        }
    };
    info!(rx_frames, tx_frames, rx_bytes, "link session ended after {:.1?}", t0.elapsed());
    if link.restore_on_exit && outcome.is_ok() {
        if let Err(e) = restore(&mut wr, &rep).await {
            warn!("restore failed: {e}");
        }
    }
    outcome
}

// ── SPP (RFCOMM) ────────────────────────────────────────────────────────

async fn connect_via_profile(dev: &Device, handle: &mut Pin<Box<ProfileHandle>>) -> Result<RfcommStream> {
    let d = dev.clone();
    let mut connect = tokio::spawn(async move { d.connect_profile(&SPP_UUID).await });
    let mut connect_done = false;
    let deadline = sleep(Duration::from_secs(90));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            res = &mut connect, if !connect_done => {
                connect_done = true;
                match res.map_err(|e| anyhow!("join: {e}"))? {
                    Err(e) => bail!("ConnectProfile(SPP) failed: {e}"),
                    Ok(()) => info!("ConnectProfile(SPP) returned OK; waiting for NewConnection"),
                }
            }
            req = handle.next() => {
                let req = req.ok_or_else(|| anyhow!("profile handle closed"))?;
                info!(device = %req.device(), version = ?req.version(), features = ?req.features(), "NewConnection from BlueZ");
                if req.device() != dev.address() {
                    warn!("connection from an unexpected device; rejecting");
                    req.reject(ReqError::Rejected);
                    continue;
                }
                return req.accept().context("accepting the RFCOMM fd");
            }
            _ = &mut deadline => {
                connect.abort();
                bail!("timed out waiting for the RFCOMM connection");
            }
            _ = stopped() => {
                connect.abort();
                bail!("stopped by user while connecting");
            }
        }
    }
}

async fn connect(adapter: Option<&str>, addr: Address, channel: Option<u8>, no_auth: bool, link: LinkOpts) -> Result<()> {
    let session = Session::new().await?;
    let adapter = open_adapter(&session, adapter).await?;
    let dev = adapter.device(addr)?;
    info!(
        adapter = adapter.name(),
        adapter_addr = %adapter.address().await?,
        device = %addr,
        name = ?dev.name().await.ok().flatten(),
        paired = dev.is_paired().await.unwrap_or(false),
        trusted = dev.is_trusted().await.unwrap_or(false),
        connected = dev.is_connected().await.unwrap_or(false),
        "target"
    );
    if !dev.is_paired().await.unwrap_or(false) {
        warn!("{addr} isn't paired on {}; expect the connect to fail or to prompt", adapter.name());
    }
    let mut profile = None;
    if channel.is_none() {
        let p = Profile {
            uuid: SPP_UUID,
            name: Some("spp-spike".into()),
            role: Some(Role::Client),
            require_authentication: Some(!no_auth),
            require_authorization: Some(false),
            auto_connect: Some(false),
            ..Default::default()
        };
        let handle = session.register_profile(p).await.context("registering the SPP client profile")?;
        info!(require_authentication = !no_auth, "registered SPP client profile with BlueZ");
        profile = Some(Box::pin(handle));
    }
    let mut hub = Hub::new(link.listen).await?;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let t0 = Instant::now();
        let stream = match channel {
            Some(ch) => {
                info!(attempt, "raw RFCOMM connect to {addr} channel {ch}");
                RfcommStream::connect(RfcommAddr::new(addr, ch)).await.map_err(anyhow::Error::from)
            }
            None => {
                info!(attempt, "ConnectProfile(SPP) on {addr}");
                connect_via_profile(&dev, profile.as_mut().unwrap()).await
            }
        };
        match stream {
            Ok(stream) => {
                info!("connected in {:.2?}", t0.elapsed());
                if let Ok(sec) = stream.as_ref().security() {
                    info!(?sec, "rfcomm socket security");
                }
                match run_link(stream, &link, &hub.clients, &mut hub.uplink_rx).await {
                    Ok(Stop::User) | Ok(Stop::Done) => break,
                    Err(e) => error!("link dropped: {e:#}"),
                }
            }
            Err(e) => error!("connect failed after {:.2?}: {e:#}", t0.elapsed()),
        }
        if !link.forever {
            break;
        }
        info!("retrying in {}s (Ctrl-C to quit)", link.retry);
        tokio::select! {
            _ = sleep(Duration::from_secs(link.retry)) => {}
            _ = stopped() => break,
        }
    }
    if channel.is_none() {
        let _ = dev.disconnect_profile(&SPP_UUID).await;
    }
    Ok(())
}

// ── Serial ──────────────────────────────────────────────────────────────

async fn serial(path: String, baud: u32, link: LinkOpts) -> Result<()> {
    use tokio_serial::SerialPortBuilderExt;
    let mut hub = Hub::new(link.listen).await?;
    loop {
        let t0 = Instant::now();
        let port = tokio_serial::new(&path, baud)
            .data_bits(tokio_serial::DataBits::Eight)
            .parity(tokio_serial::Parity::None)
            .stop_bits(tokio_serial::StopBits::One)
            .flow_control(tokio_serial::FlowControl::None)
            .open_native_async();
        match port {
            Ok(mut port) => {
                if let Err(e) = port.set_exclusive(true) {
                    warn!("TIOCEXCL failed: {e}");
                }
                info!("opened {path} at {baud} baud in {:.2?}", t0.elapsed());
                match run_link(port, &link, &hub.clients, &mut hub.uplink_rx).await {
                    Ok(_) => break,
                    Err(e) => error!("link dropped: {e:#}"),
                }
            }
            Err(e) => error!("open {path} failed: {e}"),
        }
        if !link.forever {
            break;
        }
        info!("retrying in {}s (path exists: {})", link.retry, std::path::Path::new(&path).exists());
        tokio::select! {
            _ = sleep(Duration::from_secs(link.retry)) => {}
            _ = stopped() => break,
        }
    }
    Ok(())
}
