// SPDX-License-Identifier: GPL-3.0-or-later

use arc_swap::ArcSwap;
use bytes::{Bytes, BytesMut};
use log::{debug, error, info, warn};
use moka::{future::Cache, policy::EvictionPolicy};
#[cfg(test)]
use nix::sys::socket::getsockopt;
use nix::{
    cmsg_space,
    libc::{in_addr, in_pktinfo},
    sys::socket::{
        ControlMessage, ControlMessageOwned, MsgFlags, MultiHeaders, RecvMsg, SockaddrIn, recvmmsg, sendmsg, setsockopt, sockaddr_in,
        sockopt::Ipv4OrigDstAddr,
    },
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    array, io,
    net::{Ipv4Addr, SocketAddrV4},
    os::fd::{AsFd, AsRawFd},
    slice,
    sync::Arc,
};
use tokio::{
    io::{Interest, unix::AsyncFd},
    net::UdpSocket,
    select,
    sync::{Semaphore, TryAcquireError, watch::Receiver},
    task::JoinSet,
    time::timeout,
};

use super::{
    super::utils::structs::{Actions, RuntimeConfigs},
    constants::{BUFFER_SIZE, CONN_TIMEOUT, DRAIN_DURATION, LISTEN_IP, MAX_TASKS, UDP_BATCH_SIZE, UDP_REPLY_SOCKET_LIFE, UDP_UPSTREAM_SOCKET_LIFE},
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
    let mut tasks = JoinSet::new();
    let mut force_kill = false;
    let mut buf = array::from_fn::<_, UDP_BATCH_SIZE, _>(|_| BytesMut::with_capacity(BUFFER_SIZE));
    let semaphore = Arc::new(Semaphore::new(MAX_TASKS));
    let upstream_map = Cache::<(SocketAddrV4, SocketAddrV4), Arc<UdpSocket>>::builder()
        .name("udp_upstream_socket_map")
        .time_to_idle(UDP_UPSTREAM_SOCKET_LIFE)
        .max_capacity(1000)
        .eviction_policy(EvictionPolicy::tiny_lfu())
        .build();
    let reply_map = Cache::<u16, Arc<AsyncFd<Socket>>>::builder()
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
                let mut read_guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        error!("AsyncFd error from listen socket: {e}");
                        continue 'udp_forwarder_loop;
                    }
                };

                'udp_forwarder_msg_loop: loop {
                    match read_guard.try_io(|sock_fd| multi_recvfrom_cmsg(
                        sock_fd,
                        &mut buf,
                        |packet, src, orig_dst| {
                            match semaphore.clone().try_acquire_owned() {
                                Ok(p) => {
                                    let udp_map = udp_map.clone();
                                    let upstream_map = upstream_map.clone();
                                    let reply_map = reply_map.clone();

                                    tasks.spawn(async move {
                                        let _permit = p; // hold acquired permit

                                        debug!("UDP intercepted for {orig_dst} from {src}");

                                        match udp_map.get(&orig_dst.port()) {
                                            Some(upstream) => {
                                                let upstream_sock = upstream_map.try_get_with(
                                                    (src, *upstream),
                                                    create_upstream_socket(upstream)
                                                ).await;
                                                let reply_sock = reply_map.try_get_with(
                                                    orig_dst.port(),
                                                    create_reply_socket_fd(orig_dst.port())
                                                ).await;

                                                match (upstream_sock, reply_sock) {
                                                    (Ok(us), Ok(rsfd)) => {
                                                        if let Err(e) = us.send(&packet).await {
                                                            error!("Failed to send UDP datagram to upstream {upstream} - {e}");
                                                            return;
                                                        } else {
                                                            debug!("Send {} bytes data from {src} to upstream {upstream}", packet.len())
                                                        }

                                                        let mut reply_buf = [0u8; BUFFER_SIZE];
                                                        match timeout(CONN_TIMEOUT, us.recv(&mut reply_buf)).await {
                                                            Ok(Ok(reply_len)) => {
                                                                'udp_forwarder_reply_loop: loop {
                                                                    let mut write_guard = match rsfd.writable().await {
                                                                        Ok(g) => g,
                                                                        Err(e) => {
                                                                            error!("AsyncFd error from reply socket from upstream {upstream} - {e}");
                                                                            break 'udp_forwarder_reply_loop;
                                                                        },
                                                                    };

                                                                    match write_guard.try_io(|inner| sendto_cmsg(
                                                                        inner,
                                                                        &reply_buf[..reply_len],
                                                                        &src,
                                                                        orig_dst.ip()
                                                                    )) {
                                                                        Ok(Ok(s)) => {
                                                                            if s != reply_len {
                                                                                error!("Failed to send the entire reply (unexpected!!!)");
                                                                            } else {
                                                                                debug!("Send {s} bytes reply from {upstream} back to client {src}");
                                                                            }

                                                                            break 'udp_forwarder_reply_loop;
                                                                        },
                                                                        Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                                                                            continue 'udp_forwarder_reply_loop;
                                                                        },
                                                                        Ok(Err(e)) => {
                                                                            error!("UDP reply send error: {e}");
                                                                            break 'udp_forwarder_reply_loop;
                                                                        }
                                                                        Err(_would_block) => {
                                                                            continue 'udp_forwarder_reply_loop;
                                                                        },
                                                                    };
                                                                };
                                                            },
                                                            Ok(Err(e)) => {
                                                                error!("Failed to receive UDP datagram from upstream {upstream} - {e}");
                                                            },
                                                            Err(_) => {
                                                                error!("Timed out while trying to receive UDP datagram from upstream {upstream}");
                                                            }
                                                        };
                                                    },
                                                    (Ok(_), Err(e)) => {
                                                        error!("Error creating reply socket - {e}");
                                                    },
                                                    (Err(e), Ok(_)) => {
                                                        error!("Error creating upstream socket - {e}");
                                                    },
                                                    (Err(e1), Err(e2)) => {
                                                        error!("Error creating upstream - {e1} & reply socket - {e2} ");
                                                    },
                                                };
                                            },
                                            None => {
                                                warn!("No upstream mapping provided for destination UDP port {}", orig_dst.port());
                                            }
                                        };
                                    });
                                },
                                Err(e) => match e {
                                    TryAcquireError::Closed => {
                                        error!("UDP forwarder semaphore is closed");
                                    },
                                    TryAcquireError::NoPermits => {
                                        warn!("UDP forwarder is at max...");
                                    }
                                }
                            };
                        },
                        |e| {
                            error!("UDP processing error: {e}");
                        }
                    )) {
                        Ok(Ok(r)) => {
                            debug!("Processed {r}/{UDP_BATCH_SIZE} packets in this batch");
                        },
                        Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                            continue 'udp_forwarder_msg_loop;
                        },
                        Ok(Err(e)) => {
                            error!("UDP recv error: {e}");
                            break 'udp_forwarder_msg_loop;
                        }
                        Err(_would_block) => {
                            break 'udp_forwarder_msg_loop;
                        },
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

