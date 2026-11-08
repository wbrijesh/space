//! Traces and the trajectory seam: what happened around every model request, for the
//! Trajectory view.
//!
//! An agent that speaks the `_space/*` methods sends [`TraceBatch`]es (`_space/trace`, agent →
//! client notification) while it works: each request's header (system prompt, tools, options,
//! only when they change), the new messages it sent, its timing, usage and retries, and its
//! tool calls. The host adds what only it knows ([`TraceEvent::Prompt`], [`TraceEvent::Context`],
//! [`TraceEvent::Guard`]) and hands every batch, in order, to plugins that observe
//! [`EVENT_TRACE`](crate::manifest::EVENT_TRACE) and use
//! [`USE_READ_TRACE`](crate::manifest::USE_READ_TRACE) (`trace/record`).
//!
//! A plugin that provides [`CAP_TRAJECTORY`](crate::manifest::CAP_TRAJECTORY) turns them into
//! rows the app shows: `trajectory/read` (a page of [`TrajectoryRow`]s), `trajectory/detail`
//! (one row's full content), `trajectory/stats`, and tells the host when a thread's trajectory
//! changed (`trajectory/changed`).
//!
//! Times are Unix milliseconds. Requests are numbered per session from 1; turns and steps too.

use agent_client_protocol::JsonRpcNotification;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ext::TurnUsage;

/// `_space/trace` (agent → client notification): [`TraceBatch`].
pub const TRACE_METHOD: &str = "_space/trace";
/// Host → plugin request: [`TraceRecord`]. Sent in order, one at a time per plugin; none is
/// dropped.
pub const TRACE_RECORD: &str = "trace/record";
/// Host → plugin requests of the trajectory seam.
pub const TRAJECTORY_READ: &str = "trajectory/read";
pub const TRAJECTORY_DETAIL: &str = "trajectory/detail";
pub const TRAJECTORY_STATS: &str = "trajectory/stats";
/// Plugin → host notification: [`TrajectoryChanged`].
pub const TRAJECTORY_CHANGED: &str = "trajectory/changed";

/// `_space/trace` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_space/trace")]
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct TraceBatch {
    pub session_id: String,
    pub events: Vec<TraceEvent>,
}

/// One thing that happened. Agents send all but `Prompt`, `Context` and `Guard`, which the host
/// adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TraceEvent {
    TurnStart {
        turn: u32,
        at: u64,
    },
    TurnEnd {
        turn: u32,
        at: u64,
        /// The ACP stop reason (`end_turn`, `cancelled`, ...) or `error`.
        reason: String,
    },
    /// What every following request is sent with, until the next header. Sent only when it
    /// differs from the session's previous one.
    Header {
        header_id: u32,
        reason: HeaderReason,
        system_prompt: String,
        tools: Vec<ToolDef>,
        options: RequestOptions,
    },
    /// A model request went out.
    Request {
        request: u32,
        turn: u32,
        step: u32,
        at: u64,
        header_id: u32,
        /// The messages added since the previous request (Anthropic message JSON). A tool
        /// result's content is `{"toolResultRef": <call id>}` (the output is in its
        /// [`TraceEvent::ToolEnd`]); an image is `{"image": <sha256>, "mime": …, "path": …}`.
        messages: Vec<serde_json::Value>,
        /// The history was rewritten (e.g. compacted): `messages` holds all of it.
        #[serde(default)]
        rewritten: bool,
    },
    /// The first streamed token (text, thinking or tool input) arrived.
    FirstToken {
        request: u32,
        at: u64,
    },
    /// When streamed chunks arrived: the first at `t0`, each next `dt` ms after the one before.
    Chunks {
        request: u32,
        stream: ChunkKind,
        t0: u64,
        dt: Vec<u32>,
    },
    /// The request's answer ended.
    Response {
        request: u32,
        at: u64,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
        /// The provider's stop reason (`stop`, `toolUse`, `length`, `error`, `aborted`, ...).
        stop_reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        /// `provider/model` that answered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// The reply's text and thinking (what the transcript shows for it).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The request failed and is tried again after `delay_ms`.
    Retry {
        request: u32,
        at: u64,
        attempt: u32,
        max: u32,
        delay_ms: u64,
        error: String,
    },
    ToolStart {
        request: u32,
        call_id: String,
        name: String,
        input: serde_json::Value,
        at: u64,
    },
    ToolEnd {
        request: u32,
        call_id: String,
        at: u64,
        duration_ms: u64,
        /// At most 256 KB (`truncated` when it was longer).
        output: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        truncated: bool,
    },
    /// Host: the user's message for the next turn.
    Prompt {
        at: u64,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        /// Paths of the attached images.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
    /// Host: the hidden context sections sent with the prompt.
    Context {
        at: u64,
        sections: Vec<ContextFact>,
    },
    /// Host: what the tool pipeline decided about a call, and how long it held it.
    Guard {
        call_id: String,
        at: u64,
        /// The guard plugin that decided (none: every guard passed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        /// The session's permission mode.
        mode: String,
        verdict: GuardVerdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        held_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum HeaderReason {
    /// The session's first request.
    Initial,
    /// The prompt, tools or options changed.
    Change,
    /// The session was loaded again.
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ChunkKind {
    Text,
    Thinking,
    ToolInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum GuardVerdict {
    Allow,
    Deny,
    /// Held until a guard could decide (then one of the others follows).
    Held,
}

/// A tool as the model was told about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON schema of its input.
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// A request's options (what the provider was asked for besides messages and tools).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// The `thinking` setting as sent (e.g. `{"type":"enabled","budget_tokens":…}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// One hidden context section and the plugin it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ContextFact {
    pub plugin: String,
    pub id: String,
    pub text: String,
}

/// `trace/record` params: a batch, with the thread it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecord {
    /// Empty when the host doesn't know the session's thread.
    pub thread_id: String,
    pub session_id: String,
    pub events: Vec<TraceEvent>,
}

// ----- the trajectory seam --------------------------------------------------------------------

/// What a row is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RecordKind {
    /// A header: the system prompt and tools from here on.
    System,
    User,
    /// Text the user doesn't see that went to the model (Space's sections, the agent's own).
    Context,
    #[default]
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RecordStatus {
    #[default]
    Done,
    Running,
    Failed,
    Cancelled,
}

/// One row of the ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryRow {
    /// Stable for the life of the thread (`trajectory/detail` takes it).
    pub id: String,
    pub kind: RecordKind,
    /// 0 before the first turn.
    pub turn: u32,
    /// The step (model request plus the tools it asked for) it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// The thread-wide number of the request it belongs to (assistant: its own; tool: the one
    /// that asked for it; inputs: the one they went with).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<u32>,
    /// One line, at most 512 characters.
    pub preview: String,
    /// The tool's name, the context section's plugin, the header's change ("prompt", "tools").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// Absent while running (and for inputs, which take no time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Assistant: when the first token arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(default)]
    pub status: RecordStatus,
    /// Assistant: output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Tool: what the guard decided, and how long it held the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<GuardVerdict>,
    #[serde(default)]
    pub held_ms: u64,
    /// User: how many images and other files it carried.
    #[serde(default)]
    pub images: u32,
    #[serde(default)]
    pub files: u32,
}

/// One model request, for the separators between steps and the Request inspector.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestSummary {
    pub number: u32,
    pub turn: u32,
    pub step: u32,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    #[serde(default)]
    pub status: RecordStatus,
    #[serde(default)]
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Tool calls it asked for.
    #[serde(default)]
    pub tools: u32,
}

/// `trajectory/read` params: the page of rows before `before` (the newest when absent), or
/// the rows matching `query` (every word, any case, anywhere in a row's content).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryRead {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// `trajectory/read` result, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryPage {
    pub rows: Vec<TrajectoryRow>,
    /// The requests the rows belong to.
    pub requests: Vec<RequestSummary>,
    /// Pass as `before` for the page before this one; absent at the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older: Option<String>,
    /// Grows whenever the thread's trajectory changes.
    pub version: u64,
    pub stats: TrajectoryStats,
}

