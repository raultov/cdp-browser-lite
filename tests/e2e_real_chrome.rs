use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::discovery::discover_default;
#[test]
#[ignore = "requires Chrome installed on the machine"]
fn given_real_machine_when_discovering_then_finds_executable() {
    let path = discover_default().expect("should find Chrome on this machine");
    assert!(path.is_file(), "discovered path must be a file: {path:?}");
}

/// B3 scenario on real Chrome >= 151:
///
/// 1. Launches a managed Chrome on a configured port.
/// 2. Asserts `config.profile.managed_lock_exists(port)` is `true` while
///    it runs (the `SingletonLock` may be a dangling symlink on Chrome >= 151).
/// 3. Asserts that a second `Browser::ensure` on the same base config resolves
///    a **different** port and a different profile directory, leaving the first
///    instance's lock untouched.
#[tokio::test]
#[ignore = "requires Chrome installed on the machine"]
async fn given_real_chrome_on_configured_port_when_second_manager_ensures_then_uses_different_port_and_profile()
 {
    let chrome_path = discover_default().expect("Chrome must be installed for this E2E test");

    let root = tempfile::Builder::new()
        .prefix("cdp-e2e-b3-")
        .tempdir()
        .expect("temp dir");

    let base_port: u16 = 19_300; // arbitrary fixed port for this test

    let profile = ProfileMode::PersistentPerPort {
        root: root.path().to_path_buf(),
        prefix: "profile-".to_string(),
    };

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .chrome_path(chrome_path.clone())
        .port(base_port)
        .headless(true)
        .profile(profile.clone())
        .build();

    // --- First instance ---
    let b1 = Browser::ensure(cfg)
        .await
        .expect("first managed Chrome must start");
    assert!(b1.is_managed().await, "b1 must be managed");

    let (_, p1) = b1.debug_address().await;

    // `SingletonLock` must be visible via symlink_metadata, even if Chrome wrote
    // it as a dangling symlink (Chrome >= 151).
    assert!(
        profile.managed_lock_exists(base_port),
        "managed_lock_exists must return true while Chrome is running (Chrome >= 151 B3 check)"
    );

    // --- Second instance, same base config ---
    let cfg2 = BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .chrome_path(chrome_path)
        .port(base_port)
        .headless(true)
        .profile(profile.clone())
        .build();

    let b2 = Browser::ensure(cfg2)
        .await
        .expect("second managed Chrome must start on a different port");
    assert!(b2.is_managed().await, "b2 must be managed");

    let (_, p2) = b2.debug_address().await;
    let dir1 = b1.profile_dir().await.expect("b1 must have a profile dir");
    let dir2 = b2.profile_dir().await.expect("b2 must have a profile dir");

    assert_ne!(p1, p2, "B3: second instance must use a different port");
    assert_ne!(
        dir1, dir2,
        "B3: second instance must use a different profile directory"
    );

    // First instance's lock must still exist.
    assert!(
        profile.managed_lock_exists(base_port),
        "first instance's SingletonLock must survive after second instance starts"
    );

    b1.stop().await.expect("b1 must stop cleanly");
    b2.stop().await.expect("b2 must stop cleanly");
}

/// Multi-tab scenario on real Chrome:
///
/// 1. Launches a managed headless Chrome on an ephemeral port.
/// 2. Opens three `data:` tabs and asserts each reports **its own** URL via
///    `Runtime.evaluate` — the only way to prove `sessionId` routing works
///    against a real browser (the mock cannot lie here).
/// 3. Closes one tab and asserts `list_tabs` shrinks by exactly one.
/// 4. Activates another tab without error.
/// 5. Stops the browser cleanly.
#[tokio::test]
#[ignore = "requires Chrome installed on the machine"]
async fn given_real_chrome_when_opening_three_tabs_then_each_navigates_independently() {
    let chrome_path = discover_default().expect("Chrome must be installed for this E2E test");

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(chrome_path)
        .port(0)
        .headless(true)
        .profile(ProfileMode::Ephemeral)
        .build();

    let browser = Browser::ensure(cfg)
        .await
        .expect("managed headless Chrome must start");

    let urls = [
        "data:text/html,<h1>tab-one</h1>",
        "data:text/html,<h1>tab-two</h1>",
        "data:text/html,<h1>tab-three</h1>",
    ];

    let mut tabs = Vec::new();
    for url in urls {
        tabs.push(browser.new_tab(url).await.expect("new_tab must succeed"));
    }

    for (tab, expected) in tabs.iter().zip(urls) {
        let resp = tab
            .send_raw_command(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "document.location.href",
                    "returnByValue": true,
                }),
            )
            .await
            .expect("Runtime.evaluate must succeed");
        let result = resp.result.unwrap_or_default();
        let actual = result
            .get("result")
            .and_then(|v| v.get("value"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            actual, expected,
            "each tab must be driven by its own session"
        );
    }

    let before = browser.list_tabs().await.expect("list_tabs").len();
    let closed = tabs.pop().expect("one tab to close");
    closed.close().await.expect("close must succeed");

    // Chrome removes the closed target asynchronously: poll briefly instead
    // of asserting on the very next list_tabs.
    let after = wait_for_tab_count(&browser, before - 1).await;
    assert_eq!(
        after,
        before - 1,
        "closing one tab must shrink the tab list by exactly one"
    );

    tabs.first()
        .expect("a tab remains")
        .activate()
        .await
        .expect("activate must succeed");

    browser.stop().await.expect("browser must stop cleanly");
}

/// Polls `list_tabs` until the count reaches `expected`, or returns the last
/// observed count after a bounded wait.
async fn wait_for_tab_count(browser: &Browser, expected: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let count = browser.list_tabs().await.expect("list_tabs").len();
        if count == expected || std::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