fn multi_recvfrom_cmsg<const N: usize, H, E>(sock: &AsyncFd<Socket>, buf: &mut [BytesMut; N], mut handler: H, error_handler: E) -> io::Result<usize>
where
    H: FnMut(Bytes, SocketAddrV4, SocketAddrV4),
    E: Fn(io::Error),
{
    let mut meta = [None; N];

    let count = {
        let mut iovs = array::from_fn::<_, N, _>(|_| [io::IoSliceMut::new(&mut [])]);
        for (iov, b) in iovs.iter_mut().zip(buf.iter_mut()) {
            // get uninitialized slice of the memory from BytesMut for write
            let spare = b.spare_capacity_mut();
            let ptr = spare.as_mut_ptr() as *mut _;
            let len = spare.len();
            let slice = unsafe { slice::from_raw_parts_mut(ptr, len) };

            iov[0] = io::IoSliceMut::new(slice);
        }

        let cmsg_space = cmsg_space!(sockaddr_in);
        let mut headers = MultiHeaders::<SockaddrIn>::preallocate(N, Some(cmsg_space));
        let mut count = 0;

        recvmmsg(sock.as_raw_fd(), &mut headers, &mut iovs, MsgFlags::MSG_DONTWAIT, None)?
            .enumerate()
            .for_each(|(i, msg)| {
                match parse_msg(msg) {
                    Ok((src, len, orig_dst)) => {
                        meta[i] = Some((src, len, orig_dst));
                    },
                    Err(e) => {
                        error_handler(e);
                    },
                };

                count += 1;
            });

        count
    };

    for i in 0..count {
        if let Some((src, len, orig_dst)) = meta[i] {
            unsafe {
                buf[i].set_len(len);
            }
            let cap = buf[i].capacity();
            let packet = buf[i].split().freeze();
            buf[i] = BytesMut::with_capacity(cap);

            handler(packet, src, orig_dst);
        }
    }

    Ok(count)
}

