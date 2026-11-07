//! Session model: the events a session emits and a reducer that folds them into a
//! transcript. The UI keeps one [`SessionState`] per session and calls
//! [`SessionState::apply`] for every [`SessionEvent`] it receives.

use std::path::PathBuf;

use space_protocol::SpaceStatus;

/// Opaque ACP session id.
pub type SessionId = String;

/// Tool call kind (mirrors ACP `ToolKind`, as a display string like `"execute"`).
pub type ToolKind = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq)]
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
}

/// Partial update to a tool call; `None` fields are unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallPatch {
    pub id: String,
    pub title: Option<String>,
    pub kind: Option<ToolKind>,
    pub status: Option<ToolStatus>,
    pub output: Option<String>,
    pub locations: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: PermissionKind,
}

/// A pending `session/request_permission`.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// Harness-local id used to answer the request.
    pub request_id: u64,
    pub tool_call: ToolCallPatch,
    pub options: Vec<PermissionOption>,
    /// Why the host is asking (a guardrail's reason); `None` for an agent's own request.
    pub reason: Option<String>,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// One of the request's option ids.
    Selected(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

/// Everything that can happen in a session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Created { plugin_id: String, cwd: PathBuf },
    /// The user sent a prompt (emitted by the harness, not the agent). `attachments` are
    /// the file paths sent along as ACP `resource_link` blocks.
    UserMessage { text: String, attachments: Vec<PathBuf> },
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
    PermissionResolved { request_id: u64, decision: PermissionDecision },
    /// The prompt request settled. `stop_reason` is the ACP stop reason (`end_turn`,
    /// `cancelled`, ...) or `error`.
    TurnEnded { stop_reason: String },
    Error { message: String },
}

/// One entry in the rendered transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User { text: String, attachments: Vec<PathBuf> },
    Agent(String),
    Thought(String),
    Tool(ToolCallInfo),
    Permission { request: PermissionRequest, resolved: Option<PermissionDecision> },
    Plan(Vec<PlanEntry>),
    Error(String),
    /// A turn that ended with something other than `end_turn`.
    TurnEnd(String),
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
            SessionEvent::UserMessage { text, attachments } => self
                .items
                .push(TranscriptItem::User { text: text.clone(), attachments: attachments.clone() }),
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
                    let blocked = existing.blocked.take();
                    *existing = call.clone();
                    existing.blocked = existing.blocked.take().or(blocked);
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
                })),
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
            SessionEvent::TurnEnded { stop_reason } => {
                self.running = false;
                // Any unanswered permission card is moot now.
                for item in &mut self.items {
                    if let TranscriptItem::Permission { resolved: resolved @ None, .. } = item {
                        *resolved = Some(PermissionDecision::Cancelled);
                    }
                }
                if stop_reason != "end_turn" {
                    self.items.push(TranscriptItem::TurnEnd(stop_reason.clone()));
                }
            }
            SessionEvent::Error { message } => self.items.push(TranscriptItem::Error(message.clone())),
        }
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
    fn merges_chunks_and_patches_tools() {
        let mut s = SessionState::new("s1");
        let ev = |e: SessionEvent| e;
        for e in [
            ev(SessionEvent::UserMessage { text: "hi".into(), attachments: vec![] }),
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
            })),
            ev(SessionEvent::ToolCallUpdate(ToolCallPatch {
                id: "t1".into(),
                status: Some(ToolStatus::Completed),
                output: Some("a\nb".into()),
                ..Default::default()
            })),
            ev(SessionEvent::AgentText { text: "Done".into() }),
            ev(SessionEvent::TurnEnded { stop_reason: "end_turn".into() }),
        ] {
            s.apply(&e);
        }
        assert!(!s.running);
        assert_eq!(s.items.len(), 5);
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
