//! Tiny HTTP probe for container healthchecks (the images ship no curl/wget).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// `GET path` on `127.0.0.1:port`; true only on a `200` status line.
/// Local only: never touches the BookStack backend.
pub fn probe_ok(port: u16, path: &str) -> bool {
    let Some(addr) = ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let req = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n");
    let mut head = [0u8; 12];
    s.write_all(req.as_bytes()).is_ok() && s.read_exact(&mut head).is_ok() && is_200(&head)
}

fn is_200(status_line_start: &[u8]) -> bool {
    status_line_start.starts_with(b"HTTP/1.") && status_line_start[8..].starts_with(b" 200")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_line() {
        assert!(is_200(b"HTTP/1.1 200 OK"));
        assert!(!is_200(b"HTTP/1.1 503 Ser"));
    }

    #[test]
    fn closed_port_is_unhealthy() {
        let p = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(!probe_ok(p, "/health"));
    }
}
