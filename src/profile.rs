use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, SystemTime};

use crate::config::ProfileMode;
use crate::error::BrowserError;

/// Prefix of ephemeral profile directories created in the system temp dir.
const EPHEMERAL_PREFIX: &str = "cdp-browser-lite-";

/// Ephemeral profile dirs older than this are considered orphaned and are
/// swept on the next ephemeral `prepare`.
#[cfg(unix)]
const EPHEMERAL_SWEEP_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug)]
#[doc(hidden)]
pub struct Profile {
    pub mode: ProfileMode,
    pub dir: Option<PathBuf>,
}

impl Profile {
    #[doc(hidden)]
    pub fn prepare(mode: &ProfileMode, port: u16) -> Result<Profile, BrowserError> {
        match mode {
            ProfileMode::Ephemeral => {
                #[cfg(unix)]
                sweep_orphaned_ephemeral_profiles(
                    &std::env::temp_dir(),
                    EPHEMERAL_PREFIX,
                    EPHEMERAL_SWEEP_AGE,
                );
                let temp = tempfile::Builder::new()
                    .prefix(EPHEMERAL_PREFIX)
                    .tempdir()
                    .map_err(|e| {
                        BrowserError::Profile(format!("failed to create temp dir: {e}"))
                    })?;
                let path = temp.keep();
                Ok(Profile {
                    mode: mode.clone(),
                    dir: Some(path),
                })
            }
            ProfileMode::Persistent(dir) => {
                fs::create_dir_all(dir).map_err(|e| {
                    BrowserError::Profile(format!("failed to create profile dir: {e}"))
                })?;
                patch_preferences(dir)?;
                Ok(Profile {
                    mode: mode.clone(),
                    dir: Some(dir.clone()),
                })
            }
            ProfileMode::PersistentPerPort { root, prefix } => {
                let dir = root.join(format!("{}{}", prefix, port));
                fs::create_dir_all(&dir).map_err(|e| {
                    BrowserError::Profile(format!("failed to create profile dir: {e}"))
                })?;
                patch_preferences(&dir)?;
                Ok(Profile {
                    mode: mode.clone(),
                    dir: Some(dir),
                })
            }
            ProfileMode::UserDefault => Ok(Profile {
                mode: mode.clone(),
                dir: None,
            }),
        }
    }

    pub(crate) fn read_devtools_active_port(&self) -> Option<u16> {
        let dir = self.dir.as_ref()?;
        let path = dir.join("DevToolsActivePort");
        let file = fs::File::open(&path).ok()?;
        let reader = std::io::BufReader::new(file);
        let first_line = reader.lines().next()?.ok()?;
        first_line.trim().parse().ok()
    }

    pub(crate) fn remove_singleton_lock(&self) {
        if let Some(dir) = &self.dir {
            let path = dir.join("SingletonLock");
            let _ = fs::remove_file(path);
        }
    }

    pub(crate) fn cleanup(&mut self) {
        if let Some(dir) = &self.dir {
            match self.mode {
                ProfileMode::Ephemeral => {
                    let _ = fs::remove_dir_all(dir);
                    self.dir = None;
                }
                ProfileMode::Persistent(_)
                | ProfileMode::PersistentPerPort { .. }
                | ProfileMode::UserDefault => {
                    let lock = dir.join("SingletonLock");
                    let _ = fs::remove_file(lock);
                }
            }
        }
    }
}

/// Chrome records its exit state under `profile.exit_type` inside
/// `Default/Preferences` (`Normal`, `Crashed`, or `SessionEnded`). A
/// `Crashed` value makes Chrome show the "restore pages" bubble on the next
/// launch. Patching it to `Normal` at prepare time suppresses the bubble for
/// persistent profiles that survived an unclean shutdown.
fn patch_preferences(dir: &Path) -> Result<(), BrowserError> {
    let prefs_path = dir.join("Default").join("Preferences");
    if !prefs_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&prefs_path)
        .map_err(|e| BrowserError::Profile(format!("failed to read Preferences: {e}")))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| BrowserError::Profile(format!("failed to parse Preferences JSON: {e}")))?;
    if let Some(obj) = json.as_object_mut() {
        let profile = obj
            .entry("profile")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !profile.is_object() {
            *profile = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(profile_obj) = profile.as_object_mut() {
            profile_obj.insert(
                "exit_type".to_string(),
                serde_json::Value::String("Normal".to_string()),
            );
        }
    }
    let patched = serde_json::to_string_pretty(&json)
        .map_err(|e| BrowserError::Profile(format!("failed to serialize Preferences: {e}")))?;
    fs::write(&prefs_path, patched)
        .map_err(|e| BrowserError::Profile(format!("failed to write Preferences: {e}")))?;
    Ok(())
}

