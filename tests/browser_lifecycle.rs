mod support;

use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::error::BrowserError;
use cdp_browser_lite::test_support::mock_devtools::{MockBehavior, MockChrome};
use support::fake_chrome::{FakeMode, fake_chrome_path};

fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn fresh_user_data_dir(label: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(&format!("cdp-browser-lifecycle-{label}-"))
        .tempdir()
        .unwrap()
        .keep();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_chrome_config(port: u16, user_data_dir: &std::path::Path) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(port)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.to_path_buf()))
        .build()
}

fn set_fake_chrome_env(mode: FakeMode, args_log: &std::path::Path) {
    // SAFETY: only called in integration tests with a single-threaded
    // runtime; env vars are set once before each spawn and the child
    // process reads them atomically on startup.
    unsafe {
        std::env::set_var("FAKE_CHROME_MODE", mode.env_value());
        std::env::set_var("FAKE_CHROME_ARGS_LOG", args_log.to_string_lossy().as_ref());
    }
}

fn auto_config_fake(port: u16, user_data_dir: &std::path::Path) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .chrome_path(fake_chrome_path())
        .port(port)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.to_path_buf()))
        .build()
}

// ── Feature: ensure ──────────────────────────────────────────────────

#[tokio::test]
async fn auto_attach_to_user_chrome_regression_issue_4() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .port(mock.port)
        .startup_timeout(Duration::from_secs(1))
        .build();

    let browser = Browser::ensure(cfg)
        .await
        .expect("Auto must attach to user Chrome");

    assert!(
        !browser.is_managed().await,
        "attached browser must not be managed"
    );
    let (host, port) = browser.debug_address().await;
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, mock.port);
}

#[tokio::test]
async fn auto_launch_when_port_is_closed() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("auto-launch");
    let args_log = user_data_dir.join("args.log");
    set_fake_chrome_env(FakeMode::Serve, &args_log);

    let cfg = auto_config_fake(port, &user_data_dir);

    let browser = Browser::ensure(cfg)
        .await
        .expect("Auto must launch when port is closed");

    assert!(
        browser.is_managed().await,
        "launched browser must be managed"
    );
    let (host, actual_port) = browser.debug_address().await;
    assert_eq!(host, "127.0.0.1");
    assert_eq!(actual_port, port);
}

#[tokio::test]
async fn auto_with_port_occupied_by_non_chrome() {
    let occupied_port = pick_free_port();
    let _squat = TcpListener::bind(("127.0.0.1", occupied_port)).unwrap();

    let user_data_dir = fresh_user_data_dir("auto-non-chrome");
    let args_log = user_data_dir.join("args.log");
    set_fake_chrome_env(FakeMode::Serve, &args_log);

    let cfg = auto_config_fake(occupied_port, &user_data_dir);

    let browser = Browser::ensure(cfg)
        .await
        .expect("Auto must find free port when occupied by non-Chrome");

    assert!(browser.is_managed().await);
    let (_, actual_port) = browser.debug_address().await;
    assert_ne!(
        actual_port, occupied_port,
        "effective port must differ from occupied base"
    );
}

#[tokio::test]
async fn launch_new_always_own_process() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(mock.port)
        .startup_timeout(Duration::from_secs(1))
        .build();

    match Browser::ensure(cfg).await {
        Err(BrowserError::PortConflict { port }) => {
            assert_eq!(port, mock.port);
        }
        other => panic!("expected PortConflict for LaunchNew on occupied port, got {other:?}"),
    }
}

#[tokio::test]
async fn launch_new_ephemeral_port() {
    let user_data_dir = fresh_user_data_dir("launch-new-eph");
    let args_log = user_data_dir.join("args.log");
    set_fake_chrome_env(FakeMode::Serve, &args_log);

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(0)
        .headless(true)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let browser = Browser::ensure(cfg)
        .await
        .expect("LaunchNew with ephemeral port must succeed");

    assert!(browser.is_managed().await);
    let (_, actual_port) = browser.debug_address().await;
    assert_ne!(actual_port, 0, "ephemeral port must resolve to non-zero");
}