#[inline(always)]
fn parse_msg<'a, 's>(msg: RecvMsg<'a, 's, SockaddrIn>) -> io::Result<(SocketAddrV4, usize, SocketAddrV4)> {
    if msg.flags.contains(MsgFlags::MSG_CTRUNC) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "control message truncated"));
    }

    if msg.flags.contains(MsgFlags::MSG_TRUNC) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "data truncated"));
    }

    let src = msg
        .address
        .map(SocketAddrV4::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing src addr"))?;

    let mut cmsgs = msg
        .cmsgs()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid cmsg"))?;

    let orig_dst = cmsgs
        .find_map(|cmsg| match cmsg {
            ControlMessageOwned::Ipv4OrigDstAddr(addr) => Some(SocketAddrV4::from(SockaddrIn::from(addr))),
            _ => None,
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing orig dst"))?;

    Ok((src, msg.bytes, orig_dst))
}

fn sendto_cmsg(sock: &AsyncFd<Socket>, buf: &[u8], client: &SocketAddrV4, orig_dst_ip: &Ipv4Addr) -> io::Result<usize> {
    let iov = [io::IoSlice::new(buf)];
    let cmsg = ControlMessage::Ipv4PacketInfo(&in_pktinfo {
        ipi_ifindex: 0,
        ipi_spec_dst: in_addr {
            s_addr: u32::from_ne_bytes(orig_dst_ip.octets()),
        },
        ipi_addr: in_addr { s_addr: 0 },
    });

    sendmsg(sock.as_raw_fd(), &iov, &[cmsg], MsgFlags::MSG_DONTWAIT, Some(&SockaddrIn::from(*client))).map_err(|e| io::Error::from(e))
}

fn create_udp_socket_fd(port: u16) -> io::Result<AsyncFd<Socket>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_orig_dst_addr(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(LISTEN_IP, port).into())?;
    AsyncFd::with_interest(socket, Interest::READABLE)
}

async fn create_upstream_socket(upstream: &SocketAddrV4) -> io::Result<Arc<UdpSocket>> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await?;
    s.connect(upstream).await?;
    Ok(Arc::new(s))
}

async fn create_reply_socket_fd(orig_dst_port: u16) -> io::Result<Arc<AsyncFd<Socket>>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, orig_dst_port).into())?;
    Ok(Arc::new(AsyncFd::with_interest(socket, Interest::WRITABLE)?))
}

trait ExtendedUdpSocket: AsFd {
    fn set_orig_dst_addr(&self, dst: bool) -> io::Result<()> {
        setsockopt(&self.as_fd(), Ipv4OrigDstAddr, &dst).map_err(|e| e.into())
    }

    #[cfg(test)]
    fn get_orig_dst_addr(&self) -> io::Result<bool> {
        getsockopt(&self.as_fd(), Ipv4OrigDstAddr).map_err(|e| e.into())
    }
}

