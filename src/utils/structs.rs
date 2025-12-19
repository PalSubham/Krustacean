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
    pub(crate) filelog: bool,
    pub(crate) config: PathBuf,
}

impl Args {
    pub(crate) fn new() -> Result<Self, String> {
        let config = match env::var("CONFIG_FILE") {
            Ok(f) => PathBuf::from(f),
            Err(VarError::NotPresent) => return Err("Env variable \"CONFIG_FILE\" not found".into()),
            Err(VarError::NotUnicode(_)) => return Err("Non-unicode env variable \"CONFIG_FILE\"".into()),
        };

        let filelog = match env::var("FILE_LOG") {
            Ok(_) => true,
            Err(VarError::NotPresent) => false,
            Err(VarError::NotUnicode(_)) => return Err("Non-unicode env variable \"FILE_LOG\"".into()),
        };

        Ok(Self { filelog, config })
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
