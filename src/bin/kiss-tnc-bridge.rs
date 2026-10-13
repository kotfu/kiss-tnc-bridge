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

/// `connect always`, `idle timeout 90s`: only what differs from the defaults.
fn describe_policy(connect: config::ConnectPolicy, idle_timeout: std::time::Duration) -> String {
    let mut s = String::new();
    if connect == config::ConnectPolicy::Always {
        s.push_str(", connect always");
    }
    if !idle_timeout.is_zero() {
        s.push_str(&format!(", idle timeout {}s", idle_timeout.as_secs()));
    }
    s
}

fn describe_spp(cfg: &config::Config, spp: &config::Spp) -> String {
    let mut s = format!("{} on {}", spp.address, cfg.describe_adapter(&spp.adapter));
    if let Some(ch) = spp.channel {
        s.push_str(&format!(" channel {ch}"));
    }
    s
}

fn describe_port(cfg: &config::Config, p: &config::Port) -> String {
    match &p.kind {
        config::PortKind::TcpClient {
            host,
            port,
            connect,
            idle_timeout,
        } => format!("{host}:{port}{}", describe_policy(*connect, *idle_timeout)),
        config::PortKind::SppClient {
            tnc,
            connect,
            idle_timeout,
            exit_kiss_on_disconnect,
            ..
        }
        | config::PortKind::Serial {
            tnc,
            connect,
            idle_timeout,
            exit_kiss_on_disconnect,
            ..
        } => {
            let via = if matches!(p.kind, config::PortKind::Serial { .. }) {
                "serial"
            } else {
                "Bluetooth"
            };
            let mut s = format!(
                "{tnc} over {via}{}",
                describe_policy(*connect, *idle_timeout)
            );
            if *exit_kiss_on_disconnect {
                s.push_str(", exit KISS on disconnect");
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
                cfg.describe_adapter(adapter)
            )
        }
    }
}

fn print_summary(cfg: &config::Config, report: &config::Report) {
    println!(
        "config ok: {} bridge(s), {} tnc(s), {} adapter(s), {} warning(s)",
        cfg.bridges.len(),
        cfg.tncs.len(),
        cfg.adapters.len(),
        report.warnings.len()
    );
    for a in &cfg.adapters {
        println!("  adapter {:<10} {}  ({})", a.id, a.mac, a.source);
    }
    for t in &cfg.tncs {
        println!(
            "  tnc {:<14} {:<28} model {}  ({})",
            t.id,
            format!("\"{}\"", t.name),
            t.model,
            t.source
        );
        if let Some(spp) = &t.spp {
            println!("    spp     {}", describe_spp(cfg, spp));
        }
        if let Some(serial) = &t.serial {
            println!("    serial  {} at {} baud", serial.path, serial.baud);
        }
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
                "    {:<12} {:<11} {:<4} {}",
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
        let mut report = report;
        let notes = bluetooth_host_checks(&cfg, &mut report);
        print_summary(&cfg, &report);
        for n in notes {
            println!("note: {n}");
        }
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
        if let Err(e) = run(cfg, &state_dir).await {
            tracing::error!("fatal: {e}");
            std::process::exit(1);
        }
    });
}

/// `-t` host checks that need BlueZ: whether TNCs are paired. Problems go
/// in the report as warnings; a check that couldn't run comes back as a
/// note, since `-t` often runs where there's no Bluetooth (CI, image builds).
#[cfg(target_os = "linux")]
fn bluetooth_host_checks(cfg: &config::Config, report: &mut config::Report) -> Vec<String> {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return vec![format!("skipped the Bluetooth pairing checks: {e}")],
    };
    let checks = rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            kiss_tnc_bridge::bluetooth::pairing_warnings(cfg),
        )
        .await
    });
    match checks {
        Ok(Ok(warnings)) => {
            report.warnings.extend(warnings);
            Vec::new()
        }
        Ok(Err(e)) => vec![format!("skipped the Bluetooth pairing checks: {e}")],
        Err(_) => vec!["skipped the Bluetooth pairing checks: BlueZ didn't answer".into()],
    }
}

#[cfg(not(target_os = "linux"))]
fn bluetooth_host_checks(cfg: &config::Config, _report: &mut config::Report) -> Vec<String> {
    let spp = cfg
        .bridges
        .iter()
        .filter(|b| b.enabled)
        .flat_map(|b| &b.ports)
        .any(|p| matches!(p.kind, config::PortKind::SppClient { .. }));
    if spp {
        vec!["skipped the Bluetooth pairing checks: they need BlueZ on Linux".into()]
    } else {
        Vec::new()
    }
}

/// Run the supervisor until SIGINT or SIGTERM.
async fn run(cfg: config::Config, state_dir: &Path) -> Result<(), error::Error> {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => tracing::info!("interrupted, shutting down"),
                _ = term.recv() => tracing::info!("terminated, shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
            tracing::info!("interrupted, shutting down");
        }
        stop.cancel();
    });
    kiss_tnc_bridge::supervisor::run(cfg, state_dir, shutdown).await
}
