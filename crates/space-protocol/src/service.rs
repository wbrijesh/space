//! The service-plugin protocol: what the host and a service plugin (any plugin that isn't an
//! ACP agent) say to each other.
//!
//! Framing is the same as ACP: JSON-RPC 2.0, one message per line on the plugin's
//! stdin/stdout, stderr free-form logs. The host calls the plugin; the plugin may notify the
//! host. Which calls a plugin gets follows from its manifest (`provides`, `hooks`):
//!
//! | Method | Direction | When |
//! |---|---|---|
//! | `plugin/initialize` | host → plugin | first, with the plugin's settings |
//! | `settings/schema` | host → plugin | `provides` `settings` |
//! | `settings/validate` | host → plugin | `provides` `settings`, before a new value is stored |
//! | `settings/changed` (notification) | host → plugin | the user changed a setting |
//! | `catalog/get` | host → plugin | `provides` `catalog:<name>` |
//! | `catalog/changed` (notification) | plugin → host | a catalog's entries changed |
//! | `context/get` | host → plugin | `provides` `context`, before each prompt |
//! | `sandbox/confine` | host → plugin | `provides` `sandbox`, before an agent spawns a command |
//! | `hooks/tools.preExecute` | host → plugin | hook `tools/pre-execute`, before every tool call |
//! | `hooks/ready` (notification) | plugin → host | a listener's answers may have changed (e.g. it can decide again): held calls run the chain again |
//! | `plugin/retry` (notification) | host → plugin | the user pressed a blocker's "Retry now" |
//! | `events/observe` | host → plugin | `observes` the event ([`Observed`]) |
//! | `store/list`, `store/load`, `store/write`, `store/delete`, `store/data-dir` | host → plugin | `provides` `session-store` |
//! | `title/generate` | host → plugin | `provides` `session-title`, after a thread's first message |
//! | `trace/record` | host → plugin | `observes` `trace` (and `uses` `read:trace`): every trace batch, in order |
//! | `trajectory/read`, `trajectory/detail`, `trajectory/stats` | host → plugin | `provides` `trajectory` |
//! | `trajectory/changed` (notification) | plugin → host | `provides` `trajectory`: a thread's trajectory changed |
//! | `terminal/open`, `terminal/attach`, `terminal/list`, `terminal/close` | host → plugin | `provides` `terminal` ([`crate::terminal`]) |
//! | `terminal/input`, `terminal/resize` (notifications) | host → plugin | `provides` `terminal`: keys and pastes, size |
//! | `terminal/output`, `terminal/title`, `terminal/exited` (notifications) | plugin → host | `provides` `terminal` |
//!
//! A plugin may call the host's services ([`HOST_SERVICES`]) that its manifest lists in `uses`;
//! any other call is answered [`NOT_PERMITTED`]. Notifications a plugin sends about something
//! its manifest doesn't declare (`catalog/changed` without a catalog, `hooks/ready` without a
//! hook) are ignored.
//!
//! Observed events are told one at a time, in order, and the host waits for each answer before
//! the next; nothing waits on the plugin. A plugin that falls behind loses its own newest
//! events (with a warning in the logs), never anyone else's.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::ext::{Complete, CompleteResult, ContextSection};

pub use crate::PROTOCOL_VERSION;

pub const INITIALIZE: &str = "plugin/initialize";
pub const SETTINGS_SCHEMA: &str = "settings/schema";
pub const SETTINGS_CHANGED: &str = "settings/changed";
pub const SETTINGS_VALIDATE: &str = "settings/validate";
pub const CATALOG_GET: &str = "catalog/get";
pub const CATALOG_CHANGED: &str = "catalog/changed";
pub const CONTEXT_GET: &str = "context/get";
pub const SANDBOX_CONFINE: &str = "sandbox/confine";
pub const HOOK_PRE_EXECUTE: &str = "hooks/tools.preExecute";
pub const HOOKS_READY: &str = "hooks/ready";
pub const PLUGIN_RETRY: &str = "plugin/retry";
pub const EVENTS_OBSERVE: &str = "events/observe";
/// The `session-store` seam: threads as a summary ([`ThreadMeta`]) and an append-only list of
/// events the store keeps as they are (the host decides what they mean).
pub const STORE_LIST: &str = "store/list";
pub const STORE_LOAD: &str = "store/load";
pub const STORE_WRITE: &str = "store/write";
pub const STORE_DELETE: &str = "store/delete";
pub const STORE_DATA_DIR: &str = "store/data-dir";
/// The `session-title` seam: a thread's title from its first message ([`TitleRequest`]).
pub const TITLE_GENERATE: &str = "title/generate";

