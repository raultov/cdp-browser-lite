use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Checks whether the port is open (accepts TCP connections).
pub async fn is_port_open(host: &str, port: u16, timeout_duration: Duration) -> bool {
    let addr = format!("{}:{}", host, port);
    matches!(
        timeout(timeout_duration, TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}

/// Probe HTTP /json/version endpoint with keep-alive tolerance (Chrome >= 148).
pub async fn is_chrome_cdp(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    let connect_timeout = Duration::from_millis(500);
    let read_timeout = Duration::from_millis(1500);

    let mut stream = match timeout(connect_timeout, TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let req = b"GET /json/version HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(req).await.is_err() {
        return false;
    }

    let mut buf = [0u8; 4096];
    let mut response = Vec::new();

    // Read loop with timeout to handle keep-alive
    let _ = timeout(read_timeout, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    response.extend_from_slice(&buf[..n]);
                    // Check if we have enough to confirm it's Chrome
                    if response.windows(15).any(|w| w == b"HTTP/1.1 200 OK")
                        && response.windows(10).any(|w| w == b"\"Browser\":")
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;

    let response_str = String::from_utf8_lossy(&response);
    response_str.contains("HTTP/1.1 200 OK") && response_str.contains("\"Browser\":")
}
