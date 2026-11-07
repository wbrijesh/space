//! User settings, persisted as JSON in `~/.space/settings.json` (or `SPACE_SETTINGS`).
//!
//! Missing fields take their defaults, so older or partial files load. A file that can't be
//! read or parsed loads as defaults with a warning and is left alone until the user changes a
//! setting. Every change is saved at once, atomically (temp file + rename).
//!
//! Also the app's actions that change settings (the Settings page, plugin settings, turning
//! plugins on and off).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{Context, Window};
use serde::{Deserialize, Serialize};
use space_core::protocol::ModelInfo;
use space_core::{PluginStatus, SettingError};
use space_ui::components::settings::FieldCheck;

use crate::app::SpaceApp;
use crate::notify;

/// Env var overriding the settings file path.
pub const SETTINGS_ENV: &str = "SPACE_SETTINGS";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub new_threads: NewThreads,
    pub plugins: Plugins,
    /// Older files: the alerts switches, before they were the alerts plugin's settings. Moved
    /// there on load, and no longer written.
    #[serde(skip_serializing)]
    notifications: Option<LegacyNotifications>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyNotifications {
    on_finish: Option<bool>,
    sound: Option<bool>,
}

/// The alerts plugin's id, and its switches the old `notifications` settings become.
const ALERTS: &str = "alerts";

impl Settings {
    /// Move old `notifications` switches that were off to the alerts plugin's settings (unless
    /// it has its own). Whether anything changed.
    fn migrate(&mut self) -> bool {
        let Some(old) = self.notifications.take() else { return false };
        let alerts = self.plugins.settings.entry(ALERTS.into()).or_default();
        for (key, on) in [("notify", old.on_finish), ("sound", old.sound)] {
            if on == Some(false) {
                alerts.entry(key.into()).or_insert_with(|| "false".into());
            }
        }
        if alerts.is_empty() {
            self.plugins.settings.remove(ALERTS);
        }
        true
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
        let mut settings: Settings = path.as_deref().map(read).unwrap_or_default();
        if settings.migrate()
            && let Some(path) = &path
            && let Err(e) = write(path, &settings)
        {
            tracing::warn!("saving settings to {} failed: {e:#}", path.display());
        }
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

// ----- the app's settings actions ------------------------------------------------------

impl SpaceApp {
    /// Start the service plugins (permissions and the like) with the saved plugin settings;
    /// failures show in the Plugins section and the logs.
    pub(crate) fn start_services(&mut self, cx: &mut Context<Self>) {
        let plugins = &self.settings.settings.plugins;
        self.host.set_disabled(plugins.disabled.iter().cloned());
        for (id, values) in &plugins.settings {
            self.host.set_text_settings(id, values.clone());
        }
        let host = self.host.clone();
        cx.spawn(async move |_, _| host.start_services().await).detach();
    }

    pub(crate) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = true;
        // Off the composer (it's hidden), so Esc reaches the page.
        window.focus(&self.focus, cx);
        self.refresh_permission(cx);
        cx.notify();
    }

    /// Back to the current thread.
    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = false;
        match self.selected {
            Some(key) => self.select_inner(key, false, window, cx),
            None => cx.notify(),
        }
    }

