//! Session model: the events a session emits and a reducer that folds them into a
//! transcript. The UI keeps one [`SessionState`] per session and calls
//! [`SessionState::apply`] for every [`SessionEvent`] it receives.
//!
//! Events serialize as they are, so a saved thread is the list of its events, replayed through
//! the same reducer.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use space_protocol::{SpaceStatus, TurnUsage};

/// Opaque ACP session id.
pub type SessionId = String;

/// Tool call kind (mirrors ACP `ToolKind`, as a display string like `"execute"`).
pub type ToolKind = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolStatus::Pending => "pending",
            ToolStatus::InProgress => "running",
            ToolStatus::Completed => "done",
            ToolStatus::Failed => "failed",
        }
    }
}

/// A tool call as shown in the transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolStatus,
    /// Short plain-text rendering of the tool's content/output.
    pub output: String,
    /// File paths the tool touches.
    pub locations: Vec<String>,
    /// The host's tool pipeline stopped this call, and why.
    pub blocked: Option<String>,
    /// The pipeline is holding this call until a guard can decide again, and why.
    pub held: Option<String>,
}

/// Partial update to a tool call; `None` fields are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallPatch {
    pub id: String,
    pub title: Option<String>,
    pub kind: Option<ToolKind>,
    pub status: Option<ToolStatus>,
    pub output: Option<String>,
    pub locations: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, PermissionKind::AllowOnce | PermissionKind::AllowAlways)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: PermissionKind,
}

/// A pending `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Harness-local id used to answer the request.
    pub request_id: u64,
    pub tool_call: ToolCallPatch,
    pub options: Vec<PermissionOption>,
    /// Why the host is asking (a guardrail's reason); `None` for an agent's own request.
    pub reason: Option<String>,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// One of the request's option ids.
    Selected(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

/// An image sent with a user message: a file in the thread's attachments folder (named by the
/// sha256 of its bytes), already normalized (oriented, sRGB, scaled down, metadata stripped).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageRef {
    pub path: PathBuf,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// The name it had when attached (a file name; `None` for a pasted image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A long paste sent with a user message: a text file in the thread's attachments folder (named
/// by the sha256 of its bytes). The text went to the agent inline, as an ACP embedded resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextRef {
    pub path: PathBuf,
    /// `text/plain`.
    pub mime: String,
    /// Its length in characters.
    pub chars: u64,
}

/// What the user sends: the text, a quote it answers, and what goes with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserInput {
    pub text: String,
    /// Text quoted from the transcript, sent before the message.
    pub quote: Option<String>,
    pub attachments: Vec<PathBuf>,
    pub images: Vec<ImageRef>,
    pub texts: Vec<TextRef>,
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    /// The text the agent gets: the quote as a markdown blockquote ("> " on each line) and a
    /// blank line, then the message.
    pub fn prompt_text(&self) -> String {
        let Some(quote) = self.quote.as_deref().map(str::trim).filter(|q| !q.is_empty()) else { return self.text.clone() };
        let quoted: Vec<String> = quote.lines().map(|l| if l.trim().is_empty() { ">".to_string() } else { format!("> {l}") }).collect();
        let quoted = quoted.join("\n");
        if self.text.trim().is_empty() { quoted } else { format!("{quoted}\n\n{}", self.text) }
    }

    /// The transcript's record of it.
    pub fn into_message(self, at_ms: Option<u64>) -> SessionEvent {
        SessionEvent::UserMessage { text: self.text, quote: self.quote, attachments: self.attachments, images: self.images, texts: self.texts, at_ms }
    }
}

