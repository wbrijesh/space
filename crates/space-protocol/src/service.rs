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
//! | `settings/changed` (notification) | host → plugin | the user changed a setting |
//! | `catalog/get` | host → plugin | `provides` `catalog:<name>` |
//! | `catalog/changed` (notification) | plugin → host | a catalog's entries changed |
//! | `context/get` | host → plugin | `provides` `context`, before each prompt |
//! | `sandbox/confine` | host → plugin | `provides` `sandbox`, before an agent spawns a command |
//! | `hooks/tools.preExecute` | host → plugin | hook `tools/pre-execute`, before every tool call |

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::ext::ContextSection;

pub const PROTOCOL_VERSION: u32 = 1;

pub const INITIALIZE: &str = "plugin/initialize";
pub const SETTINGS_SCHEMA: &str = "settings/schema";
pub const SETTINGS_CHANGED: &str = "settings/changed";
pub const CATALOG_GET: &str = "catalog/get";
pub const CATALOG_CHANGED: &str = "catalog/changed";
pub const CONTEXT_GET: &str = "context/get";
pub const SANDBOX_CONFINE: &str = "sandbox/confine";
pub const HOOK_PRE_EXECUTE: &str = "hooks/tools.preExecute";

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
}

/// `plugin/initialize` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Initialized {
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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    /// Plain text, stored in the settings file.
    #[default]
    Text,
    /// Stored in the macOS Keychain, never shown again once saved.
    Secret,
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

/// What stops a catalog entry from being used, and where to fix it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub text: String,
    /// Link label (e.g. "Open Settings").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
    /// What the link does: `open-settings:<plugin id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
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
}

/// `deny` stops the chain; `allow` and `next` pass the call on; `ask` is remembered and the
/// user is asked after the last listener, unless a later one denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HookVerdict {
    Allow,
    Deny,
    Ask,
    Next,
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
    }
}