/// Host services: plugin → host calls, each allowed only when the manifest lists it in `uses`.
pub const HOST_NOTIFY: &str = "host/notify";
pub const HOST_OPEN_URL: &str = "host/open-url";
/// [`Complete`] → [`CompleteResult`]: one short completion by the model of a session's agent
/// (the host forwards it as `_space/complete`).
pub const AGENT_COMPLETE: &str = "agent/complete";
pub const SESSIONS_READ: &str = "sessions/read";
pub const HOST_SERVICES: &[&str] = &[HOST_NOTIFY, HOST_OPEN_URL, AGENT_COMPLETE, SESSIONS_READ];
/// JSON-RPC error code of a call the plugin's manifest doesn't allow.
pub const NOT_PERMITTED: i64 = -32003;

/// Setting values by key. Secrets are included in clear (the host keeps them in the Keychain);
/// an unset setting is absent.
pub type SettingValues = BTreeMap<String, String>;

/// `plugin/initialize` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Initialize {
    pub protocol_version: u32,
    #[serde(default)]
    pub settings: SettingValues,
    /// Secret settings that are set but couldn't be read (e.g. the Keychain refused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    /// Space's own folder (`~/.space`), where a plugin that keeps files puts them (the session
    /// store: `threads/`). Absent when Space keeps nothing on disk (tests, scenarios).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_dir: Option<String>,
}

/// `plugin/initialize` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Initialized {
    /// The [`PROTOCOL_VERSION`] the plugin was built for (the host refuses a mismatch). The
    /// Rust SDK fills it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One field the Settings page shows for the plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: SettingKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// The value while the setting isn't set (a switch: `true` or `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    /// Plain text, stored in the settings file.
    #[default]
    Text,
    /// Stored in the macOS Keychain, never shown again once saved.
    Secret,
    /// On or off (`true` / `false`), stored in the settings file.
    Switch,
    /// One of the agent's models (`<provider>/<model id>`), or unset ("Same as agent"); stored
    /// in the settings file.
    Model,
}

/// `settings/schema` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsSchema {
    pub fields: Vec<SettingField>,
}

/// `settings/changed` params: every setting's current value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsChanged {
    pub values: SettingValues,
    /// As in [`Initialize::unreadable`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
}

/// `settings/validate` params: a value the user is about to save.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingCheck {
    pub key: String,
    pub value: String,
}

/// `settings/validate` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingVerdict {
    pub status: SettingStatus,
    /// Shown under the field (why it's invalid or couldn't be checked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SettingVerdict {
    pub fn ok() -> Self {
        Self { status: SettingStatus::Ok, message: None }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { status: SettingStatus::Invalid, message: Some(message.into()) }
    }
    pub fn unverifiable(message: impl Into<String>) -> Self {
        Self { status: SettingStatus::Unverifiable, message: Some(message.into()) }
    }
}

/// `ok`: store it. `invalid`: don't. `unverifiable`: it couldn't be checked now; the user may
/// save it anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingStatus {
    Ok,
    Invalid,
    Unverifiable,
}

/// `events/observe` params: one event the plugin `observes`. What the user and the agent wrote
/// (`output`, `title`) is included only for plugins that use `read:transcript`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all_fields = "camelCase")]
pub enum Observed {
    /// A thread's session was created.
    #[serde(rename = "session/created")]
    SessionCreated { session_id: String, agent: String, project_root: String },
    /// The user sent a prompt; the agent is working.
    #[serde(rename = "turn/started")]
    TurnStarted { session_id: String },
    /// The prompt settled. `stop_reason`: the ACP stop reason (`end_turn`, `cancelled`, ...)
    /// or `error`. `watching`: Space is frontmost on this thread.
    #[serde(rename = "turn/ended")]
    TurnEnded { session_id: String, stop_reason: String, watching: bool },
    /// A tool call waits because a guard can't decide right now, and why.
    #[serde(rename = "tool/held")]
    ToolHeld { session_id: String, call_id: String, reason: String, watching: bool },
    /// A tool call finished.
    #[serde(rename = "tool/result")]
    ToolResult {
        session_id: String,
        call_id: String,
        failed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// A thread was deleted.
    #[serde(rename = "thread/deleted")]
    ThreadDeleted { thread_id: String },
    /// A thread got its title.
    #[serde(rename = "thread/titled")]
    ThreadTitled {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

impl Observed {
    /// The event's name, as manifests list it in `observes`.
    pub fn name(&self) -> &'static str {
        use crate::manifest::*;
        match self {
            Observed::SessionCreated { .. } => EVENT_SESSION_CREATED,
            Observed::TurnStarted { .. } => EVENT_TURN_STARTED,
            Observed::TurnEnded { .. } => EVENT_TURN_ENDED,
            Observed::ToolHeld { .. } => EVENT_TOOL_HELD,
            Observed::ToolResult { .. } => EVENT_TOOL_RESULT,
            Observed::ThreadTitled { .. } => EVENT_THREAD_TITLED,
            Observed::ThreadDeleted { .. } => EVENT_THREAD_DELETED,
        }
    }

    /// The event without what the user and the agent wrote.
    pub fn without_transcript(self) -> Self {
        match self {
            Observed::ToolResult { session_id, call_id, failed, .. } => {
                Observed::ToolResult { session_id, call_id, failed, output: None }
            }
            Observed::ThreadTitled { session_id, .. } => Observed::ThreadTitled { session_id, title: None },
            other => other,
        }
    }
}

/// `host/notify` params: a macOS notification, or only the sound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notify {
    pub title: String,
    /// The second line. Default: the thread's title, when `session_id` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Clicking the notification opens this session's thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Show a banner. Without one, or when Space can't post notifications (not allowed, not
    /// bundled), only the sound plays.
    #[serde(default = "yes")]
    pub banner: bool,
    #[serde(default)]
    pub sound: bool,
}

/// `host/open-url` params: an `https://` or `http://` URL to open in the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpenUrl {
    pub url: String,
}

