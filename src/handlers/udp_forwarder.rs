// SPDX-License-Identifier: GPL-3.0-or-later

use arc_swap::ArcSwap;
use log::{debug, error, info, warn};
use moka::{future::Cache, policy::EvictionPolicy};
use nix::{
    cmsg_space,
    errno::Errno,
    sys::socket::{
        ControlMessageOwned, MsgFlags, SockaddrIn, recvmsg, setsockopt, sockaddr_in,
        sockopt::{Ipv4OrigDstAddr, Ipv4PacketInfo},
    },
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    error::Error,
    ffi::{c_int, c_void},
    fmt, io,
    net::{Ipv4Addr, SocketAddrV4},
    os::fd::{AsFd, AsRawFd},
    sync::Arc,
};
use tokio::{
    io::unix::AsyncFd,
    net::UdpSocket,
    select,
    sync::{Semaphore, TryAcquireError, watch::Receiver},
    task::JoinSet,
    time::{Instant, timeout},
};

use super::{
    super::utils::structs::{Actions, RuntimeConfigs},
    constants::{BUFFER_SIZE, CONN_BACKLOG, CONN_TIMEOUT, DRAIN_DURATION, LISTEN_IP, UDP_REPLY_SOCKET_LIFE, UDP_UPSTREAM_SOCKET_LIFE},
};

