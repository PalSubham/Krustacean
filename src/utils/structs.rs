// SPDX-License-Identifier: GPL-3.0-or-later

use core::{error::Error, fmt};
use serde::Deserialize;
use std::{
    env::{self, VarError},
    path::PathBuf,
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
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Configs {
    pub(crate) port: u16,
    pub(crate) udp: Vec<Forwarders>,
    pub(crate) tcp: Vec<Forwarders>,
}

/// Forwarder configuration structure
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Forwarders {
    pub(crate) upstream_ip: String,
    pub(crate) upstream_port: u16,
    pub(crate) orig_port: u16,
}
