use std::{process, sync::LazyLock};

use const_format::concatcp;

use super::cap_bindings::{__user_cap_header_struct, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};

/// Current PID
pub(crate) static PID: LazyLock<u32> = LazyLock::new(process::id);

/// Metadata header [`__user_cap_header_struct`] to fetch process capabilities
pub(super) static CAP_HEADER: LazyLock<__user_cap_header_struct> = LazyLock::new(Default::default);

/// Required process capabilities
pub(super) const REQUIRED_CAPS: [u32; 2] = [CAP_NET_ADMIN, CAP_NET_BIND_SERVICE];

/// Log file name
pub(super) const LOG_FILE_NAME: &str = concatcp!(env!("CARGO_PKG_NAME"), ".log");

/// Config file name
pub(super) const CONFIG_FILE_NAME: &str = concatcp!(env!("CARGO_PKG_NAME"), ".json");
