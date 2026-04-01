// SPDX-License-Identifier: GPL-3.0-or-later

use arc_swap::ArcSwap;
use log::{debug, error, info, warn};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::{io, net::SocketAddrV4, sync::Arc};
use tokio::{
    io::copy_bidirectional_with_sizes,
    net::{TcpListener, TcpStream},
    select,
    sync::watch::Receiver,
    task::JoinSet,
    time::timeout,
};

use super::{
    super::utils::structs::{Actions, RuntimeConfigs},
    constants::{BUFFER_SIZE, CONN_BACKLOG, CONN_TIMEOUT, DRAIN_DURATION, LISTEN_IP},
};

/// TCP forwarder function
pub(in super::super) async fn tcp_forwarder(mut rx: Receiver<Actions>, current_config: Arc<ArcSwap<RuntimeConfigs>>) -> io::Result<()> {
    info!("TCP forwarder starting...");

    let action = rx.borrow().clone();
    match action {
        Actions::STOP(s) => {
            info!("TCP forwarder shut down before starting as {s} failed");
            return Ok(());
        },
        Actions::PANICKED => {
            info!("TCP forwarder shut down before starting as someone panicked");
            return Ok(());
        },
        Actions::KILL | Actions::SHUTDOWN => {
            info!("TCP forwarder shut down before starting");
            return Ok(());
        },
        _ => { /* RELOAD or INIT has no effect now */ },
    };

    let (mut tcp_map, mut listener) = {
        let config = current_config.load();
        (config.tcp_map.clone(), create_tcp_listener(config.port)?)
    };
    let mut tasks = JoinSet::new();
    let mut force_kill = false;

    'tcp_forwarder_loop: loop {
        select! {
            sig = rx.changed() => {
                match sig {
                    Ok(_) => {
                        let action = rx.borrow().clone();
                        match action {
                            Actions::RELOAD(port_changed) => {
                                info!("RELOAD signal received by TCP forwarder...");

                                let config = current_config.load();
                                if port_changed {
                                    match create_tcp_listener(config.port) {
                                        Ok(l) => {
                                            listener = l;
                                            tcp_map = config.tcp_map.clone();
                                        },
                                        Err(e) => error!("{e}")
                                    };
                                } else {
                                    tcp_map = config.tcp_map.clone();
                                }

                                continue 'tcp_forwarder_loop;
                            },
                            Actions::STOP(s) => {
                                info!("{s} failed...Shutting down TCP forwarder...");
                                break 'tcp_forwarder_loop;
                            },
                            Actions::KILL => {
                                info!("KILL signal received...Killing TCP forwarder...");
                                force_kill = true;
                                break 'tcp_forwarder_loop;
                            },
                            Actions::PANICKED => {
                                info!("Someone panicked...Killing TCP forwarder...");
                                force_kill = true;
                                break 'tcp_forwarder_loop;
                            },
                            Actions::SHUTDOWN => {
                                info!("SHUTDOWN signal received...Shutting down TCP forwarder...");
                                break 'tcp_forwarder_loop;
                            },
                            _ => {/* INIT will not come here */}
                        }
                    },
                    Err(_) => {
                        error!("Signal channel closed...Shutting down TCP forwarder...");
                        break 'tcp_forwarder_loop;
                    }
                };
            }

            result = listener.accept() => {
                match result {
                    Ok((mut client, src)) => {
                        let tcp_map = tcp_map.clone();

                        tasks.spawn(async move {
                            let orig_dst = SockRef::from(&client).original_dst_v4().map(|o| o.as_socket_ipv4());

                            match orig_dst {
                                Ok(Some(orig)) => {
                                    let orig_dst_addr = orig.ip();
                                    let orig_dst_port = orig.port();
                                    debug!("TCP intercepted for {}:{} from {}", orig_dst_addr, orig_dst_port, src);

                                    match tcp_map.get(&orig_dst_port) {
                                        Some(proxy) => {
                                            match timeout(CONN_TIMEOUT, TcpStream::connect(proxy)).await {
                                                Ok(Ok(mut upstream_conn)) => {
                                                    match copy_bidirectional_with_sizes(&mut client, &mut upstream_conn, BUFFER_SIZE, BUFFER_SIZE).await {
                                                        Ok((from_client, from_upstream)) => {
                                                            debug!("TCP session completed: {from_client} bytes from client, {from_upstream} bytes from upstream");
                                                        },
                                                        Err(e) => {
                                                            error!("TCP forwarding error between client {} and upstream {}:{} - {e}", src, proxy.0, proxy.1);
                                                        },
                                                    };
                                                },
                                                Ok(Err(e)) => {
                                                    error!("Failed to connect to upstream {}:{} - {e}", proxy.0, proxy.1);
                                                },
                                                Err(_) => {
                                                    error!("Timed out while trying to connect to upstream {}:{}", proxy.0, proxy.1);
                                                }
                                            };
                                        },
                                        None => {
                                            warn!("No upstream mapping found for destination TCP port {}", orig_dst_port);
                                        }
                                    };
                                },
                                _ => {
                                    error!("Failed to get original destination for TCP connection from {}", src);
                                }
                            };
                        });
                    },
                    Err(e) => {
                        error!("Error accepting TCP connection - {e}");
                    }
                };
            }
        }

        // draining
        while tasks.try_join_next().is_some() {}
    }

    if force_kill {
        tasks.abort_all();
    }

    info!("TCP forwarder is waiting for tasks to finish...");
    if timeout(DRAIN_DURATION, async {
        (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {})
    })
    .await
    .is_err()
    {
        warn!("Forced exit in TCP forwarder: tasks didn't complete in time");
    }

    info!("TCP forwarder shut down");
    Ok(())
}

fn create_tcp_listener(port: u16) -> io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(LISTEN_IP, port).into())?;
    socket.listen(CONN_BACKLOG as i32)?;
    TcpListener::from_std(socket.into())
}
