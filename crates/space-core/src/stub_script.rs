//! Scripts for `space-stub-agent`: what a turn streams for a given prompt, including steps
//! that hold the turn open indefinitely (for UI development "scenarios").
//!
//! The stub reads a [`StubScript`] (JSON) from the file named by `SPACE_STUB_SCRIPT`. A
//! prompt whose text equals a turn's `prompt` plays that turn's steps; other prompts use the
//! stub's built-in behavior.
//!
//! ```json
//! { "turns": [ { "prompt": "Run the tests", "steps": [
//!     { "type": "thought", "text": "Run the suite first." },
//!     { "type": "tool", "id": "t1", "title": "bash: cargo test", "kind": "execute", "status": "in_progress" },
//!     { "type": "hold" } ] } ] }
//! ```

use serde::{Deserialize, Serialize};

/// Env var naming the script file the stub agent loads.
pub const SCRIPT_ENV: &str = "SPACE_STUB_SCRIPT";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StubScript {
    pub turns: Vec<ScriptedTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptedTurn {
    /// Exact prompt text this turn answers.
    pub prompt: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Stream reasoning text.
    Thought { text: String },
    /// Stream reply text (word by word).
    Text { text: String },
    /// Report a tool call, or update one with the same `id`.
    Tool {
        id: String,
        title: String,
        #[serde(default = "default_kind")]
        kind: String,
        /// `pending` | `in_progress` | `completed` | `failed`.
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Ask `session/request_permission` for tool `tool_id` and wait for the answer.
    Permission { tool_id: String, title: String },
    /// Report a tool call, put it to the host's tool pipeline (`_space/tools/preExecute`) and
    /// finish it accordingly: completed with `output`, or failed with the pipeline's reason.
    Guard {
        id: String,
        title: String,
        #[serde(default = "default_kind")]
        kind: String,
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Report context usage in `_space/status`.
    Context { tokens: u64 },
    Sleep { ms: u64 },
    /// Keep the turn open until it is cancelled.
    Hold,
    /// Fail the turn with a JSON-RPC error carrying `message`.
    Fail { message: String },
}

fn default_kind() -> String {
    "other".into()
}

impl StubScript {
    pub fn turn(&self, prompt: &str) -> Option<&ScriptedTurn> {
        self.turns.iter().find(|t| t.prompt == prompt)
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_round_trips() {
        let json = r#"{"turns":[{"prompt":"p","steps":[
            {"type":"thought","text":"t"},
            {"type":"tool","id":"1","title":"bash: ls","status":"in_progress"},
            {"type":"permission","tool_id":"1","title":"bash: rm"},
            {"type":"context","tokens":1200},
            {"type":"hold"}]}]}"#;
        let s: StubScript = serde_json::from_str(json).unwrap();
        let turn = s.turn("p").unwrap();
        assert_eq!(turn.steps.len(), 5);
        assert!(matches!(&turn.steps[1], Step::Tool { kind, .. } if kind == "other"));
        assert_eq!(turn.steps[4], Step::Hold);
        let again: StubScript = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(again, s);
        assert!(s.turn("other").is_none());
    }
}
