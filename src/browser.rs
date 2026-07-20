use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use cdp_lite::client::CdpClient;
use cdp_lite::error::CdpError;
use tokio::sync::Mutex as TokioMutex;

use crate::config::{BrowserConfig, LaunchMode};
use crate::discovery;
use crate::error::BrowserError;
use crate::ports;
use crate::probe;
use crate::process::{ChromeProcess, SpawnSpec, spawn};
use crate::profile::Profile;

const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const PORT_SEARCH_TRIES: u16 = 100;
const TERMINATE_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug)]
/// Represents a managed or attached browser instance.
pub struct Browser {
    config: BrowserConfig,
    state: Arc<TokioMutex<BrowserState>>,
}

#[doc(hidden)]
pub struct BrowserState {
    process: Option<ChromeProcess>,
    profile: Option<Profile>,
    actual_port: u16,
    managed: bool,
    stopped: bool,
    client_cache: Option<CdpClient>,
}

impl fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserState")
            .field("process", &self.process)
            .field("profile", &self.profile)
            .field("actual_port", &self.actual_port)
            .field("managed", &self.managed)
            .field("stopped", &self.stopped)
            .field(
                "client_cache",
                &self.client_cache.as_ref().map(|_| "CdpClient"),
            )
            .finish()
    }
}

impl BrowserState {
    #[doc(hidden)]
    pub fn new(
        process: Option<ChromeProcess>,
        profile: Option<Profile>,
        actual_port: u16,
        managed: bool,
    ) -> Self {
        Self {
            process,
            profile,
            actual_port,
            managed,
            stopped: false,
            client_cache: None,
        }
    }
}

impl Browser {
    /// Ensures a browser is available based on the provided configuration.
    /// Depending on `LaunchMode`, it will attach, launch a new instance, or try both.
    pub async fn ensure(config: BrowserConfig) -> Result<Browser, BrowserError> {
        config.validate()?;

        match config.mode {
            LaunchMode::AttachOnly => {
                let port = config.port;
                if probe::is_chrome_cdp(&config.host, port).await {
                    Ok(Browser {
                        config,
                        state: Arc::new(TokioMutex::new(BrowserState::new(
                            None, None, port, false,
                        ))),
                    })
                } else {
                    Err(BrowserError::RemoteUnavailable {
                        host: config.host.clone(),
                        port: config.port,
                    })
                }
            }
            LaunchMode::LaunchNew => {
                if config.port == 0 {
                    Self::spawn_managed(config).await
                } else if probe::is_port_open(&config.host, config.port, PROBE_TIMEOUT).await {
                    Err(BrowserError::PortConflict { port: config.port })
                } else {
                    Self::spawn_managed(config).await
                }
            }
            LaunchMode::Auto => Self::ensure_auto(config).await,
        }
    }

    async fn ensure_auto(mut config: BrowserConfig) -> Result<Browser, BrowserError> {
        if !probe::is_port_open(&config.host, config.port, PROBE_TIMEOUT).await {
            return Self::spawn_managed(config).await;
        }

        if !probe::is_chrome_cdp(&config.host, config.port).await {
            let port =
                ports::find_free_port_near(&config.host, config.port, PORT_SEARCH_TRIES).await?;
            config.port = port;
            return Self::spawn_managed(config).await;
        }

        let profile = Profile::prepare(&config.profile)?;
        if profile.singleton_lock_exists() {
            let port =
                ports::find_free_port_near(&config.host, config.port, PORT_SEARCH_TRIES).await?;
            config.port = port;
            Self::spawn_managed_with_profile(config, profile).await
        } else {
            let port = config.port;
            Ok(Browser {
                config,
                state: Arc::new(TokioMutex::new(BrowserState::new(None, None, port, false))),
            })
        }
    }

    async fn spawn_managed(config: BrowserConfig) -> Result<Browser, BrowserError> {
        let profile = Profile::prepare(&config.profile)?;
        Self::spawn_managed_with_profile(config, profile).await
    }

