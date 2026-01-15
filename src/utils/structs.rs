// SPDX-License-Identifier: GPL-3.0-or-later

use core::{error::Error, fmt};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet}, env::{self, VarError}, net::Ipv4Addr, path::PathBuf, sync::Arc
};

/// Logging error structure
#[derive(Debug)]
pub(crate) struct LogError {
    details: String,
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
#[derive(Deserialize, Clone, Eq, PartialEq)]
pub(crate) struct Configs {
    pub(super) port: u16,
    pub(super) udp: HashSet<Forwarders>,
    pub(super) tcp: HashSet<Forwarders>,
}

/// Forwarder configuration structure
#[derive(Deserialize, Clone, Eq, PartialEq, Hash)]
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
                cfg
                    .udp
                    .iter()
                    .map(|u| (u.orig_port, (u.upstream_ip, u.upstream_port)))
                    .collect()
                )),
            tcp_map: Arc::new(TcpMap(
                cfg
                    .tcp
                    .iter()
                    .map(|u| (u.orig_port, (u.upstream_ip, u.upstream_port)))
                    .collect()
                ))
        }
    }
}

pub(crate) trait ForwarderMap {
    fn get(&self, k: &u16) -> Option<&(Ipv4Addr, u16)>;
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TcpMap(HashMap<u16, (Ipv4Addr, u16)>);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UdpMap(HashMap<u16, (Ipv4Addr, u16)>);

impl ForwarderMap for TcpMap {
    fn get(&self, k: &u16) -> Option<&(Ipv4Addr, u16)> {
        self.0.get(k)
    }
}

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
    PANICKED
}