/// Best-effort removal of orphaned ephemeral profile directories left behind
/// by browser processes that were killed without running `Profile::cleanup`
/// (e.g. SIGKILL). Only directories under `root` matching `prefix`, older
/// than `max_age`, and whose Chrome is not alive are removed. Errors are
/// ignored: this must never prevent a launch.
///
/// On non-Unix platforms the sweep is disabled entirely: there is no portable
/// PID-liveness probe, and a partial `remove_dir_all` on a live profile would
/// corrupt it. Orphaned dirs on Windows/etc. must be cleaned manually.
#[cfg(unix)]
fn sweep_orphaned_ephemeral_profiles(root: &Path, prefix: &str, max_age: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(prefix) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| {
                now.duration_since(m)
                    .map(|age| age >= max_age)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !old_enough || ephemeral_profile_is_live(&path) {
            continue;
        }
        let _ = fs::remove_dir_all(&path);
    }
}

/// On Unix, an ephemeral profile whose `SingletonLock` symlink points at a
/// still-running PID is considered live. Chrome writes the lock as
/// `SingletonLock -> {hostname}-{pid}` (a dangling symlink on Chrome >= 151).
/// A recycled PID can keep an orphan alive until the next sweep; the stale
/// directory then survives one extra hour, which is harmless.
#[cfg(unix)]
fn ephemeral_profile_is_live(dir: &Path) -> bool {
    let Ok(target) = fs::read_link(dir.join("SingletonLock")) else {
        return false;
    };
    let Some(pid) = target
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| name.rsplit('-').next())
        .and_then(|s| s.parse::<i32>().ok())
    else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_two_ephemeral_when_prepare_then_dirs_are_distinct() {
        let p1 = Profile::prepare(&ProfileMode::Ephemeral, 0).unwrap();
        let p2 = Profile::prepare(&ProfileMode::Ephemeral, 0).unwrap();
        let d1 = p1.dir.as_ref().unwrap();
        let d2 = p2.dir.as_ref().unwrap();
        assert_ne!(d1, d2, "ephemeral dirs must be distinct");
        assert!(d1.exists());
        assert!(d2.exists());
    }

    #[test]
    fn given_ephemeral_with_files_when_cleanup_then_dir_removed() {
        let mut profile = Profile::prepare(&ProfileMode::Ephemeral, 0).unwrap();
        let dir = profile.dir.clone().unwrap();
        fs::write(dir.join("some_data"), "payload").unwrap();
        assert!(dir.exists());

        profile.cleanup();

        assert!(!dir.exists(), "ephemeral dir must be gone after cleanup");
        assert!(
            profile.dir.is_none(),
            "ephemeral dir field must be cleared after cleanup"
        );
    }

    #[test]
    fn given_persistent_with_crashed_prefs_when_prepare_then_profile_exit_type_patched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let default_dir = dir.join("Default");
        fs::create_dir_all(&default_dir).unwrap();
        let prefs = default_dir.join("Preferences");
        let original =
            r#"{"profile":{"exit_type":"Crashed"},"other_setting":42,"nested":{"key":"val"}}"#;
        fs::write(&prefs, original).unwrap();

        Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();

        let content = fs::read_to_string(&prefs).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["profile"]["exit_type"], "Normal");
        assert!(
            json.get("exit_type").is_none(),
            "root-level exit_type is ignored by Chrome and must not be written"
        );
        assert_eq!(json["other_setting"], 42, "other fields must be preserved");
        assert_eq!(
            json["nested"]["key"], "val",
            "nested fields must be preserved"
        );
    }

    #[test]
    fn given_persistent_without_profile_section_when_prepare_then_profile_section_created() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let default_dir = dir.join("Default");
        fs::create_dir_all(&default_dir).unwrap();
        let prefs = default_dir.join("Preferences");
        fs::write(&prefs, r#"{"other_setting":42}"#).unwrap();

        Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();

        let content = fs::read_to_string(&prefs).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["profile"]["exit_type"], "Normal");
        assert_eq!(json["other_setting"], 42);
    }

    #[test]
    fn given_persistent_with_non_object_profile_when_prepare_then_replaced_with_normal() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let default_dir = dir.join("Default");
        fs::create_dir_all(&default_dir).unwrap();
        let prefs = default_dir.join("Preferences");
        fs::write(&prefs, r#"{"profile":"bogus","other_setting":42}"#).unwrap();

        Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();

        let content = fs::read_to_string(&prefs).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["profile"]["exit_type"], "Normal");
        assert_eq!(json["other_setting"], 42);
    }

    #[cfg(unix)]
    #[test]
    fn given_stale_ephemeral_dirs_when_sweeping_then_removed_but_foreign_prefix_survives() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join("cdp-browser-lite-stale");
        fs::create_dir_all(stale.join("Default")).unwrap();
        fs::write(stale.join("Default").join("Preferences"), "{}").unwrap();
        // Push mtime 2 hours into the past so the dir is older than the 1 h sweep age.
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        fs::File::open(&stale)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
        let foreign = root.path().join("chrome-mcp-profile-9222");
        fs::create_dir_all(&foreign).unwrap();

        sweep_orphaned_ephemeral_profiles(
            root.path(),
            "cdp-browser-lite-",
            Duration::from_secs(60 * 60),
        );

        assert!(!stale.exists(), "stale ephemeral dir must be removed");
        assert!(foreign.exists(), "foreign-prefix dirs must survive");
    }

    #[cfg(unix)]
    #[test]
    fn given_young_ephemeral_dirs_when_sweeping_with_large_max_age_then_all_survive() {
        let root = tempfile::tempdir().unwrap();
        let young = root.path().join("cdp-browser-lite-young");
        fs::create_dir_all(&young).unwrap();
        // mtime is "now" (just created); sweep age is 1 h → must survive.

        sweep_orphaned_ephemeral_profiles(
            root.path(),
            "cdp-browser-lite-",
            Duration::from_secs(60 * 60),
        );

        assert!(young.exists(), "young dirs must survive a large max_age");
    }

    #[cfg(unix)]
    #[test]
    fn given_live_pid_lock_when_sweeping_then_dir_survives() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cdp-browser-lite-live");
        fs::create_dir_all(&dir).unwrap();
        symlink(
            format!("testhost-{}", std::process::id()),
            dir.join("SingletonLock"),
        )
        .unwrap();
        // Push mtime 2 hours into the past so the age check passes; only PID
        // liveness should keep the dir alive.
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        fs::File::open(&dir)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();

        sweep_orphaned_ephemeral_profiles(
            root.path(),
            "cdp-browser-lite-",
            Duration::from_secs(60 * 60),
        );

        assert!(dir.exists(), "profile with a live owner PID must survive");
    }

    #[cfg(unix)]
    #[test]
    fn given_dead_pid_lock_when_sweeping_then_dir_removed() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cdp-browser-lite-dead");
        fs::create_dir_all(&dir).unwrap();
        // 999999999 exceeds every platform's PID limit (Linux 4194304, macOS 99999)
        symlink("deadhost-999999999", dir.join("SingletonLock")).unwrap();
        // Push mtime 2 hours into the past so the age check passes.
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        fs::File::open(&dir)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();

        sweep_orphaned_ephemeral_profiles(
            root.path(),
            "cdp-browser-lite-",
            Duration::from_secs(60 * 60),
        );

        assert!(
            !dir.exists(),
            "profile with a dead owner PID must be removed"
        );
    }

    #[test]
    fn given_persistent_empty_when_prepare_then_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        let profile = Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();

        assert_eq!(profile.dir, Some(dir), "persistent dir must be stored");
    }

    #[test]
    fn given_persistent_with_singleton_lock_when_cleanup_then_lock_removed_data_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let data_file = dir.join("important.dat");
        fs::write(&data_file, "keep me").unwrap();
        let lock = dir.join("SingletonLock");
        fs::write(&lock, "").unwrap();

        let mut profile = Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();
        assert!(
            lock.exists(),
            "SingletonLock must be present before cleanup"
        );

        profile.cleanup();

        assert!(!lock.exists(), "SingletonLock must be removed on cleanup");
        assert!(
            data_file.exists(),
            "non-lock data must survive persistent cleanup"
        );
    }

    #[test]
    fn given_devtools_active_port_with_valid_content_when_read_then_returns_port() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let port_file = dir.join("DevToolsActivePort");
        fs::write(&port_file, "37251\n/devtools/browser/xyz").unwrap();

        let profile = Profile {
            mode: ProfileMode::Ephemeral,
            dir: Some(dir),
        };

        assert_eq!(profile.read_devtools_active_port(), Some(37251));
    }

    #[test]
    fn given_devtools_active_port_with_invalid_content_when_read_then_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let port_file = dir.join("DevToolsActivePort");
        fs::write(&port_file, "not_a_number\nline2").unwrap();

        let profile = Profile {
            mode: ProfileMode::Ephemeral,
            dir: Some(dir),
        };

        assert_eq!(profile.read_devtools_active_port(), None);
    }
}
