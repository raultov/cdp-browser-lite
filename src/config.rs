use std::path::PathBuf;
use std::time::Duration;

use crate::error::BrowserError;

/// Defines how the browser instance should be obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMode {
    /// Tries to attach if the port is open, otherwise launches a new instance.
    Auto,
    /// Always launches a new instance.
    LaunchNew,
    /// Only attaches to an existing instance.
    AttachOnly,
}

/// Defines the type of profile to use for the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileMode {
    /// Creates a temporary profile that is deleted when the browser is stopped.
    Ephemeral,
    /// Uses a persistent profile at the specified path.
    Persistent(PathBuf),
    /// Profile directory derived from the resolved port: `root/{prefix}{port}`.
    PersistentPerPort { root: PathBuf, prefix: String },
    /// Uses the default user profile.
    UserDefault,
}

/// Configuration for launching or attaching to a browser.
#[derive(Debug, Clone)]
// Independent launch flags; grouping them into a sub-struct would only obscure
// the flat, self-documenting configuration surface.
#[allow(clippy::struct_excessive_bools)]
pub struct BrowserConfig {
    pub(crate) mode: LaunchMode,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) headless: bool,
    pub(crate) enable_automation: bool,
    pub(crate) profile: ProfileMode,
    pub(crate) proxy: Option<String>,
    pub(crate) chrome_path: Option<PathBuf>,
    pub(crate) extra_args: Vec<String>,
    pub(crate) window_size: Option<(u32, u32)>,
    pub(crate) user_agent: Option<String>,
    pub(crate) connect_timeout: Duration,
    pub(crate) startup_timeout: Duration,
    pub(crate) command_timeout: Duration,
    pub(crate) keep_alive_on_drop: bool,
    pub(crate) auto_relaunch: bool,
    pub(crate) no_sandbox: Option<bool>,
}

