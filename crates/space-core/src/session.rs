//! Session model: the events a session emits and a reducer that folds them into a
//! transcript. The UI keeps one [`SessionState`] per session and calls
//! [`SessionState::apply`] for every [`SessionEvent`] it receives.
//!
//! Events serialize as they are, so a saved thread is the list of its events, replayed through
//! the same reducer.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use space_protocol::{ForkOf, RetryState, SpaceStatus, TurnUsage};

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
    /// A subagent made the call (not the conversation itself).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
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

/// A new message id (a UUID, as agents take them).
pub fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// What the user sends: the text, a quote it answers, and what goes with it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserInput {
    pub text: String,
    /// Text quoted from the transcript, sent before the message.
    pub quote: Option<String>,
    pub attachments: Vec<PathBuf>,
    pub images: Vec<ImageRef>,
    pub texts: Vec<TextRef>,
    /// Told to the agent before the message but not part of it (the transcript doesn't show
    /// it): say, that the folder was switched to another branch.
    pub note: Option<String>,
    /// The client's id for the message (a UUID): the agent knows it by this when asked to go back
    /// to it. `None`: one is made when it's sent.
    pub id: Option<String>,
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    /// The text the agent gets: the note and a blank line, the quote as a markdown blockquote
    /// ("> " on each line) and a blank line, then the message.
    pub fn prompt_text(&self) -> String {
        let message = self.message_text();
        match self.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(note) => format!("{note}\n\n{message}"),
            None => message,
        }
    }

    fn message_text(&self) -> String {
        let Some(quote) = self.quote.as_deref().map(str::trim).filter(|q| !q.is_empty()) else { return self.text.clone() };
        let quoted: Vec<String> = quote.lines().map(|l| if l.trim().is_empty() { ">".to_string() } else { format!("> {l}") }).collect();
        let quoted = quoted.join("\n");
        if self.text.trim().is_empty() { quoted } else { format!("{quoted}\n\n{}", self.text) }
    }

    /// The transcript's record of it. `steered`: it went to a running turn.
    pub fn into_message(self, at_ms: Option<u64>, steered: bool) -> SessionEvent {
        SessionEvent::UserMessage {
            text: self.text,
            quote: self.quote,
            attachments: self.attachments,
            images: self.images,
            texts: self.texts,
            at_ms,
            id: self.id,
            steered,
        }
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
        /// Its id (see [`UserInput::id`]; `None` in threads saved before Space kept them).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// It went to a running turn (steered), so no turn starts with it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        steered: bool,
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
    /// A model request waits out a bad network (`None`: nothing waits). Live only: not saved.
    Retry(Option<RetryState>),
    /// A model request failed after part of its reply streamed and is sent again: that part
    /// (the text and thoughts since the last other item) leaves the transcript.
    AttemptDiscarded,
    /// The thread goes on in a new agent session (the saved one couldn't be continued): what came
    /// before isn't in the agent's conversation.
    NewSession,
    /// The thread went back to just before message `message_id` (emitted by Space): it and
    /// everything after it leave the transcript, and the next message goes on from a copy of
    /// session `session_id`'s conversation up to there ([`SessionState::fork`]).
    Rewound { message_id: String, session_id: String },
    /// The last [`SessionEvent::Rewound`] is undone: what it took away comes back (and what was
    /// sent since goes), and the thread goes on in the session it rewound.
    RewindUndone,
    /// The files the turn's edits wrote, as they were when it ended: a hash of the contents
    /// (`None`: no such file). Emitted by Space; going back compares them to tell your own
    /// later changes from the agent's.
    Written { files: Vec<(PathBuf, Option<String>)> },
}

/// One entry in the rendered transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TranscriptItem {
    User {
        text: String,
        quote: Option<String>,
        attachments: Vec<PathBuf>,
        images: Vec<ImageRef>,
        texts: Vec<TextRef>,
        at_ms: Option<u64>,
        /// The message's id (`None` in threads saved before Space kept them).
        id: Option<String>,
        /// It went to a running turn.
        steered: bool,
    },
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

/// The divider's reason when the thread goes on in a new session ([`SessionEvent::NewSession`]).
pub const NEW_SESSION: &str = "new-session";

impl TranscriptItem {
    /// A divider Space adds itself (e.g. `new-session`), with no time or usage.
    pub fn divider(reason: impl Into<String>) -> Self {
        TranscriptItem::TurnEnd { reason: reason.into(), at_ms: None, usage: None }
    }
}

/// What a [`SessionEvent::Rewound`] took away, to undo it.
#[derive(Debug, Clone, PartialEq)]
pub struct Rewind {
    /// Where it cut the transcript.
    pub at: usize,
    pub message_id: String,
    /// The session the thread was in.
    pub session_id: String,
    /// What it took away.
    pub items: Vec<TranscriptItem>,
    /// The fork the thread was waiting to make before it.
    pub fork: Option<ForkOf>,
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
    /// The running turn's model request waits out a bad network.
    pub retry: Option<RetryState>,
    /// The thread went back to an earlier message and nothing was sent since: the next message
    /// goes to a copy of this session's conversation up to there.
    pub fork: Option<ForkOf>,
    /// The rewinds that can be undone, the last one last.
    pub rewinds: Vec<Rewind>,
    /// What the agent's edits last wrote to each file ([`SessionEvent::Written`]).
    pub written: HashMap<PathBuf, Option<String>>,
}

