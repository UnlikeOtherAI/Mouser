use std::net::SocketAddr;

use quinn::{Endpoint, ServerConfig};

use crate::NetError;

/// Loopback wildcard bind address (port 0 = OS-assigned) for an interactive plane.
pub fn loopback_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// Wildcard client-bind address matching the destination's family (port 0 =
/// OS-assigned). A client that dials a LAN/remote peer must bind the unspecified
/// address so the OS routes egress out the right interface.
pub fn client_bind_for(dest: SocketAddr) -> SocketAddr {
    match dest {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16, 0, 0, 0, 0, 0, 0, 0], 0)),
    }
}

/// Unspecified IPv6 bind (`[::]:0`) for a dual-stack server listener.
pub fn dual_stack_addr() -> SocketAddr {
    SocketAddr::from(([0u16, 0, 0, 0, 0, 0, 0, 0], 0))
}

/// Bind a server [`Endpoint`] that accepts both IPv4 and IPv6 dialers when given a
/// wildcard IPv6 address. `quinn::Endpoint::server` binds a plain `UdpSocket` and never
/// clears `IPV6_V6ONLY`, so on Windows a `[::]` server is IPv6-only.
pub(crate) fn bind_dual_stack_server(
    server_config: ServerConfig,
    addr: SocketAddr,
) -> Result<Endpoint, NetError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| NetError::Io(e.to_string()))?;
    if addr.is_ipv6() {
        let _ = socket.set_only_v6(false);
    }
    socket
        .bind(&addr.into())
        .map_err(|e| NetError::Io(e.to_string()))?;
    let runtime =
        quinn::default_runtime().ok_or_else(|| NetError::Io("no async runtime found".into()))?;
    Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket.into(),
        runtime,
    )
    .map_err(|e| NetError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_bind_is_routable_not_loopback() {
        let v4_peer: SocketAddr = "192.168.1.229:61600".parse().expect("v4 addr");
        let bind = client_bind_for(v4_peer);
        assert!(bind.ip().is_unspecified(), "{bind} must be unspecified");
        assert!(!bind.ip().is_loopback(), "{bind} must not be loopback");
        assert!(bind.is_ipv4(), "IPv4 peer -> IPv4 bind");
        assert_eq!(bind.port(), 0, "OS-assigned ephemeral port");

        let v6_peer: SocketAddr = "[fe80::1]:61600".parse().expect("v6 addr");
        let bind6 = client_bind_for(v6_peer);
        assert!(bind6.ip().is_unspecified());
        assert!(bind6.is_ipv6(), "IPv6 peer -> IPv6 bind");
    }
}