impl BrowserConfig {
    pub fn builder() -> BrowserConfigBuilder {
        BrowserConfigBuilder::default()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn validate(&self) -> Result<(), BrowserError> {
        match self.mode {
            LaunchMode::AttachOnly => {
                if self.port == 0 {
                    return Err(BrowserError::InvalidConfig(
                        "AttachOnly requires an explicit port; port 0 (ephemeral) is not \
                         allowed because the mode must connect to a known endpoint"
                            .to_string(),
                    ));
                }
            }
            LaunchMode::Auto => {
                if self.port == 0 {
                    return Err(BrowserError::InvalidConfig(
                        "Auto requires an explicit port; port 0 (ephemeral) is not allowed \
                         because Auto means 'attach to that port or launch on it'"
                            .to_string(),
                    ));
                }
            }
            LaunchMode::LaunchNew => {}
        }

        if matches!(self.mode, LaunchMode::Auto | LaunchMode::LaunchNew)
            && !is_local_host(&self.host)
        {
            let mode_name = match self.mode {
                LaunchMode::Auto => "Auto",
                LaunchMode::LaunchNew => "LaunchNew",
                LaunchMode::AttachOnly => unreachable!("guarded by the matches! above"),
            };
            return Err(BrowserError::InvalidConfig(format!(
                "{mode_name} cannot be used with a remote host '{host}'; \
                 only AttachOnly supports remote hosts",
                host = self.host
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
// Mirrors `BrowserConfig`'s independent launch flags (see note above).
#[allow(clippy::struct_excessive_bools)]
pub struct BrowserConfigBuilder {
    mode: LaunchMode,
    host: String,
    port: u16,
    headless: bool,
    enable_automation: bool,
    profile: ProfileMode,
    proxy: Option<String>,
    chrome_path: Option<PathBuf>,
    extra_args: Vec<String>,
    window_size: Option<(u32, u32)>,
    user_agent: Option<String>,
    connect_timeout: Duration,
    startup_timeout: Duration,
    command_timeout: Duration,
    keep_alive_on_drop: bool,
    auto_relaunch: bool,
    no_sandbox: Option<bool>,
}

impl BrowserConfigBuilder {
    pub fn mode(mut self, mode: LaunchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn headless(mut self, v: bool) -> Self {
        self.headless = v;
        self
    }

    pub fn enable_automation(mut self, v: bool) -> Self {
        self.enable_automation = v;
        self
    }

    pub fn profile(mut self, mode: ProfileMode) -> Self {
        self.profile = mode;
        self
    }

    pub fn proxy(mut self, url: impl Into<String>) -> Self {
        self.proxy = Some(url.into());
        self
    }

    pub fn chrome_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.chrome_path = Some(p.into());
        self
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.extra_args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn window_size(mut self, w: u32, h: u32) -> Self {
        self.window_size = Some((w, h));
        self
    }

    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    pub fn startup_timeout(mut self, d: Duration) -> Self {
        self.startup_timeout = d;
        self
    }

    pub fn command_timeout(mut self, d: Duration) -> Self {
        self.command_timeout = d;
        self
    }

    pub fn keep_alive_on_drop(mut self, v: bool) -> Self {
        self.keep_alive_on_drop = v;
        self
    }

    pub fn auto_relaunch(mut self, v: bool) -> Self {
        self.auto_relaunch = v;
        self
    }

    pub fn no_sandbox(mut self, v: bool) -> Self {
        self.no_sandbox = Some(v);
        self
    }

    pub fn build(self) -> BrowserConfig {
        BrowserConfig {
            mode: self.mode,
            host: self.host,
            port: self.port,
            headless: self.headless,
            enable_automation: self.enable_automation,
            profile: self.profile,
            proxy: self.proxy,
            chrome_path: self.chrome_path,
            extra_args: self.extra_args,
            window_size: self.window_size,
            user_agent: self.user_agent,
            connect_timeout: self.connect_timeout,
            startup_timeout: self.startup_timeout,
            command_timeout: self.command_timeout,
            keep_alive_on_drop: self.keep_alive_on_drop,
            auto_relaunch: self.auto_relaunch,
            no_sandbox: self.no_sandbox,
        }
    }
}

impl Default for BrowserConfigBuilder {
    fn default() -> Self {
        Self {
            mode: LaunchMode::Auto,
            host: "127.0.0.1".to_string(),
            port: 0,
            headless: false,
            enable_automation: false,
            profile: ProfileMode::Ephemeral,
            proxy: None,
            chrome_path: None,
            extra_args: Vec::new(),
            window_size: None,
            user_agent: None,
            connect_timeout: Duration::from_secs(10),
            startup_timeout: Duration::from_secs(10),
            command_timeout: Duration::from_secs(10),
            keep_alive_on_drop: false,
            auto_relaunch: false,
            no_sandbox: None,
        }
    }
}

impl ProfileMode {
    /// Pure. No filesystem side effects. `None` for `UserDefault`
    /// (and for `Ephemeral`, whose directory does not exist until prepared).
    pub fn dir_for_port(&self, port: u16) -> Option<PathBuf> {
        match self {
            ProfileMode::Ephemeral | ProfileMode::UserDefault => None,
            ProfileMode::Persistent(dir) => Some(dir.clone()),
            ProfileMode::PersistentPerPort { root, prefix } => {
                Some(root.join(format!("{}{}", prefix, port)))
            }
        }
    }

    /// Pure. True if a *managed* instance appears to hold this port.
    /// Always false for `Ephemeral` and `UserDefault`.
    pub fn managed_lock_exists(&self, port: u16) -> bool {
        self.dir_for_port(port)
            .map(|d| d.join("SingletonLock").exists())
            .unwrap_or(false)
    }
}

fn is_local_host(host: &str) -> bool {
    let h = host.trim();
    matches!(h, "127.0.0.1" | "::1" | "[::1]") || h.eq_ignore_ascii_case("localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_config() -> BrowserConfig {
        BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .port(9222)
            .build()
    }

    #[test]
    // Linear field-by-field assertions on the builder defaults; `BrowserConfig`
    // deliberately does not derive `PartialEq`, so a single `assert_eq!` is not
    // an option and the assertion count exceeds the (lowered) complexity budget.
    #[allow(clippy::cognitive_complexity)]
    fn given_default_builder_when_built_then_defaults_are_correct() {
        let cfg = BrowserConfig::builder().build();
        assert_eq!(cfg.mode, LaunchMode::Auto);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 0);
        assert!(!cfg.headless);
        assert!(!cfg.enable_automation);
        assert_eq!(cfg.profile, ProfileMode::Ephemeral);
        assert!(cfg.proxy.is_none());
        assert!(cfg.chrome_path.is_none());
        assert!(cfg.extra_args.is_empty());
        assert!(cfg.window_size.is_none());
        assert!(cfg.user_agent.is_none());
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
        assert_eq!(cfg.startup_timeout, Duration::from_secs(10));
        assert_eq!(cfg.command_timeout, Duration::from_secs(10));
        assert!(!cfg.keep_alive_on_drop);
        assert!(!cfg.auto_relaunch);
        assert!(cfg.no_sandbox.is_none());
    }

    #[test]
    fn given_attach_only_with_port_zero_when_validated_then_invalid_config() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .build();
        match cfg.validate() {
            Err(BrowserError::InvalidConfig(msg)) => {
                assert!(msg.contains("port"), "missing 'port' in: {msg}");
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn given_auto_with_port_zero_when_validated_then_invalid_config() {
        let cfg = BrowserConfig::builder().build();
        match cfg.validate() {
            Err(BrowserError::InvalidConfig(msg)) => {
                assert!(msg.contains("port"), "missing 'port' in: {msg}");
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn given_remote_host_with_auto_when_validated_then_invalid_config() {
        let cfg = BrowserConfig::builder()
            .host("10.0.0.5")
            .mode(LaunchMode::Auto)
            .port(9222)
            .build();
        match cfg.validate() {
            Err(BrowserError::InvalidConfig(msg)) => {
                assert!(
                    msg.to_lowercase().contains("remote"),
                    "missing 'remote' in: {msg}"
                );
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn given_remote_host_with_launch_new_when_validated_then_invalid_config() {
        let cfg = BrowserConfig::builder()
            .host("10.0.0.5")
            .mode(LaunchMode::LaunchNew)
            .build();
        match cfg.validate() {
            Err(BrowserError::InvalidConfig(msg)) => {
                assert!(
                    msg.to_lowercase().contains("remote"),
                    "missing 'remote' in: {msg}"
                );
            }
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn given_remote_host_with_attach_only_when_validated_then_ok() {
        let cfg = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .host("10.0.0.5")
            .port(9222)
            .build();
        cfg.validate()
            .expect("AttachOnly with remote host must validate");
    }

    #[test]
    fn given_attach_only_with_local_port_when_validated_then_ok() {
        ok_config()
            .validate()
            .expect("local AttachOnly must validate");
    }

    #[test]
    fn given_launch_new_with_port_zero_when_validated_then_ok() {
        let cfg = BrowserConfig::builder().mode(LaunchMode::LaunchNew).build();
        cfg.validate()
            .expect("LaunchNew with ephemeral port must validate");
    }

    #[test]
    fn given_custom_args_when_built_then_preserved_in_order() {
        let cfg = BrowserConfig::builder()
            .arg("--foo=1")
            .arg("--bar=2")
            .args(["--baz=3", "--qux=4"])
            .build();
        assert_eq!(
            cfg.extra_args,
            vec![
                "--foo=1".to_string(),
                "--bar=2".to_string(),
                "--baz=3".to_string(),
                "--qux=4".to_string(),
            ]
        );
    }

    #[test]
    fn given_proxy_args_window_size_when_built_then_preserved() {
        let cfg = BrowserConfig::builder()
            .proxy("http://p:8080")
            .arg("--lang=es")
            .window_size(1280, 800)
            .user_agent("ua/1.0")
            .build();
        assert_eq!(cfg.proxy.as_deref(), Some("http://p:8080"));
        assert_eq!(cfg.extra_args, vec!["--lang=es".to_string()]);
        assert_eq!(cfg.window_size, Some((1280, 800)));
        assert_eq!(cfg.user_agent.as_deref(), Some("ua/1.0"));
    }

    #[test]
    fn given_persistent_profile_when_built_then_preserved() {
        let dir = PathBuf::from("/tmp/profile");
        let cfg = BrowserConfig::builder()
            .profile(ProfileMode::Persistent(dir.clone()))
            .build();
        assert_eq!(cfg.profile, ProfileMode::Persistent(dir));
    }

    #[test]
    fn test_is_local_host() {
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("localhost"));
        assert!(is_local_host("LOCALHOST"));
        assert!(is_local_host("Localhost"));
        assert!(is_local_host("::1"));
        assert!(is_local_host("[::1]"));
        assert!(is_local_host("  127.0.0.1  "));

        assert!(!is_local_host("10.0.0.5"));
        assert!(!is_local_host("192.168.1.1"));
        assert!(!is_local_host("example.com"));
        assert!(!is_local_host(""));
        assert!(!is_local_host("127"));
        assert!(!is_local_host("localhost.example.com"));
        assert!(!is_local_host("::2"));
    }
}
