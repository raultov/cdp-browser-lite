use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Child;
use tokio::time::Instant;

use crate::config::{BrowserConfig, ProfileMode};
use crate::error::BrowserError;
use crate::probe::is_port_open;
use crate::profile::Profile;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const TERMINATE_POLL: Duration = Duration::from_millis(50);

/// Handle to a live Chrome process. Built via [`spawn`] and terminated with
/// [`ChromeProcess::terminate`] (or left to die in `Drop` as a safety net).
#[doc(hidden)]
pub struct ChromeProcess {
    child: Child,
    /// PID on Unix; on Windows this is the opaque handle returned by `Child::id`.
    pid: u32,
    keep_alive: bool,
}

impl std::fmt::Debug for ChromeProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeProcess")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl ChromeProcess {
    #[doc(hidden)]
    pub fn id(&self) -> u32 {
        self.pid
    }

    #[doc(hidden)]
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Portable stepped termination:
    /// - Unix: `SIGTERM` via `nix` → wait `grace` (non-blocking, via
    ///   `tokio::time::sleep`) → if still alive, `child.start_kill()`.
    /// - Windows: no `SIGTERM`; uses `child.start_kill()` directly.
    ///
    /// Returns `Ok(())` once the process has been collected. It is idempotent:
    /// if already terminated, it just completes the pending `wait()`.
    #[doc(hidden)]
    pub async fn terminate(&mut self, grace: Duration) -> Result<(), BrowserError> {
        if !self.is_running() {
            let _ = self.child.wait().await;
            return Ok(());
        }

        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            if self.pid != 0 {
                let _ = kill(Pid::from_raw(self.pid as i32), Signal::SIGTERM);
            }

            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.try_wait() {
                    let _ = self.child.wait().await;
                    return Ok(());
                }
                tokio::time::sleep(TERMINATE_POLL).await;
            }
        }

        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        Ok(())
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        if !self.keep_alive {
            // Safety net: the Child already carries `kill_on_drop(!keep_alive_on_drop)` (set in
            // `spawn`), but we request an explicit kill to not rely solely on the
            // flag. Synchronous best-effort: in `Drop` we cannot wait for the process.
            let _ = self.child.start_kill();
        }
    }
}

/// Spawn specification. `port` is passed to `--remote-debugging-port`
/// (0 = ephemeral). `env` allows injecting variables (useful for test doubles
/// like the fake chrome).
#[doc(hidden)]
pub struct SpawnSpec<'a> {
    pub path: &'a Path,
    pub port: u16,
    pub profile: &'a Profile,
    pub config: &'a BrowserConfig,
    pub env: Vec<(String, String)>,
}

/// Builds the Chrome command-line arguments (PURE — testable without spawn).
///
/// Order is stable (aligned with `start_instance` from chrome-debug-mcp):
/// 1. Base flags
/// 2. `--user-data-dir` unless `ProfileMode::UserDefault`
/// 3. `--enable-automation` + `--disable-infobars` (conditional)
/// 4. `--headless=new` (conditional)
/// 5. `--no-sandbox` (conditional)
/// 6. `--proxy-server=`, `--window-size=`, `--user-agent=` (conditional)
/// 7. User `extra_args` at the end (can override previous flags)
#[doc(hidden)]
pub fn build_args(config: &BrowserConfig, profile: &Profile, port: u16) -> Vec<String> {
    let mut args = Vec::new();

    args.push(format!("--remote-debugging-port={port}"));
    args.push("--no-first-run".to_string());
    args.push("--no-default-browser-check".to_string());
    args.push("--disable-session-crashed-bubble".to_string());
    args.push("--noerrdialogs".to_string());
    args.push("--disable-dev-shm-usage".to_string());

    if !matches!(profile.mode, ProfileMode::UserDefault)
        && let Some(dir) = &profile.dir
    {
        args.push(format!("--user-data-dir={}", dir.display()));
    }

    if config.enable_automation {
        args.push("--enable-automation".to_string());
        args.push("--disable-infobars".to_string());
    }

    if config.headless {
        args.push("--headless=new".to_string());
    }

    let no_sandbox = config
        .no_sandbox
        .unwrap_or_else(|| config.headless || std::env::var_os("CHROME_NO_SANDBOX").is_some());
    if no_sandbox {
        args.push("--no-sandbox".to_string());
    }

    if let Some(proxy) = &config.proxy {
        args.push(format!("--proxy-server={proxy}"));
    }

    if let Some((w, h)) = config.window_size {
        args.push(format!("--window-size={w},{h}"));
    }

    if let Some(ua) = &config.user_agent {
        args.push(format!("--user-agent={ua}"));
    }

    args.extend(config.extra_args.iter().cloned());

    args
}

