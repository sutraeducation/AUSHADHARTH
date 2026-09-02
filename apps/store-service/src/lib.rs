pub mod api;
pub mod domain;
pub mod infrastructure;
pub mod platform;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const API_VERSION: &str = "v1";
pub const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_PORT: u16 = 47_831;

pub fn loopback_address(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_address_is_loopback_only() {
        assert!(loopback_address(DEFAULT_PORT).ip().is_loopback());
        assert_eq!(
            loopback_address(DEFAULT_PORT).to_string(),
            "127.0.0.1:47831"
        );
    }
}
