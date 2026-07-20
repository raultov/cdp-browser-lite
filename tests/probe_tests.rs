mod support;

use cdp_browser_lite::probe::{is_chrome_cdp, is_port_open};
use std::time::Duration;
use support::mock_devtools::{MockBehavior, MockChrome};

#[tokio::test]
async fn given_chrome_modern_keep_alive_when_probing_then_true() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;
    assert!(is_chrome_cdp("127.0.0.1", mock.port).await);
}

#[tokio::test]
async fn given_chrome_legacy_closes_when_probing_then_true() {
    let mock = MockChrome::start(MockBehavior::CloseAfterResponse).await;
    assert!(is_chrome_cdp("127.0.0.1", mock.port).await);
}

#[tokio::test]
async fn given_silent_peer_when_probing_then_false() {
    let mock = MockChrome::start(MockBehavior::SilentPeer).await;
    assert!(!is_chrome_cdp("127.0.0.1", mock.port).await);
}

#[tokio::test]
async fn given_not_chrome_when_probing_then_false() {
    let mock = MockChrome::start(MockBehavior::NotChrome).await;
    assert!(!is_chrome_cdp("127.0.0.1", mock.port).await);
}

#[tokio::test]
async fn given_closed_port_when_probing_then_false() {
    assert!(!is_port_open("127.0.0.1", 12345, Duration::from_millis(100)).await);
    assert!(!is_chrome_cdp("127.0.0.1", 12345).await);
}

#[tokio::test(flavor = "current_thread")]
async fn given_current_thread_runtime_when_probing_then_no_deadlock() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;
    assert!(is_chrome_cdp("127.0.0.1", mock.port).await);
}

#[tokio::test]
async fn given_ipv6_localhost_when_probing_then_no_panic() {
    // just make sure it parses ::1 without panicking
    let _ = is_chrome_cdp("::1", 12345).await;
    let _ = is_port_open("::1", 12345, Duration::from_millis(50)).await;
}
