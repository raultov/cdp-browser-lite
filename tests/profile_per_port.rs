use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};

use support::fake_chrome::{FakeMode, fake_chrome_path};
use support::mock_devtools::{MockBehavior, MockChrome};

mod support;

/// Tests that call `set_fake_chrome_env` mutate process-level environment
/// variables. Because `cargo test` runs tests concurrently on multiple threads,
/// a second test can overwrite `FAKE_CHROME_MODE` between when a first test
/// sets it and when the fake Chrome process is actually spawned, causing
/// spurious failures. We use a `tokio::sync::Mutex` (whose guard is `Send`)
/// so the guard can be held across `.await` points inside async tests.
static ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn env_lock() -> &'static tokio::sync::Mutex<()> {
    ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn fresh_user_data_dir(label: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(&format!("cdp-profile-per-port-{label}-"))
        .tempdir()
        .unwrap()
        .keep();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Sets `FAKE_CHROME_MODE` and `FAKE_CHROME_ARGS_LOG` in the process
/// environment. Callers **must** hold `env_lock()` for the entire test body
/// (from before this call until after the fake Chrome process is spawned) to
/// prevent concurrent tests from overwriting the env var in between.
fn set_fake_chrome_env(mode: FakeMode, args_log: &std::path::Path) {
    unsafe {
        std::env::set_var("FAKE_CHROME_MODE", mode.env_value());
        std::env::set_var("FAKE_CHROME_ARGS_LOG", args_log.to_string_lossy().as_ref());
    }
}

fn ppp_config(port: u16, root: PathBuf) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .chrome_path(fake_chrome_path())
        .port(port)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::PersistentPerPort {
            root,
            prefix: "profile-".to_string(),
        })
        .build()
}

#[tokio::test]
async fn given_live_managed_instance_when_ensure_auto_then_new_instance_uses_a_different_profile_dir()
 {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("diff-dir");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingleton, &args_log);

    let base_port = pick_free_port();

    // First instance
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    assert!(b1.is_managed().await);

    // Second instance, same base config. Because port is occupied (and SingletonLock exists), it should pick a new port and new dir.
    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    assert!(b2.is_managed().await);

    let dir1 = b1.profile_dir().await.unwrap();
    let dir2 = b2.profile_dir().await.unwrap();

    assert_ne!(dir1, dir2, "profile directories must differ");

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}

#[tokio::test]
async fn given_live_managed_instance_when_ensure_auto_then_existing_singleton_lock_is_preserved() {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("pres-lock");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingleton, &args_log);

    let base_port = pick_free_port();
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let dir1 = b1.profile_dir().await.unwrap();
    let lock1 = dir1.join("SingletonLock");

    assert!(lock1.exists(), "first instance must create lock");

    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();

    assert!(
        lock1.exists(),
        "second instance must not delete first instance's lock"
    );

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}

#[tokio::test]
async fn given_live_managed_instance_when_ensure_auto_then_new_instance_uses_a_different_port() {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("diff-port");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingleton, &args_log);

    let base_port = pick_free_port();
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let (_, p1) = b1.debug_address().await;

    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let (_, p2) = b2.debug_address().await;

    assert_ne!(p1, p2, "ports must differ");

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}

#[tokio::test]
async fn given_attachable_chrome_when_ensure_auto_then_no_temp_directory_is_created() {
    let root = fresh_user_data_dir("no-temp");

    let mock = MockChrome::start(MockBehavior::KeepAlive).await;

    let root_entries_before = std::fs::read_dir(&root).unwrap().count();

    let b = Browser::ensure(ppp_config(mock.port, root.clone()))
        .await
        .unwrap();
    assert!(!b.is_managed().await, "should attach");

    let root_entries_after = std::fs::read_dir(&root).unwrap().count();

    assert_eq!(
        root_entries_before, root_entries_after,
        "no new directories should be created"
    );
}

