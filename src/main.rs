// SPDX-License-Identifier: GPL-3.0-or-later
/*!
 * Copyright (C) 2025 Subham Pal
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use arc_swap::ArcSwap;
use log::{error, info, warn};
use sd_notify::NotifyState;
use std::{process::ExitCode, sync::Arc};
use tokio::{sync::watch, task::JoinSet};

mod handlers;
mod utils;

use crate::{
    handlers::{
        forwarders::{tcp_forwarder, udp_forwarder},
        signal_handler::signal_handler,
    },
    utils::{
        structs::{Actions, Args, RuntimeConfigs},
        utils::{banner, enable_logging, is_capable, read_config},
    },
};

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> ExitCode {
    let capable = match is_capable() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        },
    };

    if !capable {
        eprintln!("Both CAP_NET_ADMIN & CAP_NET_BIND_SERVICE need to be effective");
        return ExitCode::FAILURE;
    }

    let args = match Args::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        },
    };

    let _handle = match enable_logging(args.logdir.as_ref()) {
        Ok(handle) => handle,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        },
    };

    banner!("banner.txt");

    info!("Application starting...");

    let configs = match read_config(&args.config).await {
        Ok(c) => Arc::new(ArcSwap::from_pointee(RuntimeConfigs::from(&c))),
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        },
    };

    let (tx, rx) = watch::channel(Actions::INIT);
    let mut tasks = JoinSet::new();

    {
        let tx = tx.clone();
        let rx = rx.clone();
        let configs = configs.clone();
        let label = "Shutdown handler";

        tasks.spawn(async move {
            match signal_handler(tx.clone(), rx, &args.config, configs).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    {
        let rx = rx.clone();
        let configs = configs.clone();
        let label = "UDP forwarder";

        tasks.spawn(async move {
            match udp_forwarder(rx, configs).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    {
        let rx = rx.clone();
        let configs = configs.clone();
        let label = "TCP forwarder";

        tasks.spawn(async move {
            match tcp_forwarder(rx, configs).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    info!("Application started");

    if let Err(e) = sd_notify::notify(false, &[NotifyState::Ready]) {
        warn!("Systemd READY notify failed {e}");
    }

    let mut stopping = false;
    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(Ok((_, l))) => info!("{l} - exited cleanly"),
            Ok(Err((e, l))) => {
                if !stopping {
                    stopping = true;
                    tx.send_replace(Actions::STOP(l));
                }

                error!("{l} - error: {e}");
            },
            Err(e) => {
                if !stopping {
                    stopping = true;
                    tx.send_replace(Actions::PANICKED);
                }

                error!("Task join error: {e}");
            },
        };
    }

    info!("Application shutting down...");

    if let Err(e) = sd_notify::notify(false, &[NotifyState::Stopping]) {
        warn!("Systemd STOPPING notify failed {e}");
    }

    info!("Application shut down");
    ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
compile_error!("This program is only supported in Linux!");
