// SPDX-License-Identifier: GPL-3.0-or-later

use arc_swap::ArcSwap;
use log::{error, info, warn};
use sd_notify::{NotifyState, notify};
use std::{
    io::{Error, ErrorKind, Result},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    select,
    signal::unix::{SignalKind, signal},
    sync::watch::{Receiver, Sender},
};

use crate::utils::{
    structs::{Actions, RuntimeConfigs},
    utils::read_config,
};

use super::constants::LISTEN_IP;

/// Handles signals (SIGINT, SIGTERM, SIGQUIT & SIGHUP)
pub(crate) async fn signal_handler(
    tx: Sender<Actions>, mut rx: Receiver<Actions>, config_path: &PathBuf, current_config: Arc<ArcSwap<RuntimeConfigs>>,
) -> Result<()> {
    info!("Signal handler starting...");

    let action = rx.borrow().clone();
    match action {
        Actions::STOP(s) => {
            info!("Signal handler shut down before starting as {s} failed");
            return Ok(());
        },
        Actions::PANICKED => {
            info!("Signal handler shut down before starting as someone panicked");
            return Ok(());
        },
        _ => { /* At most INIT may come, which is to be ignored */ },
    };

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to set up SIGINT handler: {}", e);
            return Err(Error::new(ErrorKind::Other, "SIGINT handling failure"));
        },
    };

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to set up SIGTERM handler: {}", e);
            return Err(Error::new(ErrorKind::Other, "SIGTERM handling failure"));
        },
    };

    let mut sigquit = match signal(SignalKind::quit()) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to set up SIGQUIT handler: {}", e);
            return Err(Error::new(ErrorKind::Other, "SIGQUIT handling failure"));
        },
    };

    let mut sighup = match signal(SignalKind::hangup()) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to set up SIGHUP handler: {}", e);
            return Err(Error::new(ErrorKind::Other, "SIGHUP handling failure"));
        },
    };

    'signal_handler_loop: loop {
        select! {
            sig = rx.changed() => {
                match sig {
                    Ok(_) => {
                        let action = rx.borrow().clone();
                        match action {
                            Actions::STOP(s) => {
                                info!("{s} failed...Shutting down Signal handler...");
                                break 'signal_handler_loop;
                            },
                            Actions::PANICKED => {
                                info!("Someone panicked...Shutting down Signal handler...");
                                break 'signal_handler_loop;
                            },
                            _ => {
                                /* At most RELOAD may come which is to be ignored */
                                continue 'signal_handler_loop;
                            }
                        }
                    },
                    Err(_) => {
                        error!("Signal channel closed...Shutting down Signal handler...");
                        break 'signal_handler_loop;
                    }
                };
            }

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

                let clock_monotonic = match NotifyState::monotonic_usec_now() {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Reload aborted due to failure in determining CLOCK_MONOTONIC - {e}");
                        continue 'signal_handler_loop;
                    }
                };

                if let Err(e) = notify(false, &[NotifyState::Reloading, clock_monotonic]) {
                    warn!("Systemd RELOADING & MONOTONIC_USEC notify failed - {e}");
                }

                match read_config(config_path).await {
                    Ok(new_file_config) => {
                        let new_config = RuntimeConfigs::from(&new_file_config);

                        let (needs_update, port_changed) = {
                            let old_cfg = current_config.load();
                            (**old_cfg != new_config, old_cfg.port != new_config.port)
                        };

                        if needs_update {
                            current_config.store(Arc::new(new_config));
                            tx.send_replace(Actions::RELOAD(port_changed));
                        } else {
                            info!("Configuration unchanged");
                        }
                    },
                    Err(e) => error!("{e}")
                };

                if let Err(e) = notify(false, &[NotifyState::Ready]) {
                    warn!("Systemd READY notify failed after reload - {e}");
                }

                if let Err(e) = notify(
                    false,
                    &[NotifyState::Status(
                        &format!("Configured to listen at {}:{}", LISTEN_IP, current_config.load().port)
                    )]
                ) {
                    warn!("Systemd STATUS notify failed - {e}");
                }

                continue 'signal_handler_loop;
            },
        }
    }

    info!("Signal handler shut down");
    Ok(())
}