/// `catalog/get` params, and `catalog/changed` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogName {
    pub name: String,
}

/// `catalog/get` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

/// One entry of a catalog (for `permission-modes`: one mode).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    /// Icon name the UI knows (e.g. `shield-check`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub description: String,
    /// The entry new threads start with.
    #[serde(default)]
    pub default: bool,
    /// Entries that need confirming the first time they are picked in a thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    /// `false`: selectable, but sending is blocked until `blocker` is resolved.
    #[serde(default = "yes")]
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<Blocker>,
}

fn yes() -> bool {
    true
}

/// What stops a catalog entry from being used, and how to fix it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub text: String,
    /// Links shown after the text, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<BlockerAction>,
    /// When the provider last tried to resolve it, and when it tries next (Unix ms). The UI
    /// shows them as live "last tried … ago · next try in …" times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_ms: Option<u64>,
}

/// A blocker's link. `action` is one of `open-settings:<plugin id>`, `open-url:<url>`,
/// `retry:<plugin id>` (sends `plugin/retry`) and `use-mode:<entry id>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlockerAction {
    pub label: String,
    pub action: String,
}

impl BlockerAction {
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Self { label: label.into(), action: action.into() }
    }
}

/// `context/get` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRequest {
    pub session_id: String,
    /// The thread's permission mode (an entry id of `permission-modes`).
    pub mode: String,
    pub project_root: String,
}

/// `context/get` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSections {
    pub sections: Vec<ContextSection>,
}

/// `sandbox/confine` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfineRequest {
    pub session_id: String,
    pub mode: String,
    pub project_root: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

/// `sandbox/confine` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Confined {
    pub argv: Vec<String>,
}

/// `hooks/tools.preExecute` params: the pending call, the thread's mode, and what the host
/// knows of the thread (from its own records, never from the agent's text).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookCall {
    pub session_id: String,
    pub mode: String,
    pub project_root: String,
    pub call: ToolCallFacts,
    /// Earlier items of the thread, oldest first (the pending call is not included).
    #[serde(default)]
    pub history: Vec<HistoryItem>,
}

/// A tool call as the agent will run it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallFacts {
    pub id: String,
    pub tool: String,
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub cwd: String,
}

/// One earlier item of a thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HistoryItem {
    /// A message the user typed.
    User { text: String },
    /// A tool call the pipeline let through or stopped (arguments only, never output).
    Tool { id: String, tool: String, kind: String, args: serde_json::Value, allowed: bool },
}

/// A hook listener's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookDecision {
    pub decision: HookVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl HookDecision {
    pub fn next() -> Self {
        Self { decision: HookVerdict::Next, reason: None }
    }
    pub fn allow() -> Self {
        Self { decision: HookVerdict::Allow, reason: None }
    }
    pub fn deny(reason: impl Into<String>) -> Self {
        Self { decision: HookVerdict::Deny, reason: Some(reason.into()) }
    }
    pub fn ask(reason: impl Into<String>) -> Self {
        Self { decision: HookVerdict::Ask, reason: Some(reason.into()) }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self { decision: HookVerdict::Unavailable, reason: Some(reason.into()) }
    }
}