impl SessionState {
    pub fn new(id: impl Into<SessionId>) -> Self {
        let id = id.into();
        Self { status: SpaceStatus { session_id: id.clone(), ..Default::default() }, id, ..Default::default() }
    }

    pub fn apply(&mut self, event: &SessionEvent) {
        // The request got through (or the turn is over): nothing waits.
        if matches!(
            event,
            SessionEvent::AgentText { .. } | SessionEvent::AgentThought { .. } | SessionEvent::ToolCall(_) | SessionEvent::TurnEnded { .. } | SessionEvent::Error { .. }
        ) {
            self.retry = None;
        }
        match event {
            SessionEvent::Created { plugin_id, cwd } => {
                self.plugin_id.clone_from(plugin_id);
                self.cwd.clone_from(cwd);
            }
            SessionEvent::UserMessage { text, quote, attachments, images, texts, at_ms, id, steered } => {
                // The fork (if one was waiting) was made for this message.
                self.fork = None;
                self.items.push(TranscriptItem::User {
                    text: text.clone(),
                    quote: quote.clone(),
                    attachments: attachments.clone(),
                    images: images.clone(),
                    texts: texts.clone(),
                    at_ms: *at_ms,
                    id: id.clone(),
                    steered: *steered,
                })
            }
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
                        subagent: false,
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
                    subagent: false,
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
                    subagent: false,
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
            SessionEvent::Retry(state) => self.retry.clone_from(state),
            SessionEvent::AttemptDiscarded => {
                while matches!(self.items.last(), Some(TranscriptItem::Agent(_) | TranscriptItem::Thought(_))) {
                    self.items.pop();
                }
            }
            SessionEvent::NewSession => self.items.push(TranscriptItem::divider(NEW_SESSION)),
            SessionEvent::Rewound { message_id, session_id } => {
                let Some(at) = self.message_index(message_id) else {
                    tracing::warn!("rewind to message {message_id}, which isn't in the transcript");
                    return;
                };
                let items = self.items.split_off(at);
                let fork = self.fork.replace(ForkOf { session_id: session_id.clone(), message_id: message_id.clone() });
                self.rewinds.push(Rewind { at, message_id: message_id.clone(), session_id: session_id.clone(), items, fork });
                self.running = false;
                self.retry = None;
                // The context gauge shows the discarded conversation until the agent says again.
                self.status.context_tokens = None;
            }
            SessionEvent::RewindUndone => {
                let Some(r) = self.rewinds.pop() else { return };
                self.items.truncate(r.at);
                self.items.extend(r.items);
                self.fork = r.fork;
                self.running = false;
                self.retry = None;
                self.status.context_tokens = None;
            }
            SessionEvent::Written { files } => {
                for (path, hash) in files {
                    self.written.insert(path.clone(), hash.clone());
                }
            }
        }
    }

