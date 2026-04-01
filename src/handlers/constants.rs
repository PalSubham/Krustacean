use std::{net::Ipv4Addr, time::Duration};

/// Connection timeout for upstream
pub(super) const CONN_TIMEOUT: Duration = Duration::from_secs(5);

/// TCP and UDP data buffer size
pub(super) const BUFFER_SIZE: usize = 8 * 1024;

/// Wait time for forwarder tasks to finish
pub(super) const DRAIN_DURATION: Duration = Duration::from_secs(5);

/// Proxy listen IP
pub(in super::super) const LISTEN_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

/// TCP connection backlog and UDP semaphore size
pub(super) const CONN_BACKLOG: u32 = 100;

pub(super) const UDP_UPSTREAM_SOCKET_LIFE: Duration = Duration::from_secs(60);

pub(super) const UDP_REPLY_SOCKET_LIFE: Duration = Duration::from_secs(300);
