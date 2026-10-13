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

use std::path::{Path, PathBuf};

use clap::Parser;
use kiss_tnc_bridge::{config, error};
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_DIR: &str = "/etc/kiss-tnc-bridge";
const DEFAULT_STATE_DIR: &str = "/var/lib/kiss-tnc-bridge";
const DEFAULT_RUNTIME_DIR: &str = "/run/kiss-tnc-bridge";
const LOCK_FILE_NAME: &str = "kiss-tnc-bridge.lock";

#[derive(Parser)]
#[command(
    version,
    about = "KISS switchboard: bridges a KISS TNC to apps over BLE and TCP"
)]
struct Cli {
    /// Main config file; conf.d/ next to it is read too
    /// [default: $CONFIGURATION_DIRECTORY/kiss-tnc-bridge.toml, else /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml]
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Directory for state that persists across restarts
    /// [default: $STATE_DIRECTORY, else /var/lib/kiss-tnc-bridge]
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// Directory for the lock file and control socket
    /// [default: $RUNTIME_DIRECTORY, else /run/kiss-tnc-bridge]
    #[arg(long, value_name = "DIR")]
    runtime_dir: Option<PathBuf>,

    /// Check the config, print the effective bridges and exit (0 = valid, 1 = errors)
    #[arg(short = 't', long = "test-config")]
    test_config: bool,

    /// Increase log verbosity (-d = debug, -dd = trace)
    #[arg(short = 'd', long = "debug", action = clap::ArgAction::Count)]
    debug: u8,
}

