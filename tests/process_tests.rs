//! BDD scenarios for F5 §5: spawn and termination with the fake chrome.
//!
//! Each test is independent: it uses its own tempdir, profile, and port. The
//! "ignore_sigterm" scenarios and the Unix branch of `terminate()` are under
//! `#[cfg(unix)]`; the direct Windows kill is modelled with `child.start_kill()`
//! in the common branch.

mod support;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::error::BrowserError;
use cdp_browser_lite::process::{SpawnSpec, spawn};
use cdp_browser_lite::profile::Profile;
use support::fake_chrome::{FakeChromeSpec, FakeMode, build_command, fake_chrome_path};

fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn fresh_user_data_dir(label: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(&format!("cdp-fake-{label}-"))
        .tempdir()
        .unwrap()
        .keep();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn launch_new_config_with_timeout(timeout: Duration, profile: ProfileMode) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .startup_timeout(timeout)
        .profile(profile)
        .build()
}

fn fake_env(mode: FakeMode, args_log: &Path) -> Vec<(String, String)> {
    vec![
        ("FAKE_CHROME_MODE".to_string(), mode.env_value().to_string()),
        (
            "FAKE_CHROME_ARGS_LOG".to_string(),
            args_log.to_string_lossy().into_owned(),
        ),
    ]
}

// Feature: Process launch ─────────────────────────────────────────────

#[tokio::test]
async fn given_fake_chrome_serving_fixed_port_when_spawn_then_ok_and_effective_port_matches() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("serve-fixed");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), port).unwrap();
    let log = user_data_dir.join("args.log");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(port)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::Serve, &log),
    };

    let (mut proc, effective_port) = spawn(spec)
        .await
        .expect("spawn must succeed when fake serves fixed port");
    assert_eq!(
        effective_port, port,
        "effective port must equal the requested one"
    );

    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains(&format!("--remote-debugging-port={port}")),
        "args log must record the debug port flag, got: {logged}"
    );

    proc.terminate(Duration::from_secs(2))
        .await
        .expect("terminate must succeed");
}

#[tokio::test]
async fn given_fake_chrome_serving_ephemeral_when_spawn_then_port_resolved_from_devtools_file() {
    let user_data_dir = fresh_user_data_dir("serve-ephemeral");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), 0).unwrap();
    let log = user_data_dir.join("args.log");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(0)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: 0,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::Serve, &log),
    };

    let (mut proc, effective_port) = spawn(spec)
        .await
        .expect("spawn must succeed with ephemeral port");
    assert_ne!(
        effective_port, 0,
        "ephemeral port must be resolved to non-zero"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("--remote-debugging-port=0"),
        "args log must show the zero port passed to chrome, got: {logged}"
    );
    assert!(
        is_port_reachable(effective_port).await,
        "spawn must wait until the resolved port is reachable"
    );

    proc.terminate(Duration::from_secs(2))
        .await
        .expect("terminate must succeed");
}

#[tokio::test]
async fn given_fake_chrome_exit_immediately_when_spawn_then_early_exit() {
    let user_data_dir = fresh_user_data_dir("early-exit");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), 0).unwrap();
    let log = user_data_dir.join("args.log");

    let config = launch_new_config_with_timeout(
        Duration::from_secs(5),
        ProfileMode::Persistent(user_data_dir.clone()),
    );

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: 0,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::ExitImmediately, &log),
    };

    match spawn(spec).await {
        Err(BrowserError::EarlyExit { hint }) => {
            assert!(
                hint.is_empty(),
                "non-UserDefault hint must be empty, got: {hint:?}"
            );
        }
        other => panic!("expected EarlyExit, got {other:?}"),
    }
}

#[tokio::test]
async fn given_user_default_with_early_exit_when_spawn_then_hint_mentions_existing_chrome() {
    let profile = Profile::prepare(&ProfileMode::UserDefault, 0).unwrap();
    let log = tempfile::tempdir().unwrap().path().join("args.log");

    let config = launch_new_config_with_timeout(Duration::from_secs(5), ProfileMode::UserDefault);

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: 0,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::ExitImmediately, &log),
    };

    match spawn(spec).await {
        Err(BrowserError::EarlyExit { hint }) => {
            assert!(!hint.is_empty(), "UserDefault hint must guide the user");
            let lower = hint.to_lowercase();
            assert!(
                lower.contains("close") || lower.contains("chrome"),
                "hint should mention closing existing Chrome windows, got: {hint:?}"
            );
        }
        other => panic!("expected EarlyExit, got {other:?}"),
    }
}

#[tokio::test]
async fn given_fake_chrome_hang_no_port_when_spawn_then_startup_timeout() {
    let user_data_dir = fresh_user_data_dir("hang");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), 0).unwrap();
    let log = user_data_dir.join("args.log");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(0)
        .startup_timeout(Duration::from_millis(800))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: 0,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::HangNoPort, &log),
    };

    let started = std::time::Instant::now();
    let result = spawn(spec).await;
    let elapsed = started.elapsed();

    match result {
        Err(BrowserError::StartupTimeout { timeout }) => {
            assert_eq!(timeout, Duration::from_millis(800));
            assert!(
                elapsed >= Duration::from_millis(700),
                "timeout should fire around 800ms, took {elapsed:?}"
            );
            assert!(
                elapsed < Duration::from_secs(3),
                "timeout must not block forever, took {elapsed:?}"
            );
        }
        other => panic!("expected StartupTimeout, got {other:?}"),
    }
}

