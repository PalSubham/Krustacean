// SPDX-License-Identifier: GPL-3.0-or-later

use core::convert::Into;
use log::{error, info, warn};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::{
    io::Result,
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    select,
    sync::{RwLock, Semaphore, TryAcquireError, watch::Receiver},
    task::JoinSet,
    time::{Duration, timeout},
};

use crate::utils::structs::{Actions, ForwarderMap};

use super::helpers::{recvfrom_cmsg_async, CONN_BACKLOG, DRAIN_DURATION, create_tcp_listener, create_udp_socket_fd};

const CONN_TIMEOUT: Duration = Duration::from_secs(2u64);
const BUFFER_SIZE: usize = 4096;

/// UDP forwarder function
pub(crate) async fn udp_forwarder(mut rx: Receiver<Actions>) -> Result<()> {
    info!("UDP forwarder starting...");

    let action = rx.borrow().clone();
    let (udp_map, mut port, mut udp_fd) = match action {
        Actions::INIT(c) | Actions::RELOAD(c) => {
            (Arc::new(RwLock::new(c.udp_config())), c.port, create_udp_socket_fd(c.port)?)
        },
        Actions::STOP(s) => {
            info!("UDP forwarder shut down before starting as {s} failed");
            return Ok(());
        },
        Actions::PANICKED => {
            info!("UDP forwarder shut down before starting as someone panicked");
            return Ok(());
        },
        Actions::KILL | Actions::SHUTDOWN => {
            info!("TCP forwarder shut down before starting");
            return Ok(());
        }
    };

    let semaphore = Arc::new(Semaphore::new(CONN_BACKLOG as usize));
    let mut tasks = JoinSet::new();
    let mut force_kill = false;
    let mut buf = [0u8; BUFFER_SIZE];

    'udp_forwarder_loop: loop {
        select! {
            sig = rx.changed() => {
                match sig {
                    Ok(_) => {
                        let action = rx.borrow().clone();
                        match action {
                            Actions::RELOAD(c) => {
                                info!("RELOAD signal received by UDP forwarder...");

                                let mut map = udp_map.write().await;
                                *map = c.udp_config();
                                
                                if c.port != port {
                                    match create_udp_socket_fd(c.port) {
                                        Ok(f) => {
                                            udp_fd = f;
                                            port = c.port;
                                        },
                                        Err(e) => {
                                            error!("{e}");
                                            continue 'udp_forwarder_loop;
                                        }
                                    }
                                }
                            },
                            Actions::STOP(s) => {
                                info!("{s} failed...Shutting down UDP forwarder...");
                                break 'udp_forwarder_loop;
                            },
                            Actions::KILL => {
                                info!("KILL signal received...Killing UDP forwarder...");
                                force_kill = true;
                                break 'udp_forwarder_loop;
                            },
                            Actions::PANICKED => {
                                info!("Someone panicked...Killing UDP forwarder...");
                                force_kill = true;
                                break 'udp_forwarder_loop;
                            },
                            Actions::SHUTDOWN => {
                                info!("SHUTDOWN signal received...Shutting down UDP forwarder...");
                                break 'udp_forwarder_loop;
                            },
                            Actions::INIT(_) => {/* INIT will not come here */}
                        }
                    },
                    Err(_) => {
                        error!("Signal channel closed...Shutting down UDP forwarder...");
                        break 'udp_forwarder_loop;
                    }
                };
            }

            result = udp_fd.readable() => {
                let mut guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        error!("AsyncFd error: {e}");
                        continue 'udp_forwarder_loop;
                    }
                };

                let recv_res = recvfrom_cmsg_async(&udp_fd, &mut buf).await;

                guard.clear_ready();

                if let Some((src, len, orig_dst)) = recv_res {
                    match semaphore.clone().try_acquire_owned() {
                        Ok(p) => {
                            let packet = buf[..len].to_vec();
                            let udp_map = udp_map.clone();

                            tasks.spawn(async move {
                                let _permit = p; // hold acquired permit

                                let orig_dst_addr = orig_dst.ip();
                                let orig_dst_port = orig_dst.port();
                                info!("UDP intercepted for {orig_dst_addr}:{orig_dst_port} from {src}");

                                let proxy = {
                                    let map = udp_map.read().await;
                                    map.get(&orig_dst_port).cloned()
                                };

                                match proxy {
                                    Some(proxy) => {
                                        match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await {
                                            Ok(upstream_socket) => {
                                                if let Err(e) = upstream_socket.send_to(&packet, proxy).await {
                                                    error!("Failed to send UDP datagram to upstream {}:{} - {e}", proxy.0, proxy.1);
                                                    return;
                                                }

                                                let mut reply_buf = [0u8; BUFFER_SIZE];

                                                match timeout(CONN_TIMEOUT, upstream_socket.recv_from(&mut reply_buf)).await {
                                                    Ok(Ok((reply_len, _))) => {
                                                        match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
                                                            Ok(reply_socket) => {
                                                                if let Err(e) = reply_socket.set_reuse_address(true) {
                                                                    error!("Failed to set reuse address on UDP reply socket - {e}");
                                                                    return;
                                                                }

                                                                if let Err(e) = reply_socket.set_reuse_port(true){
                                                                    error!("Failed to set reuse port on UDP reply socket - {e}");
                                                                    return;
                                                                }

                                                                if let Err(e) = reply_socket.set_ip_transparent_v4(true) {
                                                                    error!("Failed to set IP transparent on UDP reply socket - {e}");
                                                                    return;
                                                                }

                                                                if let Err(e) = reply_socket.set_nonblocking(true) {
                                                                    error!("Failed to set non-blocking on UDP reply socket - {e}");
                                                                    return;
                                                                }

                                                                if let Err(e) = reply_socket.bind(&SocketAddrV4::new(*orig_dst_addr, orig_dst_port).into()) {
                                                                    error!("Failed to bind UDP reply socket to original destination {}:{} - {e}", orig_dst_addr, orig_dst_port);
                                                                    return;
                                                                }

                                                                match UdpSocket::from_std(reply_socket.into()) {
                                                                    Ok(reply_udp) => {
                                                                        match reply_udp.send_to(&reply_buf[..reply_len], src).await {
                                                                            Ok(_) => {
                                                                                info!("UDP reply forwarded back to client {}", src);
                                                                            },
                                                                            Err(e) => {
                                                                                error!("Failed to forward UDP reply back to client {} - {e}", src);
                                                                            }
                                                                        };

                                                                        return;
                                                                    },
                                                                    Err(e) => {
                                                                        error!("Failed to create UDP socket from std for reply - {e}");
                                                                        return;
                                                                    }
                                                                };
                                                            },
                                                            Err(e) => {
                                                                error!("Failed to create UDP socket for reply - {e}");
                                                                return;
                                                            }
                                                        };
                                                    },
                                                    Ok(Err(e)) => {
                                                        error!("Failed to receive UDP datagram from upstream {}:{} - {e}", proxy.0, proxy.1);
                                                        return;
                                                    },
                                                    Err(_) => {
                                                        error!("Timed out while trying to receive UDP datagram from upstream {}:{}", proxy.0, proxy.1);
                                                        return;
                                                    }
                                                };
                                            },
                                            Err(e) => {
                                                error!("Failed to create and bind upstream UDP socket {e}");
                                                return;
                                            }
                                        };
                                    },
                                    None => {
                                        warn!("No upstream mapping provided for destination UDP port {orig_dst_port}");
                                        return;
                                    }
                                };
                            });
                        },
                        Err(e) => match e {
                            TryAcquireError::Closed => {
                                error!("UDP forwarder backlog semaphore is closed");
                            },
                            TryAcquireError::NoPermits => {
                                warn!("UDP forwarder is busy, dropping packets...");
                            }
                        }
                    };
                }
            }
        }

        // draining
        while tasks.try_join_next().is_some() {}
    }

    if force_kill {
        tasks.abort_all();
    }

    info!("UDP forwarder is waiting for tasks to finish...");

    let drain = async {
        (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {})
    };

    if timeout(DRAIN_DURATION, drain).await.is_err() {
        warn!("Forced exit in UDP forwarder: tasks didn't complete in time");
    }

    info!("UDP forwarder shut down");
    Ok(())
}