/// Everything that can happen in a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionEvent {
    Created { plugin_id: String, cwd: PathBuf },
    /// The user sent a prompt (emitted by the harness, not the agent). `attachments` are
    /// the file paths sent along as ACP `resource_link` blocks; `images` went as ACP image blocks
    /// and `texts` (long pastes) as embedded resources.
    UserMessage {
        text: String,
        /// Text quoted from the transcript; the agent got it before `text`, as a blockquote.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quote: Option<String>,
        attachments: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        texts: Vec<TextRef>,
        /// When it was sent (Unix ms; `None` in threads saved before Space kept times).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
    },
    TurnStarted,
    AgentText { text: String },
    AgentThought { text: String },
    ToolCall(ToolCallInfo),
    ToolCallUpdate(ToolCallPatch),
    Plan(Vec<PlanEntry>),
    Status(SpaceStatus),
    PermissionRequested(PermissionRequest),
    /// The host's tool pipeline denied a tool call (a guard plugin, or the user via its card).
    ToolCallBlocked { id: String, reason: String },
    /// The pipeline holds a tool call (`reason`: a guard can't decide right now; it waits) or
    /// lets it go again (`None`). `title` names the call for a card the agent hasn't sent yet.
    ToolCallHeld { id: String, title: String, reason: Option<String> },
    PermissionResolved { request_id: u64, decision: PermissionDecision },
    /// The prompt request settled. `stop_reason` is the ACP stop reason (`end_turn`,
    /// `cancelled`, ...) or `error`; `at_ms` when (Unix ms); `usage` the tokens the turn used,
    /// if the agent said.
    TurnEnded {
        stop_reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
    },
    Error { message: String },
}

/// One entry in the rendered transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User { text: String, quote: Option<String>, attachments: Vec<PathBuf>, images: Vec<ImageRef>, texts: Vec<TextRef>, at_ms: Option<u64> },
    Agent(String),
    Thought(String),
    Tool(ToolCallInfo),
    Permission { request: PermissionRequest, resolved: Option<PermissionDecision> },
    Plan(Vec<PlanEntry>),
    Error(String),
    /// The end of a turn: why (`end_turn`, `cancelled`, `error`, ...), when, and what it used.
    TurnEnd { reason: String, at_ms: Option<u64>, usage: Option<TurnUsage> },
}

impl SessionEvent {
    /// A turn end without a time or usage (tests, and turns Space ends itself).
    pub fn turn_ended(stop_reason: impl Into<String>) -> Self {
        SessionEvent::TurnEnded { stop_reason: stop_reason.into(), at_ms: None, usage: None }
    }
}

impl TranscriptItem {
    /// A divider Space adds itself (e.g. `new-session`), with no time or usage.
    pub fn divider(reason: impl Into<String>) -> Self {
        TranscriptItem::TurnEnd { reason: reason.into(), at_ms: None, usage: None }
    }
}

/// Folded state of a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionState {
    pub id: SessionId,
    pub plugin_id: String,
    pub cwd: PathBuf,
    pub status: SpaceStatus,
    pub items: Vec<TranscriptItem>,
    /// A prompt is in flight.
    pub running: bool,
}

impl SessionState {
    pub fn new(id: impl Into<SessionId>) -> Self {
        let id = id.into();
        Self { status: SpaceStatus { session_id: id.clone(), ..Default::default() }, id, ..Default::default() }
    }

