//! User settings, persisted as JSON in `~/.space/settings.json` (or `SPACE_SETTINGS`).
//!
//! Missing fields take their defaults, so older or partial files load. A file that can't be
//! read or parsed loads as defaults with a warning and is left alone until the user changes a
//! setting. Every change is saved at once, atomically (temp file + rename).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Env var overriding the settings file path.
pub const SETTINGS_ENV: &str = "SPACE_SETTINGS";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub new_threads: NewThreads,
    pub notifications: Notifications,
    pub plugins: Plugins,
}

/// Applied when a new thread's session is created; `None` keeps the agent's default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewThreads {
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
    /// Permission mode id (from the `permission-modes` catalog); `None`: the catalog's default.
    pub permission_mode: Option<String>,
}

/// Plugins turned off, and plugins' text settings (secrets live in the Keychain, not here).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Plugins {
    pub disabled: Vec<String>,
    /// Text setting values by plugin id, then key.
    pub settings: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Notifications {
    /// System notification when a turn finishes in a thread you're not looking at.
    pub on_finish: bool,
    /// Play a sound when a turn finishes.
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { on_finish: true, sound: true }
    }
}

/// The settings file location: `SPACE_SETTINGS`, else `~/.space/settings.json`.
pub fn default_path() -> Option<PathBuf> {
    match std::env::var_os(SETTINGS_ENV).filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".space/settings.json")),
    }
}

/// Settings plus where they are saved (`None`: kept in memory only).
#[derive(Debug, Clone, Default)]
pub struct SettingsStore {
    path: Option<PathBuf>,
    pub settings: Settings,
}

impl SettingsStore {
    pub fn load(path: Option<PathBuf>) -> Self {
        let settings = path.as_deref().map(read).unwrap_or_default();
        Self { path, settings }
    }

    /// Apply `f` and save if anything changed.
    pub fn update(&mut self, f: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        f(&mut self.settings);
        if self.settings != before
            && let Some(path) = &self.path
            && let Err(e) = write(path, &self.settings)
        {
            tracing::warn!("saving settings to {} failed: {e:#}", path.display());
        }
    }
}

fn read(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{} is not valid settings ({e}); using defaults", path.display());
            Settings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            tracing::warn!("reading {} failed ({e}); using defaults", path.display());
            Settings::default()
        }
    }
}

fn write(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(settings)?;
    json.push('\n');
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("space-settings-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("nested/settings.json")
    }

    #[test]
    fn defaults_when_missing_and_round_trip() {
        let path = temp("roundtrip");
        let mut store = SettingsStore::load(Some(path.clone()));
        assert_eq!(store.settings, Settings::default());
        assert!(store.settings.notifications.on_finish && store.settings.notifications.sound);
        assert!(!path.exists(), "loading never writes");
        store.update(|s| {
            s.new_threads.model = Some(ModelRef { provider: "stub".into(), id: "stub-fast".into() });
            s.new_threads.thinking_level = Some("high".into());
            s.new_threads.permission_mode = Some("read-only".into());
            s.notifications.sound = false;
            s.plugins.disabled = vec!["permissions".into()];
        });
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("\"newThreads\"") && json.contains("\"thinkingLevel\"") && json.contains("\"onFinish\""));
        assert!(json.contains("\"permissionMode\"") && json.contains("\"disabled\""));
        assert_eq!(SettingsStore::load(Some(path)).settings, store.settings);
    }

    #[test]
    fn partial_file_takes_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "notifications": { "sound": false } }"#).unwrap();
        assert!(s.notifications.on_finish);
        assert!(!s.notifications.sound);
        assert_eq!(s.new_threads, NewThreads::default());
    }

    #[test]
    fn corrupt_file_loads_defaults_and_is_kept_until_a_change() {
        let path = temp("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let mut store = SettingsStore::load(Some(path.clone()));
        assert_eq!(store.settings, Settings::default());
        store.update(|_| {});
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        store.update(|s| s.notifications.on_finish = false);
        assert!(!SettingsStore::load(Some(path)).settings.notifications.on_finish);
    }
}