/// TCP forwarder function
pub(crate) async fn tcp_forwarder(mut rx: Receiver<Actions>) -> Result<()> {
    info!("TCP forwarder starting...");

    let action = rx.borrow().clone();
    let (tcp_map, mut port, mut listener) = match action {
        Actions::INIT(c) | Actions::RELOAD(c) => {
            (Arc::new(RwLock::new(c.tcp_config())), c.port, create_tcp_listener(c.port)?)
        },
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
        }
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
                            Actions::RELOAD(c) => {
                                info!("RELOAD signal received by UDP forwarder...");

                                let mut map = tcp_map.write().await;
                                *map = c.tcp_config();
                                
                                if c.port != port {
                                    match create_tcp_listener(c.port) {
                                        Ok(l) => {
                                            listener = l;
                                            port = c.port;
                                        },
                                        Err(e) => {
                                            error!("{e}");
                                            continue 'tcp_forwarder_loop;
                                        }
                                    }
                                }
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
                            Actions::INIT(_) => {/* INIT will not come here */}
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
                                    info!("TCP intercepted for {}:{} from {}", orig_dst_addr, orig_dst_port, src);

                                    let proxy = {
                                        let map = tcp_map.read().await;
                                        map.get(&orig_dst_port).cloned()
                                    };

                                    match proxy {
                                        Some(proxy) => {
                                            match timeout(CONN_TIMEOUT, TcpStream::connect(proxy)).await {
                                                Ok(Ok(mut upstream_conn)) => {
                                                    let mut buf = [0u8; BUFFER_SIZE];

                                                    match client.read(&mut buf).await {
                                                        Ok(len) => {
                                                            if let Err(e) = upstream_conn.write_all(&buf[..len]).await {
                                                                error!("Failed to forward TCP to upstream {}:{} - {e}", proxy.0, proxy.1);
                                                                return;
                                                            };

                                                            match timeout(CONN_TIMEOUT, upstream_conn.read(&mut buf)).await {
                                                                Ok(Ok(reply_len)) => {
                                                                    match client.write_all(&buf[..reply_len]).await {
                                                                        Ok(_) => {
                                                                            info!("TCP reply forwarded back to client {}", src);
                                                                        },
                                                                        Err(e) => {
                                                                            error!("Failed to forward TCP reply back to client {} - {e}", src);
                                                                        }
                                                                    };
                                                                },
                                                                Ok(Err(e)) => {
                                                                    error!("Failed to read TCP reply from upstream {}:{} - {e}", proxy.0, proxy.1);
                                                                },
                                                                Err(_) => {
                                                                    error!("Timed out while trying to read TCP reply from upstream {}:{}", proxy.0, proxy.1);
                                                                }
                                                            };
                                                        },
                                                        Err(e) => {
                                                            error!("Failed to read from TCP client {} - {e}", src);
                                                        }
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

    let drain = async {
        (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {})
    };

    if timeout(DRAIN_DURATION, drain).await.is_err() {
        warn!("Forced exit in TCP forwarder: tasks didn't complete in time");
    }

    info!("TCP forwarder shut down");
    Ok(())
}
