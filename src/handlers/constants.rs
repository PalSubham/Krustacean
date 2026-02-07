use std::{net::Ipv4Addr, time::Duration};

/// Connection timeout for upstream
pub(super) const CONN_TIMEOUT: Duration = Duration::from_secs(2u64);

/// TCP and UDP data buffer size, 4KB
pub(super) const BUFFER_SIZE: usize = 4096;

/// Wait time for forwarder tasks to finish
pub(super) const DRAIN_DURATION: Duration = Duration::from_secs(5u64);

/// Proxy listen IP - `127.0.0.2`
pub(crate) const LISTEN_IP: Ipv4Addr = Ipv4Addr::new(127u8, 0u8, 0u8, 2u8);

/// TCP connection backlog and UDP semaphore size
pub(super) const CONN_BACKLOG: u32 = 100;