    async fn spawn_managed_with_profile(
        config: BrowserConfig,
        profile: Profile,
    ) -> Result<Browser, BrowserError> {
        profile.remove_singleton_lock();
        let chrome_path = config
            .chrome_path
            .clone()
            .map(Ok)
            .unwrap_or_else(discovery::discover_default)?;

        let spec = SpawnSpec {
            path: &chrome_path,
            port: config.port,
            profile: &profile,
            config: &config,
            env: vec![],
        };

        let (process, actual_port) = spawn(spec).await?;

        Ok(Browser {
            config,
            state: Arc::new(TokioMutex::new(BrowserState::new(
                Some(process),
                Some(profile),
                actual_port,
                true,
            ))),
        })
    }

    /// Opens a fresh CDP connection to `host:port`.
    ///
    /// Connection establishment (the `/json/version` lookup plus the WebSocket
    /// handshake) is bounded by `connect_timeout`; `command_timeout` becomes the
    /// per-command response timeout carried by the returned [`CdpClient`].
    async fn connect_cdp(&self, port: u16) -> Result<CdpClient, BrowserError> {
        let addr = format!("{}:{}", self.config.host, port);
        match tokio::time::timeout(
            self.config.connect_timeout,
            CdpClient::new(&addr, self.config.command_timeout),
        )
        .await
        {
            Ok(result) => result.map_err(BrowserError::Cdp),
            Err(_elapsed) => Err(BrowserError::RemoteUnavailable {
                host: self.config.host.clone(),
                port,
            }),
        }
    }

    /// Retrieves a CDP client connected to this browser.
    /// Reuses existing client connection if available and alive.
    pub async fn client(&self) -> Result<CdpClient, BrowserError> {
        let mut state = self.state.lock().await;

        if state.stopped {
            return Err(BrowserError::Stopped);
        }

        // Check cached client liveness
        let cached_dead = if let Some(ref client) = state.client_cache {
            matches!(
                client
                    .send_raw_command("Browser.getVersion", serde_json::json!({}))
                    .await,
                Err(CdpError::Disconnected) | Err(CdpError::Timeout { .. })
            )
        } else {
            true
        };

        if !cached_dead {
            return Ok(state.client_cache.as_ref().unwrap().clone());
        }

        if cached_dead {
            state.client_cache = None;
        }

        if state.managed {
            let process_alive = state
                .process
                .as_mut()
                .map(|p| p.is_running())
                .unwrap_or(false);

            if !process_alive {
                if self.config.auto_relaunch {
                    drop(state);
                    return self.relaunch_and_connect().await;
                } else {
                    return Err(BrowserError::Stopped);
                }
            }
        }

        let client = self.connect_cdp(state.actual_port).await?;

        state.client_cache = Some(client.clone());
        Ok(client)
    }

    async fn relaunch_and_connect(&self) -> Result<CdpClient, BrowserError> {
        let new_browser = Self::ensure(self.config.clone()).await?;

        let new_port = {
            let new_state = new_browser.state.lock().await;
            new_state.actual_port
        };

        let client = self.connect_cdp(new_port).await?;

        let (new_process, new_profile, new_actual_port, new_managed) = {
            let mut new_state = new_browser.state.lock().await;
            (
                new_state.process.take(),
                new_state.profile.take(),
                new_state.actual_port,
                new_state.managed,
            )
        };

        {
            let mut state = self.state.lock().await;
            state.process = new_process;
            state.profile = new_profile;
            state.actual_port = new_actual_port;
            state.managed = new_managed;
            state.stopped = false;
            state.client_cache = Some(client.clone());
        }

        Ok(client)
    }

