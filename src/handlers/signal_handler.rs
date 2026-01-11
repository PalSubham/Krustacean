// SPDX-License-Identifier: GPL-3.0-or-later

use log::{error, info};
use std::{io::{Error, ErrorKind, Result}, path::PathBuf, sync::Arc};
use tokio::{select, signal::unix::{SignalKind, signal}, sync::{RwLock, watch::Sender}};

use crate::utils::{structs::{Actions, Configs}, utils::read_config};

/// Handles signals (SIGINT, SIGTERM, SIGQUIT & SIGHUP)
pub(crate) async fn signal_handler(tx: Sender<Actions>, config_path: &PathBuf, current_config: Arc<RwLock<Configs>>) -> Result<()> {
    info!("Signal handler starting...");

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(signal) => {
            info!("SIGINT handler ready");
            signal
        },
        Err(e) => {
            error!("Failed to set up SIGINT handler: {}", e);
            tx.send_replace(Actions::KILL);
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
            tx.send_replace(Actions::KILL);
            return Err(Error::new(ErrorKind::Other, "SIGTERM handling failure"));
        },
    };

    let mut sigquit = match signal(SignalKind::quit()) {
        Ok(signal) => {
            info!("SIGQUIT handler ready");
            signal
        },
        Err(e) => {
            error!("Failed to set up SIGQUIT handler: {}", e);
            tx.send_replace(Actions::KILL);
            return Err(Error::new(ErrorKind::Other, "SIGQUIT handling failure"));
        },
    };

    let mut sighup = match signal(SignalKind::hangup()) {
        Ok(signal) => {
            info!("SIGHUP handler ready");
            signal
        },
        Err(e) => {
            error!("Failed to set up SIGHUP handler: {}", e);
            tx.send_replace(Actions::KILL);
            return Err(Error::new(ErrorKind::Other, "SIGHUP handling failure"));
        },
    };

    'signal_handler_loop: loop {
        select! {
            biased;

            _ = sigquit.recv() =>  {
                info!("Received SIGQUIT");
                tx.send_replace(Actions::KILL);
                break 'signal_handler_loop;
            },
            _ = sigint.recv() => {
                info!("Received SIGINT");
                tx.send_replace(Actions::SHUTDOWN);
                break 'signal_handler_loop;
            },
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
                tx.send_replace(Actions::SHUTDOWN);
                break 'signal_handler_loop;
            },
            _ = sighup.recv() => {
                info!("Received SIGHUP");

                match read_config(config_path).await {
                    Ok(new_config) => {
                        let needs_update = {
                            let cfg = current_config.read().await;
                            *cfg != new_config
                        };

                        if needs_update {
                            let mut cfg = current_config.write().await;
                            tx.send_replace(Actions::RELOAD(new_config.clone()));
                            *cfg = new_config;
                        }
                    },
                    Err(e) => error!("{e}")
                };

                continue 'signal_handler_loop;
            },
        }
    }

    info!("Signal handler shut down");
    Ok(())
}