    /// Where message `id` is in the transcript.
    pub fn message_index(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|i| matches!(i, TranscriptItem::User { id: Some(m), .. } if m == id))
    }

    /// Whether the agent can go back to the message at `ix`: it has an id and is in the agent's
    /// current conversation (not before a [`SessionEvent::NewSession`]).
    pub fn can_rewind_to(&self, ix: usize) -> bool {
        let Some(TranscriptItem::User { id: Some(_), .. }) = self.items.get(ix) else { return false };
        !self.items[ix..].iter().any(|i| matches!(i, TranscriptItem::TurnEnd { reason, .. } if reason == NEW_SESSION))
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
        let noted = UserInput { note: Some("<branch-changed/>".into()), ..input("why?", Some("q")) };
        assert_eq!(noted.prompt_text(), "<branch-changed/>\n\n> q\n\nwhy?");
        let SessionEvent::UserMessage { text, quote, .. } = input("why?", Some("one")).into_message(None, false) else { panic!() };
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
            ev(SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None, id: None, steered: false }),
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
                subagent: false,
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
    fn a_dropped_attempt_leaves_and_waiting_ends_with_new_content() {
        use space_protocol::{RetryKind, RetryState};
        let mut s = SessionState::new("s1");
        let tool = ToolCallInfo {
            id: "t".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: ToolStatus::Completed,
            output: String::new(),
            locations: Vec::new(),
            blocked: None,
            held: None,
            subagent: false,
        };
        let waiting = RetryState { kind: RetryKind::Offline, attempt: 1, reason: "ECONNRESET".into() };
        for e in [
            SessionEvent::TurnStarted,
            SessionEvent::AgentText { text: "Looking. ".into() },
            SessionEvent::ToolCall(tool),
            SessionEvent::AgentThought { text: "half a".into() },
            SessionEvent::AgentText { text: "Broken off".into() },
            SessionEvent::AttemptDiscarded,
            SessionEvent::Retry(Some(waiting.clone())),
        ] {
            s.apply(&e);
        }
        // Only the broken attempt left: the reply before the tool stays.
        assert!(matches!(&s.items[..], [TranscriptItem::Agent(a), TranscriptItem::Tool(_)] if a == "Looking. "));
        assert_eq!(s.retry, Some(waiting.clone()));
        s.apply(&SessionEvent::AgentText { text: "Whole".into() });
        assert_eq!(s.retry, None);
        // The turn ending clears it too.
        s.apply(&SessionEvent::Retry(Some(waiting)));
        s.apply(&SessionEvent::turn_ended("cancelled"));
        assert_eq!(s.retry, None);
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
            subagent: false,
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
            SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: Some(5), id: Some("m1".into()), steered: true },
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
        s.apply(&held(Some("the guard is restarting")));
        assert_eq!(s.held(), Some("the guard is restarting"));
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
            subagent: false,
        }));
        assert_eq!(s.held(), Some("the guard is restarting"));
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

    fn said(id: &str) -> SessionEvent {
        SessionEvent::UserMessage { text: id.into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None, id: Some(id.into()), steered: false }
    }

    fn turn(s: &mut SessionState, id: &str, reply: &str) {
        for e in [said(id), SessionEvent::TurnStarted, SessionEvent::AgentText { text: reply.into() }, SessionEvent::turn_ended("end_turn")] {
            s.apply(&e);
        }
    }

    fn texts(s: &SessionState) -> Vec<String> {
        s.items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::User { text, .. } | TranscriptItem::Agent(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_rewind_cuts_before_the_message_and_undo_brings_it_back() {
        let mut s = SessionState::new("s1");
        turn(&mut s, "a", "A");
        turn(&mut s, "b", "B");
        turn(&mut s, "c", "C");
        s.status.context_tokens = Some(900);
        s.apply(&SessionEvent::Rewound { message_id: "b".into(), session_id: "s1".into() });
        assert_eq!(texts(&s), ["a", "A"]);
        assert_eq!(s.fork, Some(ForkOf { session_id: "s1".into(), message_id: "b".into() }));
        assert_eq!(s.status.context_tokens, None);
        // The edited message goes out (the fork is made for it), and its turn runs.
        turn(&mut s, "b2", "B2");
        assert_eq!(s.fork, None);
        assert_eq!(texts(&s), ["a", "A", "b2", "B2"]);
        // Undo: the edit and its turn go, what the rewind took comes back.
        s.apply(&SessionEvent::RewindUndone);
        assert_eq!(texts(&s), ["a", "A", "b", "B", "c", "C"]);
        assert!(s.rewinds.is_empty() && s.fork.is_none());
        // Nothing to undo, or an unknown message: nothing happens.
        s.apply(&SessionEvent::RewindUndone);
        s.apply(&SessionEvent::Rewound { message_id: "zzz".into(), session_id: "s1".into() });
        assert_eq!(texts(&s).len(), 6);
    }

    #[test]
    fn rewinds_stack_and_undo_one_at_a_time() {
        let mut s = SessionState::new("s1");
        for id in ["a", "b", "c"] {
            turn(&mut s, id, &id.to_uppercase());
        }
        s.apply(&SessionEvent::Rewound { message_id: "c".into(), session_id: "s1".into() });
        s.apply(&SessionEvent::Rewound { message_id: "b".into(), session_id: "s1".into() });
        assert_eq!(texts(&s), ["a", "A"]);
        assert_eq!(s.fork.as_ref().map(|f| f.message_id.as_str()), Some("b"));
        s.apply(&SessionEvent::RewindUndone);
        assert_eq!(texts(&s), ["a", "A", "b", "B"]);
        // Back to waiting to fork before `c`.
        assert_eq!(s.fork.as_ref().map(|f| f.message_id.as_str()), Some("c"));
        s.apply(&SessionEvent::RewindUndone);
        assert_eq!(texts(&s), ["a", "A", "b", "B", "c", "C"]);
        assert_eq!(s.fork, None);
    }

    #[test]
    fn messages_before_a_new_session_cant_be_rewound_to() {
        let mut s = SessionState::new("s1");
        turn(&mut s, "a", "A");
        s.apply(&SessionEvent::NewSession);
        turn(&mut s, "b", "B");
        let ix = |id| s.message_index(id).unwrap();
        assert!(!s.can_rewind_to(ix("a")));
        assert!(s.can_rewind_to(ix("b")));
        // Saved before Space kept ids: no id, no rewind.
        let old: SessionEvent = serde_json::from_str(r#"{"UserMessage":{"text":"hi","attachments":[]}}"#).unwrap();
        s.apply(&old);
        assert!(!s.can_rewind_to(s.items.len() - 1));
        let json = serde_json::to_string(&SessionEvent::Rewound { message_id: "b".into(), session_id: "s".into() }).unwrap();
        assert_eq!(serde_json::from_str::<SessionEvent>(&json).unwrap(), SessionEvent::Rewound { message_id: "b".into(), session_id: "s".into() });
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