/// `trajectory/detail` params: a row's id, or `request:<n>` for a request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryDetailRequest {
    pub thread_id: String,
    pub id: String,
}

/// `trajectory/detail` result: everything the inspector shows for a row or a request. Fields
/// that don't apply are absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryDetail {
    pub id: String,
    /// User, context and assistant text (assistant: markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// System: the prompt and tools, and the header before it (for the diff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<HeaderDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_header: Option<HeaderDetail>,
    /// Tool: its input and output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<GuardDetail>,
    /// The request the row belongs to (or is), in full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestDetail>,
    /// User: the attached images' paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// The trace events behind it, as recorded.
    #[serde(default)]
    pub raw: Vec<TraceEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeaderDetail {
    pub header_id: u32,
    pub reason: Option<HeaderReason>,
    pub system_prompt: String,
    pub tools: Vec<ToolDef>,
    pub options: RequestOptions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardDetail {
    pub plugin: Option<String>,
    pub mode: String,
    pub verdict: Option<GuardVerdict>,
    pub reason: Option<String>,
    pub held_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestDetail {
    pub summary: RequestSummary,
    pub header_id: u32,
    pub options: RequestOptions,
    /// The messages the request added (see [`TraceEvent::Request`]).
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub rewritten: bool,
    /// When streamed chunks arrived, per stream.
    pub chunks: Vec<ChunkRun>,
    pub retries: Vec<RetryDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRun {
    pub stream: ChunkKind,
    pub t0: u64,
    pub dt: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetryDetail {
    pub at: u64,
    pub attempt: u32,
    pub max: u32,
    pub delay_ms: u64,
    pub error: String,
}

/// `trajectory/stats` params: [`crate::service::ThreadRef`]. Result (also in every page): the
/// thread's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryStats {
    pub turns: u32,
    /// Model requests.
    pub steps: u32,
    /// Usage summed over every request.
    pub usage: TurnUsage,
    /// Output tokens and generation time (first token to end) of the requests that streamed,
    /// for tokens per second.
    pub generated_tokens: u64,
    pub generation_ms: u64,
}

impl TrajectoryStats {
    /// Output tokens per second while generating.
    pub fn tokens_per_second(&self) -> Option<f64> {
        (self.generation_ms > 0).then(|| self.generated_tokens as f64 * 1000. / self.generation_ms as f64)
    }
}

/// `trajectory/changed` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryChanged {
    pub thread_id: String,
    pub version: u64,
}
