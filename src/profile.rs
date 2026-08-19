use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::config::ProfileMode;
use crate::error::BrowserError;

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
                let temp = tempfile::Builder::new()
                    .prefix("cdp-browser-lite-")
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
        obj.insert(
            "exit_type".to_string(),
            serde_json::Value::String("Normal".to_string()),
        );
        obj.insert("exited_cleanly".to_string(), serde_json::Value::Bool(true));
    }
    let patched = serde_json::to_string_pretty(&json)
        .map_err(|e| BrowserError::Profile(format!("failed to serialize Preferences: {e}")))?;
    fs::write(&prefs_path, patched)
        .map_err(|e| BrowserError::Profile(format!("failed to write Preferences: {e}")))?;
    Ok(())
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
    fn given_persistent_with_crashed_prefs_when_prepare_then_prefs_patched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let default_dir = dir.join("Default");
        fs::create_dir_all(&default_dir).unwrap();
        let prefs = default_dir.join("Preferences");
        let original = r#"{"exit_type":"Crashed","other_setting":42,"nested":{"key":"val"}}"#;
        fs::write(&prefs, original).unwrap();

        Profile::prepare(&ProfileMode::Persistent(dir.clone()), 0).unwrap();

        let content = fs::read_to_string(&prefs).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["exit_type"], "Normal");
        assert_eq!(json["exited_cleanly"], true);
        assert_eq!(json["other_setting"], 42, "other fields must be preserved");
        assert_eq!(
            json["nested"]["key"], "val",
            "nested fields must be preserved"
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