impl ExtendedUdpSocket for Socket {}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::net::SocketAddr;

    use crate::handlers::udp_forwarder::sendto_cmsg;

    use super::*;

    #[tokio::test]
    async fn test_multi_recvfrom_cmsg() {
        let mut buf = array::from_fn::<_, 2, _>(|_| BytesMut::with_capacity(8));
        let payload = b"payload";

        // OK
        {
            let recv_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            recv_sock.set_orig_dst_addr(true).unwrap();
            recv_sock.set_nonblocking(true).unwrap();
            recv_sock
                .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
                .unwrap();

            let recv_addr = recv_sock.local_addr().unwrap().as_socket_ipv4().unwrap();
            let recv_fd = AsyncFd::new(recv_sock).unwrap();

            let send_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0u16)).await.unwrap();
            let size = send_sock.send_to(payload, &recv_addr).await.unwrap();
            assert_eq!(size, payload.len());

            let _ = recv_fd.readable().await.unwrap();
            let res = multi_recvfrom_cmsg(
                &recv_fd,
                &mut buf,
                |p, src, orig_dst| {
                    assert_eq!(p.len(), payload.len());
                    assert_eq!(*p, *payload);
                    assert_eq!(orig_dst.ip(), recv_addr.ip());
                    assert_eq!(orig_dst.port(), recv_addr.port());
                    assert_eq!(src.ip(), &Ipv4Addr::LOCALHOST);
                },
                |_| {
                    unreachable!();
                },
            )
            .unwrap();
            assert_eq!(1, res);
        }

        // WouldBlock
        {
            let recv_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            recv_sock.set_orig_dst_addr(true).unwrap();
            recv_sock.set_nonblocking(true).unwrap();
            recv_sock
                .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
                .unwrap();

            let recv_fd = AsyncFd::new(recv_sock).unwrap();
            let res = multi_recvfrom_cmsg(
                &recv_fd,
                &mut buf,
                |_, _, _| {
                    unreachable!();
                },
                |_| {
                    unreachable!();
                },
            );
            assert!(res.is_err());
        }

        // No RECVORIGDSTADDR
        {
            let recv_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            recv_sock.set_nonblocking(true).unwrap();
            recv_sock
                .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16).into())
                .unwrap();

            let recv_addr = recv_sock.local_addr().unwrap().as_socket_ipv4().unwrap();
            let recv_fd = AsyncFd::new(recv_sock).unwrap();
            let send_sock = UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0u16))
                .await
                .unwrap();

            let size = send_sock.send_to(payload, &recv_addr).await.unwrap();
            assert_eq!(size, payload.len());

            let _ = recv_fd.readable().await.unwrap();
            let res = multi_recvfrom_cmsg(
                &recv_fd,
                &mut buf,
                |_, _, _| {
                    unreachable!();
                },
                |e| {
                    assert_eq!(io::ErrorKind::InvalidData, e.kind());
                },
            )
            .unwrap();
            assert_eq!(1, res);
        }
    }

    #[tokio::test]
    async fn test_sendto_cmsg() {
        let payload = b"payload";

        let recv_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0u16)).await.unwrap();
        let recv_addr = match recv_sock.local_addr().unwrap() {
            SocketAddr::V4(socket_addr_v4) => socket_addr_v4,
            _ => unreachable!(),
        };

        let send_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        send_sock
            .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0u16).into())
            .unwrap();
        let send_fd = AsyncFd::new(send_sock).unwrap();
        let send_src_ip = Ipv4Addr::new(127, 0, 0, 3);

        let _ = send_fd.writable().await.unwrap();
        let res = sendto_cmsg(&send_fd, payload, &recv_addr, &send_src_ip).unwrap();
        assert_eq!(payload.len(), res);

        let mut buf = [0u8; 7];
        let (size, addr) = recv_sock
            .recv_from(&mut buf)
            .await
            .map(|(s, a)| {
                (
                    s,
                    match a {
                        SocketAddr::V4(socket_addr_v4) => socket_addr_v4,
                        _ => unreachable!(),
                    },
                )
            })
            .unwrap();
        assert_eq!(*payload, buf);
        assert_eq!(payload.len(), size);
        assert_eq!(send_src_ip, *addr.ip());
    }

    #[test]
    fn test_orig_dst_addr() {
        // set
        {
            let sock1 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            sock1.set_orig_dst_addr(true).unwrap();

            let r1 = sock1.get_orig_dst_addr().unwrap();
            assert_eq!(true, r1);
        }

        // not set
        {
            let sock2 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            sock2.set_orig_dst_addr(false).unwrap();

            let r2 = sock2.get_orig_dst_addr().unwrap();
            assert_eq!(false, r2);
        }
    }
}
