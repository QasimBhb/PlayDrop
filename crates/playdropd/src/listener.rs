//! Exclusive use of the listening port on Windows (SR-31).
//!
//! Without `SO_EXCLUSIVEADDRUSE`, another program running as the same user can bind the
//! port with `SO_REUSEADDR` and take incoming connections over. The core cannot set it (it
//! forbids `unsafe` and socket2 does not expose the option), so the daemon hands it this
//! hook through `HostConfig::listener_setup`.
//!
//! Not compiled or tested on the Linux build machine; Windows CI runs the test below.

use std::io;
use std::os::windows::io::AsRawSocket;

use playdrop_core::host::{ListenSocket, ListenerSetup};
use windows::Win32::Networking::WinSock::{
    setsockopt, SOCKET, SOCKET_ERROR, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
};

/// Sets `SO_EXCLUSIVEADDRUSE` on the listening socket before it is bound.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExclusivePort;

impl ListenerSetup for ExclusivePort {
    #[allow(unsafe_code)]
    fn before_bind(&self, socket: &ListenSocket) -> io::Result<()> {
        let on = 1i32.to_ne_bytes();
        let raw = usize::try_from(socket.as_raw_socket()).map_err(io::Error::other)?;
        // SAFETY: `raw` is the open, not yet bound socket that `socket` owns for the whole
        // call; `optval` is a live 4-byte buffer holding the BOOL value 1.
        let r = unsafe { setsockopt(SOCKET(raw), SOL_SOCKET, SO_EXCLUSIVEADDRUSE, Some(&on)) };
        if r == SOCKET_ERROR {
            // Winsock errors are per thread like GetLastError, which this reads.
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use playdrop_core::net::Server;
    use playdrop_core::session::Limits;
    use playdrop_core::Error;

    use super::*;

    /// A squatter: sets `SO_REUSEADDR`, which without the owner's exclusive flag would let
    /// it bind a port already in use.
    #[derive(Debug)]
    struct ReuseAddr;

    impl ListenerSetup for ReuseAddr {
        #[allow(unsafe_code)]
        fn before_bind(&self, socket: &ListenSocket) -> io::Result<()> {
            use windows::Win32::Networking::WinSock::SO_REUSEADDR;
            let on = 1i32.to_ne_bytes();
            let raw = usize::try_from(socket.as_raw_socket()).map_err(io::Error::other)?;
            // SAFETY: as in `ExclusivePort::before_bind`.
            let r = unsafe { setsockopt(SOCKET(raw), SOL_SOCKET, SO_REUSEADDR, Some(&on)) };
            if r == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// SR-31: a second bind on a held port is `PortHeld`, and a plain `SO_REUSEADDR`
    /// socket cannot take the port over either.
    #[tokio::test]
    async fn second_bind_is_port_held() {
        let first = Server::bind(
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
            Limits::default(),
            Some(&ExclusivePort),
        )
        .unwrap();
        let addr = first.local_addr();
        let second = Server::bind(addr, Limits::default(), Some(&ExclusivePort));
        assert!(
            matches!(&second, Err(Error::PortHeld { port }) if *port == addr.port()),
            "{:?}",
            second.as_ref().err()
        );

        let squatter = Server::bind(addr, Limits::default(), Some(&ReuseAddr));
        assert!(
            matches!(&squatter, Err(Error::PortHeld { .. })),
            "SO_REUSEADDR cannot steal the port: {:?}",
            squatter.as_ref().err()
        );
        drop(first);
    }
}
