//! `space-alerts`: tells you when an agent finishes, pauses or needs your approval. A
//! notification when you're not looking at the thread (with the sound unless it's off), the
//! sound alone when you are. A tool call waiting for a guard alerts once per turn; each approval
//! card alerts. Each of the two can be turned off in Settings.
//!
//! It only decides; Space posts the notification (`host/notify`), so clicking it opens the
//! thread, and when Space can't post notifications only the sound plays.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};

const NOTIFY: &str = "notify";
const SOUND: &str = "sound";

/// Which alerts you want.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Prefs {
    notify: bool,
    sound: bool,
}

impl Prefs {
    /// Both on unless turned off.
    fn from(values: &SettingValues) -> Self {
        let on = |key| values.get(key).is_none_or(|v| v != "false");
        Self { notify: on(NOTIFY), sound: on(SOUND) }
    }
}

/// What to post for `event`, if anything. `paused`: sessions whose turn already said it's
/// paused.
fn alert(event: &Observed, prefs: Prefs, paused: &mut HashSet<String>) -> Option<Notify> {
    let (session_id, title, watching) = match event {
        Observed::TurnStarted { session_id } => {
            paused.remove(session_id);
            return None;
        }
        Observed::TurnEnded { session_id, stop_reason, watching } => {
            paused.remove(session_id);
            let title = match stop_reason.as_str() {
                "cancelled" => return None,
                "error" => "Agent stopped with an error",
                _ => "Agent finished",
            };
            (session_id, title.to_string(), *watching)
        }
        // Once per turn: a call may stay paused a long time.
        Observed::ToolHeld { session_id, reason, watching, .. } => {
            if !paused.insert(session_id.clone()) {
                return None;
            }
            (session_id, format!("Paused: {}", reason.trim_end_matches('.')), *watching)
        }
        // Each card waits for you, so each one alerts.
        Observed::PermissionRequested { session_id, watching, .. } => (session_id, "Needs your approval".to_string(), *watching),
        _ => return None,
    };
    let banner = !watching && prefs.notify;
    (banner || prefs.sound).then(|| Notify {
        title,
        // Space shows the thread's title.
        body: None,
        session_id: Some(session_id.clone()),
        banner,
        sound: prefs.sound,
    })
}

struct Alerts {
    prefs: Mutex<Prefs>,
    paused: Mutex<HashSet<String>>,
    host: OnceLock<Host>,
}

impl Plugin for Alerts {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        *self.prefs.lock().unwrap() = Prefs::from(&init.settings);
        let _ = self.host.set(host);
        Ok(Initialized::default())
    }

    fn settings_schema(&self) -> SettingsSchema {
        let switch = |key: &str, label: &str, description: &str| SettingField {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            kind: SettingKind::Switch,
            placeholder: None,
            default: Some("true".into()),
        };
        SettingsSchema {
            fields: vec![
                switch(NOTIFY, "Notify when an agent finishes or needs you", "Shows a notification when you're not looking at the thread."),
                switch(SOUND, "Play a sound", "When an agent finishes or needs you."),
            ],
        }
    }

    fn settings_changed(&self, changed: SettingsChanged) {
        *self.prefs.lock().unwrap() = Prefs::from(&changed.values);
    }

    async fn observe(&self, event: Observed) {
        let prefs = *self.prefs.lock().unwrap();
        let Some(notify) = alert(&event, prefs, &mut self.paused.lock().unwrap()) else { return };
        let Some(host) = self.host.get() else { return };
        let params = serde_json::to_value(notify).expect("serializable");
        if let Err(e) = host.request(HOST_NOTIFY, params).await {
            eprintln!("space-alerts: {HOST_NOTIFY}: {e}");
        }
    }
}

fn main() {
    let prefs = Prefs::from(&SettingValues::new());
    space_plugin::run(Alerts { prefs: Mutex::new(prefs), paused: Mutex::new(HashSet::new()), host: OnceLock::new() });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Prefs = Prefs { notify: true, sound: true };

    fn ended(reason: &str, watching: bool) -> Observed {
        Observed::TurnEnded { session_id: "s".into(), stop_reason: reason.into(), watching }
    }

    fn held(watching: bool) -> Observed {
        Observed::ToolHeld { session_id: "s".into(), call_id: "c".into(), reason: "OpenAI is unreachable.".into(), watching }
    }

    /// (title, banner, sound) of the alert, if any.
    fn decide(event: Observed, prefs: Prefs) -> Option<(String, bool, bool)> {
        alert(&event, prefs, &mut HashSet::new()).map(|n| (n.title, n.banner, n.sound))
    }

    #[test]
    fn finished_turns() {
        // Not watching: a banner with the sound; watching: the sound alone.
        assert_eq!(decide(ended("end_turn", false), ON), Some(("Agent finished".into(), true, true)));
        assert_eq!(decide(ended("end_turn", true), ON), Some(("Agent finished".into(), false, true)));
        assert_eq!(decide(ended("error", false), ON), Some(("Agent stopped with an error".into(), true, true)));
        // You stopped it: nothing.
        assert_eq!(decide(ended("cancelled", false), ON), None);
    }

    #[test]
    fn settings() {
        let silent = Prefs { notify: true, sound: false };
        assert_eq!(decide(ended("end_turn", false), silent), Some(("Agent finished".into(), true, false)));
        assert_eq!(decide(ended("end_turn", true), silent), None);
        let sound_only = Prefs { notify: false, sound: true };
        assert_eq!(decide(ended("end_turn", false), sound_only), Some(("Agent finished".into(), false, true)));
        assert_eq!(decide(ended("end_turn", false), Prefs { notify: false, sound: false }), None);
        // Unset settings are on; "false" turns one off.
        assert_eq!(Prefs::from(&SettingValues::new()), ON);
        let values: SettingValues = [(SOUND.to_string(), "false".to_string())].into();
        assert_eq!(Prefs::from(&values), silent);
    }

    #[test]
    fn a_paused_call_alerts_once_per_turn() {
        let mut paused = HashSet::new();
        let first = alert(&held(false), ON, &mut paused).unwrap();
        assert_eq!((first.title.as_str(), first.session_id.as_deref()), ("Paused: OpenAI is unreachable", Some("s")));
        assert!(alert(&held(false), ON, &mut paused).is_none());
        assert!(alert(&Observed::TurnStarted { session_id: "s".into() }, ON, &mut paused).is_none());
        assert!(alert(&held(true), ON, &mut paused).is_some_and(|n| !n.banner && n.sound));
    }

    #[test]
    fn every_approval_card_alerts() {
        let asked = |watching| Observed::PermissionRequested { session_id: "s".into(), call_id: "c".into(), reason: None, watching };
        let mut paused = HashSet::new();
        let first = alert(&asked(false), ON, &mut paused).unwrap();
        assert_eq!((first.title.as_str(), first.session_id.as_deref(), first.banner), ("Needs your approval", Some("s"), true));
        assert!(alert(&asked(false), ON, &mut paused).is_some());
        assert_eq!(decide(asked(true), ON), Some(("Needs your approval".into(), false, true)));
    }
}
