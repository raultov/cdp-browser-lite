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