/// UDP forwarder function
pub(in super::super) async fn udp_forwarder(mut rx: Receiver<Actions>, current_config: Arc<ArcSwap<RuntimeConfigs>>) -> io::Result<()> {
    info!("UDP forwarder starting...");

    let action = rx.borrow().clone();
    match action {
        Actions::STOP(s) => {
            info!("UDP forwarder shut down before starting as {s} failed");
            return Ok(());
        },
        Actions::PANICKED => {
            info!("UDP forwarder shut down before starting as someone panicked");
            return Ok(());
        },
        Actions::KILL | Actions::SHUTDOWN => {
            info!("UDP forwarder shut down before starting");
            return Ok(());
        },
        _ => { /* RELOAD or INIT has no effect now */ },
    };

    let (mut udp_map, mut udp_fd) = {
        let config = current_config.load();
        (config.udp_map.clone(), create_udp_socket_fd(config.port)?)
    };
    let semaphore = Arc::new(Semaphore::new(CONN_BACKLOG as usize));
    let mut tasks = JoinSet::new();
    let mut force_kill = false;
    let mut buf = [0u8; BUFFER_SIZE];
    let upstream_map = Cache::<(SocketAddrV4, SocketAddrV4), Arc<UdpSocket>>::builder()
        .name("udp_upstream_socket_map")
        .time_to_idle(UDP_UPSTREAM_SOCKET_LIFE)
        .max_capacity(1000)
        .eviction_policy(EvictionPolicy::tiny_lfu())
        .build();
    let reply_map = Cache::<u16, Arc<UdpSocket>>::builder()
        .name("udp_reply_socket_map")
        .time_to_idle(UDP_REPLY_SOCKET_LIFE)
        .max_capacity(1000)
        .eviction_policy(EvictionPolicy::tiny_lfu())
        .build();

    'udp_forwarder_loop: loop {
        select! {
            sig = rx.changed() => {
                match sig {
                    Ok(_) => {
                        let action = rx.borrow().clone();
                        match action {
                            Actions::RELOAD(port_changed) => {
                                info!("RELOAD signal received by UDP forwarder...");

                                let config = current_config.load();
                                if port_changed {
                                    match create_udp_socket_fd(config.port) {
                                        Ok(f) => {
                                            udp_fd = f;
                                            udp_map = config.udp_map.clone();
                                        },
                                        Err(e) => error!("{e}")
                                    };
                                } else {
                                    udp_map = config.udp_map.clone();
                                }

                                continue 'udp_forwarder_loop;
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
                            _ => {/* INIT will not come here */}
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

                'udp_forwarder_inner_loop: loop {
                    let (src, len, orig_dst) = match recvfrom_cmsg(&udp_fd, &mut buf) {
                        Ok(r) => r,
                        Err(UdpRecvError::WouldBlock) => {
                            guard.clear_ready();
                            break 'udp_forwarder_inner_loop;
                        },
                        Err(_) => {
                            break 'udp_forwarder_inner_loop;
                        }
                    };

                    match semaphore.clone().try_acquire_owned() {
                        Ok(p) => {
                            let packet = buf[..len].to_vec();
                            let udp_map = udp_map.clone();
                            let upstream_map = upstream_map.clone();
                            let reply_map = reply_map.clone();

                            tasks.spawn(async move {
                                let _permit = p; // hold acquired permit

                                let orig_dst_addr = orig_dst.ip();
                                let orig_dst_port = orig_dst.port();
                                debug!("UDP intercepted for {orig_dst_addr}:{orig_dst_port} from {src}");

                                match udp_map.get(&orig_dst_port) {
                                    Some(proxy) => {
                                        let upstream_sock = upstream_map.try_get_with(
                                            (src, orig_dst),
                                            create_upstream_socket(&orig_dst)
                                        ).await;
                                        let reply_sock = upstream_map.try_get_with(
                                            (src, orig_dst),
                                            create_reply_socket(orig_dst_port)
                                        ).await;
                                        /* let flow = match flows.entry(flow_key(src, orig_dst)) {
                                            Occupied(mut entry) => {
                                                let state = entry.get_mut();
                                                state.last_used = Instant::now();
                                                state
                                            },
                                            Vacant(entry) => {
                                                match (create_upstream_socket().await, create_reply_socket(orig_dst_addr, orig_dst_port)) {
                                                    (Ok(upstream), Ok(reply)) => {
                                                        let mut new_flow = entry.insert(UdpFlowState {
                                                            upstream,
                                                            reply,
                                                            last_used: Instant::now()
                                                        });
                                                        new_flow.value_mut()
                                                    },
                                                    _ => {
                                                        return;
                                                    },
                                                }
                                            },
                                        };

                                        if let Err(e) = flow.upstream.send_to(&packet, proxy).await {
                                            error!("Failed to send UDP datagram to upstream {}:{} - {e}", proxy.0, proxy.1);
                                            return;
                                        }

                                        let mut reply_buf = [0u8; BUFFER_SIZE];
                                        match timeout(CONN_TIMEOUT, flow.upstream.recv_from(&mut reply_buf)).await {
                                            Ok(Ok((reply_len, _))) => {
                                                match flow.reply.send_to(&reply_buf[..reply_len], src).await {
                                                    Ok(_) => {
                                                        debug!("UDP reply forwarded back to client {}", src);
                                                    },
                                                    Err(e) => {
                                                        error!("Failed to forward UDP reply back to client {} - {e}", src);
                                                    }
                                                };
                                            },
                                            Ok(Err(e)) => {
                                                error!("Failed to receive UDP datagram from upstream {}:{} - {e}", proxy.0, proxy.1);
                                            },
                                            Err(_) => {
                                                error!("Timed out while trying to receive UDP datagram from upstream {}:{}", proxy.0, proxy.1);
                                            }
                                        }; */
                                    },
                                    None => {
                                        warn!("No upstream mapping provided for destination UDP port {orig_dst_port}");
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
                };
            }
        }

        // draining
        while tasks.try_join_next().is_some() {}
    }

    if force_kill {
        tasks.abort_all();
    }

    info!("UDP forwarder is waiting for tasks to finish...");
    if timeout(DRAIN_DURATION, async {
        (!tasks.is_empty()).then(async || while tasks.join_next().await.is_some() {})
    })
    .await
    .is_err()
    {
        warn!("Forced exit in UDP forwarder: tasks didn't complete in time");
    }

    info!("UDP forwarder shut down");
    Ok(())
}

#[derive(Debug)]
enum UdpRecvError {
    WouldBlock,
    Invalid,
}

impl fmt::Display for UdpRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UdpRecvError::WouldBlock => write!(f, "WouldBlock"),
            UdpRecvError::Invalid => write!(f, "Invalid"),
        }
    }
}

impl Error for UdpRecvError {}

fn recvfrom_cmsg(sock: &AsyncFd<Socket>, buf: &mut [u8]) -> Result<(SocketAddrV4, usize, SocketAddrV4), UdpRecvError> {
    let mut cmsg_buf = cmsg_space!(sockaddr_in);
    let mut iov = [io::IoSliceMut::new(buf)];

    match recvmsg::<SockaddrIn>(
        sock.as_raw_fd(),
        &mut iov,
        Some(&mut cmsg_buf),
        MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_TRUNC,
    ) {
        Ok(msg) => {
            let src = msg
                .address
                .map(SocketAddrV4::from)
                .ok_or(UdpRecvError::Invalid)?;

            let orig_dst = msg
                .cmsgs()
                .ok()
                .and_then(|mut cmsgs| {
                    cmsgs.find_map(|cmsg| match cmsg {
                        ControlMessageOwned::Ipv4OrigDstAddr(addr) => Some(SocketAddrV4::from(SockaddrIn::from(addr))),
                        _ => None,
                    })
                })
                .ok_or(UdpRecvError::Invalid)?;

            Ok((src, msg.bytes, orig_dst))
        },
        Err(Errno::EWOULDBLOCK) => Err(UdpRecvError::WouldBlock),
        Err(e) => {
            error!("recvmsg failed: {e}");
            Err(UdpRecvError::Invalid)
        },
    }
}

fn create_udp_socket_fd(port: u16) -> io::Result<AsyncFd<Socket>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_recv_orig_dst_addr(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(LISTEN_IP, port).into())?;
    AsyncFd::new(socket)
}

async fn create_upstream_socket(upstream: &SocketAddrV4) -> io::Result<Arc<UdpSocket>> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await?;
    s.connect(upstream).await?;
    Ok(Arc::new(s))
}

async fn create_reply_socket(orig_dst_port: u16) -> io::Result<Arc<UdpSocket>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, orig_dst_port).into())?;
    Ok(Arc::new(UdpSocket::from_std(socket.into())?))
}