    /// Ask macOS whether notifications are allowed (answers on another thread).
    pub(crate) fn refresh_permission(&mut self, cx: &mut Context<Self>) {
        let answer = notify::permission();
        cx.spawn(async move |this, cx| {
            if let Ok(permission) = answer.await {
                let _ = this.update(cx, |this, cx| {
                    if this.permission != permission {
                        this.permission = permission;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    pub(crate) fn allow_notifications(&mut self, cx: &mut Context<Self>) {
        let answered = notify::request_permission();
        cx.spawn(async move |this, cx| {
            let _ = answered.await;
            let _ = this.update(cx, |this, cx| this.refresh_permission(cx));
        })
        .detach();
    }

    pub(crate) fn set_default_model(&mut self, model: Option<ModelInfo>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.model = model.map(|m| ModelRef { provider: m.provider, id: m.id }));
        cx.notify();
    }

    pub(crate) fn set_default_thinking(&mut self, level: Option<String>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.thinking_level = level);
        cx.notify();
    }


    pub(crate) fn set_default_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.permission_mode = mode);
        self.sync_session_modes();
        cx.notify();
    }

    /// Save a plugin setting from its Settings field. The plugin checks a new value first:
    /// the field shows "Checking…", then the outcome (a rejected value isn't stored; one that
    /// couldn't be checked can be saved anyway).
    pub(crate) fn save_plugin_setting(
        &mut self,
        plugin_id: String,
        key: String,
        kind: SaveKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = (plugin_id.clone(), key.clone());
        let input = self.setting_inputs.get(&field).map(|(i, _)| i.clone());
        let value = match kind {
            SaveKind::Clear => None,
            SaveKind::Checked | SaveKind::Anyway => {
                let v = input.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                if v.is_empty() {
                    return;
                }
                Some(v)
            }
        };
        let secret = self
            .plugin_settings
            .get(&plugin_id)
            .and_then(|fields| fields.iter().find(|f| f.field.key == key))
            .is_some_and(|f| f.field.kind == space_core::protocol::service::SettingKind::Secret);
        self.field_checks.insert(field.clone(), if value.is_some() { FieldCheck::Checking } else { FieldCheck::Idle });
        let host = self.host.clone();
        let force = kind == SaveKind::Anyway;
        cx.spawn_in(window, async move |this, cx| {
            let result = host.set_setting(&plugin_id, &key, value.clone(), force).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let check = match result {
                    Ok(()) => {
                        // Text values live in the settings file (secrets only in the Keychain).
                        if !secret {
                            let (id, k, v) = (plugin_id.clone(), key.clone(), value.clone());
                            this.settings.update(|s| {
                                let map = s.plugins.settings.entry(id).or_default();
                                match v {
                                    Some(v) => map.insert(k, v),
                                    None => map.remove(&k),
                                };
                            });
                        }
                        if let Some(input) = &input {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        if value.is_some() { FieldCheck::Saved } else { FieldCheck::Idle }
                    }
                    Err(SettingError::Invalid(m)) => FieldCheck::Invalid(m.into()),
                    Err(SettingError::Unverifiable(m)) => FieldCheck::Unverifiable(m.into()),
                    Err(SettingError::Failed(e)) => {
                        this.banner = Some(format!("Couldn't save the setting: {e}"));
                        FieldCheck::Idle
                    }
                };
                this.field_checks.insert(field, check);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Set a switch or model setting of a plugin (`None` clears it), saved once the plugin has it.
    pub(crate) fn set_plugin_value(&mut self, plugin_id: String, key: String, value: Option<String>, cx: &mut Context<Self>) {
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let result = host.set_setting(&plugin_id, &key, value.clone(), false).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.settings.update(|s| {
                        let values = s.plugins.settings.entry(plugin_id).or_default();
                        match value {
                            Some(v) => values.insert(key, v),
                            None => values.remove(&key),
                        };
                    }),
                    Err(e) => this.banner = Some(format!("Couldn't save the setting: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Turn a service plugin on or off (saved; takes effect at once).
    pub(crate) fn set_plugin_enabled(&mut self, plugin_id: String, enabled: bool, cx: &mut Context<Self>) {
        if self.host.plugin(&plugin_id).is_some_and(|p| p.manifest.required) {
            return; // required guards stay on
        }
        self.settings.update(|s| {
            s.plugins.disabled.retain(|id| *id != plugin_id);
            if !enabled {
                s.plugins.disabled.push(plugin_id.clone());
            }
        });
        self.host.set_disabled(self.settings.settings.plugins.disabled.iter().cloned());
        if enabled {
            let host = self.host.clone();
            cx.spawn(async move |_, _| host.start_services().await).detach();
        } else {
            self.plugin_settings.remove(&plugin_id);
            self.plugin_status.insert(plugin_id, PluginStatus::Stopped);
        }
        cx.notify();
    }

    /// Approve or deny a plugin from your home or project folder (saved). An approved one
    /// starts at once.
    pub(crate) fn answer_plugin_approval(&mut self, plugin_id: String, approved: bool, cx: &mut Context<Self>) {
        if let Err(e) = self.host.answer_approval(&plugin_id, approved) {
            self.banner = Some(format!("Couldn't save your answer: {e:#}"));
        } else if approved && self.agent_id.as_deref() == Some(plugin_id.as_str()) {
            self.restart_agent(cx);
        } else if approved {
            let host = self.host.clone();
            cx.spawn(async move |_, _| host.start_services().await).detach();
        }
        cx.notify();
    }
}

/// What a Settings field's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveKind {
    /// Save the field's value once the plugin has checked it.
    Checked,
    /// Save it although it couldn't be checked.
    Anyway,
    /// Remove the setting.
    Clear,
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
        assert!(!path.exists(), "loading never writes");
        store.update(|s| {
            s.new_threads.model = Some(ModelRef { provider: "stub".into(), id: "stub-fast".into() });
            s.new_threads.thinking_level = Some("high".into());
            s.new_threads.permission_mode = Some("read-only".into());
            s.plugins.disabled = vec!["permissions".into()];
        });
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("\"newThreads\"") && json.contains("\"thinkingLevel\""));
        assert!(!json.contains("notifications"));
        assert!(json.contains("\"permissionMode\"") && json.contains("\"disabled\""));
        assert_eq!(SettingsStore::load(Some(path)).settings, store.settings);
    }

    #[test]
    fn partial_file_takes_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "newThreads": { "thinkingLevel": "high" } }"#).unwrap();
        assert_eq!(s.new_threads.thinking_level.as_deref(), Some("high"));
        assert_eq!(s.plugins, Plugins::default());
    }

    #[test]
    fn old_notification_switches_move_to_the_alerts_plugin() {
        let path = temp("legacy");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{ "notifications": { "onFinish": true, "sound": false } }"#).unwrap();
        let store = SettingsStore::load(Some(path.clone()));
        let alerts = &store.settings.plugins.settings[ALERTS];
        assert_eq!((alerts.get("notify"), alerts.get("sound").map(String::as_str)), (None, Some("false")));
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("notifications") && json.contains("\"alerts\""), "{json}");
        // Both on: nothing to keep.
        let mut s: Settings = serde_json::from_str(r#"{ "notifications": { "sound": true } }"#).unwrap();
        assert!(s.migrate());
        assert!(s.plugins.settings.is_empty());
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
        store.update(|s| s.new_threads.thinking_level = Some("low".into()));
        assert_eq!(SettingsStore::load(Some(path)).settings.new_threads.thinking_level.as_deref(), Some("low"));
    }
}
