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

use crate::error::Error;
use configparser::ini::Ini;

#[derive(Debug, Clone)]
pub struct Config {
    pub global: GlobalConfig,
    pub tncs: Vec<TncConfig>,
}

#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub log_level: String,
    pub adapter: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TncConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub max_clients: usize,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Error> {
        let mut ini = Ini::new_cs();
        ini.load(path).map_err(|e| Error::Config(e.to_string()))?;

        let global = Self::parse_global(&ini);
        let tncs = Self::parse_tncs(&ini)?;

        if tncs.is_empty() {
            return Err(Error::Config("no TNC sections defined".into()));
        }

        Ok(Config { global, tncs })
    }

    fn parse_global(ini: &Ini) -> GlobalConfig {
        GlobalConfig {
            log_level: ini
                .get("global", "log_level")
                .unwrap_or_else(|| "info".into()),
            adapter: ini.get("global", "adapter"),
        }
    }

    fn parse_tncs(ini: &Ini) -> Result<Vec<TncConfig>, Error> {
        let mut tncs = Vec::new();

        for section in ini.sections() {
            if section == "global" || section == "DEFAULT" {
                continue;
            }

            let host = ini
                .get(&section, "host")
                .ok_or_else(|| Error::Config(format!("[{section}]: missing 'host'")))?;

            let port_str = ini
                .get(&section, "port")
                .ok_or_else(|| Error::Config(format!("[{section}]: missing 'port'")))?;
            let port: u16 = port_str
                .parse()
                .map_err(|_| Error::Config(format!("[{section}]: invalid port '{port_str}'")))?;

            let max_clients: usize = ini
                .get(&section, "max_clients")
                .unwrap_or_else(|| "3".into())
                .parse()
                .map_err(|_| Error::Config(format!("[{section}]: invalid max_clients")))?;

            if max_clients == 0 {
                return Err(Error::Config(format!(
                    "[{section}]: max_clients must be >= 1"
                )));
            }

            tncs.push(TncConfig {
                name: section,
                host,
                port,
                max_clients,
            });
        }

        Ok(tncs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parse_minimal_config() {
        let f = write_config(
            "\
[APRS iGate]
host = 127.0.0.1
port = 8001
",
        );
        let cfg = Config::load(f.path().to_str().unwrap()).unwrap();
        assert_eq!(cfg.tncs.len(), 1);
        assert_eq!(cfg.tncs[0].name, "APRS iGate");
        assert_eq!(cfg.tncs[0].host, "127.0.0.1");
        assert_eq!(cfg.tncs[0].port, 8001);
        assert_eq!(cfg.tncs[0].max_clients, 3);
        assert_eq!(cfg.global.log_level, "info");
        assert!(cfg.global.adapter.is_none());
    }

    #[test]
    fn parse_full_config() {
        let f = write_config(
            "\
[global]
log_level = debug
adapter = hci1

[APRS iGate]
host = 127.0.0.1
port = 8001
max_clients = 5

[Winlink TNC]
host = 192.168.1.50
port = 8100
max_clients = 2
",
        );
        let cfg = Config::load(f.path().to_str().unwrap()).unwrap();
        assert_eq!(cfg.global.log_level, "debug");
        assert_eq!(cfg.global.adapter.as_deref(), Some("hci1"));
        assert_eq!(cfg.tncs.len(), 2);
    }

    #[test]
    fn missing_host_errors() {
        let f = write_config(
            "\
[Bad TNC]
port = 8001
",
        );
        let err = Config::load(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("missing 'host'"));
    }

    #[test]
    fn missing_port_errors() {
        let f = write_config(
            "\
[Bad TNC]
host = 127.0.0.1
",
        );
        let err = Config::load(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("missing 'port'"));
    }

    #[test]
    fn no_tnc_sections_errors() {
        let f = write_config(
            "\
[global]
log_level = info
",
        );
        let err = Config::load(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("no TNC sections"));
    }

    #[test]
    fn zero_max_clients_errors() {
        let f = write_config(
            "\
[Bad TNC]
host = 127.0.0.1
port = 8001
max_clients = 0
",
        );
        let err = Config::load(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("max_clients must be >= 1"));
    }
}
