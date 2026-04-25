use std::{ffi::c_int, net::Ipv4Addr, time::Duration};

/// Connection timeout for upstream
pub(super) const CONN_TIMEOUT: Duration = Duration::from_secs(5);

/// TCP and UDP data buffer size
pub(super) const BUFFER_SIZE: usize = 8 * 1024;

/// Wait time for forwarder tasks to finish
pub(super) const DRAIN_DURATION: Duration = Duration::from_secs(5);

/// Proxy listen IP
pub(in super::super) const LISTEN_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

/// TCP connection backlog and UDP semaphore size
pub(super) const MAX_TASKS: usize = 100;

/// TCP connection backlog
pub(super) const TCP_CONN_BACKLOG: c_int = 128;

/// Cache lifetime of UDP upstream socket
pub(super) const UDP_UPSTREAM_SOCKET_LIFE: Duration = Duration::from_secs(60);

/// Cache lifetime of UDP reply socket
pub(super) const UDP_REPLY_SOCKET_LIFE: Duration = Duration::from_secs(300);

/// Size of batch for [`nix::sys::socket::recvmmsg`] to process in a single pass
pub(super) const UDP_BATCH_SIZE: usize = 32;