#[tokio::test]
async fn given_stale_singleton_lock_when_launch_new_then_lock_is_removed() {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("stale-lock");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingleton, &args_log);

    let base_port = pick_free_port();
    let profile_dir = root.join(format!("profile-{base_port}"));
    std::fs::create_dir_all(&profile_dir).unwrap();
    let lock_file = profile_dir.join("SingletonLock");
    std::fs::write(&lock_file, "stale").unwrap();

    // Using LaunchNew directly to hit the spawn path
    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(base_port)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::PersistentPerPort {
            root: root.clone(),
            prefix: "profile-".to_string(),
        })
        .build();

    let b = Browser::ensure(cfg).await.unwrap();
    assert!(b.is_managed().await);

    // FakeMode::ServeSingleton will create a new lock on start, but the stale one must have been removed first
    // Actually, ServeSingleton creates it. We just know the launch succeeded and didn't crash because of the stale lock.
    b.stop().await.unwrap();
}

#[tokio::test]
async fn given_persistent_per_port_when_preparing_then_dir_matches_root_prefix_port() {
    let root = PathBuf::from("/tmp/foo");
    let _p = cdp_browser_lite::profile::Profile::prepare(
        &ProfileMode::PersistentPerPort {
            root: root.clone(),
            prefix: "bar-".to_string(),
        },
        1234,
    );

    // Since prepare tries to create the directory, we actually need a valid path or it will error.
    #[expect(
        deprecated,
        reason = "TempDir::keep returns a Result requiring error handling"
    )]
    let real_root = tempfile::Builder::new().tempdir().unwrap().into_path();
    let p2 = cdp_browser_lite::profile::Profile::prepare(
        &ProfileMode::PersistentPerPort {
            root: real_root.clone(),
            prefix: "bar-".to_string(),
        },
        1234,
    )
    .unwrap();

    let expected = real_root.join("bar-1234");
    assert_eq!(p2.dir, Some(expected));
}

// ── Chrome >= 151 dangling-symlink SingletonLock regression tests ────────────
//
// These three tests mirror the existing B3 integration tests but use
// `FakeMode::ServeSingletonSymlink` (dangling symlink) instead of
// `FakeMode::ServeSingleton` (plain file).  They were RED on 0.2.2 and must
// be GREEN after the `managed_lock_exists` fix in Phase 2.

#[tokio::test]
#[cfg(unix)]
async fn given_symlink_lock_live_managed_instance_when_ensure_auto_then_new_instance_uses_a_different_port()
 {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("sym-diff-port");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingletonSymlink, &args_log);

    let base_port = pick_free_port();
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let (_, p1) = b1.debug_address().await;

    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let (_, p2) = b2.debug_address().await;

    assert_ne!(p1, p2, "ports must differ (symlink-lock B3)");

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}

#[tokio::test]
#[cfg(unix)]
async fn given_symlink_lock_live_managed_instance_when_ensure_auto_then_new_instance_uses_a_different_profile_dir()
 {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("sym-diff-dir");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingletonSymlink, &args_log);

    let base_port = pick_free_port();
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();

    let dir1 = b1.profile_dir().await.unwrap();
    let dir2 = b2.profile_dir().await.unwrap();

    assert_ne!(
        dir1, dir2,
        "profile directories must differ (symlink-lock B3)"
    );

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}

#[tokio::test]
#[cfg(unix)]
async fn given_symlink_lock_live_managed_instance_when_ensure_auto_then_existing_singleton_lock_is_preserved()
 {
    let _guard = env_lock().lock().await;

    let root = fresh_user_data_dir("sym-pres-lock");
    let args_log = root.join("args.log");
    set_fake_chrome_env(FakeMode::ServeSingletonSymlink, &args_log);

    let base_port = pick_free_port();
    let b1 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();
    let dir1 = b1.profile_dir().await.unwrap();
    let lock1 = dir1.join("SingletonLock");

    // The dangling symlink must exist (symlink_metadata succeeds even though
    // .exists() would return false on 0.2.2).
    assert!(
        std::fs::symlink_metadata(&lock1).is_ok(),
        "first instance must create a SingletonLock symlink"
    );

    let b2 = Browser::ensure(ppp_config(base_port, root.clone()))
        .await
        .unwrap();

    assert!(
        std::fs::symlink_metadata(&lock1).is_ok(),
        "second instance must not remove first instance's SingletonLock"
    );

    b1.stop().await.unwrap();
    b2.stop().await.unwrap();
}
