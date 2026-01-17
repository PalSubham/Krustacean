// SPDX-License-Identifier: GPL-3.0-or-later

use core::result::Result;
use libc::{SYS_capget, syscall};
use log::LevelFilter;
use log4rs::{
    Handle,
    append::{console::ConsoleAppender, file::FileAppender},
    config::{Appender, Root, runtime::Config},
    encode::pattern::PatternEncoder,
    filter::threshold::ThresholdFilter,
    init_config,
};
use serde_json::from_str;
use std::{
    io::{Error, ErrorKind, Result as IoResult},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::LazyLock,
};
use tokio::fs::read_to_string;

use super::{
    bindings::{__user_cap_data_struct, __user_cap_header_struct, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE},
    structs::{Configs, LogError},
};

/// Metadata header to fetch process capabilities
static CAP_HEADER: LazyLock<__user_cap_header_struct> = LazyLock::new(__user_cap_header_struct::default);

/// Required process capabilities
const REQUIRED_CAPS: [u32; 2] = [CAP_NET_ADMIN, CAP_NET_BIND_SERVICE];

/// Checks if required capabilities are effective
#[inline(always)]
pub(crate) fn is_capable() -> IoResult<bool> {
    // A total of 64 capabilities are there
    // Each field of each __user_cap_data_struct holds 32 of them as u32 bitmap (Hence, two are used)
    // When enabled, the corresponding bit in that field is 1
    let mut data = <[__user_cap_data_struct; 2] as Default>::default();

    match unsafe { syscall(SYS_capget, &*CAP_HEADER as *const _, &mut data as *mut _) } {
        0 => Ok(REQUIRED_CAPS.iter().all(|&cap| {
            let idx = (cap / 32) as usize; // The __user_cap_data_struct which has this capability
            let bit = cap % 32; // The corresponding bit in bitmap for that capability

            (data[idx].effective & (1 << bit)) != 0 // Check if the capability bit is 1 in the effective field of that __user_cap_data_struct
        })),
        _ => Err(Error::last_os_error()),
    }
}

/// Read and parse configuration file
#[inline(always)]
pub(crate) async fn read_config(path: &PathBuf) -> IoResult<Configs> {
    if !path.exists() {
        return Err(Error::new(ErrorKind::NotFound, "Configuration file not found"));
    } else if !path.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Provided configuration path is not a file"));
    }

    from_str(&read_to_string(path).await?).map_err(|e| Error::new(ErrorKind::InvalidData, format!("Failed to deserialize configuration file - {e}")))
}

const LOG_FILE_NAME: &str = "Krustacean.log";

/// Enable logging based on provided optional log directory. If provided it logs to file, else falls back to console logging
#[inline(always)]
pub(crate) fn enable_logging(log_dir: Option<&PathBuf>) -> Result<Handle, LogError> {
    let config = match log_dir {
        Some(dir) => {
            if !dir.exists() {
                return Err(LogError::cause("Log directory not found"));
            } else if !dir.is_dir() {
                return Err(LogError::cause("Provided log directory is not a directory"));
            }

            let metadata = dir
                .metadata()
                .map_err(|_| LogError::cause("Failed to fetch log directory metadata"))?;
            let readonly = metadata.permissions().mode() & 0o200 == 0;
            if readonly {
                return Err(LogError::cause("Provided log directory is readonly for the user"));
            }

            let file = FileAppender::builder()
                .encoder(Box::new(PatternEncoder::default()))
                .build(dir.join(LOG_FILE_NAME))
                .map_err(|_| LogError::cause("Failed to create FileAppender"))?;

            Config::builder()
                .appender(
                    Appender::builder()
                        .filter(Box::new(ThresholdFilter::new(LevelFilter::Info)))
                        .build("file", Box::new(file)),
                )
                .build(Root::builder().appender("file").build(LevelFilter::max()))
                .map_err(|_| LogError::cause("Failed to create FileAppender log config"))?
        },
        None => {
            let console = ConsoleAppender::builder().build();

            Config::builder()
                .appender(
                    Appender::builder()
                        .filter(Box::new(ThresholdFilter::new(LevelFilter::Info)))
                        .build("console", Box::new(console)),
                )
                .build(
                    Root::builder()
                        .appender("console")
                        .build(LevelFilter::max()),
                )
                .map_err(|_| LogError::cause("Failed to create ConsoleAppender log config"))?
        },
    };

    Ok(init_config(config).map_err(|_| LogError::cause("Failed to create logger handle"))?)
}

/// Banner macro to log application banner with version
macro_rules! banner {
    ($file:literal) => {{
        let banner = ::const_format::str_replace!(::core::include_str!($file), "@project_version@", ::core::env!("CARGO_PKG_VERSION"));
        ::log::info!("{banner}");
    }};
}

pub(crate) use banner;