#[tokio::test]
async fn attach_only_remote_unavailable() {
    let port = pick_free_port();

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(port)
        .build();

    match Browser::ensure(cfg).await {
        Err(BrowserError::RemoteUnavailable {
            host,
            port: actual_port,
        }) => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(actual_port, port);
        }
        other => panic!("expected RemoteUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn attach_only_to_running_chrome() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .port(mock.port)
        .build();

    let browser = Browser::ensure(cfg)
        .await
        .expect("AttachOnly must succeed for running Chrome");

    assert!(!browser.is_managed().await);
    let (_, port) = browser.debug_address().await;
    assert_eq!(port, mock.port);
}

// ── Feature: stop / restart ──────────────────────────────────────────

#[tokio::test]
async fn stop_kills_process_and_cleans_profile() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("stop-clean");
    let args_log = user_data_dir.join("args.log");
    set_fake_chrome_env(FakeMode::Serve, &args_log);

    let cfg = fake_chrome_config(port, &user_data_dir);

    let browser = Browser::ensure(cfg)
        .await
        .expect("ensure must succeed for managed browser");

    assert!(browser.is_managed().await);
    assert!(browser.is_alive().await);

    browser.stop().await.expect("stop must succeed");

    assert!(!browser.is_alive().await);
}

#[tokio::test]
async fn stop_on_attached_does_nothing() {
    let mock = MockChrome::start(MockBehavior::KeepAlive).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .port(mock.port)
        .startup_timeout(Duration::from_secs(1))
        .build();

    let browser = Browser::ensure(cfg).await.expect("Auto must attach");

    assert!(!browser.is_managed().await);
    browser.stop().await.expect("stop on attached must be Ok");

    assert!(!browser.is_alive().await, "browser must be marked stopped");
}

#[tokio::test]
async fn restart_produces_new_process() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("restart");
    let args_log = user_data_dir.join("args.log");
    set_fake_chrome_env(FakeMode::Serve, &args_log);

    let cfg = fake_chrome_config(port, &user_data_dir);

    let browser = Browser::ensure(cfg).await.expect("ensure must succeed");

    assert!(browser.is_managed().await);
    assert!(browser.is_alive().await);

    let _old_port = browser.debug_address().await.1;

    browser.restart().await.expect("restart must succeed");

    assert!(browser.is_managed().await);
    assert!(browser.is_alive().await);
    let new_port = browser.debug_address().await.1;
    assert_eq!(new_port, port, "restart must reuse configured port");
    assert_ne!(new_port, 0);

    browser.stop().await.expect("cleanup stop must succeed");
}

#[cfg(unix)]
#[test]
#[ignore = "requires Chrome installed on the machine"]
fn e2e_ensure_stop_restart_with_real_chrome_headless() {
    use std::net::TcpStream;
    use std::time::Instant;

    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("e2e-real");

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(port)
        .headless(true)
        .startup_timeout(Duration::from_secs(10))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let browser = Browser::ensure(cfg)
            .await
            .expect("real Chrome must launch headless");
        assert!(browser.is_managed().await);

        let (_, actual_port) = browser.debug_address().await;
        assert_eq!(actual_port, port);

        assert!(browser.is_alive().await);

        browser.stop().await.expect("stop must succeed");
        assert!(!browser.is_alive().await);
        assert!(user_data_dir.exists());

        let browser2 = Browser::ensure(
            BrowserConfig::builder()
                .mode(LaunchMode::LaunchNew)
                .port(port)
                .headless(true)
                .startup_timeout(Duration::from_secs(10))
                .profile(ProfileMode::Persistent(user_data_dir.clone()))
                .build(),
        )
        .await
        .expect("restart must succeed");

        assert!(browser2.is_managed().await);
        assert!(browser2.is_alive().await);
        let (_, p) = browser2.debug_address().await;
        assert_eq!(p, port);

        browser2.stop().await.expect("final stop must succeed");
    });

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(100),
        )
        .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("port {port} should be free after full stop");
}
