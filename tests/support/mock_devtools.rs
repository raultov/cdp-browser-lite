use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MockBehavior {
    KeepAlive,
    CloseAfterResponse,
    KeepAliveThenCloseAfter(Duration),
    SilentPeer,
    NotChrome,
    /// Mimics Chrome >= 151: silently closes the connection when the request
    /// line uses HTTP/1.0, and replies normally for HTTP/1.1.
    IgnoresHttp10,
}

pub struct MockChrome {
    pub port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl MockChrome {
    pub async fn start(behavior: MockBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept_res = listener.accept() => {
                        if let Ok((mut stream, _)) = accept_res {
                            tokio::spawn(async move {
                                match behavior {
                                    MockBehavior::KeepAlive => {
                                        let mut buf = [0u8; 1024];
                                        let _ = stream.read(&mut buf).await;
                                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"Browser\":\"Chrome\"}";
                                        let _ = stream.write_all(resp).await;
                                        tokio::time::sleep(Duration::from_secs(10)).await;
                                    }
                                    MockBehavior::CloseAfterResponse => {
                                        let mut buf = [0u8; 1024];
                                        let _ = stream.read(&mut buf).await;
                                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"Browser\":\"Chrome\"}";
                                        let _ = stream.write_all(resp).await;
                                    }
                                    MockBehavior::KeepAliveThenCloseAfter(dur) => {
                                        let mut buf = [0u8; 1024];
                                        let _ = stream.read(&mut buf).await;
                                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"Browser\":\"Chrome\"}";
                                        let _ = stream.write_all(resp).await;
                                        tokio::time::sleep(dur).await;
                                    }
                                    MockBehavior::SilentPeer => {
                                        tokio::time::sleep(Duration::from_secs(10)).await;
                                    }
                                    MockBehavior::NotChrome => {
                                        let mut buf = [0u8; 1024];
                                        let _ = stream.read(&mut buf).await;
                                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n{\"Server\":\"Nginx\"}";
                                        let _ = stream.write_all(resp).await;
                                    }
                                    MockBehavior::IgnoresHttp10 => {
                                        let mut buf = [0u8; 1024];
                                        let n = stream.read(&mut buf).await.unwrap_or(0);
                                        let request = String::from_utf8_lossy(&buf[..n]);
                                        // Drop the connection with no response when the client
                                        // uses HTTP/1.0, replicating Chrome >= 151 behaviour.
                                        if request.contains("HTTP/1.0") {
                                            return;
                                        }
                                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"Browser\":\"Chrome\"}";
                                        let _ = stream.write_all(resp).await;
                                    }
                                }
                            });
                        }
                    }
                }
            }
        });

        Self {
            port,
            shutdown_tx: Some(shutdown_tx),
        }
    }
}

impl Drop for MockChrome {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MockWsBehavior {
    StayOpen,
    CloseAfterOneCommand,
}

pub struct MockDevTools {
    pub http_port: u16,
    pub ws_port: u16,
    connection_count: Arc<AtomicU64>,
    drop_new_tx: Option<broadcast::Sender<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl MockDevTools {
    pub async fn start(ws_behavior: MockWsBehavior) -> Self {
        let connection_count = Arc::new(AtomicU64::new(0));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (drop_new_tx, _) = broadcast::channel::<()>(16);

        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();

        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_listener.local_addr().unwrap().port();

        let ws_port_for_json = ws_port;

        let http_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut rx = http_shutdown;
            loop {
                if *rx.borrow() {
                    break;
                }
                tokio::select! {
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            break;
                        }
                    }
                    accept_res = http_listener.accept() => {
                        if let Ok((stream, _)) = accept_res {
                            tokio::spawn(handle_http(stream, ws_port_for_json));
                        }
                    }
                }
            }
        });

        let ws_shutdown = shutdown_rx.clone();
        let cnt = Arc::clone(&connection_count);
        let drop_new = drop_new_tx.clone();
        tokio::spawn(async move {
            let mut rx = ws_shutdown;
            loop {
                if *rx.borrow() {
                    break;
                }
                tokio::select! {
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            break;
                        }
                    }
                    accept_res = ws_listener.accept() => {
                        if let Ok((stream, _)) = accept_res {
                            let cnt = Arc::clone(&cnt);
                            let mut drop_new = drop_new.subscribe();
                            let beh = ws_behavior;
                            tokio::spawn(async move {
                                cnt.fetch_add(1, Ordering::SeqCst);
                                handle_ws(stream, &mut drop_new, beh).await;
                                cnt.fetch_sub(1, Ordering::SeqCst);
                            });
                        }
                    }
                }
            }
        });

        Self {
            http_port,
            ws_port,
            connection_count,
            drop_new_tx: Some(drop_new_tx),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn connection_count(&self) -> u64 {
        self.connection_count.load(Ordering::SeqCst)
    }

    pub fn drop_new_connections(&mut self) {
        if let Some(ref tx) = self.drop_new_tx {
            let _ = tx.send(());
        }
    }
}

impl Drop for MockDevTools {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }
}

async fn handle_http(mut stream: TcpStream, ws_port: u16) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf).await {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);

    let (status, body) = if request.contains("/json/version") {
        (
            "HTTP/1.1 200 OK",
            json!({
                "Browser": "Chrome/Mock",
                "Protocol-Version": "1.3",
                "User-Agent": "MockChrome/1.0",
                "V8-Version": "12.0.0",
                "WebKit-Version": "537.36",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/browser/mock", ws_port)
            })
            .to_string(),
        )
    } else if request.contains("/json/list") {
        (
            "HTTP/1.1 200 OK",
            json!([{
                "title": "about:blank",
                "type": "page",
                "url": "about:blank",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/page/mock-1", ws_port)
            }])
            .to_string(),
        )
    } else if request.contains("/json/new") {
        (
            "HTTP/1.1 200 OK",
            json!({
                "title": "about:blank",
                "type": "page",
                "url": "about:blank",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/page/mock-1", ws_port)
            })
            .to_string(),
        )
    } else {
        ("HTTP/1.1 404 Not Found", "{}".to_string())
    };

    let resp = format!(
        "{}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
}

async fn handle_ws(
    stream: TcpStream,
    drop_new: &mut broadcast::Receiver<()>,
    behavior: MockWsBehavior,
) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return,
    };

    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    loop {
        tokio::select! {
            biased;
            _ = drop_new.recv() => {
                let _ = ws_sink.close().await;
                break;
            }
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let request: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        let id = request.get("id").and_then(|v| v.as_u64());
                        let method = request
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        let response = match method {
                            "Browser.getVersion" => json!({
                                "id": id,
                                "result": {
                                    "product": "MockChrome/1.0",
                                    "userAgent": "MockChrome/1.0",
                                    "protocolVersion": "1.3"
                                }
                            }),
                            _ => json!({
                                "id": id,
                                "result": {}
                            }),
                        };

                        let _ = ws_sink
                            .send(Message::Text(response.to_string().into()))
                            .await;

                        if behavior == MockWsBehavior::CloseAfterOneCommand {
                            let _ = ws_sink.close().await;
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(_)) => break,
                    None => break,
                    _ => {}
                }
            }
        }
    }
}
