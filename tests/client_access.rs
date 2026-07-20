mod support;

use std::time::Duration;

use cdp_browser_lite::browser::{Browser, BrowserState};
use cdp_browser_lite::config::{BrowserConfig, LaunchMode};
use cdp_browser_lite::error::BrowserError;
use support::mock_devtools::{MockDevTools, MockWsBehavior};

#[tokio::test]
async fn first_call_connects_and_works() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .build();

    let browser = Browser::ensure(cfg).await.expect("ensure must succeed");

    let client = browser.client().await.expect("client must connect");

    let resp = client
        .send_raw_command("Browser.getVersion", serde_json::json!({}))
        .await
        .expect("send_raw_command must succeed");

    assert!(resp.result.is_some(), "CDP response must contain result");
}

#[tokio::test]
async fn repeated_calls_reuse_connection() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .build();

    let browser = Browser::ensure(cfg).await.expect("ensure must succeed");

    let _client1 = browser.client().await.expect("first client");
    let _client2 = browser.client().await.expect("second client");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let ws_conns = mock.connection_count();
    assert_eq!(
        ws_conns, 1,
        "expected exactly 1 active WS connection (reused), got {ws_conns}"
    );
}

#[tokio::test]
async fn reconnects_after_ws_drop() {
    let mock = MockDevTools::start(MockWsBehavior::CloseAfterOneCommand).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .build();

    let browser = Browser::ensure(cfg).await.expect("ensure must succeed");

    // First call: creates WS connection #1 (no liveness check needed, cache is empty)
    let _client1 = browser.client().await.expect("first client");

    // The liveness check inside client() IS the one command.
    // For the first call, cache was empty so no liveness check was done.
    // We need to drive the mock to close the WS. Let's do a command:
    let client_temp = browser.client().await.expect("get cached client");
    // This send_raw_command triggers the mock's CloseAfterOneCommand
    let _ = client_temp
        .send_raw_command("Browser.getVersion", serde_json::json!({}))
        .await;

    // Give the WS close time to propagate
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Second client() call: liveness check on cached client fails with Disconnected,
    // so it reconnects with a new WS connection.
    let client2 = browser.client().await.expect("reconnected client");

    let resp = client2
        .send_raw_command("Runtime.evaluate", serde_json::json!({"expression": "1+1"}))
        .await
        .expect("reconnected client must work");

    assert!(
        resp.result.is_some(),
        "reconnected client must be functional"
    );
}

#[tokio::test]
async fn dead_process_without_auto_relaunch_returns_error() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .auto_relaunch(false)
        .build();

    let state = BrowserState::new(None, None, mock.http_port, true);
    let browser = Browser::test_from_state(cfg, state);

    let result = browser.client().await;
    match result {
        Err(BrowserError::Stopped) => {}
        Err(e) => panic!("expected Stopped, got other error: {e}"),
        Ok(_) => panic!("expected Stopped, got Ok"),
    }
}

#[tokio::test]
async fn dead_process_with_auto_relaunch_relaunches_and_reconnects() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .auto_relaunch(true)
        .build();

    let state = BrowserState::new(None, None, mock.http_port, true);
    let browser = Browser::test_from_state(cfg, state);

    let client = browser
        .client()
        .await
        .expect("auto_relaunch must reconnect to mock");

    let resp = client
        .send_raw_command("Browser.getVersion", serde_json::json!({}))
        .await
        .expect("CDP command must work after auto_relaunch");

    assert!(
        resp.result.is_some(),
        "auto-relaunched client must be functional"
    );
}

#[tokio::test]
async fn stopped_browser_returns_error() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .build();

    let browser = Browser::ensure(cfg).await.expect("ensure must succeed");
    browser.stop().await.expect("stop must succeed");

    let result = browser.client().await;
    match result {
        Err(BrowserError::Stopped) => {}
        Err(e) => panic!("expected Stopped after browser stopped, got other error: {e}"),
        Ok(_) => panic!("expected Stopped after browser stopped, got Ok"),
    }
}
