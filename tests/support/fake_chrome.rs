//! Support for the fake chrome executable.
//!
//! Exposes:
//! - [`fake_chrome_path`]: path to the compiled binary.
//! - [`FakeMode`]: behaviour mode selected via `FAKE_CHROME_MODE`.
//! - [`FakeChromeSpec`] + [`build_command`]: idiomatic way to invoke the fake
//!   from the integration tests.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// Behaviour modes of the fake chrome binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Each test uses a subset.
pub enum FakeMode {
    /// Binds the requested port, writes `DevToolsActivePort`, accepts connections.
    Serve,
    /// Exits with code 1 immediately (simulates Chrome delegating to an existing session).
    ExitImmediately,
    /// Stays alive but never opens a port (for `StartupTimeout`).
    HangNoPort,
    /// Unix: installs a Tokio handler that ignores SIGTERM to force the
    /// escalation to SIGKILL in `terminate()`.
    IgnoreSigterm,
}

impl FakeMode {
    pub fn env_value(self) -> &'static str {
        match self {
            FakeMode::Serve => "serve",
            FakeMode::ExitImmediately => "exit_immediately",
            FakeMode::HangNoPort => "hang_no_port",
            FakeMode::IgnoreSigterm => "ignore_sigterm",
        }
    }
}

/// Specification for building a command against the fake chrome.
#[allow(dead_code)]
pub struct FakeChromeSpec<'a> {
    pub mode: FakeMode,
    /// Path to the file where the fake writes the received argv.
    pub args_log: &'a Path,
    /// `--user-data-dir` that the fake will see (None for `UserDefault`).
    pub user_data_dir: Option<&'a Path>,
}

/// Absolute path to the `fake_chrome_helper` binary, resolved at
/// compile time via `CARGO_BIN_EXE_*`.
#[allow(dead_code)]
pub fn fake_chrome_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake_chrome_helper"))
}

/// Builds a `tokio::process::Command` ready for `spawn()`, with the mode,
/// the args log, and the `stdio` redirected to null.
#[allow(dead_code)]
pub fn build_command(spec: &FakeChromeSpec<'_>) -> Command {
    let mut cmd = Command::new(fake_chrome_path());
    cmd.env("FAKE_CHROME_MODE", spec.mode.env_value());
    cmd.env("FAKE_CHROME_ARGS_LOG", spec.args_log);
    if let Some(dir) = spec.user_data_dir {
        cmd.env("FAKE_CHROME_USER_DATA_DIR", dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}
