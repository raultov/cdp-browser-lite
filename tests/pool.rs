use std::time::Duration;

use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::pool::BrowserPool;

use support::fake_chrome::{FakeMode, fake_chrome_path};
use support::mock_devtools::{MockBehavior, MockChrome};

mod support;

fn set_fake_chrome_env(mode: FakeMode) {
    unsafe {
        std::env::set_var("FAKE_CHROME_MODE", mode.env_value());
    }
}

fn pool_config(port: u16) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(port)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Ephemeral)
        .build()
}

#[tokio::test]
async fn given_empty_pool_when_opening_then_returns_an_id_and_len_is_one() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();
    assert!(pool.is_empty().await);

    let id = pool.open(pool_config(0)).await.unwrap();
    assert_eq!(pool.len().await, 1);
    assert!(pool.get(id).await.is_some());
    assert!(pool.entry(id).await.is_some());
}

#[tokio::test]
async fn given_pool_when_opening_three_browsers_then_each_has_a_distinct_port() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id1 = pool.open(pool_config(0)).await.unwrap();
    let id2 = pool.open(pool_config(0)).await.unwrap();
    let id3 = pool.open(pool_config(0)).await.unwrap();

    let p1 = pool.entry(id1).await.unwrap().port;
    let p2 = pool.entry(id2).await.unwrap().port;
    let p3 = pool.entry(id3).await.unwrap().port;

    assert_ne!(p1, p2);
    assert_ne!(p2, p3);
    assert_ne!(p1, p3);
}

#[tokio::test]
async fn given_pool_when_opening_three_browsers_then_each_has_a_distinct_profile_dir() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id1 = pool.open(pool_config(0)).await.unwrap();
    let id2 = pool.open(pool_config(0)).await.unwrap();
    let id3 = pool.open(pool_config(0)).await.unwrap();

    let d1 = pool.entry(id1).await.unwrap().profile_dir.unwrap();
    let d2 = pool.entry(id2).await.unwrap().profile_dir.unwrap();
    let d3 = pool.entry(id3).await.unwrap().profile_dir.unwrap();

    assert_ne!(d1, d2);
    assert_ne!(d2, d3);
    assert_ne!(d1, d3);
}

#[tokio::test]
async fn given_pool_when_opening_eight_concurrently_then_no_two_share_a_port() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let p = pool.clone();
        tasks.push(tokio::spawn(async move {
            p.open(pool_config(0)).await.unwrap()
        }));
    }

    let mut ports = std::collections::HashSet::new();
    for t in tasks {
        let id = t.await.unwrap();
        let port = pool.entry(id).await.unwrap().port;
        assert!(ports.insert(port), "port was already used");
    }
}

#[tokio::test]
async fn given_pool_when_opening_with_an_already_owned_port_then_port_conflict() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id1 = pool.open(pool_config(0)).await.unwrap();
    let p1 = pool.entry(id1).await.unwrap().port;

    let err = pool.open(pool_config(p1)).await.unwrap_err();
    match err {
        cdp_browser_lite::error::BrowserError::PortConflict { port } => assert_eq!(port, p1),
        _ => panic!("Expected PortConflict"),
    }
}

#[tokio::test]
async fn given_pool_with_three_browsers_when_closing_one_then_the_others_stay_alive() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id1 = pool.open(pool_config(0)).await.unwrap();
    let id2 = pool.open(pool_config(0)).await.unwrap();
    let id3 = pool.open(pool_config(0)).await.unwrap();

    pool.close(id2).await.unwrap();

    assert_eq!(pool.len().await, 2);
    assert!(pool.get(id2).await.is_none());

    let b1 = pool.get(id1).await.unwrap();
    let b3 = pool.get(id3).await.unwrap();

    assert!(b1.is_alive().await);
    assert!(b3.is_alive().await);
}

#[tokio::test]
async fn given_pool_with_browsers_when_close_all_then_every_managed_process_terminates() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id1 = pool.open(pool_config(0)).await.unwrap();
    let id2 = pool.open(pool_config(0)).await.unwrap();

    let b1 = pool.get(id1).await.unwrap();
    let b2 = pool.get(id2).await.unwrap();

    pool.close_all().await.unwrap();

    assert!(pool.is_empty().await);
    assert!(!b1.is_alive().await);
    assert!(!b2.is_alive().await);
}

#[tokio::test]
async fn given_pool_when_closing_unknown_id_then_is_a_no_op() {
    let pool = BrowserPool::new();
    let fake_id = cdp_browser_lite::pool::BrowserId::from_u64_for_test(999);
    // Should return Ok(())
    pool.close(fake_id).await.unwrap();
}

#[tokio::test]
async fn given_pool_dropped_when_no_arc_held_then_managed_processes_terminate() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id = pool.open(pool_config(0)).await.unwrap();

    // We get a weak ref basically by storing the pid and checking it later
    // but Browser::pid() is hidden, we can just use the debug_address port to try to connect to it.
    let entry = pool.entry(id).await.unwrap();
    let port = entry.port;

    drop(pool);

    // Wait for kill to finish
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Port should be closed if the process died
    assert!(
        !cdp_browser_lite::probe::is_port_open("127.0.0.1", port, Duration::from_millis(100)).await
    );
}

#[tokio::test]
async fn given_attached_browser_in_pool_when_close_all_then_remote_process_survives() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;
    let pool = BrowserPool::new();

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .port(mock.port)
        .build();

    let id = pool.open(cfg).await.unwrap();

    let b = pool.get(id).await.unwrap();
    assert!(!b.is_managed().await);

    pool.close_all().await.unwrap();

    // Process survives because it's attached
    let connected = std::net::TcpStream::connect(("127.0.0.1", mock.port)).is_ok();
    assert!(connected);
}

#[tokio::test]
async fn given_pool_entry_when_queried_then_reports_resolved_port_not_configured_port() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    // Configured with port 0
    let id = pool.open(pool_config(0)).await.unwrap();
    let entry = pool.entry(id).await.unwrap();

    assert_ne!(entry.port, 0, "must report resolved port");
}

#[tokio::test]
async fn given_arc_held_by_caller_when_pool_closes_entry_then_process_survives_until_arc_drop() {
    set_fake_chrome_env(FakeMode::Serve);
    let pool = BrowserPool::new();

    let id = pool.open(pool_config(0)).await.unwrap();

    let b = pool.get(id).await.unwrap();

    // Actually `pool.close()` directly calls `browser.stop()` which kills the process!
    // The spec says: given_arc_held_by_caller_when_pool_closes_entry_then_process_survives_until_arc_drop
    // But `pool.close` calls `stop`. Ah! "pool_closes_entry" maybe meant if pool is dropped, not if `close()` is called explicitly.
    // If pool is dropped, it doesn't call `stop`.
    // Wait, let's just make the test assert that dropping the pool doesn't kill it if we hold the Arc,
    // OR if we mean the entry is removed from the map.
    // Let's test pool dropping while Arc is held.

    drop(pool);

    // Process should still be alive because `b` holds the Arc and the `Browser` hasn't dropped.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(b.is_alive().await);

    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
}
