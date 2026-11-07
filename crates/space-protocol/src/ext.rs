//! Space extensions to ACP.
//!
//! Extension methods use the `_space/` prefix (ACP reserves leading-underscore method
//! names for extensions), so any ACP peer that does not know them can ignore them.

use agent_client_protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Method name of the [`SpaceStatus`] notification (agent → client).
pub const STATUS_METHOD: &str = "_space/status";
/// Method name of the [`ConfigureSession`] request (client → agent).
pub const CONFIGURE_METHOD: &str = "_space/session/configure";
/// Method name of the [`PreExecute`] request (agent → client).
pub const PRE_EXECUTE_METHOD: &str = "_space/tools/preExecute";
/// Method name of the [`Confine`] request (agent → client).
pub const CONFINE_METHOD: &str = "_space/sandbox/confine";
/// `_meta` key of a `session/prompt` carrying the prompt's hidden [`ContextSection`]s.
pub const CONTEXT_META_KEY: &str = "space/context";

/// A model the agent can switch a session to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    /// Display name as the provider reports it (e.g. `"Claude Opus 5.5"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `_space/status` (agent → client notification, also the response of
/// `_space/session/configure`): what the session is running on and what it could run on.
///
/// All fields except `sessionId` are optional so an agent can send partial updates; the
/// client merges them.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    JsonRpcNotification,
    JsonRpcResponse,
)]
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
    /// Models the session can be switched to with `_space/session/configure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_models: Option<Vec<ModelInfo>>,
    /// Thinking levels the current model supports (e.g. `["low", "medium", "high"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_levels: Option<Vec<String>>,
    /// Estimated tokens currently in the model context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// Context window of the current model, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

impl SpaceStatus {
    /// Overlay the fields present in `other` onto `self`.
    pub fn merge(&mut self, other: &SpaceStatus) {
        fn take<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
            if src.is_some() {
                dst.clone_from(src);
            }
        }
        if !other.session_id.is_empty() {
            self.session_id.clone_from(&other.session_id);
        }
        take(&mut self.provider, &other.provider);
        take(&mut self.model, &other.model);
        take(&mut self.thinking_level, &other.thinking_level);
        take(&mut self.available_models, &other.available_models);
        take(&mut self.thinking_levels, &other.thinking_levels);
        take(&mut self.context_tokens, &other.context_tokens);
        take(&mut self.context_window, &other.context_window);
    }

    /// The [`ModelInfo`] of the current model, if the agent listed it.
    pub fn current_model(&self) -> Option<&ModelInfo> {
        let model = self.model.as_deref()?;
        self.available_models.as_ref()?.iter().find(|m| {
            m.id == model && self.provider.as_deref().is_none_or(|p| p == m.provider)
        })
    }
}

/// `_space/session/configure` (client → agent request): change a session's model and/or
/// thinking level. Omitted fields stay as they are. `provider` defaults to the session's
/// current provider. The agent answers with the session's full [`SpaceStatus`] after the
/// change (a thinking level the model does not support may be clamped).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_space/session/configure", response = SpaceStatus)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureSession {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

/// `_space/tools/preExecute` (agent → client request): may the agent run this tool call now?
/// The client runs its tool pipeline (and may ask the user) before answering, so the request
/// can take as long as the user does. An agent that gets an error must not run the call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_space/tools/preExecute", response = PreExecuteResult)]
#[serde(rename_all = "camelCase")]
pub struct PreExecute {
    pub session_id: String,
    pub tool_call_id: String,
    /// The agent's tool name (`bash`, `read`, `edit`, ...).
    pub tool: String,
    /// ACP tool kind (`read`, `edit`, `execute`, `search`, `fetch`, `other`, ...).
    pub kind: String,
    /// The tool's arguments as the agent will run them.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Working directory the call runs in, if not the session's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// The answer to [`PreExecute`]: `allow` or `deny` (an "ask" has already been put to the user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct PreExecuteResult {
    pub decision: Verdict,
    /// Why a call is denied, for the model and the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Deny,
}

/// `_space/sandbox/confine` (agent → client request): the argv to spawn instead of `argv` for
/// this session (wrapped in a sandbox, or unchanged). An agent that gets an error must not
/// spawn the command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_space/sandbox/confine", response = ConfineResult)]
#[serde(rename_all = "camelCase")]
pub struct Confine {
    pub session_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ConfineResult {
    pub argv: Vec<String>,
}

/// Hidden text a plugin adds to a prompt (the agent puts it in its system prompt, never in
/// the user's message or the transcript).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextSection {
    /// Stable id (the agent may use it as a prompt-section name).
    pub id: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;

    #[test]
    fn status_wire_format() {
        let json = r#"{"sessionId":"s1","model":"claude-opus-5-5","provider":"claude-cli","thinkingLevel":"high",
            "availableModels":[{"provider":"claude-cli","id":"claude-opus-5-5","name":"Claude Opus 5.5"}],
            "thinkingLevels":["low","high"],"contextTokens":1200,"contextWindow":1000000}"#;
        let s: SpaceStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s.session_id, "s1");
        assert_eq!(s.thinking_level.as_deref(), Some("high"));
        assert_eq!(s.current_model().and_then(|m| m.name.as_deref()), Some("Claude Opus 5.5"));
        assert_eq!(s.thinking_levels.as_deref().map(<[String]>::len), Some(2));
        assert_eq!(s.context_window, Some(1_000_000));
        assert_eq!(s.method(), STATUS_METHOD);
    }

    #[test]
    fn configure_wire_format() {
        let req = ConfigureSession {
            session_id: "s1".into(),
            model: Some("claude-sonnet-5-5".into()),
            ..Default::default()
        };
        assert_eq!(req.method(), CONFIGURE_METHOD);
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            serde_json::json!({"sessionId": "s1", "model": "claude-sonnet-5-5"})
        );
    }

    #[test]
    fn pre_execute_wire_format() {
        let req: PreExecute = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "toolCallId": "c1", "tool": "bash", "kind": "execute", "args": {"command": "ls"}
        }))
        .unwrap();
        assert_eq!(req.method(), PRE_EXECUTE_METHOD);
        assert_eq!(req.args["command"], "ls");
        let res = PreExecuteResult { decision: Verdict::Deny, reason: Some("no".into()) };
        assert_eq!(serde_json::to_value(&res).unwrap(), serde_json::json!({"decision": "deny", "reason": "no"}));
        let c = Confine { session_id: "s".into(), argv: vec!["ls".into()], cwd: "/".into() };
        assert_eq!(c.method(), CONFINE_METHOD);
    }

    #[test]
    fn status_merge() {
        let mut a = SpaceStatus { session_id: "s".into(), model: Some("m".into()), ..Default::default() };
        a.merge(&SpaceStatus {
            session_id: String::new(),
            provider: Some("p".into()),
            thinking_levels: Some(vec!["low".into()]),
            ..Default::default()
        });
        assert_eq!(a.model.as_deref(), Some("m"));
        assert_eq!(a.provider.as_deref(), Some("p"));
        assert_eq!(a.thinking_levels, Some(vec!["low".to_string()]));
        assert_eq!(a.session_id, "s");
    }
}
