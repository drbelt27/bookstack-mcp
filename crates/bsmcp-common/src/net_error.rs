//! Diagnostics for failed outbound HTTP requests.
//!
//! `reqwest::Error`'s `Display` is just "error sending request for url (..)":
//! the real cause (DNS, refused, unreachable IPv6, TLS...) lives in the
//! `source()` chain and was never logged. [`describe`] returns a stable
//! machine-readable kind plus the chain; the URL is deliberately left out
//! because it can carry query-string secrets.

use std::error::Error;

/// Cause chain joined with " -> ", outermost first, URL excluded.
fn chain(e: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    let mut cur = e.source();
    while let Some(err) = cur {
        parts.push(err.to_string());
        cur = err.source();
    }
    parts.join(" -> ")
}

fn kind_of(e: &reqwest::Error, chain: &str) -> &'static str {
    let c = chain.to_ascii_lowercase();
    if e.is_redirect() {
        "redirect_blocked"
    } else if e.is_timeout() {
        "timeout"
    } else if c.contains("dns error")
        || c.contains("failed to lookup")
        || c.contains("name or service not known")
        || c.contains("no address associated")
    {
        "dns"
    } else if c.contains("certificate") || c.contains("tls") || c.contains("ssl") {
        "tls"
    } else if c.contains("connection refused")
        || c.contains("os error 111") // Linux ECONNREFUSED
        || c.contains("os error 10061")
    // Windows WSAECONNREFUSED; the text is localized
    {
        "connection_refused"
    } else if c.contains("network is unreachable")
        || c.contains("no route to host")
        || c.contains("os error 101")
        || c.contains("os error 113")
    {
        "network_unreachable"
    } else if e.is_connect() {
        "connect_failed"
    } else {
        "request_failed"
    }
}

/// `(kind, cause chain)` for a failed request.
pub fn describe(e: &reqwest::Error) -> (&'static str, String) {
    let chain = chain(e);
    (kind_of(e, &chain), chain)
}

/// Log a failed request with its cause chain and return the short,
/// caller-safe message ("Request failed (kind)"). `event` is the log name.
pub fn log_and_message(event: &'static str, e: &reqwest::Error) -> String {
    let (kind, chain) = describe(e);
    tracing::error!(kind, cause = %chain, "{event}");
    format!("Request failed ({kind})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn err_for(url: &str) -> reqwest::Error {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn refused_connection_is_classified_and_url_free() {
        // Bind then drop to get a port nobody listens on.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = err_for(&format!("http://127.0.0.1:{port}/?secret=hunter2")).await;
        let (kind, chain) = describe(&e);
        assert_eq!(kind, "connection_refused");
        assert!(!chain.contains("hunter2"), "chain leaked the URL: {chain}");
        assert!(!log_and_message("t", &e).contains("hunter2"));
    }

    #[tokio::test]
    async fn unresolvable_host_is_dns() {
        // `.invalid` is reserved (RFC 2606): never resolves.
        let e = err_for("http://bsmcp-test.invalid/").await;
        assert_eq!(describe(&e).0, "dns");
    }

    #[tokio::test]
    async fn blocked_redirect_is_classified() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let e = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(0))
            .build()
            .unwrap()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(describe(&e).0, "redirect_blocked");
    }
}
