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

#[cfg(target_os = "linux")]
mod ble;
mod bridge;
mod config;
mod error;
mod kiss;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Bridge BLE KISS TNCs to TCP KISS TNCs")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "/etc/kiss-tnc-bridge.conf")]
    config: String,

    /// Parse the config file and exit (0 = valid, 1 = error)
    #[arg(short = 't', long = "test-config")]
    test_config: bool,

    /// Increase log verbosity (-d = debug, -dd = trace)
    #[arg(short = 'd', long = "debug", action = clap::ArgAction::Count)]
    debug: u8,
}

fn main() {
    let cli = Cli::parse();

    let cfg = match config::Config::load(&cli.config) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if cli.test_config {
        println!("config ok: {} TNC(s) defined", cfg.tncs.len());
        for tnc in &cfg.tncs {
            println!(
                "  [{}] {}:{} (max {} clients)",
                tnc.name, tnc.host, tnc.port, tnc.max_clients
            );
        }
        std::process::exit(0);
    }

    let log_level = match cli.debug {
        0 => cfg.global.log_level.clone(),
        1 => "debug".into(),
        _ => "trace".into(),
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_new(&log_level).unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("kiss-tnc-bridge starting");
    for tnc in &cfg.tncs {
        tracing::info!(
            name = tnc.name,
            host = tnc.host,
            port = tnc.port,
            max_clients = tnc.max_clients,
            "configured TNC"
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

async fn run(_cfg: config::Config) -> Result<(), error::Error> {
    #[cfg(not(target_os = "linux"))]
    {
        tracing::error!("BLE support requires Linux — this build can only test config and KISS/TCP layers");
        return Err(error::Error::Config(
            "BLE support requires Linux".into(),
        ));
    }

    #[cfg(target_os = "linux")]
    {
        use std::collections::HashMap;
        use std::time::Duration;

        use crate::ble::gatt;
        use crate::bridge::manager::BridgeManager;
        use futures::StreamExt;
        use tokio::sync::mpsc;

        // Events flowing from adapter monitors and session watcher into
        // the supervisor loop.
        enum SupervisorEvent {
            AdapterAdded(String),
            AdapterRemoved(String),
            AdapterPoweredOff(String),
            AdapterPoweredOn(String),
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
            tncs: &[config::TncConfig],
        ) -> Result<BridgeState, error::Error> {
            let adapter = session.adapter(adapter_name)?;
            adapter.set_powered(true).await.map_err(|e| {
                error::Error::Config(format!(
                    "failed to power on Bluetooth adapter '{}': {e}",
                    adapter_name
                ))
            })?;
            adapter.set_pairable(false).await?;

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
                            Some(SupervisorEvent::AdapterPoweredOff(adapter_name.clone()))
                        }
                        AdapterEvent::PropertyChanged(AdapterProperty::Powered(true)) => {
                            Some(SupervisorEvent::AdapterPoweredOn(adapter_name.clone()))
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
        let (sv_tx, mut sv_rx) = mpsc::channel::<SupervisorEvent>(32);

        // Forward session-level adapter add/remove events.
        let session_events = session.events().await?;
        let tx = sv_tx.clone();
        tokio::spawn(async move {
            use bluer::SessionEvent;
            tokio::pin!(session_events);
            while let Some(evt) = session_events.next().await {
                let msg = match evt {
                    SessionEvent::AdapterAdded(name) => SupervisorEvent::AdapterAdded(name),
                    SessionEvent::AdapterRemoved(name) => SupervisorEvent::AdapterRemoved(name),
                };
                if tx.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // ── Resolve adapter names ────────────────────────────────────

        let available = session.adapter_names().await.unwrap_or_default();

        let resolved_default: Option<String> = if let Some(ref name) = _cfg.global.adapter {
            if !available.is_empty() && !available.contains(name) {
                tracing::warn!(
                    adapter = %name,
                    available = ?available,
                    "configured Bluetooth adapter not found, waiting for it to appear"
                );
            }
            Some(name.clone())
        } else if !available.is_empty() {
            tracing::debug!(adapter = %available[0], "using default Bluetooth adapter");
            Some(available[0].clone())
        } else {
            None
        };

        // Group TNCs by their effective adapter.  TNCs whose adapter
        // cannot be resolved yet (no adapter configured and none
        // available) go into a pending list and get assigned to the
        // first adapter that appears.
        let mut tncs_by_adapter: HashMap<String, Vec<config::TncConfig>> = HashMap::new();
        let mut unresolved_tncs: Vec<config::TncConfig> = Vec::new();

        for tnc in &_cfg.tncs {
            match tnc.adapter.as_ref().or(resolved_default.as_ref()) {
                Some(name) => {
                    tncs_by_adapter
                        .entry(name.clone())
                        .or_default()
                        .push(tnc.clone());
                }
                None => {
                    unresolved_tncs.push(tnc.clone());
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
                let monitor =
                    spawn_adapter_monitor(adapter, adapter_name.clone(), sv_tx.clone());
                active_monitors.insert(adapter_name.clone(), monitor);
            }

            match setup_bridge(&session, &adapter_name, &tncs_by_adapter[&adapter_name])
                .await
            {
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
                        Some(SupervisorEvent::AdapterAdded(name)) => {
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
                                if !active_monitors.contains_key(&name) {
                                    if let Ok(adapter) = session.adapter(&name) {
                                        let monitor = spawn_adapter_monitor(
                                            adapter,
                                            name.clone(),
                                            sv_tx.clone(),
                                        );
                                        active_monitors.insert(name.clone(), monitor);
                                    }
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

                        Some(SupervisorEvent::AdapterRemoved(name)) => {
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

                        Some(SupervisorEvent::AdapterPoweredOff(name)) => {
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

                        Some(SupervisorEvent::AdapterPoweredOn(name)) => {
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
