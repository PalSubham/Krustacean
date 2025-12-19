use libc::{IP_RECVORIGDSTADDR, IPPROTO_IP, c_int, c_void, setsockopt, sockaddr_in, socklen_t};
use log::error;
use nix::{
    cmsg_space,
    errno::Errno,
    sys::socket::{ControlMessageOwned, MsgFlags, SockaddrIn, recvmsg},
};
use socket2::Socket;
use std::{
    io::{Error, IoSliceMut, Result},
    mem::size_of,
    net::SocketAddrV4,
    os::fd::AsRawFd,
};
use tokio::io::unix::AsyncFd;

pub(super) trait ExtendedSocket {
    fn set_recv_orig_dst_addr(&self, recv: bool) -> Result<()>;
}

impl ExtendedSocket for Socket {
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

#[inline(always)]
pub(super) async fn recvfrom_cmsg_async(sock: &AsyncFd<Socket>, buf: &mut [u8]) -> Option<(SocketAddrV4, usize, SocketAddrV4)> {
    let mut cmsg_buf = cmsg_space!(sockaddr_in);
    let mut iov = [IoSliceMut::new(buf)];

    match recvmsg::<SockaddrIn>(sock.as_raw_fd(), &mut iov, Some(&mut cmsg_buf), MsgFlags::MSG_DONTWAIT) {
        Ok(msg) => {
            let src = match msg.address {
                Some(a) => {
                    let s = SocketAddrV4::from(a);

                    if s.ip().is_unspecified() {
                        error!("recvmsg(): source unspecified... dropping packet...");
                        None
                    } else {
                        Some(s)
                    }
                },
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
