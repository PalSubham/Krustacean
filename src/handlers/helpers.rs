// SPDX-License-Identifier: GPL-3.0-or-later

use libc::{IP_RECVORIGDSTADDR, IPPROTO_IP, c_int, c_void, setsockopt, sockaddr_in, socklen_t};
use log::error;
use nix::{
    cmsg_space,
    errno::Errno,
    sys::socket::{ControlMessageOwned, MsgFlags, SockaddrIn, recvmsg},
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io::{Error, IoSliceMut, Result},
    mem::size_of,
    net::{Ipv4Addr, SocketAddrV4},
    os::fd::AsRawFd,
    time::Duration,
};
use tokio::{io::unix::AsyncFd, net::TcpListener};

pub(super) const DRAIN_DURATION: Duration = Duration::from_secs(5u64);

#[inline(always)]
pub(super) fn recvfrom_cmsg(sock: &AsyncFd<Socket>, buf: &mut [u8]) -> Option<(SocketAddrV4, usize, SocketAddrV4)> {
    let mut cmsg_buf = cmsg_space!(sockaddr_in);
    let mut iov = [IoSliceMut::new(buf)];

    match recvmsg::<SockaddrIn>(sock.as_raw_fd(), &mut iov, Some(&mut cmsg_buf), MsgFlags::MSG_DONTWAIT) {
        Ok(msg) => {
            let src = match msg.address {
                Some(a) => Some(SocketAddrV4::from(a)),
                None => {
                    error!("recvmsg(): missing source address...dropping packet...");
                    None
                },
            };

            let orig_dst = match msg.cmsgs() {
                Ok(mut cmsgs) => match cmsgs.find_map(|cmsg| match cmsg {
                    ControlMessageOwned::Ipv4OrigDstAddr(addr) => Some(SocketAddrV4::from(SockaddrIn::from(addr))),
                    _ => None,
                }) {
                    Some(orig) => Some(orig),
                    None => {
                        error!("Couldn't find original destination");
                        None
                    },
                },
                Err(e) => {
                    error!("Allocated space for CMSGs too small...errno: {e}");
                    None
                },
            };

            if let (Some(src), Some(orig_dst)) = (src, orig_dst) {
                let len = msg.bytes;

                Some((src, len, orig_dst))
            } else {
                None
            }
        },
        Err(e) => {
            if e != Errno::EWOULDBLOCK {
                error!("recvmsg(): failed...errno: {e}");
            }

            None
        },
    }
}

const LISTEN_IP: [u8; 4] = [127, 0, 0, 2];
pub(super) const CONN_BACKLOG: u32 = 100;

pub(super) fn create_udp_socket_fd(port: u16) -> Result<AsyncFd<Socket>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_recv_orig_dst_addr(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::from(LISTEN_IP), port).into())?;
    AsyncFd::new(socket)
}

pub(super) fn create_tcp_listener(port: u16) -> Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_ip_transparent_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::from(LISTEN_IP), port).into())?;
    socket.listen(CONN_BACKLOG as i32)?;
    TcpListener::from_std(socket.into())
}

trait ExtendedSocket {
    fn set_recv_orig_dst_addr(&self, recv: bool) -> Result<()>;
}

impl ExtendedSocket for Socket {
    #[inline(always)]
    fn set_recv_orig_dst_addr(&self, recv: bool) -> Result<()> {
        let recv = recv as c_int;

        match unsafe {
            setsockopt(
                self.as_raw_fd(),
                IPPROTO_IP,
                IP_RECVORIGDSTADDR,
                &recv as *const _ as *const c_void,
                size_of::<c_int>() as socklen_t,
            )
        } {
            -1 => Err(Error::last_os_error()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {

    use libc::getsockopt;
    use tokio::net::UdpSocket;

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
        let (src, len, orig_dst) = recvfrom_cmsg(&fd1, &mut buf).unwrap();
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
        let res1 = recvfrom_cmsg(&fd2, &mut buf);
        assert!(res1.is_none());

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
        let res2 = recvfrom_cmsg(&fd3, &mut buf);
        assert!(res2.is_none());
    }

    #[test]
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
    }
}