/// `deny` stops the chain; `allow` and `next` pass the call on; `ask` is remembered and the
/// user is asked after the last listener, unless a later one denies. `unavailable`: the
/// listener can't decide right now (fail closed): the host holds the call and runs the whole
/// chain again once the listener sends `hooks/ready`, the thread's mode changes, or the user
/// stops the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HookVerdict {
    Allow,
    Deny,
    Ask,
    Next,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_formats() {
        let call: HookCall = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "mode": "read-only", "projectRoot": "/p",
            "call": {"id": "c", "tool": "bash", "kind": "execute", "args": {"command": "ls"}, "cwd": "/p"},
            "history": [{"type": "user", "text": "hi"},
                        {"type": "tool", "id": "c0", "tool": "read", "kind": "read", "args": {"path": "a"}, "allowed": true}]
        }))
        .unwrap();
        assert_eq!(call.history.len(), 2);
        assert!(matches!(&call.history[1], HistoryItem::Tool { allowed: true, .. }));
        assert_eq!(
            serde_json::to_value(HookDecision::ask("why")).unwrap(),
            serde_json::json!({"decision": "ask", "reason": "why"})
        );
        let e: CatalogEntry = serde_json::from_value(serde_json::json!({"id": "a", "name": "A"})).unwrap();
        assert!(e.ready && !e.default && e.blocker.is_none());
        let f: SettingField = serde_json::from_value(serde_json::json!({"key": "k", "label": "K", "kind": "secret"})).unwrap();
        assert_eq!(f.kind, SettingKind::Secret);
        assert_eq!(
            serde_json::to_value(HookDecision::unavailable("down")).unwrap(),
            serde_json::json!({"decision": "unavailable", "reason": "down"})
        );
        let b = Blocker {
            text: "t".into(),
            actions: vec![BlockerAction::new("Retry now", "retry:p")],
            last_attempt_ms: Some(1),
            next_attempt_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            serde_json::json!({"text": "t", "actions": [{"label": "Retry now", "action": "retry:p"}], "lastAttemptMs": 1})
        );
        assert_eq!(
            serde_json::to_value(SettingVerdict::unverifiable("offline")).unwrap(),
            serde_json::json!({"status": "unverifiable", "message": "offline"})
        );
    }

    #[test]
    fn observed_events() {
        let ended = Observed::TurnEnded { session_id: "s".into(), stop_reason: "end_turn".into(), watching: true };
        assert_eq!(
            serde_json::to_value(&ended).unwrap(),
            serde_json::json!({"event": "turn/ended", "sessionId": "s", "stopReason": "end_turn", "watching": true})
        );
        // The tag is the name manifests use.
        let result = Observed::ToolResult { session_id: "s".into(), call_id: "c".into(), failed: false, output: Some("x".into()) };
        let titled = Observed::ThreadTitled { session_id: "s".into(), title: Some("t".into()) };
        for e in [ended, result.clone(), titled.clone()] {
            assert_eq!(serde_json::to_value(&e).unwrap()["event"], e.name());
            assert!(crate::manifest::EVENTS.contains(&e.name()));
        }
        assert_eq!(
            result.without_transcript(),
            Observed::ToolResult { session_id: "s".into(), call_id: "c".into(), failed: false, output: None }
        );
        assert_eq!(titled.without_transcript(), Observed::ThreadTitled { session_id: "s".into(), title: None });
        let n: Notify = serde_json::from_value(serde_json::json!({"title": "Done"})).unwrap();
        assert!(n.banner && !n.sound && n.body.is_none());
    }
}

/// A saved thread, as the sidebar lists it (`meta.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMeta {
    pub id: String,
    pub title: String,
    /// The project folder the thread works in.
    pub project: String,
    /// Its permission mode (a `permission-modes` id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The agent plugin, and its session to load to continue the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Unix milliseconds.
    pub created_ms: u64,
    pub updated_ms: u64,
    /// `title` was generated (`session-title`), not taken from the first message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub titled: bool,
    /// When the thread was closed (Unix milliseconds): it's kept, off the sidebar, to be resumed
    /// until Space deletes it. `None`: open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_ms: Option<u64>,
}

/// `title/generate` params: the thread's first message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TitleRequest {
    pub session_id: String,
    pub first_message: String,
    pub project_root: String,
}

/// `title/generate` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Title {
    pub title: String,
}

/// `store/load`, `store/delete` and `store/data-dir` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub thread_id: String,
}

/// `store/write` params: append `events` to the thread's log, and replace its summary when
/// `meta` is set. Nothing is written for a thread until it has a summary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreWrite {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ThreadMeta>,
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    /// How many of the thread's events Space had saved before these. The store skips the ones
    /// it already has, so a write that is sent again (after a failure, or at quit) isn't kept
    /// twice.
    #[serde(default)]
    pub first: u64,
}

/// `store/list` result: every saved thread, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredThreads {
    pub threads: Vec<ThreadMeta>,
}

/// `store/load` result. A damaged event line is left out (the store logs it).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StoredThread {
    pub meta: ThreadMeta,
    pub events: Vec<serde_json::Value>,
}

/// `store/data-dir` result: the folder the thread's agent may keep the session's own files in,
/// and the one Space keeps the thread's attached images in (both deleted with the thread).
/// They may not exist yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DataDir {
    pub data_dir: String,
    /// `None` from a store that doesn't keep attachments (Space then keeps them in a temporary
    /// folder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_dir: Option<String>,
}
