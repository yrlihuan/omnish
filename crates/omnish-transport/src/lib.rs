pub mod rpc_client;
pub mod rpc_server;
pub mod tls;

use std::time::Duration;

/// Bound on setting up a connection: TCP connect plus TLS handshake on the
/// client, TLS handshake on the server. A handshake whose packets are
/// silently dropped (e.g. across a network change) otherwise waits until TCP
/// gives up, which on macOS kept a client from reconnecting for over 12
/// minutes after the network was back.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Turn on TCP keepalive so a peer that vanished without closing (network
/// change, sleep) is noticed in about a minute instead of never: an idle
/// connection with no pending writes otherwise stays ESTABLISHED forever.
pub fn set_keepalive(stream: &tokio::net::TcpStream) {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&ka) {
        tracing::debug!("set_tcp_keepalive failed: {}", e);
    }
}

#[derive(Debug, Clone)]
pub enum TransportAddr {
    Unix(String),
    Tcp(String),
}

pub fn parse_addr(addr: &str) -> TransportAddr {
    if let Some(hp) = addr.strip_prefix("tcp://") {
        TransportAddr::Tcp(hp.to_string())
    } else if !addr.starts_with('/') && !addr.starts_with('.') && addr.contains(':') {
        TransportAddr::Tcp(addr.to_string())
    } else {
        TransportAddr::Unix(addr.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unix_absolute_path() {
        assert!(matches!(parse_addr("/tmp/omnish.sock"), TransportAddr::Unix(_)));
    }

    #[test]
    fn test_parse_unix_relative_path() {
        assert!(matches!(parse_addr("./omnish.sock"), TransportAddr::Unix(_)));
    }

    #[test]
    fn test_parse_unix_no_colon() {
        assert!(matches!(parse_addr("omnish.sock"), TransportAddr::Unix(_)));
    }

    #[test]
    fn test_parse_tcp_ipv4_port() {
        assert!(matches!(parse_addr("127.0.0.1:9876"), TransportAddr::Tcp(_)));
    }

    #[test]
    fn test_parse_tcp_localhost_port() {
        assert!(matches!(parse_addr("localhost:9876"), TransportAddr::Tcp(_)));
    }

    #[test]
    fn test_parse_tcp_hostname_port() {
        assert!(matches!(parse_addr("myhost:9876"), TransportAddr::Tcp(_)));
    }

    #[test]
    fn test_parse_tcp_ipv6_port() {
        assert!(matches!(parse_addr("[::1]:9876"), TransportAddr::Tcp(_)));
    }

    #[test]
    fn test_parse_tcp_zero_zero_port() {
        assert!(matches!(parse_addr("0.0.0.0:8080"), TransportAddr::Tcp(_)));
    }

    #[test]
    fn test_parse_tcp_prefix() {
        match parse_addr("tcp://127.0.0.1:9500") {
            TransportAddr::Tcp(hp) => assert_eq!(hp, "127.0.0.1:9500"),
            _ => panic!("expected Tcp"),
        }
    }
}
