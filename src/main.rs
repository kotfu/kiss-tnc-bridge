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

        use crate::ble::gatt;
        use crate::bridge::manager::BridgeManager;

        let session = bluer::Session::new().await?;

        // Discover available Bluetooth adapters
        let available = session.adapter_names().await?;
        if available.is_empty() {
            return Err(error::Error::Config(
                "no Bluetooth adapters found — is a Bluetooth device connected and is BlueZ running?".into(),
            ));
        }

        // Resolve the default adapter (global config or first available)
        let default_adapter = if let Some(ref name) = _cfg.global.adapter {
            if !available.contains(name) {
                return Err(error::Error::Config(format!(
                    "Bluetooth adapter '{}' not found (available: {})",
                    name,
                    available.join(", ")
                )));
            }
            name.clone()
        } else {
            tracing::debug!(adapter = %available[0], "using default Bluetooth adapter");
            available[0].clone()
        };

        // Group TNCs by their effective adapter (per-TNC overrides global)
        let mut tncs_by_adapter: HashMap<String, Vec<config::TncConfig>> = HashMap::new();
        for tnc in &_cfg.tncs {
            let effective = tnc.adapter.as_ref().unwrap_or(&default_adapter);
            if !available.contains(effective) {
                return Err(error::Error::Config(format!(
                    "[{}]: Bluetooth adapter '{}' not found (available: {})",
                    tnc.name,
                    effective,
                    available.join(", ")
                )));
            }
            tncs_by_adapter
                .entry(effective.clone())
                .or_default()
                .push(tnc.clone());
        }

        // Set up each adapter with its TNCs
        let mut tasks = Vec::new();
        let mut app_handles = Vec::new();

        for (adapter_name, tncs) in &tncs_by_adapter {
            let adapter = session.adapter(adapter_name)?;
            adapter.set_powered(true).await.map_err(|e| {
                error::Error::Config(format!(
                    "failed to power on Bluetooth adapter '{}': {e}",
                    adapter_name
                ))
            })?;
            adapter.set_pairable(false).await?;

            // Set the adapter alias so connected clients see a TNC name
            // instead of the system hostname.
            if let Some(first_tnc) = tncs.first() {
                adapter.set_alias(first_tnc.name.clone()).await?;
                tracing::info!(
                    adapter = %adapter_name,
                    alias = first_tnc.name,
                    tnc_count = tncs.len(),
                    "BLE adapter powered on"
                );
            }

            let (app, tnc_handles) = gatt::build_application(tncs);
            let app_handle = adapter.serve_gatt_application(app).await?;
            app_handles.push(app_handle);
            tracing::info!(adapter = %adapter_name, "GATT application registered");

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
        }

        tracing::info!("daemon ready");
        tokio::signal::ctrl_c().await?;
        tracing::info!("shutting down");

        drop(app_handles);

        Ok(())
    }
}