    /// Stops the browser if managed, or simply closes the connection if attached.
    /// Also cleans up ephemeral profiles.
    pub async fn stop(&self) -> Result<(), BrowserError> {
        let mut state = self.state.lock().await;

        if state.stopped {
            return Ok(());
        }

        state.client_cache = None;

        if state.managed {
            if let Some(ref mut proc) = state.process {
                proc.terminate(TERMINATE_GRACE).await?;
            }

            if let Some(ref mut profile) = state.profile {
                profile.cleanup();
            }
        }

        state.process = None;
        state.profile = None;
        state.stopped = true;

        Ok(())
    }

    /// Restarts the browser instance using the original configuration.
    pub async fn restart(&self) -> Result<(), BrowserError> {
        self.stop().await?;
        let new = Self::ensure(self.config.clone()).await?;
        let mut new_state = new.state.lock().await;
        let mut state = self.state.lock().await;
        *state = BrowserState {
            process: new_state.process.take(),
            profile: new_state.profile.take(),
            actual_port: new_state.actual_port,
            managed: new_state.managed,
            stopped: false,
            client_cache: None,
        };
        Ok(())
    }

    /// Returns true if this browser was launched (and is managed) by us.
    pub fn is_managed(&self) -> bool {
        self.state.try_lock().map(|s| s.managed).unwrap_or(false)
    }

    /// Returns true if the browser is alive (running and not stopped).
    pub fn is_alive(&self) -> bool {
        let mut state = match self.state.try_lock() {
            Ok(s) => s,
            Err(_) => return false,
        };

        if state.stopped {
            return false;
        }
        if state.managed {
            state
                .process
                .as_mut()
                .map(|p| p.is_running())
                .unwrap_or(false)
        } else {
            true
        }
    }

    /// Returns the remote debugging host and port this browser is listening on.
    pub fn debug_address(&self) -> (&str, u16) {
        // Config reference lives in `self`, so &str is valid.
        let port = self
            .state
            .try_lock()
            .map(|s| s.actual_port)
            .unwrap_or(self.config.port);
        (&self.config.host, port)
    }

    #[doc(hidden)]
    pub fn pid(&self) -> Option<u32> {
        let mut state = self.state.try_lock().ok()?;
        state.process.as_mut().map(|p| p.id())
    }

