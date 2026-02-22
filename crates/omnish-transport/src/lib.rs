pub mod rpc_client;
pub mod rpc_server;

#[derive(Debug, Clone)]
pub enum TransportAddr {
    Unix(String),
    Tcp(String),
}

pub fn parse_addr(addr: &str) -> TransportAddr {
    if !addr.starts_with('/') && !addr.starts_with('.') && addr.contains(':') {
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
}
