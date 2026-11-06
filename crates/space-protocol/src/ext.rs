//! Space extensions to ACP.
//!
//! Extension methods use the `_space/` prefix (ACP reserves leading-underscore method
//! names for extensions), so any ACP peer that does not know them can ignore them.

use agent_client_protocol::JsonRpcNotification;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Method name of the [`SpaceStatus`] notification (agent → client).
pub const STATUS_METHOD: &str = "_space/status";

/// `_space/status` (agent → client notification): what the agent is currently running on.
/// Shown in the session header. All fields except `sessionId` are optional so an agent can
/// send partial updates; the client merges them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_space/status")]
#[serde(rename_all = "camelCase")]
pub struct SpaceStatus {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

impl SpaceStatus {
    /// Overlay the fields present in `other` onto `self`.
    pub fn merge(&mut self, other: &SpaceStatus) {
        if !other.session_id.is_empty() {
            self.session_id.clone_from(&other.session_id);
        }
        if other.provider.is_some() {
            self.provider.clone_from(&other.provider);
        }
        if other.model.is_some() {
            self.model.clone_from(&other.model);
        }
        if other.thinking_level.is_some() {
            self.thinking_level.clone_from(&other.thinking_level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;

    #[test]
    fn status_wire_format() {
        let json = r#"{"sessionId":"s1","model":"claude-opus-5-5","provider":"claude-cli","thinkingLevel":"high"}"#;
        let s: SpaceStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s.session_id, "s1");
        assert_eq!(s.thinking_level.as_deref(), Some("high"));
        assert_eq!(s.method(), STATUS_METHOD);
    }

    #[test]
    fn status_merge() {
        let mut a = SpaceStatus { session_id: "s".into(), model: Some("m".into()), ..Default::default() };
        a.merge(&SpaceStatus { session_id: String::new(), provider: Some("p".into()), ..Default::default() });
        assert_eq!(a.model.as_deref(), Some("m"));
        assert_eq!(a.provider.as_deref(), Some("p"));
        assert_eq!(a.session_id, "s");
    }
}
