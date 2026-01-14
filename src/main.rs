// SPDX-License-Identifier: GPL-3.0-or-later
/*!
 * Copyright (C) 2025 Subham Pal
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use log::{error, info, warn};
use sd_notify::NotifyState;
use std::{process::ExitCode, sync::Arc};
use tokio::{sync::{RwLock, watch}, task::JoinSet};

mod handlers;
mod utils;

use crate::{
    handlers::{
        forwarders::{tcp_forwarder, udp_forwarder},
        signal_handler::signal_handler,
    },
    utils::{
        structs::{Actions, Args},
        utils::{banner, enable_logging, is_capable, read_config},
    },
};

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

    let configs = match read_config(&args.config).await {
        Ok(c) => Arc::new(RwLock::new(c)),
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

    /*let udp_map = match configs
        .udp
        .into_iter()
        .map(|u| match Ipv4Addr::from_str(&u.upstream_ip) {
            Ok(ip) => Ok((u.orig_port, (ip, u.upstream_port))),
            Err(_) => {
                error!("Invalid upstream IP address for UDP: {}", u.upstream_ip);
                Err(())
            },
        })
        .collect::<Result<HashMap<_, _>, _>>()
    {
        Ok(map) => Arc::new(map),
        Err(_) => return ExitCode::FAILURE,
    };

    let tcp_map = match configs
        .tcp
        .into_iter()
        .map(|t| match Ipv4Addr::from_str(&t.upstream_ip) {
            Ok(ip) => Ok((t.orig_port, (ip, t.upstream_port))),
            Err(_) => {
                error!("Invalid upstream IP address for TCP: {}", t.upstream_ip);
                Err(())
            },
        })
        .collect::<Result<HashMap<_, _>, _>>()
    {
        Ok(map) => Arc::new(map),
        Err(_) => return ExitCode::FAILURE,
    };*/

    let (tx, rx) = {
        let cfg = configs.read().await;
        watch::channel(Actions::INIT(cfg.clone()))
    };
    let mut tasks = JoinSet::new();

    {
        let tx = tx.clone();
        let configs = configs.clone();
        let label = "Shutdown handler";

        tasks.spawn(async move {
            match signal_handler(tx, &args.config, configs).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    {
        let rx = rx.clone();
        let label = "UDP forwarder";

        tasks.spawn(async move {
            match udp_forwarder(rx).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    {
        let rx = rx.clone();
        let label = "TCP forwarder";

        tasks.spawn(async move {
            match tcp_forwarder(rx).await {
                Ok(_) => Ok(((), label)),
                Err(e) => Err((e, label)),
            }
        });
    }

    info!("Application started");

    if let Err(e) = sd_notify::notify(false, &[NotifyState::Ready]) {
        warn!("Systemd READY notify failed {e}");
    }

    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(Ok((_, l))) => info!("{} - exited cleanly", l),
            Ok(Err((e, l))) => error!("{} - error: {}", l, e),
            Err(e) => error!("Task join error: {}", e),
        }
    }

    info!("Application shutting down...");

    if let Err(e) = sd_notify::notify(false, &[NotifyState::Stopping]) {
        warn!("Systemd STOPPING notify failed {e}");
    }

    info!("Application shut down");
    return ExitCode::SUCCESS;
}