    pub fn apply(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Created { plugin_id, cwd } => {
                self.plugin_id.clone_from(plugin_id);
                self.cwd.clone_from(cwd);
            }
            SessionEvent::UserMessage { text, quote, attachments, images, texts, at_ms } => self.items.push(TranscriptItem::User {
                text: text.clone(),
                quote: quote.clone(),
                attachments: attachments.clone(),
                images: images.clone(),
                texts: texts.clone(),
                at_ms: *at_ms,
            }),
            SessionEvent::TurnStarted => self.running = true,
            SessionEvent::AgentText { text } => match self.items.last_mut() {
                Some(TranscriptItem::Agent(buf)) => buf.push_str(text),
                _ => self.items.push(TranscriptItem::Agent(text.clone())),
            },
            SessionEvent::AgentThought { text } => match self.items.last_mut() {
                Some(TranscriptItem::Thought(buf)) => buf.push_str(text),
                _ => self.items.push(TranscriptItem::Thought(text.clone())),
            },
            SessionEvent::ToolCall(call) => {
                if let Some(existing) = self.tool_mut(&call.id) {
                    let (blocked, held) = (existing.blocked.take(), existing.held.take());
                    *existing = call.clone();
                    existing.blocked = existing.blocked.take().or(blocked);
                    existing.held = existing.held.take().or(held);
                } else {
                    self.items.push(TranscriptItem::Tool(call.clone()));
                }
            }
            SessionEvent::ToolCallUpdate(patch) => {
                if let Some(existing) = self.tool_mut(&patch.id) {
                    patch_tool(existing, patch);
                } else {
                    // Update for an unknown call: materialize it.
                    let mut call = ToolCallInfo {
                        id: patch.id.clone(),
                        title: String::new(),
                        kind: "other".into(),
                        status: ToolStatus::Pending,
                        output: String::new(),
                        locations: Vec::new(),
                        blocked: None,
                        held: None,
                    };
                    patch_tool(&mut call, patch);
                    self.items.push(TranscriptItem::Tool(call));
                }
            }
            SessionEvent::Plan(entries) => {
                if let Some(TranscriptItem::Plan(existing)) =
                    self.items.iter_mut().rev().find(|i| matches!(i, TranscriptItem::Plan(_)))
                {
                    existing.clone_from(entries);
                } else {
                    self.items.push(TranscriptItem::Plan(entries.clone()));
                }
            }
            SessionEvent::Status(status) => self.status.merge(status),
            SessionEvent::ToolCallBlocked { id, reason } => match self.tool_mut(id) {
                Some(call) => call.blocked = Some(reason.clone()),
                None => self.items.push(TranscriptItem::Tool(ToolCallInfo {
                    id: id.clone(),
                    title: String::new(),
                    kind: "other".into(),
                    status: ToolStatus::Failed,
                    output: String::new(),
                    locations: Vec::new(),
                    blocked: Some(reason.clone()),
                    held: None,
                })),
            },
            SessionEvent::ToolCallHeld { id, title, reason } => match self.tool_mut(id) {
                Some(call) => call.held.clone_from(reason),
                None if reason.is_some() => self.items.push(TranscriptItem::Tool(ToolCallInfo {
                    id: id.clone(),
                    title: title.clone(),
                    kind: "other".into(),
                    status: ToolStatus::Pending,
                    output: String::new(),
                    locations: Vec::new(),
                    blocked: None,
                    held: reason.clone(),
                })),
                None => {}
            },
            SessionEvent::PermissionRequested(request) => {
                self.items.push(TranscriptItem::Permission { request: request.clone(), resolved: None })
            }
            SessionEvent::PermissionResolved { request_id, decision } => {
                for item in self.items.iter_mut().rev() {
                    if let TranscriptItem::Permission { request, resolved } = item
                        && request.request_id == *request_id
                    {
                        *resolved = Some(decision.clone());
                        break;
                    }
                }
            }
            SessionEvent::TurnEnded { stop_reason, at_ms, usage } => {
                self.running = false;
                // Nothing waits once the turn is over.
                for item in &mut self.items {
                    if let TranscriptItem::Tool(t) = item {
                        t.held = None;
                    }
                }
                // Any unanswered permission card is moot now.
                for item in &mut self.items {
                    if let TranscriptItem::Permission { resolved: resolved @ None, .. } = item {
                        *resolved = Some(PermissionDecision::Cancelled);
                    }
                }
                self.items.push(TranscriptItem::TurnEnd { reason: stop_reason.clone(), at_ms: *at_ms, usage: usage.clone() });
            }
            SessionEvent::Error { message } => self.items.push(TranscriptItem::Error(message.clone())),
        }
    }

    /// Why the turn is paused: the reason of a tool call the pipeline is holding, if any.
    pub fn held(&self) -> Option<&str> {
        self.items.iter().find_map(|i| match i {
            TranscriptItem::Tool(t) => t.held.as_deref(),
            _ => None,
        })
    }

    /// The first unresolved permission request, if any.
    pub fn pending_permission(&self) -> Option<&PermissionRequest> {
        self.items.iter().find_map(|i| match i {
            TranscriptItem::Permission { request, resolved: None } => Some(request),
            _ => None,
        })
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCallInfo> {
        self.items.iter_mut().rev().find_map(|i| match i {
            TranscriptItem::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }
}

fn patch_tool(call: &mut ToolCallInfo, patch: &ToolCallPatch) {
    if let Some(v) = &patch.title {
        call.title.clone_from(v);
    }
    if let Some(v) = &patch.kind {
        call.kind.clone_from(v);
    }
    if let Some(v) = patch.status {
        call.status = v;
    }
    if let Some(v) = &patch.output {
        call.output.clone_from(v);
    }
    if let Some(v) = &patch.locations {
        call.locations.clone_from(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_goes_first_as_a_blockquote() {
        let input = |text: &str, quote: Option<&str>| UserInput { quote: quote.map(String::from), ..UserInput::text(text) };
        assert_eq!(input("why?", None).prompt_text(), "why?");
        assert_eq!(input("why?", Some("one\n\ntwo")).prompt_text(), "> one\n>\n> two\n\nwhy?");
        assert_eq!(input("", Some("just this")).prompt_text(), "> just this");
        assert_eq!(input("why?", Some("  ")).prompt_text(), "why?");
        let SessionEvent::UserMessage { text, quote, .. } = input("why?", Some("one")).into_message(None) else { panic!() };
        assert_eq!((text.as_str(), quote.as_deref()), ("why?", Some("one")));
        // Threads saved before quotes load.
        let old: SessionEvent = serde_json::from_str(r#"{"UserMessage":{"text":"hi","attachments":[]}}"#).unwrap();
        assert!(matches!(old, SessionEvent::UserMessage { quote: None, .. }));
    }

    #[test]
    fn merges_chunks_and_patches_tools() {
        let mut s = SessionState::new("s1");
        let ev = |e: SessionEvent| e;
        for e in [
            ev(SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None }),
            ev(SessionEvent::TurnStarted),
            ev(SessionEvent::AgentThought { text: "think".into() }),
            ev(SessionEvent::AgentThought { text: "ing".into() }),
            ev(SessionEvent::AgentText { text: "Hel".into() }),
            ev(SessionEvent::AgentText { text: "lo".into() }),
            ev(SessionEvent::ToolCall(ToolCallInfo {
                id: "t1".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: ToolStatus::Pending,
                output: String::new(),
                locations: vec![],
                blocked: None,
                held: None,
            })),
            ev(SessionEvent::ToolCallUpdate(ToolCallPatch {
                id: "t1".into(),
                status: Some(ToolStatus::Completed),
                output: Some("a\nb".into()),
                ..Default::default()
            })),
            ev(SessionEvent::AgentText { text: "Done".into() }),
            ev(SessionEvent::turn_ended("end_turn")),
        ] {
            s.apply(&e);
        }
        assert!(!s.running);
        assert_eq!(s.items.len(), 6);
        assert_eq!(s.items[1], TranscriptItem::Thought("thinking".into()));
        assert_eq!(s.items[2], TranscriptItem::Agent("Hello".into()));
        match &s.items[3] {
            TranscriptItem::Tool(t) => {
                assert_eq!(t.status, ToolStatus::Completed);
                assert_eq!(t.output, "a\nb");
                assert_eq!(t.title, "ls");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.items[4], TranscriptItem::Agent("Done".into()));
        assert_eq!(s.items[5], TranscriptItem::divider("end_turn"));
    }

    #[test]
    fn blocked_calls_keep_their_reason() {
        let mut s = SessionState::new("s1");
        // The block can arrive before the agent reports the call.
        s.apply(&SessionEvent::ToolCallBlocked { id: "t".into(), reason: "no".into() });
        s.apply(&SessionEvent::ToolCall(ToolCallInfo {
            id: "t".into(),
            title: "bash: rm".into(),
            kind: "execute".into(),
            status: ToolStatus::InProgress,
            output: String::new(),
            locations: vec![],
            blocked: None,
            held: None,
        }));
        s.apply(&SessionEvent::ToolCallUpdate(ToolCallPatch {
            id: "t".into(),
            status: Some(ToolStatus::Failed),
            ..Default::default()
        }));
        let [TranscriptItem::Tool(t)] = s.items.as_slice() else { panic!("{:?}", s.items) };
        assert_eq!((t.title.as_str(), t.blocked.as_deref(), t.status), ("bash: rm", Some("no"), ToolStatus::Failed));
        // After the agent's report, too.
        s.apply(&SessionEvent::ToolCallBlocked { id: "t".into(), reason: "again".into() });
        assert_eq!(s.items.len(), 1);
    }

    #[test]
    fn times_and_usage_round_trip_and_old_events_still_load() {
        let usage = TurnUsage { input: 4, output: 4, cache_read: Some(0), cache_write: Some(2668), reasoning: None, models: vec!["a/b".into()] };
        let events = [
            SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: Some(5) },
            SessionEvent::TurnEnded { stop_reason: "end_turn".into(), at_ms: Some(9), usage: Some(usage) },
        ];
        for e in &events {
            let json = serde_json::to_string(e).unwrap();
            assert_eq!(&serde_json::from_str::<SessionEvent>(&json).unwrap(), e, "{json}");
        }
        // Saved before Space kept times and usage.
        let old: SessionEvent = serde_json::from_str(r#"{"TurnEnded":{"stop_reason":"end_turn"}}"#).unwrap();
        assert_eq!(old, SessionEvent::turn_ended("end_turn"));
        let old: SessionEvent = serde_json::from_str(r#"{"UserMessage":{"text":"hi","attachments":[]}}"#).unwrap();
        assert!(matches!(old, SessionEvent::UserMessage { at_ms: None, .. }));
    }

    #[test]
    fn held_calls_show_until_released_or_the_turn_ends() {
        let mut s = SessionState::new("s1");
        let held = |reason: Option<&str>| SessionEvent::ToolCallHeld {
            id: "t".into(),
            title: "bash: ls".into(),
            reason: reason.map(Into::into),
        };
        // Held before the agent reports the call: a card with the host's title.
        s.apply(&held(Some("OpenAI rejected your API key")));
        assert_eq!(s.held(), Some("OpenAI rejected your API key"));
        let [TranscriptItem::Tool(t)] = s.items.as_slice() else { panic!("{:?}", s.items) };
        assert_eq!((t.title.as_str(), t.status), ("bash: ls", ToolStatus::Pending));
        // The agent's report keeps the hold; releasing clears it.
        s.apply(&SessionEvent::ToolCall(ToolCallInfo {
            id: "t".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: ToolStatus::Pending,
            output: String::new(),
            locations: vec![],
            blocked: None,
            held: None,
        }));
        assert_eq!(s.held(), Some("OpenAI rejected your API key"));
        s.apply(&held(None));
        assert_eq!(s.held(), None);
        // A release for an unknown call adds nothing; the end of a turn clears any hold.
        s.apply(&SessionEvent::ToolCallHeld { id: "x".into(), title: String::new(), reason: None });
        assert_eq!(s.items.len(), 1);
        s.apply(&held(Some("again")));
        s.apply(&SessionEvent::turn_ended("cancelled"));
        assert_eq!(s.held(), None);
    }

    #[test]
    fn permission_lifecycle() {
        let mut s = SessionState::new("s1");
        let req = PermissionRequest {
            request_id: 7,
            tool_call: ToolCallPatch { id: "t".into(), ..Default::default() },
            options: vec![PermissionOption { id: "ok".into(), name: "Allow".into(), kind: PermissionKind::AllowOnce }],
            reason: None,
        };
        s.apply(&SessionEvent::PermissionRequested(req));
        assert_eq!(s.pending_permission().map(|r| r.request_id), Some(7));
        s.apply(&SessionEvent::PermissionResolved {
            request_id: 7,
            decision: PermissionDecision::Selected("ok".into()),
        });
        assert!(s.pending_permission().is_none());
    }

    #[test]
    fn status_merges() {
        let mut s = SessionState::new("s1");
        s.apply(&SessionEvent::Status(SpaceStatus {
            session_id: "s1".into(),
            model: Some("m".into()),
            ..Default::default()
        }));
        s.apply(&SessionEvent::Status(SpaceStatus {
            session_id: "s1".into(),
            thinking_level: Some("high".into()),
            ..Default::default()
        }));
        assert_eq!(s.status.model.as_deref(), Some("m"));
        assert_eq!(s.status.thinking_level.as_deref(), Some("high"));
    }
}
