//! Fake Chrome binary used as a test double for `process.rs`.
//!
//! Compiled into the workspace as `fake_chrome_helper` (regular binary in
//! `src/bin/`) and invoked via `env!("CARGO_BIN_EXE_fake_chrome_helper")` from
//! the integration test suite.
//!
//! Behaviour is selected through the `FAKE_CHROME_MODE` env var:
//!
//! - `serve` (default): binds a TCP listener on `127.0.0.1:<port>` (where
//!   `<port>` comes from `--remote-debugging-port`, `0` = ephemeral). After
//!   binding, writes `<user-data-dir>/DevToolsActivePort` with the actual
//!   port. Then loops accepting (and discarding) incoming connections until
//!   killed. SIGTERM terminates the process (default behaviour).
//! - `serve_singleton`: like `serve`, but before binding it checks for
//!   `<user-data-dir>/SingletonLock`. If the lock exists the process exits
//!   with code 1 (simulates Chrome delegating to an existing instance).
//!   Otherwise it creates the lock **as a plain file**, proceeds as `serve`,
//!   and removes the lock on clean SIGTERM shutdown (unix).
//! - `serve_singleton_symlink`: like `serve_singleton`, but creates
//!   `SingletonLock` as a **dangling symlink** (target never exists), mimicking
//!   real Chrome >= 151 behaviour. Uses `symlink_metadata` to detect
//!   an existing lock so it correctly sees both plain files and dangling
//!   symlinks created by prior instances.
//! - `exit_immediately`: returns exit code 1 right away. Used to simulate the
//!   "Chrome delegated to an existing session" failure mode.
//! - `hang_no_port`: stays alive but never binds anything. Used to drive
//!   `StartupTimeout`.
//! - `ignore_sigterm` (unix only): installs a Tokio SIGTERM handler that
//!   swallows the signal so `terminate()` must escalate to SIGKILL.
//!
//! Additionally, every invocation writes the received argv (one per line) to
//! the path in `FAKE_CHROME_ARGS_LOG` so integration tests can assert on the
//! exact flags passed to the binary.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

/// Selects how `SingletonLock` is written by the fake.
#[derive(Clone, Copy)]
enum LockKind {
    /// No singleton lock behaviour; skip the lock check entirely.
    None,
    /// Write a plain file (mimics fake Chrome / Chrome < 151).
    PlainFile,
    /// Write a dangling symlink (mimics real Chrome >= 151).
    DanglingSymlink,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    if let Ok(log_path) = env::var("FAKE_CHROME_ARGS_LOG") {
        let _ = fs::write(&log_path, args.join("\n"));
    }

    let mode = env::var("FAKE_CHROME_MODE").unwrap_or_else(|_| "serve".to_string());

    match mode.as_str() {
        "exit_immediately" => ExitCode::from(1),
        "hang_no_port" => loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        },
        "ignore_sigterm" => serve(args, true, LockKind::None).await,
        "serve" => serve(args, false, LockKind::None).await,
        "serve_singleton" => serve(args, false, LockKind::PlainFile).await,
        "serve_singleton_symlink" => serve(args, false, LockKind::DanglingSymlink).await,
        _ => ExitCode::from(2),
    }
}

fn parse_args(args: &[String]) -> (u16, Option<PathBuf>) {
    let mut port: u16 = 0;
    let mut user_data_dir: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(v) = arg.strip_prefix("--remote-debugging-port=") {
            port = v.parse().unwrap_or(0);
        } else if arg == "--remote-debugging-port" {
            if let Some(next) = iter.next() {
                port = next.parse().unwrap_or(0);
            }
        } else if let Some(v) = arg.strip_prefix("--user-data-dir=") {
            user_data_dir = Some(PathBuf::from(v));
        } else if arg == "--user-data-dir"
            && let Some(next) = iter.next()
        {
            user_data_dir = Some(PathBuf::from(next));
        }
    }
    (port, user_data_dir)
}

async fn serve(args: Vec<String>, ignore_sigterm: bool, lock_kind: LockKind) -> ExitCode {
    let (port, user_data_dir) = parse_args(&args);

    if let Some(early) = setup_singleton_lock(lock_kind, &user_data_dir) {
        return early;
    }

    let bind = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(_) => return ExitCode::from(2),
    };
    let actual_port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(_) => return ExitCode::from(3),
    };

    if let Some(ref dir) = user_data_dir {
        let _ = fs::create_dir_all(dir);
        let file = dir.join("DevToolsActivePort");
        let _ = fs::write(&file, format!("{actual_port}\n"));
    }

    serve_loop(listener, ignore_sigterm, lock_kind, user_data_dir).await
}

/// Checks and creates the `SingletonLock` entry for singleton modes.
///
/// Returns `Some(ExitCode::from(1))` if a lock already exists (the new process
/// should delegate to the existing instance), or `None` if the lock was created
/// successfully (or if `lock_kind` is `LockKind::None`).
fn setup_singleton_lock(lock_kind: LockKind, user_data_dir: &Option<PathBuf>) -> Option<ExitCode> {
    let dir = match (matches!(lock_kind, LockKind::None), user_data_dir) {
        (false, Some(d)) => d,
        _ => return None,
    };

    let lock = dir.join("SingletonLock");
    // Use `symlink_metadata` so that dangling symlinks (Chrome >= 151) are
    // detected as "lock exists" just like plain files.
    if fs::symlink_metadata(&lock).is_ok() {
        return Some(ExitCode::from(1));
    }

    let _ = fs::create_dir_all(dir);
    match lock_kind {
        LockKind::PlainFile => {
            let _ = fs::write(&lock, "");
        }
        LockKind::DanglingSymlink => {
            #[cfg(unix)]
            {
                // Create a symlink whose target (`nonexistent-target`) is never
                // created, mimicking Chrome >= 151.
                let _ = std::os::unix::fs::symlink("nonexistent-target", &lock);
            }
            #[cfg(not(unix))]
            {
                // On non-unix, fall back to a plain file so the binary still
                // compiles; the symlink-specific tests are unix-only.
                let _ = fs::write(&lock, "");
            }
        }
        LockKind::None => {}
    }
    None
}

/// Runs the main accept loop after the TCP listener is bound.
async fn serve_loop(
    listener: tokio::net::TcpListener,
    ignore_sigterm: bool,
    lock_kind: LockKind,
    user_data_dir: Option<PathBuf>,
) -> ExitCode {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return ExitCode::from(4),
            };
        loop {
            tokio::select! {
                biased;
                _ = sigterm.recv() => {
                    if ignore_sigterm {
                        // Swallow SIGTERM so the parent must escalate to SIGKILL.
                    } else {
                        if !matches!(lock_kind, LockKind::None)
                            && let Some(ref dir) = user_data_dir
                        {
                            let _ = fs::remove_file(dir.join("SingletonLock"));
                        }
                        return ExitCode::SUCCESS;
                    }
                }
                accept_res = listener.accept() => {
                    if accept_res.is_err() {
                        return ExitCode::from(5);
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ignore_sigterm;
        let _ = lock_kind;
        let _ = user_data_dir;
        loop {
            if listener.accept().await.is_err() {
                return ExitCode::from(5);
            }
        }
    }
}