/// Launches Chrome and waits for the port to become ready.
///
/// - Fixed `port`: polls `is_port_open(host, port)` every 200 ms.
/// - `port = 0`: polls `profile.read_devtools_active_port()` every 200 ms (Chrome
///   writes the real port to `<user-data-dir>/DevToolsActivePort`).
///
/// On each iteration it checks `child.try_wait()` to detect `EarlyExit` before
/// the port comes up.
#[doc(hidden)]
pub async fn spawn(spec: SpawnSpec<'_>) -> Result<(ChromeProcess, u16), BrowserError> {
    let args = build_args(spec.config, spec.profile, spec.port);

    let mut command = tokio::process::Command::new(spec.path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(!spec.config.keep_alive_on_drop);

    for (k, v) in &spec.env {
        command.env(k, v);
    }

    let mut child = command
        .spawn()
        .map_err(|source| BrowserError::SpawnFailed {
            path: spec.path.to_path_buf(),
            source,
        })?;

    let pid = child.id().unwrap_or(0);

    let startup_timeout = spec.config.startup_timeout;
    let budget = StartupBudget {
        deadline: Instant::now() + startup_timeout,
        timeout: startup_timeout,
    };
    let host = spec.config.host.clone();

    let effective_port = if spec.port == 0 {
        wait_for_ephemeral_port(&mut child, spec.profile, &host, budget).await?
    } else {
        wait_for_fixed_port(&mut child, &host, spec.port, budget).await?
    };

    Ok((
        ChromeProcess {
            child,
            pid,
            keep_alive: spec.config.keep_alive_on_drop,
        },
        effective_port,
    ))
}

/// Time budget for the startup poll loops: an absolute `deadline` plus the
/// original `timeout` carried through for the `StartupTimeout` error message.
#[derive(Clone, Copy)]
struct StartupBudget {
    deadline: Instant,
    timeout: Duration,
}

async fn wait_for_ephemeral_port(
    child: &mut Child,
    profile: &Profile,
    host: &str,
    budget: StartupBudget,
) -> Result<u16, BrowserError> {
    loop {
        if let Some(_status) = child.try_wait().map_err(BrowserError::Io)? {
            let hint = early_exit_hint(&profile.mode);
            return Err(BrowserError::EarlyExit { hint });
        }
        if let Some(port) = profile.read_devtools_active_port()
            && is_port_open(host, port, PROBE_TIMEOUT).await
        {
            return Ok(port);
        }
        if Instant::now() >= budget.deadline {
            let _ = child.start_kill();
            return Err(BrowserError::StartupTimeout {
                timeout: budget.timeout,
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_fixed_port(
    child: &mut Child,
    host: &str,
    port: u16,
    budget: StartupBudget,
) -> Result<u16, BrowserError> {
    loop {
        if let Some(_status) = child.try_wait().map_err(BrowserError::Io)? {
            let hint = String::new();
            return Err(BrowserError::EarlyExit { hint });
        }
        if is_port_open(host, port, PROBE_TIMEOUT).await {
            return Ok(port);
        }
        if Instant::now() >= budget.deadline {
            let _ = child.start_kill();
            return Err(BrowserError::StartupTimeout {
                timeout: budget.timeout,
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn early_exit_hint(mode: &ProfileMode) -> String {
    if matches!(mode, ProfileMode::UserDefault) {
        " (close existing Chrome windows or pass an explicit --user-data-dir via ProfileMode::Persistent)".to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BrowserConfig, LaunchMode, ProfileMode};
    use crate::profile::Profile;

    fn base_config() -> BrowserConfig {
        BrowserConfig::builder().mode(LaunchMode::LaunchNew).build()
    }

    fn has_arg(args: &[String], needle: &str) -> bool {
        args.iter().any(|a| a == needle)
    }

    fn has_arg_prefix(args: &[String], prefix: &str) -> bool {
        args.iter().any(|a| a.starts_with(prefix))
    }

    fn arg_pos(args: &[String], needle: &str) -> Option<usize> {
        args.iter().position(|a| a == needle)
    }

    // Feature: Argument construction ──────────────────────────────────────

    #[test]
    fn given_default_config_when_building_args_then_base_flags_and_user_data_dir() {
        let cfg = base_config();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        assert!(has_arg(&args, "--no-first-run"));
        assert!(has_arg(&args, "--no-default-browser-check"));
        assert!(has_arg(&args, "--disable-session-crashed-bubble"));
        assert!(has_arg(&args, "--noerrdialogs"));
        assert!(has_arg(&args, "--disable-dev-shm-usage"));
        assert!(has_arg(&args, "--remote-debugging-port=9222"));
        assert!(
            has_arg_prefix(&args, "--user-data-dir="),
            "Ephemeral profile must inject --user-data-dir, got: {args:?}"
        );
        assert!(!has_arg(&args, "--headless=new"));
        assert!(!has_arg(&args, "--no-sandbox"));
        assert!(!has_arg(&args, "--enable-automation"));
        assert!(!has_arg_prefix(&args, "--proxy-server="));
        assert!(!has_arg_prefix(&args, "--window-size="));
        assert!(!has_arg_prefix(&args, "--user-agent="));
    }

    #[test]
    fn given_headless_when_building_args_then_headless_new_and_no_sandbox() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .headless(true)
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 0);

        assert!(has_arg(&args, "--headless=new"));
        assert!(
            has_arg(&args, "--no-sandbox"),
            "headless must imply --no-sandbox by default, got: {args:?}"
        );
    }

    #[test]
    fn given_user_default_profile_when_building_args_then_user_data_dir_omitted() {
        let cfg = base_config();
        let profile = Profile::prepare(&ProfileMode::UserDefault).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        assert!(
            !has_arg_prefix(&args, "--user-data-dir="),
            "UserDefault must omit --user-data-dir, got: {args:?}"
        );
    }

    #[test]
    fn given_proxy_window_size_and_user_agent_when_building_args_then_present() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .proxy("http://p:8080")
            .window_size(1280, 800)
            .user_agent("ua/1.0")
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        assert!(has_arg(&args, "--proxy-server=http://p:8080"));
        assert!(has_arg(&args, "--window-size=1280,800"));
        assert!(has_arg(&args, "--user-agent=ua/1.0"));
    }

    #[test]
    fn given_user_args_when_building_args_then_appended_last_and_preserved_literally() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .arg("--foo=1")
            .arg("--weird-flag with spaces")
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        let foo_pos = arg_pos(&args, "--foo=1").expect("--foo=1 must be present");
        let weird_pos = arg_pos(&args, "--weird-flag with spaces")
            .expect("weird arg with spaces must be preserved verbatim");
        assert!(
            foo_pos > 0,
            "--foo=1 must not be at position 0 (port flag goes first), got {foo_pos}"
        );
        assert!(
            weird_pos > foo_pos,
            "user args must be in insertion order, foo@{foo_pos} weird@{weird_pos}"
        );
        assert_eq!(args[args.len() - 2], "--foo=1", "user args must come last");
        assert_eq!(
            args[args.len() - 1],
            "--weird-flag with spaces",
            "user args must come last"
        );
    }

    #[test]
    fn given_port_zero_when_building_args_then_remote_debugging_port_is_zero() {
        let cfg = base_config();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 0);

        assert!(
            has_arg(&args, "--remote-debugging-port=0"),
            "port 0 must be passed verbatim (Chrome picks ephemeral), got: {args:?}"
        );
    }

    #[test]
    fn given_enable_automation_when_building_args_then_includes_automation_flags() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .enable_automation(true)
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        assert!(has_arg(&args, "--enable-automation"));
        assert!(has_arg(&args, "--disable-infobars"));
    }

    #[test]
    fn given_persistent_profile_when_building_args_then_user_data_dir_matches_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let cfg = base_config();
        let profile = Profile::prepare(&ProfileMode::Persistent(dir.clone())).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        let udd = args
            .iter()
            .find(|a| a.starts_with("--user-data-dir="))
            .expect("--user-data-dir must be set for Persistent");
        assert!(
            udd.contains(dir.to_str().unwrap()),
            "--user-data-dir must point to the persistent dir, got: {udd}"
        );
    }

    #[test]
    fn given_explicit_no_sandbox_false_when_not_headless_then_no_sandbox_omitted() {
        // Override disabling the default (which would be true because
        // headless=false, but CHROME_NO_SANDBOX could be set in the test
        // runner's environment). Force the override to false without relying
        // on the env.
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .headless(false)
            .no_sandbox(false)
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        assert!(
            !has_arg(&args, "--no-sandbox"),
            "explicit no_sandbox(false) without env must omit the flag, got: {args:?}"
        );
    }

    #[test]
    fn given_user_args_when_building_args_then_can_override_base_flags() {
        // PLAN §5 F5 comment: "user args at the end (can override)". Verify
        // that the ordering allows it (Chrome applies the last occurrence).
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .arg("--remote-debugging-port=55555")
            .build();
        let profile = Profile::prepare(&ProfileMode::Ephemeral).unwrap();

        let args = build_args(&cfg, &profile, 9222);

        let first_pos = arg_pos(&args, "--remote-debugging-port=9222")
            .expect("configured port must appear first");
        let user_pos = arg_pos(&args, "--remote-debugging-port=55555")
            .expect("user override must appear last");
        assert!(user_pos > first_pos, "user override must come last");
    }

    // Feature: early-exit hint ───────────────────────────────────────────

    #[test]
    fn given_user_default_mode_when_computing_hint_then_mentions_existing_chrome() {
        let hint = early_exit_hint(&ProfileMode::UserDefault);
        assert!(
            hint.to_lowercase().contains("close") || hint.contains("Chrome"),
            "UserDefault hint must guide the user to close existing windows, got: {hint:?}"
        );
    }

    #[test]
    fn given_ephemeral_mode_when_computing_hint_then_empty() {
        let hint = early_exit_hint(&ProfileMode::Ephemeral);
        assert!(
            hint.is_empty(),
            "ephemeral hint must be empty, got: {hint:?}"
        );
    }

    #[test]
    fn given_persistent_mode_when_computing_hint_then_empty() {
        let hint = early_exit_hint(&ProfileMode::Persistent(std::path::PathBuf::from("/tmp/p")));
        assert!(
            hint.is_empty(),
            "persistent hint must be empty, got: {hint:?}"
        );
    }
}
