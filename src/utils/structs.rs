// SPDX-License-Identifier: GPL-3.0-or-later

use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    env::{self, VarError},
    error::Error,
    fmt,
    net::Ipv4Addr,
    path::PathBuf,
    sync::Arc,
};

/// Logging error structure
#[derive(Debug)]
pub(crate) struct LogError {
    pub(self) details: String,
}

impl LogError {
    pub(super) fn cause(details: &str) -> Self {
        LogError { details: details.into() }
    }
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Logging error: {}", self.details)
    }
}

impl Error for LogError {}

/// Env variable arguments structure
pub(crate) struct Args {
    pub(crate) config: PathBuf,
    pub(crate) logdir: Option<PathBuf>,
}

impl Args {
    pub(crate) fn new() -> Result<Self, String> {
        let config = match env::var("CONFIG_FILE") {
            Ok(f) => PathBuf::from(f),
            Err(VarError::NotPresent) => return Err("Env variable \"CONFIG_FILE\" not found".into()),
            Err(VarError::NotUnicode(_)) => return Err("Non-unicode env variable \"CONFIG_FILE\"".into()),
        };

        let logdir = match env::var("LOGS_DIRECTORY") {
            Ok(l) => Some(PathBuf::from(l)),
            Err(VarError::NotPresent) => None,
            Err(VarError::NotUnicode(_)) => return Err("Non-unicode env variable \"LOGS_DIRECTORY\"".into()),
        };

        Ok(Self { config, logdir })
    }
}

/// Application configuration structure
#[derive(Debug, Deserialize, Clone, Eq, PartialEq)]
pub(crate) struct Configs {
    pub(super) port: u16,
    pub(super) udp: HashSet<Forwarders>,
    pub(super) tcp: HashSet<Forwarders>,
}

/// Forwarder configuration structure
#[derive(Debug, Deserialize, Clone, Eq, PartialEq, Hash)]
pub(super) struct Forwarders {
    pub(super) upstream_ip: Ipv4Addr,
    pub(super) upstream_port: u16,
    pub(super) orig_port: u16,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RuntimeConfigs {
    pub(crate) port: u16,
    pub(crate) udp_map: Arc<UdpMap>,
    pub(crate) tcp_map: Arc<TcpMap>,
}

impl From<&Configs> for RuntimeConfigs {
    fn from(cfg: &Configs) -> Self {
        Self {
            port: cfg.port,
            udp_map: Arc::new(UdpMap(
                cfg.udp
                    .iter()
                    .map(|u| (u.orig_port, (u.upstream_ip, u.upstream_port)))
                    .collect(),
            )),
            tcp_map: Arc::new(TcpMap(
                cfg.tcp
                    .iter()
                    .map(|u| (u.orig_port, (u.upstream_ip, u.upstream_port)))
                    .collect(),
            )),
        }
    }
}

pub(crate) trait ForwarderMap {
    fn get(&self, k: &u16) -> Option<&(Ipv4Addr, u16)>;
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TcpMap(HashMap<u16, (Ipv4Addr, u16)>);

impl ForwarderMap for TcpMap {
    fn get(&self, k: &u16) -> Option<&(Ipv4Addr, u16)> {
        self.0.get(k)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UdpMap(HashMap<u16, (Ipv4Addr, u16)>);

impl ForwarderMap for UdpMap {
    fn get(&self, k: &u16) -> Option<&(Ipv4Addr, u16)> {
        self.0.get(k)
    }
}

#[derive(Clone)]
pub(crate) enum Actions {
    INIT,
    RELOAD(bool),
    KILL,
    SHUTDOWN,
    STOP(&'static str),
    PANICKED,
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;

    macro_rules! hashset {
        ($($value:expr,)+) => { hashset!($($value),+) };
        ($($value:expr),*) => {
            {
                let cap = <[()]>::len(&[$({ let _ = &$value; () }),*]);
                let mut set = std::collections::HashSet::with_capacity(cap);
                $(
                    set.insert($value);
                )*
                set
            }
        };
    }

    macro_rules! hashmap {
        ($($key:expr => $value:expr,)+) => { hashmap!($($key => $value),+) };
        ($($key:expr => $value:expr),*) => {
            {
                let cap = <[()]>::len(&[$({ let _ = &$key; () }),*]);
                let mut map = std::collections::HashMap::with_capacity(cap);
                $(
                    map.insert($key, $value);
                )*
                map
            }
        };
    }

    #[test]
    fn test_LogError_cause() {
        let msg = String::from("err");
        let log_err = LogError::cause(&msg);
        assert_eq!(msg, log_err.details);
    }

    #[test]
    fn test_LogError_display() {
        let msg = String::from("err");
        let log_err = LogError { details: msg.clone() };
        assert_eq!(format!("Logging error: {msg}"), log_err.to_string());
    }

    #[test]
    #[serial(env)]
    fn test_Args_new() {
        let dir = tempdir().unwrap();
        let dir_str = dir.path().as_os_str();

        let mut args = Args::new();
        assert!(args.is_err());

        unsafe {
            env::set_var("CONFIG_FILE", dir_str);
        };
        args = Args::new();
        assert!(args.is_ok());
        assert_eq!(dir_str, args.as_ref().unwrap().config.as_os_str());
        assert!(args.as_ref().unwrap().logdir.is_none());

        unsafe {
            env::set_var("LOGS_DIRECTORY", dir_str);
        };
        args = Args::new();
        assert!(args.is_ok());
        assert!(args.as_ref().unwrap().logdir.is_some());
        assert_eq!(dir_str, args.as_ref().unwrap().logdir.as_ref().unwrap().as_os_str());

        unsafe {
            env::remove_var("CONFIG_FILE");
            env::remove_var("LOGS_DIRECTORY");
        }
    }

    #[test]
    fn test_RuntimeConfigs_from() {
        let ip = Ipv4Addr::from([10, 0, 0, 1]);
        let inner_port = 53u16;
        let outer_port = 8080u16;

        let configs = Configs {
            port: outer_port,
            udp: hashset! {
                Forwarders {
                    upstream_ip: ip,
                    upstream_port: inner_port,
                    orig_port: inner_port
                }
            },
            tcp: hashset! {
                Forwarders {
                    upstream_ip: ip,
                    upstream_port: inner_port,
                    orig_port: inner_port
                }
            },
        };

        let runtime_configs = RuntimeConfigs::from(&configs);
        assert_eq!(outer_port, runtime_configs.port);
        assert_eq!(
            hashmap! {
                inner_port => (ip, inner_port)
            },
            runtime_configs.tcp_map.0
        );
        assert_eq!(
            hashmap! {
                inner_port => (ip, inner_port)
            },
            runtime_configs.udp_map.0
        );
    }

    #[test]
    fn test_ForwarderMap_get() {
        let ip = Ipv4Addr::from([10, 0, 0, 1]);
        let port = 53u16;
        let no_port = 123u16;
        let map = hashmap! {
            port => (ip, port)
        };

        let tcp_map = TcpMap(map.clone());
        assert_eq!(Some(&(ip, port)), tcp_map.get(&port));
        assert_eq!(None, tcp_map.get(&no_port));

        let udp_map = UdpMap(map.clone());
        assert_eq!(Some(&(ip, port)), udp_map.get(&port));
        assert_eq!(None, udp_map.get(&no_port));
    }
}
