use core::result::Result;
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

use super::structs::{Configs, LogError};

/// Read and parse configuration file
#[inline(always)]
pub(crate) async fn read_config(path: &PathBuf) -> IoResult<Configs> {
    if !path.exists() {
        return Err(Error::new(ErrorKind::NotFound, "Configuration file not found"));
    } else if !path.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Provided configuration path is not a file"));
    }

    from_str(&read_to_string(path).await?).map_err(|e| {
        Error::new(ErrorKind::InvalidData, format!("Failed to deserialize configuration file - {e}"))
    })
}

const LOG_FILE_NAME: &str = "Krustacean.log";
const LOG_DIR: &str = "/var/log/Krustacean";

/// Enable logging based on configuration
#[inline(always)]
pub(crate) async fn enable_logging(file_logging: bool) -> Result<Handle, LogError> {
    let config = match file_logging {
        true => {
            let dir = PathBuf::from(LOG_DIR);

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

        false => {
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
        let banner = ::core::include_str!($file);
        let final_banner = banner.replace("@project_version@", ::core::env!("CARGO_PKG_VERSION"));
        ::log::info!("{}", final_banner);
    }};
}

pub(crate) use banner;
