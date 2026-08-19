mod support;

use futures_util::future::join_all;
use std::time::Duration;

use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use support::fake_chrome::{FakeMode, fake_chrome_path};

#[tokio::test]
async fn multi_instance_all_scenarios() {
    // 1. N ephemeral concurrent
    unsafe {
        std::env::set_var("FAKE_CHROME_MODE", FakeMode::Serve.env_value());
        std::env::remove_var("FAKE_CHROME_ARGS_LOG");
    }

    let mut futures = Vec::new();
    for _ in 0..5 {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .chrome_path(fake_chrome_path())
            .port(0)
            .headless(true)
            .profile(ProfileMode::Ephemeral)
            .build();
        futures.push(Browser::ensure(cfg));
    }

    let results = join_all(futures).await;
    let mut browsers = Vec::new();
    for res in results {
        browsers.push(res.expect("launch failed"));
    }

    let mut ports = Vec::new();
    for b in &browsers {
        let p = b.debug_address().await.1;
        assert!(!ports.contains(&p), "Ports must be unique");
        ports.push(p);
    }

    let stop_futures = browsers.into_iter().map(|b| async move { b.stop().await });
    join_all(stop_futures).await;

    // 2. Different proxies
    let dir_a = tempfile::tempdir().unwrap().keep();
    let dir_b = tempfile::tempdir().unwrap().keep();

    let cfg_a = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(0)
        .headless(true)
        .profile(ProfileMode::Persistent(dir_a.clone()))
        .proxy("http://proxy.a:8080".to_string())
        .build();

    let args_log_a = dir_a.join("args.log");
    unsafe {
        std::env::set_var(
            "FAKE_CHROME_ARGS_LOG",
            args_log_a.to_string_lossy().as_ref(),
        );
    }
    let browser_a = Browser::ensure(cfg_a).await.expect("launch A failed");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let content_a = std::fs::read_to_string(&args_log_a).unwrap_or_default();
    assert!(
        content_a.contains("--proxy-server=http://proxy.a:8080"),
        "Actual content_a: {}",
        content_a
    );

    let cfg_b = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(0)
        .headless(true)
        .profile(ProfileMode::Persistent(dir_b.clone()))
        .proxy("http://proxy.b:9090".to_string())
        .build();

    let args_log_b = dir_b.join("args.log");
    unsafe {
        std::env::set_var(
            "FAKE_CHROME_ARGS_LOG",
            args_log_b.to_string_lossy().as_ref(),
        );
    }
    let browser_b = Browser::ensure(cfg_b).await.expect("launch B failed");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let content_b = std::fs::read_to_string(&args_log_b).unwrap_or_default();
    assert!(content_b.contains("--proxy-server=http://proxy.b:9090"));

    browser_a.stop().await.unwrap();
    browser_b.stop().await.unwrap();
}