trait ExtendedUdpSocket: AsFd {
    fn set_recv_orig_dst_addr(&self, recv: bool) -> io::Result<()> {
        setsockopt(&self.as_fd(), Ipv4OrigDstAddr, &recv).map_err(|e| io::Error::from_raw_os_error(e as i32))
    }

    fn set_pass_ip_pkt_info(&self, pass: bool) -> io::Result<()> {
        setsockopt(&self.as_fd(), Ipv4PacketInfo, &pass).map_err(|e| io::Error::from_raw_os_error(e as i32))
    }
}

impl ExtendedUdpSocket for Socket {}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_recvfrom_cmsg() {
        let mut buf = [0u8; 128];
        let payload = b"payload";

        // OK
        let sock1 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock1.set_recv_orig_dst_addr(true).unwrap();
        sock1.set_nonblocking(true).unwrap();
        sock1
            .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
            .unwrap();

        let local_addr1 = sock1.local_addr().unwrap().as_socket_ipv4().unwrap();
        let fd1: AsyncFd<Socket> = AsyncFd::new(sock1).unwrap();
        let send_sock1 = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0u16)).await.unwrap();

        let size1 = send_sock1.send_to(payload, &local_addr1).await.unwrap();
        assert_eq!(size1, payload.len());

        let _ = fd1.readable().await.unwrap();
        let res1 = recvfrom_cmsg(&fd1, &mut buf);
        assert!(res1.is_ok());

        let (src, len, orig_dst) = res1.unwrap();
        assert_eq!(len, payload.len());
        assert_eq!(&buf[..len], payload);
        assert_eq!(orig_dst.ip(), local_addr1.ip());
        assert_eq!(orig_dst.port(), local_addr1.port());
        assert_eq!(src.ip(), &Ipv4Addr::LOCALHOST);

        // EWOULDBLOCK
        let sock2 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock2.set_recv_orig_dst_addr(true).unwrap();
        sock2.set_nonblocking(true).unwrap();
        sock2
            .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
            .unwrap();

        let fd2 = AsyncFd::new(sock2).unwrap();
        let res2 = recvfrom_cmsg(&fd2, &mut buf);
        assert!(res2.is_err());

        // No RECVORIGDSTADDR
        let sock3 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock3.set_nonblocking(true).unwrap();
        sock3
            .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
            .unwrap();

        let local_addr2 = sock3.local_addr().unwrap().as_socket_ipv4().unwrap();
        let fd3 = AsyncFd::new(sock3).unwrap();
        let send_sock2 = UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16))
            .await
            .unwrap();

        let size2 = send_sock2.send_to(payload, &local_addr2).await.unwrap();
        assert_eq!(size2, payload.len());

        let _ = fd3.readable().await.unwrap();
        let res3 = recvfrom_cmsg(&fd3, &mut buf);
        assert!(res3.is_err());
    }

    /*#[test]
    fn test_set_recv_orig_dst_addr() {
        let mut value = 0 as c_int;
        let mut len = size_of::<c_int>() as socklen_t;

        // set
        let sock1 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock1.set_recv_orig_dst_addr(true).unwrap();

        let rc1 = unsafe {
            getsockopt(
                sock1.as_raw_fd(),
                IPPROTO_IP,
                IP_RECVORIGDSTADDR,
                &mut value as *mut _ as *mut c_void,
                &mut len,
            )
        };
        assert_eq!(0, rc1);
        assert_eq!(1, value);

        // not set
        let sock2 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock2.set_recv_orig_dst_addr(false).unwrap();

        let rc2 = unsafe {
            getsockopt(
                sock2.as_raw_fd(),
                IPPROTO_IP,
                IP_RECVORIGDSTADDR,
                &mut value as *mut _ as *mut c_void,
                &mut len,
            )
        };
        assert_eq!(0, rc2);
        assert_eq!(0, value);
    }*/
}
