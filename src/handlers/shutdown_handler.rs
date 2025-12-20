// SPDX-License-Identifier: GPL-3.0-or-later

use log::{error, info};
use std::{
    io::{Error, ErrorKind, Result},
    sync::Arc,
};
use tokio::signal::unix::{SignalKind, signal};
use tokio::{select, sync::Notify};

/// Handle shutdown signals (SIGINT, SIGTERM)
pub(crate) async fn shutdown_handler(shutdown: Arc<Notify>) -> Result<()> {
    info!("Shutdown handler starting...");

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(signal) => {
            info!("SIGINT handler ready");
            signal
        },
        Err(e) => {
            error!("Failed to set up SIGINT handler: {}", e);
            shutdown.notify_waiters();
            return Err(Error::new(ErrorKind::Other, "SIGINT handling failure"));
        },
    };

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(signal) => {
            info!("SIGTERM handler ready");
            signal
        },
        Err(e) => {
            error!("Failed to set up SIGTERM handler: {}", e);
            shutdown.notify_waiters();
            return Err(Error::new(ErrorKind::Other, "SIGTERM handling failure"));
        },
    };

    select! {
        _ = sigint.recv() => info!("Received SIGINT, initiating shutdown..."),

        _ = sigterm.recv() => info!("Received SIGTERM, initiating shutdown...")
    }

    shutdown.notify_waiters();

    info!("Shutdown handler shut down");
    Ok(())
}