    #[doc(hidden)]
    pub fn test_from_state(config: BrowserConfig, state: BrowserState) -> Self {
        Browser {
            config,
            state: Arc::new(TokioMutex::new(state)),
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.try_lock()
            && state.managed
            && !self.config.keep_alive_on_drop
            && !state.stopped
        {
            // Force synchronous kill best-effort
            if let Some(process) = state.process.take() {
                drop(process); // will trigger ChromeProcess::drop which start_kill()s
            }
            if let Some(mut profile) = state.profile.take() {
                profile.cleanup();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileMode;

    fn make_attach_only_config(port: u16) -> BrowserConfig {
        BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .port(port)
            .build()
    }

    fn make_launch_new_config(port: u16) -> BrowserConfig {
        BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .port(port)
            .startup_timeout(Duration::from_millis(800))
            .build()
    }

    fn make_auto_config(port: u16) -> BrowserConfig {
        BrowserConfig::builder()
            .mode(LaunchMode::Auto)
            .port(port)
            .startup_timeout(Duration::from_millis(800))
            .build()
    }

    #[test]
    fn given_managed_browser_when_is_managed_then_true() {
        let browser = Browser {
            config: make_launch_new_config(9222),
            state: Arc::new(TokioMutex::new(BrowserState::new(None, None, 9222, true))),
        };
        assert!(browser.is_managed());
    }

    #[test]
    fn given_attached_browser_when_is_managed_then_false() {
        let browser = Browser {
            config: make_attach_only_config(9222),
            state: Arc::new(TokioMutex::new(BrowserState::new(None, None, 9222, false))),
        };
        assert!(!browser.is_managed());
    }

    #[test]
    fn given_stopped_browser_when_is_alive_then_false() {
        let browser = Browser {
            config: make_launch_new_config(9222),
            state: Arc::new(TokioMutex::new({
                let mut s = BrowserState::new(None, None, 9222, true);
                s.stopped = true;
                s
            })),
        };
        assert!(!browser.is_alive());
    }

    #[test]
    fn given_attached_not_stopped_when_is_alive_then_true() {
        let browser = Browser {
            config: make_attach_only_config(9222),
            state: Arc::new(TokioMutex::new(BrowserState::new(None, None, 9222, false))),
        };
        assert!(browser.is_alive());
    }

    #[test]
    fn given_managed_no_process_when_is_alive_then_false() {
        let browser = Browser {
            config: make_launch_new_config(9222),
            state: Arc::new(TokioMutex::new(BrowserState::new(None, None, 9222, true))),
        };
        assert!(!browser.is_alive());
    }

    #[test]
    fn given_browser_when_debug_address_then_returns_host_and_actual_port() {
        let browser = Browser {
            config: BrowserConfig::builder()
                .mode(LaunchMode::LaunchNew)
                .host("127.0.0.1")
                .port(9222)
                .startup_timeout(Duration::from_secs(1))
                .build(),
            state: Arc::new(TokioMutex::new(BrowserState::new(None, None, 37251, true))),
        };
        let (host, port) = browser.debug_address();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 37251);
    }

    #[test]
    fn given_attach_only_valid_port_when_validated_then_ok() {
        make_attach_only_config(9222)
            .validate()
            .expect("AttachOnly with port must validate");
    }

    #[test]
    fn given_attach_only_port_zero_when_validated_then_err() {
        let cfg = make_attach_only_config(0);
        assert!(
            cfg.validate().is_err(),
            "AttachOnly with port 0 must fail validation"
        );
    }

    #[test]
    fn given_auto_port_zero_when_validated_then_err() {
        let cfg = make_auto_config(0);
        assert!(
            cfg.validate().is_err(),
            "Auto with port 0 must fail validation"
        );
    }

    #[test]
    fn given_auto_remote_host_when_validated_then_err() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::Auto)
            .host("10.0.0.5")
            .port(9222)
            .build();
        assert!(
            cfg.validate().is_err(),
            "Auto with remote host must fail validation"
        );
    }

    #[test]
    fn given_launch_new_remote_host_when_validated_then_err() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .host("10.0.0.5")
            .port(9222)
            .build();
        assert!(
            cfg.validate().is_err(),
            "LaunchNew with remote host must fail validation"
        );
    }

    #[test]
    fn given_launch_new_port_zero_when_validated_then_ok() {
        make_launch_new_config(0)
            .validate()
            .expect("LaunchNew with ephemeral port must validate");
    }

    #[test]
    fn given_stopped_browser_when_stop_again_then_ok_and_idempotent() {
        let browser = Browser {
            config: make_attach_only_config(9222),
            state: Arc::new(TokioMutex::new({
                let mut s = BrowserState::new(None, None, 9222, false);
                s.stopped = true;
                s
            })),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            browser
                .stop()
                .await
                .expect("stop on already-stopped browser must be Ok");
        });
    }

    #[test]
    fn given_ephemeral_profile_when_singleton_lock_check_then_false() {
        let profile =
            Profile::prepare(&ProfileMode::Ephemeral).expect("ephemeral profile must succeed");
        assert!(!profile.singleton_lock_exists());
    }

    #[test]
    fn given_persistent_profile_no_lock_when_check_then_false() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let profile = Profile::prepare(&ProfileMode::Persistent(dir))
            .expect("persistent profile must succeed");
        assert!(!profile.singleton_lock_exists());
    }

    #[test]
    fn given_persistent_profile_with_lock_when_check_then_true() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("SingletonLock"), "").unwrap();
        let profile = Profile::prepare(&ProfileMode::Persistent(dir))
            .expect("persistent profile must succeed");
        assert!(profile.singleton_lock_exists());
    }
}
