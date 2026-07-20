use std::io;
use std::path::PathBuf;
use std::time::Duration;

use cdp_lite::error::CdpError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BrowserError {
    #[error(
        "Chrome executable not found; searched: {searched:?}. Set CHROME_PATH or use chrome_path()"
    )]
    ExecutableNotFound { searched: Vec<PathBuf> },

    #[error("failed to spawn '{path}': {source}")]
    SpawnFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Chrome exited before opening the debugging port{hint}")]
    EarlyExit { hint: String },

    #[error("Chrome did not open the DevTools port within {timeout:?}")]
    StartupTimeout { timeout: Duration },

    #[error("port {port} is in use by a non-Chrome process")]
    PortConflict { port: u16 },

    #[error("no Chrome CDP endpoint reachable at {host}:{port}")]
    RemoteUnavailable { host: String, port: u16 },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("browser was stopped")]
    Stopped,

    #[error("profile error: {0}")]
    Profile(String),

    #[error(transparent)]
    Cdp(#[from] CdpError),

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(err: &BrowserError) -> String {
        format!("{err}")
    }

    #[test]
    fn executable_not_found_hint_mentions_chrome_path() {
        let err = BrowserError::ExecutableNotFound {
            searched: vec![PathBuf::from("/usr/bin/chrome")],
        };
        let msg = display(&err);
        assert!(
            msg.contains("CHROME_PATH"),
            "missing CHROME_PATH hint in: {msg}"
        );
        assert!(
            msg.contains("/usr/bin/chrome"),
            "missing searched path in: {msg}"
        );
    }

    #[test]
    fn spawn_failed_shows_path_and_underlying_io() {
        let err = BrowserError::SpawnFailed {
            path: PathBuf::from("/no/such/bin"),
            source: io::Error::new(io::ErrorKind::NotFound, "nope"),
        };
        let msg = display(&err);
        assert!(
            msg.contains("/no/such/bin"),
            "missing binary path in: {msg}"
        );
        let source = std::error::Error::source(&err)
            .map(|s| format!("{s}"))
            .unwrap_or_default();
        assert!(
            source.contains("nope"),
            "missing underlying IO message in chain: {source}"
        );
    }

    #[test]
    fn early_exit_message_has_hole_for_hint() {
        let err = BrowserError::EarlyExit {
            hint: " (close existing Chrome windows)".to_string(),
        };
        let msg = display(&err);
        assert!(msg.contains("Chrome"), "missing Chrome context: {msg}");
        assert!(
            msg.contains("debugging port"),
            "missing port context: {msg}"
        );
        assert!(
            msg.contains("close existing Chrome windows"),
            "missing hint text: {msg}"
        );
    }

    #[test]
    fn early_exit_with_empty_hint_omits_blank_parentheses() {
        let err = BrowserError::EarlyExit {
            hint: String::new(),
        };
        let msg = display(&err);
        assert!(!msg.contains("()"), "unexpected empty parens in: {msg}");
    }

    #[test]
    fn startup_timeout_mentions_devtools_port() {
        let err = BrowserError::StartupTimeout {
            timeout: Duration::from_secs(10),
        };
        let msg = display(&err);
        assert!(
            msg.contains("DevTools port"),
            "missing DevTools context: {msg}"
        );
    }

    #[test]
    fn port_conflict_mentions_port_number() {
        let err = BrowserError::PortConflict { port: 9222 };
        let msg = display(&err);
        assert!(msg.contains("9222"), "missing port: {msg}");
    }

    #[test]
    fn remote_unavailable_mentions_host_and_port() {
        let err = BrowserError::RemoteUnavailable {
            host: "10.0.0.5".to_string(),
            port: 9222,
        };
        let msg = display(&err);
        assert!(msg.contains("10.0.0.5"), "missing host: {msg}");
        assert!(msg.contains("9222"), "missing port: {msg}");
    }

    #[test]
    fn invalid_config_carries_message() {
        let err = BrowserError::InvalidConfig("bad".to_string());
        let msg = display(&err);
        assert!(msg.contains("invalid"), "missing 'invalid' word: {msg}");
        assert!(msg.contains("bad"), "missing nested message: {msg}");
    }

    #[test]
    fn stopped_is_self_describing() {
        let msg = display(&BrowserError::Stopped);
        assert!(
            msg.to_lowercase().contains("stopped"),
            "missing 'stopped' word: {msg}"
        );
    }

    #[test]
    fn profile_carries_message() {
        let err = BrowserError::Profile("cannot create dir".to_string());
        let msg = display(&err);
        assert!(msg.contains("profile"), "missing 'profile' word: {msg}");
        assert!(msg.contains("cannot create dir"), "missing detail: {msg}");
    }

    #[test]
    fn test_error_display_messages_are_actionable() {
        let cases = [
            BrowserError::ExecutableNotFound { searched: vec![] },
            BrowserError::EarlyExit {
                hint: String::new(),
            },
            BrowserError::InvalidConfig("test".into()),
            BrowserError::Stopped,
            BrowserError::Profile("test".into()),
        ];
        for err in cases {
            let msg = display(&err);
            assert!(!msg.trim().is_empty(), "{err:?} produced empty display");
        }
    }

    #[test]
    fn from_io_error() {
        let io_err = io::Error::other("boom");
        let err: BrowserError = io_err.into();
        assert!(matches!(err, BrowserError::Io(_)));
    }

    #[test]
    fn from_cdp_error() {
        let cdp_err = CdpError::Disconnected;
        let err: BrowserError = cdp_err.into();
        assert!(matches!(err, BrowserError::Cdp(_)));
    }
}
