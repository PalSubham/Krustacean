use core::convert::Into;
use log::{error, info, warn};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::{
    collections::HashMap,
    io::Result,
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, unix::AsyncFd},
    net::{TcpListener, TcpStream, UdpSocket},
    select,
    sync::{Notify, Semaphore, TryAcquireError},
    task::JoinSet,
    time::{Duration, timeout},
};

use super::helpers::{ExtendedSocket, recvfrom_cmsg_async};

const CONN_BACKLOG: u32 = 100;
const CONN_TIMEOUT: Duration = Duration::from_secs(2u64);
const BUFFER_SIZE: usize = 4096;
const LISTEN_IP: [u8; 4] = [127, 0, 0, 2];

/// UDP forwarder function
pub(crate) async fn udp_forwarder(udp_map: Arc<HashMap<u16, (Ipv4Addr, u16)>>, local_port: u16, shutdown: Arc<Notify>) -> Result<()> {
    info!("UDP forwarder starting...");

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_recv_orig_dst_addr(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::from(LISTEN_IP), local_port).into())?;
    let udp_fd = AsyncFd::new(socket)?;

    let semaphore = Arc::new(Semaphore::new(CONN_BACKLOG as usize));
    let mut tasks = JoinSet::new();
    let mut buf = [0u8; BUFFER_SIZE];

    'udp_forwarder_loop: loop {
        select! {
            biased;

            _ = shutdown.notified() => {
                info!("Shutting down UDP forwarder...");
                break 'udp_forwarder_loop;
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
                                let _permit = p;

                                let orig_dst_addr = *orig_dst.ip();
                                let orig_dst_port = orig_dst.port();
                                info!("UDP intercepted for {orig_dst_addr}:{orig_dst_port} from {src}");

                                match udp_map.get(&orig_dst_port) {
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

                                                                if let Err(e) = reply_socket.bind(&SocketAddrV4::new(orig_dst_addr, orig_dst_port).into()) {
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

    info!("UDP forwarder is waiting for tasks to finish...");
    (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {});

    info!("UDP forwarder shut down");
    Ok(())
}

/// TCP forwarder function
pub(crate) async fn tcp_forwarder(tcp_map: Arc<HashMap<u16, (Ipv4Addr, u16)>>, local_port: u16, shutdown: Arc<Notify>) -> Result<()> {
    info!("TCP forwarder starting...");

    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::from(LISTEN_IP), local_port).into())?;
    socket.listen(CONN_BACKLOG as i32)?;
    let listener = TcpListener::from_std(socket.into())?;

    let mut tasks = JoinSet::new();

    'main_loop: loop {
        select! {
            biased;

            _ = shutdown.notified() => {
                info!("Shutting down TCP forwarder...");
                break 'main_loop;
            }

            result = listener.accept() => {
                match result {
                    Ok((mut client, src)) => {
                        let tcp_map = tcp_map.clone();

                        tasks.spawn(async move {
                            let orig_dst = SockRef::from(&client).original_dst_v4().map(|o| o.as_socket_ipv4());

                            match orig_dst {
                                Ok(Some(orig)) => {
                                    let orig_dst_addr = *orig.ip();
                                    let orig_dst_port = orig.port();
                                    info!("TCP intercepted for {}:{} from {}", orig_dst_addr, orig_dst_port, src);

                                    match tcp_map.get(&orig_dst_port) {
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

    info!("TCP forwarder is waiting for tasks to finish...");
    (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {});

    info!("TCP forwarder shut down");
    Ok(())
}
