use std::path::{Path, PathBuf};

use crate::error::BrowserError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Candidate {
    Path(PathBuf),
    InPath(String),
}

pub(crate) trait EnvProvider {
    fn var(&self, name: &str) -> Option<String>;
    fn current_os(&self) -> Os;
}

pub(crate) trait FsProbe {
    fn is_executable(&self, path: &Path) -> bool;
    fn which(&self, binary: &str) -> Option<PathBuf>;
}

pub(crate) struct RealEnvProvider;

impl EnvProvider for RealEnvProvider {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn current_os(&self) -> Os {
        if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

pub(crate) struct RealFsProbe;

impl RealFsProbe {
    fn is_executable_impl(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    fn which_impl(binary: &str) -> Option<PathBuf> {
        let path_var = std::env::var("PATH").ok()?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(binary);
            if Self::is_executable_impl(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

impl FsProbe for RealFsProbe {
    fn is_executable(&self, path: &Path) -> bool {
        Self::is_executable_impl(path)
    }

    fn which(&self, binary: &str) -> Option<PathBuf> {
        Self::which_impl(binary)
    }
}

pub(crate) fn candidate_paths(os: Os, env: &dyn EnvProvider) -> Vec<Candidate> {
    if let Some(chrome_path) = env.var("CHROME_PATH") {
        return vec![Candidate::Path(PathBuf::from(chrome_path))];
    }

    match os {
        Os::Linux => linux_candidates(env),
        Os::Mac => macos_candidates(env),
        Os::Windows => windows_candidates(env),
    }
}

fn linux_candidates(_env: &dyn EnvProvider) -> Vec<Candidate> {
    let mut v = Vec::new();
    for bin in &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        v.push(Candidate::InPath((*bin).to_string()));
    }
    v.push(Candidate::Path(PathBuf::from("/usr/bin/google-chrome")));
    v.push(Candidate::Path(PathBuf::from("/opt/google/chrome/chrome")));
    v.push(Candidate::Path(PathBuf::from("/snap/bin/chromium")));
    v
}

fn macos_candidates(env: &dyn EnvProvider) -> Vec<Candidate> {
    let home = env.var("HOME");
    let apps = ["Google Chrome", "Chromium", "Google Chrome Canary"];
    let mut v = Vec::new();

    for app in &apps {
        let rel = format!("/Applications/{app}.app/Contents/MacOS/{app}");
        v.push(Candidate::Path(PathBuf::from(&rel)));
        if let Some(ref h) = home {
            v.push(Candidate::Path(PathBuf::from(format!("{h}{rel}"))));
        }
    }

    v
}

fn windows_candidates(env: &dyn EnvProvider) -> Vec<Candidate> {
    let mut v = Vec::new();
    let suffix = r"Google\Chrome\Application\chrome.exe";

    for var_name in &["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(base) = env.var(var_name) {
            v.push(Candidate::Path(PathBuf::from(format!("{base}\\{suffix}"))));
        }
    }

    v.push(Candidate::InPath("chrome".to_string()));
    v
}

pub(crate) fn discover(env: &dyn EnvProvider, fs: &dyn FsProbe) -> Result<PathBuf, BrowserError> {
    let os = env.current_os();
    let candidates = candidate_paths(os, env);
    let mut searched = Vec::new();

    for candidate in &candidates {
        match candidate {
            Candidate::Path(path) => {
                if fs.is_executable(path) {
                    return Ok(path.clone());
                }
                searched.push(path.clone());
            }
            Candidate::InPath(binary) => {
                if let Some(found) = fs.which(binary) {
                    return Ok(found);
                }
                searched.push(PathBuf::from(binary));
            }
        }
    }

    Err(BrowserError::ExecutableNotFound { searched })
}

pub fn discover_default() -> Result<PathBuf, BrowserError> {
    discover(&RealEnvProvider, &RealFsProbe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    struct MockEnv {
        vars: HashMap<String, String>,
        os: Os,
    }

    impl MockEnv {
        fn new(os: Os, vars: &[(&str, &str)]) -> Self {
            Self {
                vars: vars
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                os,
            }
        }
    }

    impl EnvProvider for MockEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn current_os(&self) -> Os {
            self.os
        }
    }

    struct MockFs {
        executables: HashSet<PathBuf>,
        which_map: HashMap<String, PathBuf>,
    }

    impl MockFs {
        fn new(executables: &[&str], which_map: &[(&str, &str)]) -> Self {
            Self {
                executables: executables.iter().map(PathBuf::from).collect(),
                which_map: which_map
                    .iter()
                    .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
                    .collect(),
            }
        }
    }

    impl FsProbe for MockFs {
        fn is_executable(&self, path: &Path) -> bool {
            self.executables.contains(path)
        }

        fn which(&self, binary: &str) -> Option<PathBuf> {
            self.which_map.get(binary).cloned()
        }
    }

    #[test]
    fn given_chrome_path_env_when_discovering_then_it_wins() {
        let env = MockEnv::new(Os::Linux, &[("CHROME_PATH", "/custom/chrome")]);
        let fs = MockFs::new(&["/custom/chrome"], &[]);

        let result = discover(&env, &fs).expect("should find chrome via CHROME_PATH");
        assert_eq!(result, PathBuf::from("/custom/chrome"));
    }

    #[test]
    fn given_invalid_chrome_path_env_when_discovering_then_error_without_fallback() {
        let env = MockEnv::new(Os::Linux, &[("CHROME_PATH", "/no/existe")]);
        let fs = MockFs::new(&[], &[]);

        let err = discover(&env, &fs).expect_err("should fail for non-existent CHROME_PATH");
        match err {
            BrowserError::ExecutableNotFound { searched } => {
                assert_eq!(searched.len(), 1);
                assert_eq!(searched[0], PathBuf::from("/no/existe"));
            }
            other => panic!("expected ExecutableNotFound, got {other:?}"),
        }
    }

    #[test]
    fn given_linux_with_only_chromium_when_discovering_then_finds_chromium() {
        let env = MockEnv::new(Os::Linux, &[]);
        let fs = MockFs::new(&[], &[("chromium", "/usr/bin/chromium")]);

        let result = discover(&env, &fs).expect("should find chromium");
        assert_eq!(result, PathBuf::from("/usr/bin/chromium"));
    }

    #[test]
    fn given_macos_default_install_when_discovering_then_finds_app_bundle() {
        let env = MockEnv::new(Os::Mac, &[("HOME", "/Users/test")]);
        let chrome_path = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let fs = MockFs::new(&[chrome_path], &[]);

        let result = discover(&env, &fs).expect("should find Google Chrome app bundle");
        assert_eq!(result, PathBuf::from(chrome_path));
    }

    #[test]
    fn given_windows_program_files_install_when_discovering_then_finds_exe() {
        let env = MockEnv::new(
            Os::Windows,
            &[
                ("ProgramFiles", r"C:\Program Files"),
                ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
                ("LocalAppData", r"C:\Users\test\AppData\Local"),
            ],
        );
        let expected = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        let fs = MockFs::new(&[expected], &[]);

        let result = discover(&env, &fs).expect("should find chrome in Program Files");
        assert_eq!(result, PathBuf::from(expected));
    }

    #[test]
    fn given_nothing_installed_when_discovering_then_error_lists_all_candidates() {
        let env = MockEnv::new(Os::Linux, &[]);
        let fs = MockFs::new(&[], &[]);

        let err = discover(&env, &fs).expect_err("should fail with nothing installed");
        match err {
            BrowserError::ExecutableNotFound { searched } => {
                assert_eq!(searched.len(), 7);
                assert_eq!(searched[0], PathBuf::from("google-chrome"));
                assert_eq!(searched[1], PathBuf::from("google-chrome-stable"));
                assert_eq!(searched[2], PathBuf::from("chromium"));
                assert_eq!(searched[3], PathBuf::from("chromium-browser"));
                assert_eq!(searched[4], PathBuf::from("/usr/bin/google-chrome"));
                assert_eq!(searched[5], PathBuf::from("/opt/google/chrome/chrome"));
                assert_eq!(searched[6], PathBuf::from("/snap/bin/chromium"));
            }
            other => panic!("expected ExecutableNotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_candidate_order_is_stable_on_linux() {
        let empty_env = MockEnv::new(Os::Linux, &[]);

        let linux = candidate_paths(Os::Linux, &empty_env);

        let expected = vec![
            Candidate::InPath("google-chrome".to_string()),
            Candidate::InPath("google-chrome-stable".to_string()),
            Candidate::InPath("chromium".to_string()),
            Candidate::InPath("chromium-browser".to_string()),
            Candidate::Path(PathBuf::from("/usr/bin/google-chrome")),
            Candidate::Path(PathBuf::from("/opt/google/chrome/chrome")),
            Candidate::Path(PathBuf::from("/snap/bin/chromium")),
        ];
        assert_eq!(linux, expected);
    }

    #[test]
    fn test_candidate_order_is_stable_on_macos() {
        let mac_env = MockEnv::new(Os::Mac, &[("HOME", "/Users/t")]);
        let macos = candidate_paths(Os::Mac, &mac_env);
        assert_eq!(macos.len(), 6);
        assert_eq!(
            macos[0],
            Candidate::Path(PathBuf::from(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
            ))
        );
        assert_eq!(
            macos[1],
            Candidate::Path(PathBuf::from(
                "/Users/t/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
            ))
        );
    }

    #[test]
    fn test_candidate_order_is_stable_on_windows() {
        let win_env = MockEnv::new(
            Os::Windows,
            &[("ProgramFiles", r"C:\PF"), ("LocalAppData", r"C:\Local")],
        );
        let windows = candidate_paths(Os::Windows, &win_env);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows[0],
            Candidate::Path(PathBuf::from(r"C:\PF\Google\Chrome\Application\chrome.exe"))
        );
        assert!(matches!(&windows[2], Candidate::InPath(s) if s == "chrome"));
    }
}