/// A directory from a systemd `*Directory=` environment variable, else the
/// default. systemd may list several directories separated by ':'; the
/// first is ours.
fn dir_from_env(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .ok()
        .and_then(|v| v.split(':').next().map(str::to_string))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

/// Take the exclusive instance lock in the runtime directory. The returned
/// file must stay open for the lifetime of the process; the lock goes away
/// with it, including on crashes and SIGKILL.
fn acquire_instance_lock(runtime_dir: &Path) -> Result<std::fs::File, String> {
    use std::fs::File;

    std::fs::create_dir_all(runtime_dir).map_err(|e| {
        format!(
            "cannot create runtime directory '{}': {e}",
            runtime_dir.display()
        )
    })?;
    let lock_path = runtime_dir.join(LOCK_FILE_NAME);
    let file = File::create(&lock_path)
        .map_err(|e| format!("cannot create lock file '{}': {e}", lock_path.display()))?;

    // LOCK_EX = exclusive, LOCK_NB = non-blocking (fail immediately)
    let rc = unsafe {
        libc::flock(
            std::os::unix::io::AsRawFd::as_raw_fd(&file),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            return Err(format!(
                "another instance of kiss-tnc-bridge is already running (lock held on '{}')",
                lock_path.display()
            ));
        }
        return Err(format!(
            "failed to acquire lock '{}': {err}",
            lock_path.display()
        ));
    }

    Ok(file)
}

/// An adapter as the user named it: `dongle (AC:A7:…)` when an
/// `[[adapter]]` id covers the MAC, otherwise the MAC or hciN itself.
fn describe_adapter(cfg: &config::Config, sel: &config::AdapterSel) -> String {
    if let config::AdapterSel::Mac(mac) = sel
        && let Some(a) = cfg.adapters.iter().find(|a| a.mac == *mac)
    {
        return format!("{} ({mac})", a.id);
    }
    sel.to_string()
}

fn describe_port(cfg: &config::Config, p: &config::Port) -> String {
    match &p.kind {
        config::PortKind::TcpClient {
            host,
            port,
            connect,
            idle_timeout,
        } => {
            let mut s = format!("{host}:{port}");
            if *connect == config::ConnectPolicy::Always {
                s.push_str(", connect always");
            }
            if !idle_timeout.is_zero() {
                s.push_str(&format!(", idle timeout {}s", idle_timeout.as_secs()));
            }
            s
        }
        config::PortKind::TcpServer {
            listen,
            max_clients,
            allow,
            announce,
        } => {
            let mut s = format!("listen {listen}");
            if let Some(m) = max_clients {
                s.push_str(&format!(", max {m} clients"));
            }
            if !allow.is_empty() {
                s.push_str(&format!(
                    ", allow {}",
                    allow
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            if *announce {
                s.push_str(", announced");
            }
            s
        }
        config::PortKind::BleServer {
            adapter,
            max_clients,
        } => {
            let noun = if *max_clients == 1 {
                "client"
            } else {
                "clients"
            };
            format!(
                "{}, max {max_clients} {noun}",
                describe_adapter(cfg, adapter)
            )
        }
    }
}

fn print_summary(cfg: &config::Config, report: &config::Report) {
    println!(
        "config ok: {} bridge(s), {} adapter(s), {} warning(s)",
        cfg.bridges.len(),
        cfg.adapters.len(),
        report.warnings.len()
    );
    for a in &cfg.adapters {
        println!("  adapter {:<10} {}  ({})", a.id, a.mac, a.source);
    }
    for b in &cfg.bridges {
        println!(
            "  {:<10} {:<28} {:<8} ({})",
            b.id,
            format!("\"{}\"", b.name),
            if b.enabled { "enabled" } else { "disabled" },
            b.source
        );
        if b.public_name != b.name {
            println!("    public name \"{}\"", b.public_name);
        }
        for p in &b.ports {
            println!(
                "    {:<10} {:<11} {:<4} {}",
                p.id,
                p.type_name(),
                p.role,
                describe_port(cfg, p)
            );
        }
    }
    for w in &report.warnings {
        println!("warning: {w}");
    }
}

fn main() {
    let cli = Cli::parse();

    let config_file = cli.config.clone().unwrap_or_else(|| {
        dir_from_env("CONFIGURATION_DIRECTORY", DEFAULT_CONFIG_DIR)
            .join(config::load::MAIN_FILE_NAME)
    });
    let state_dir = cli
        .state_dir
        .clone()
        .unwrap_or_else(|| dir_from_env("STATE_DIRECTORY", DEFAULT_STATE_DIR));
    let runtime_dir = cli
        .runtime_dir
        .clone()
        .unwrap_or_else(|| dir_from_env("RUNTIME_DIRECTORY", DEFAULT_RUNTIME_DIR));

    let (cfg, report) = match config::load_all(&config_file, true) {
        Ok(x) => x,
        Err(problems) => {
            for p in &problems {
                eprintln!("error: {p}");
            }
            eprintln!("{}: {} error(s)", config_file.display(), problems.len());
            std::process::exit(1);
        }
    };

    if cli.test_config {
        print_summary(&cfg, &report);
        std::process::exit(0);
    }

    let _lock_file = match acquire_instance_lock(&runtime_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    let log_level = match cli.debug {
        0 => cfg.global.log_level.clone(),
        1 => "debug".into(),
        _ => "trace".into(),
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_new(&log_level).unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    tracing::info!(
        config = %config_file.display(),
        state_dir = %state_dir.display(),
        runtime_dir = %runtime_dir.display(),
        "kiss-tnc-bridge starting"
    );
    for w in &report.warnings {
        tracing::warn!("{w}");
    }
    for b in &cfg.bridges {
        tracing::info!(
            bridge = %b.id,
            name = %b.name,
            enabled = b.enabled,
            ports = b.ports.len(),
            source = %b.source,
            "configured bridge"
        );
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(async {
        if let Err(e) = run(cfg).await {
            tracing::error!("fatal: {e}");
            std::process::exit(1);
        }
    });
}

// The 0.5 adapter supervisor and bridge manager, driven from the new config
// until the Phase 1 bridge core, ports and reconciler replace them. Not worth
// polishing: it goes away whole.
#[allow(clippy::collapsible_if, clippy::map_entry, clippy::extend_with_drain)]
async fn run(cfg: config::Config) -> Result<(), error::Error> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = &cfg;
        tracing::error!(
            "BLE support requires Linux — this build can only test config and KISS/TCP layers"
        );
        Err(error::Error::Config("BLE support requires Linux".into()))
    }

    #[cfg(target_os = "linux")]
    {
        use std::collections::HashMap;
        use std::time::Duration;

        use futures::StreamExt;
        use kiss_tnc_bridge::ble::gatt;
        use kiss_tnc_bridge::bridge::manager::BridgeManager;
        use kiss_tnc_bridge::config::legacy::TncConfig;
        use kiss_tnc_bridge::config::{AdapterSel, PortKind};
        use tokio::sync::mpsc;

        // Events flowing from adapter monitors and session watcher into
        // the supervisor loop.
        enum SupervisorEvent {
            Added(String),
            Removed(String),
            PoweredOff(String),
            PoweredOn(String),
        }

        // GATT application + bridge manager tasks for one adapter.
        struct BridgeState {
            _app_handle: bluer::gatt::local::ApplicationHandle,
            tasks: Vec<tokio::task::JoinHandle<()>>,
        }

        impl BridgeState {
            fn shutdown(self) {
                drop(self._app_handle);
                for task in self.tasks {
                    task.abort();
                }
            }
        }

        // Power on an adapter, register the GATT application, and spawn
        // a BridgeManager task for each TNC.
        async fn setup_bridge(
            session: &bluer::Session,
            adapter_name: &str,
            tncs: &[TncConfig],
        ) -> Result<BridgeState, error::Error> {
            let adapter = session.adapter(adapter_name)?;
            adapter.set_powered(true).await.map_err(|e| {
                error::Error::Config(format!(
                    "failed to power on Bluetooth adapter '{}': {e}",
                    adapter_name
                ))
            })?;
            // The adapter must be pairable so that the "Just Works" bond
            // triggered when BlueZ probes a connected client's protected
            // characteristics can complete.  Combined with the
            // NoInputNoOutput agent, this pairs silently with no code
            // prompt.  If the adapter is not pairable, BlueZ rejects the
            // pairing with "Pairing not supported" and the client is
            // disconnected.
            adapter.set_pairable(true).await?;

            if let Some(first_tnc) = tncs.first() {
                adapter.set_alias(first_tnc.name.clone()).await?;
                tracing::info!(
                    adapter = adapter_name,
                    alias = first_tnc.name,
                    tnc_count = tncs.len(),
                    "BLE adapter powered on"
                );
            }

            let (app, tnc_handles) = gatt::build_application(tncs);
            let app_handle = adapter.serve_gatt_application(app).await?;
            tracing::info!(adapter = adapter_name, "GATT application registered");

            let mut tasks = Vec::new();
            for handle in tnc_handles {
                let tnc_name = handle.tnc_config.name.clone();
                let manager = BridgeManager::new(
                    handle.tnc_config,
                    adapter.clone(),
                    handle.tx_control,
                    handle.rx_control,
                );
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = manager.run().await {
                        tracing::error!(tnc = tnc_name, error = %e, "bridge manager failed");
                    }
                }));
            }

            Ok(BridgeState {
                _app_handle: app_handle,
                tasks,
            })
        }

        // Watch an adapter's Powered property and forward changes to the
        // supervisor.  The stream ends naturally when the D-Bus object
        // disappears (USB unplug), so this task is self-cleaning.
        fn spawn_adapter_monitor(
            adapter: bluer::Adapter,
            adapter_name: String,
            tx: mpsc::Sender<SupervisorEvent>,
        ) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                use bluer::{AdapterEvent, AdapterProperty};
                use futures::StreamExt as _;
                let Ok(events) = adapter.events().await else {
                    return;
                };
                tokio::pin!(events);
                while let Some(evt) = events.next().await {
                    let msg = match evt {
                        AdapterEvent::PropertyChanged(AdapterProperty::Powered(false)) => {
                            Some(SupervisorEvent::PoweredOff(adapter_name.clone()))
                        }
                        AdapterEvent::PropertyChanged(AdapterProperty::Powered(true)) => {
                            Some(SupervisorEvent::PoweredOn(adapter_name.clone()))
                        }
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            })
        }

        // ── Session and event channel ────────────────────────────────

        let session = bluer::Session::new().await?;

        // Register a NoInputNoOutput pairing agent so that any pairing
        // triggered by BlueZ (e.g. when it probes a connected central's
        // protected characteristics) uses the "Just Works" model instead
        // of numeric comparison.  Without this, BlueZ falls back to a
        // DisplayYesNo capability that prompts for a pairing code the
        // headless daemon can never confirm, causing clients to fail to
        // connect.  An Agent with no interaction callbacks advertises the
        // NoInputNoOutput capability.  The handle must be held for the
        // lifetime of the process.
        let _agent_handle = session
            .register_agent(bluer::agent::Agent {
                request_default: true,
                ..Default::default()
            })
            .await?;
        tracing::debug!("registered NoInputNoOutput pairing agent");

        let (sv_tx, mut sv_rx) = mpsc::channel::<SupervisorEvent>(32);

        // Forward session-level adapter add/remove events.
        let session_events = session.events().await?;
        let tx = sv_tx.clone();
        tokio::spawn(async move {
            use bluer::SessionEvent;
            tokio::pin!(session_events);
            while let Some(evt) = session_events.next().await {
                let msg = match evt {
                    SessionEvent::AdapterAdded(name) => SupervisorEvent::Added(name),
                    SessionEvent::AdapterRemoved(name) => SupervisorEvent::Removed(name),
                };
                if tx.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // ── Map bridges onto the 0.5 bridge manager ──────────────────
        // Temporary, until the Phase 1 bridge core and ports land: each
        // enabled bridge with a tcp-client TNC port becomes one 0.5 "TNC"
        // per ble-server port. tcp-server ports aren't served yet.

        let available = session.adapter_names().await.unwrap_or_default();
        let mut by_mac: HashMap<kiss_tnc_bridge::config::Mac, String> = HashMap::new();
        for name in &available {
            if let Ok(a) = session.adapter(name)
                && let Ok(addr) = a.address().await
            {
                by_mac.insert(kiss_tnc_bridge::config::Mac(addr.0), name.clone());
            }
        }
        let hci_for = |sel: &AdapterSel| -> Result<Option<String>, String> {
            match sel {
                AdapterSel::Default => Ok(None),
                AdapterSel::Hci(h) => Ok(Some(h.clone())),
                AdapterSel::Mac(m) => by_mac
                    .get(m)
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| format!("adapter {m} is not present")),
            }
        };

        let resolved_default: Option<String> = match hci_for(&cfg.global.adapter) {
            Ok(Some(name)) => {
                if !available.contains(&name) {
                    tracing::warn!(
                        adapter = %name,
                        available = ?available,
                        "configured Bluetooth adapter not found, waiting for it to appear"
                    );
                }
                Some(name)
            }
            Ok(None) => {
                if let Some(first) = available.first() {
                    tracing::debug!(adapter = %first, "using default Bluetooth adapter");
                }
                available.first().cloned()
            }
            Err(e) => {
                tracing::warn!("[global] adapter: {e}; using the default adapter");
                available.first().cloned()
            }
        };

        let mut tncs_by_adapter: HashMap<String, Vec<TncConfig>> = HashMap::new();
        let mut unresolved_tncs: Vec<TncConfig> = Vec::new();

        for b in cfg.bridges.iter().filter(|b| b.enabled) {
            let tnc_endpoint = b.tnc_ports().find_map(|p| match &p.kind {
                PortKind::TcpClient { host, port, .. } => Some((host.clone(), *port)),
                _ => None,
            });
            let Some((host, port)) = tnc_endpoint else {
                tracing::warn!(bridge = %b.id, "the TNC port isn't a tcp-client; not supported yet, skipping this bridge");
                continue;
            };
            for p in b.app_ports() {
                match &p.kind {
                    PortKind::BleServer {
                        adapter,
                        max_clients,
                    } => {
                        let tnc = TncConfig {
                            name: b.public_name.clone(),
                            host: host.clone(),
                            port,
                            max_clients: *max_clients as usize,
                            adapter: None,
                        };
                        match hci_for(adapter) {
                            Ok(Some(name)) => tncs_by_adapter.entry(name).or_default().push(tnc),
                            Ok(None) => match &resolved_default {
                                Some(name) => {
                                    tncs_by_adapter.entry(name.clone()).or_default().push(tnc)
                                }
                                None => unresolved_tncs.push(tnc),
                            },
                            Err(e) => {
                                tracing::warn!(bridge = %b.id, port = %p.id, "{e}; skipping this port");
                            }
                        }
                    }
                    PortKind::TcpServer { .. } => {
                        tracing::warn!(bridge = %b.id, port = %p.id, "tcp-server ports aren't implemented yet; skipping");
                    }
                    PortKind::TcpClient { .. } => {
                        tracing::warn!(bridge = %b.id, port = %p.id, "a tcp-client app port isn't implemented yet; skipping");
                    }
                }
            }
        }

        if !unresolved_tncs.is_empty() {
            tracing::warn!(
                "no Bluetooth adapter configured and none available, \
                 waiting for one to appear"
            );
        }

        // ── Initial setup ────────────────────────────────────────────

        let mut active_bridges: HashMap<String, BridgeState> = HashMap::new();
        let mut active_monitors: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

        for adapter_name in tncs_by_adapter.keys().cloned().collect::<Vec<_>>() {
            if !available.contains(&adapter_name) {
                tracing::warn!(
                    adapter = %adapter_name,
                    "Bluetooth adapter not available, waiting for it to appear"
                );
                continue;
            }

            // Spawn a power-state monitor for this adapter.
            if let Ok(adapter) = session.adapter(&adapter_name) {
                let monitor = spawn_adapter_monitor(adapter, adapter_name.clone(), sv_tx.clone());
                active_monitors.insert(adapter_name.clone(), monitor);
            }

            match setup_bridge(&session, &adapter_name, &tncs_by_adapter[&adapter_name]).await {
                Ok(state) => {
                    active_bridges.insert(adapter_name, state);
                }
                Err(e) => {
                    tracing::warn!(
                        adapter = %adapter_name,
                        error = %e,
                        "failed to set up adapter, will retry when available"
                    );
                }
            }
        }

        tracing::info!("daemon ready");

        // ── Supervisor loop ──────────────────────────────────────────

        loop {
            tokio::select! {
                event = sv_rx.recv() => {
                    match event {
                        Some(SupervisorEvent::Added(name)) => {
                            // Assign any unresolved TNCs to this adapter.
                            if !unresolved_tncs.is_empty()
                                && !tncs_by_adapter.contains_key(&name)
                            {
                                tracing::debug!(
                                    adapter = %name,
                                    "assigning TNCs to newly available adapter"
                                );
                                tncs_by_adapter
                                    .entry(name.clone())
                                    .or_default()
                                    .extend(unresolved_tncs.drain(..));
                            }

                            if let Some(tncs) = tncs_by_adapter.get(&name) {
                                // Start a power monitor if we don't have one.
                                if let std::collections::hash_map::Entry::Vacant(slot) =
                                    active_monitors.entry(name.clone())
                                    && let Ok(adapter) = session.adapter(&name)
                                {
                                    slot.insert(spawn_adapter_monitor(adapter, name.clone(), sv_tx.clone()));
                                }

                                if !active_bridges.contains_key(&name) {
                                    tracing::warn!(
                                        adapter = %name,
                                        "Bluetooth adapter appeared, resuming"
                                    );
                                    // Give BlueZ a moment to finish initialising.
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    match setup_bridge(&session, &name, tncs).await {
                                        Ok(state) => {
                                            active_bridges.insert(name, state);
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                adapter = %name,
                                                error = %e,
                                                "failed to set up adapter"
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        Some(SupervisorEvent::Removed(name)) => {
                            if let Some(state) = active_bridges.remove(&name) {
                                tracing::warn!(
                                    adapter = %name,
                                    "Bluetooth adapter removed, waiting for it to return"
                                );
                                state.shutdown();
                            }
                            // The monitor's event stream will end because the
                            // D-Bus object is gone, so the task exits on its
                            // own.
                            active_monitors.remove(&name);
                        }

                        Some(SupervisorEvent::PoweredOff(name)) => {
                            if let Some(state) = active_bridges.remove(&name) {
                                tracing::warn!(
                                    adapter = %name,
                                    "Bluetooth adapter powered off, \
                                     waiting for it to be re-enabled"
                                );
                                state.shutdown();
                            }
                            // Keep the monitor alive — it will tell us when
                            // the adapter powers back on.
                        }

                        Some(SupervisorEvent::PoweredOn(name)) => {
                            if let Some(tncs) = tncs_by_adapter.get(&name) {
                                if !active_bridges.contains_key(&name) {
                                    tracing::warn!(
                                        adapter = %name,
                                        "Bluetooth adapter powered on, resuming"
                                    );
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    match setup_bridge(&session, &name, tncs).await {
                                        Ok(state) => {
                                            active_bridges.insert(name, state);
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                adapter = %name,
                                                error = %e,
                                                "failed to set up adapter after power on"
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        None => {
                            tracing::error!(
                                "D-Bus event stream closed, shutting down"
                            );
                            break;
                        }
                    }
                }

                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}
