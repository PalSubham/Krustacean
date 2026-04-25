use const_format::concatcp;

use super::cap_bindings::{CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};

/// Required process capabilities
pub(super) const REQUIRED_CAPS: [u32; 2] = [CAP_NET_ADMIN, CAP_NET_BIND_SERVICE];

/// Log file name
pub(super) const LOG_FILE_NAME: &str = concatcp!(env!("CARGO_PKG_NAME"), ".log");

/// Config file name
pub(super) const CONFIG_FILE_NAME: &str = concatcp!(env!("CARGO_PKG_NAME"), ".json");
