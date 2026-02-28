// SPDX-License-Identifier: GPL-3.0-or-later

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
};
use tokio::fs::read_to_string;

use super::{
    cap_bindings::{__user_cap_data_struct, cap_to_index, cap_to_mask},
    constants::{CAP_HEADER, LOG_FILE_NAME, REQUIRED_CAPS},
    structs::{Configs, LogError},
};

/// Checks if required capabilities are effective
///
/// * A total of 64 capabilities are there
/// * Each field of each [`__user_cap_data_struct`] holds 32 of them as u32 bitmap (Hence, two are used)
/// * When enabled, the corresponding bit in that field is 1
/// * Here we are using [`__user_cap_data_struct::effective`] for our purpose
pub(crate) fn is_capable() -> IoResult<bool> {
    let mut data = <[__user_cap_data_struct; 2] as Default>::default();

    match unsafe { syscall(SYS_capget, &*CAP_HEADER as *const _, &mut data as *mut _) } {
        0 => Ok(REQUIRED_CAPS
            .iter()
            .all(|&cap| (data[cap_to_index(cap)].effective & cap_to_mask(cap)) != 0)),
        _ => Err(Error::last_os_error()),
    }
}

/// Read and parse configuration file
pub(crate) async fn read_config(path: &PathBuf) -> IoResult<Configs> {
    if !path.exists() {
        return Err(Error::new(ErrorKind::NotFound, "Configuration file not found"));
    } else if !path.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Provided configuration path is not a file"));
    }

    from_str(&read_to_string(path).await?).map_err(|e| Error::new(ErrorKind::InvalidData, format!("Failed to deserialize configuration file - {e}")))
}

/// Enable logging based on provided optional log directory. If provided it logs to file, else falls back to console logging
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
    ($file:literal) => {
        #[cfg(not(test))]
        {
            let pid_string = (*$crate::utils::constants::PID).to_string();
            let banner = ::const_format::str_replace!(::std::include_str!($file), "@project_version@", ::std::env!("CARGO_PKG_VERSION"))
                .replace("@pid@", &pid_string);
            ::log::info!("{banner}");
        }
        #[cfg(test)]
        {}
    };
}

pub(crate) use banner;

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use serde_json::json;
    use tempfile::tempdir;
    use tokio::fs::{File, write};

    use super::*;

    #[tokio::test]
    async fn test_read_config() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // non-existent file
        let file_path_nonexistent = dir_path.join("nonexistent_config.conf");
        assert!(!file_path_nonexistent.exists());
        let mut result = read_config(&file_path_nonexistent).await;
        assert!(result.is_err());
        assert_eq!(ErrorKind::NotFound, result.unwrap_err().kind());

        // not a file
        result = read_config(&dir_path).await;
        assert!(result.is_err());
        assert_eq!(ErrorKind::InvalidInput, result.unwrap_err().kind());

        // using actual file
        let file_path = dir_path.join("config.conf");
        File::create(file_path.clone()).await.unwrap();
        assert!(file_path.exists());

        write(&file_path, b"abcd").await.unwrap();
        result = read_config(&file_path).await;
        assert!(result.is_err());
        assert_eq!(ErrorKind::InvalidData, result.unwrap_err().kind());

        let conf = json!({
            "port": 8080,
            "udp": [{
                "upstream_ip": "10.0.0.1",
                "upstream_port": 53,
                "orig_port": 53
            }],
            "tcp": [{
                "upstream_ip": "10.0.0.1",
                "upstream_port": 53,
                "orig_port": 53
            }]
        });
        write(&file_path, serde_json::to_string(&conf).unwrap())
            .await
            .unwrap();
        result = read_config(&file_path).await;
        assert!(result.is_ok());
    }
}