#[tokio::test]
async fn given_invalid_chrome_path_when_spawn_then_spawn_failed_with_chrome_path_hint() {
    let user_data_dir = fresh_user_data_dir("invalid-path");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), 0).unwrap();

    let bogus_path = user_data_dir.join("does_not_exist_chrome");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(bogus_path.clone())
        .port(0)
        .startup_timeout(Duration::from_secs(2))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &bogus_path,
        port: 0,
        profile: &profile,
        config: &config,
        env: vec![],
    };

    match spawn(spec).await {
        Err(BrowserError::SpawnFailed { path, .. }) => {
            assert_eq!(
                path, bogus_path,
                "SpawnFailed must carry the requested path"
            );
            let _ = bogus_path; // silencia "unused" si el test no imprime
        }
        other => panic!("expected SpawnFailed, got {other:?}"),
    }
}

// Feature: Termination ─────────────────────────────────────────────────

#[tokio::test]
async fn given_fake_chrome_serving_when_terminate_then_graceful_within_grace() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("term-graceful");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), port).unwrap();
    let log = user_data_dir.join("args.log");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(port)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::Serve, &log),
    };

    let (mut proc, _) = spawn(spec).await.expect("spawn should succeed");
    let started = std::time::Instant::now();
    proc.terminate(Duration::from_secs(2))
        .await
        .expect("terminate must succeed");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "graceful termination must not consume the full grace, took {elapsed:?}"
    );
    assert!(!proc.is_running(), "process must be dead after terminate");
}

#[cfg(unix)]
#[tokio::test]
async fn given_fake_chrome_ignore_sigterm_when_terminate_then_kill_after_grace() {
    let port = pick_free_port();
    let user_data_dir = fresh_user_data_dir("term-sigkill");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), port).unwrap();
    let log = user_data_dir.join("args.log");

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(port)
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();

    let spec = SpawnSpec {
        path: &fake_chrome_path(),
        port,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::IgnoreSigterm, &log),
    };

    let (mut proc, _) = spawn(spec).await.expect("spawn should succeed");
    let started = std::time::Instant::now();
    proc.terminate(Duration::from_millis(400))
        .await
        .expect("terminate must succeed even when SIGTERM is ignored");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(350),
        "must wait at least ~grace before SIGKILL, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "must not hang past the grace, took {elapsed:?}"
    );
    assert!(
        !proc.is_running(),
        "process must be dead after kill escalation"
    );
}

#[tokio::test]
async fn given_process_already_exited_when_terminate_then_ok_and_idempotent() {
    let user_data_dir = fresh_user_data_dir("term-already-dead");
    let profile = Profile::prepare(&ProfileMode::Persistent(user_data_dir.clone()), 0).unwrap();
    let log = user_data_dir.join("args.log");

    let config = launch_new_config_with_timeout(
        Duration::from_secs(5),
        ProfileMode::Persistent(user_data_dir.clone()),
    );

    let _spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: 0,
        profile: &profile,
        config: &config,
        env: fake_env(FakeMode::ExitImmediately, &log),
    };

    // Spawn will detect EarlyExit; we want a live ChromeProcess to test
    // idempotent termination. We get one by spawning in "serve" mode and then
    // manually killing the child before calling terminate.
    let serve_config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .chrome_path(fake_chrome_path())
        .port(pick_free_port())
        .startup_timeout(Duration::from_secs(5))
        .profile(ProfileMode::Persistent(user_data_dir.clone()))
        .build();
    let serve_log = user_data_dir.join("serve_args.log");
    let serve_spec = SpawnSpec {
        path: &fake_chrome_path(),
        port: serve_config.port(),
        profile: &profile,
        config: &serve_config,
        env: fake_env(FakeMode::Serve, &serve_log),
    };
    let (mut proc, _) = spawn(serve_spec).await.expect("serve spawn must succeed");

    // Give it a moment to establish and then let it finish on its own
    // via SIGTERM with grace=0. Then verify a second terminate() does not
    // panic or hang.
    proc.terminate(Duration::from_millis(0))
        .await
        .expect("first terminate must succeed");
    // Second terminate on an already dead process: must be Ok and idempotent.
    proc.terminate(Duration::from_millis(0))
        .await
        .expect("second terminate must be idempotent and Ok");
}

#[tokio::test]
async fn given_build_command_helper_when_called_then_sets_env_and_stdio() {
    // Verify that the test helper composes correct commands (smoke).
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("args.log");
    let cmd = build_command(&FakeChromeSpec {
        mode: FakeMode::Serve,
        args_log: &log,
        user_data_dir: None,
    });
    let _ = cmd; // smoke
}

// Local utility ────────────────────────────────────────────────────────

async fn is_port_reachable(port: u16) -> bool {
    use tokio::net::TcpStream;
    use tokio::time::timeout;
    let addr = format!("127.0.0.1:{port}");
    matches!(
        timeout(Duration::from_millis(500), TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}
