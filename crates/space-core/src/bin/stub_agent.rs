//! `space-stub-agent`: a tiny scripted ACP agent for tests and offline development.
//!
//! Every prompt streams: a `_space/status` update, a thought chunk, a tool call with a
//! status update, then message chunks, and ends the turn with `end_turn`.
//! Prompt keywords change the script:
//! * `permission` — ask `session/request_permission` before running the tool.
//! * `slow` — stream 50 chunks 100 ms apart (use with cancel).
//! * `crash` — exit with code 3 mid-turn.
//!
//! With `SPACE_STUB_SCRIPT=<file>` (see `space_core::stub_script`), a prompt matching a
//! scripted turn plays that turn's steps instead, which can hold a turn open indefinitely
//! (thinking, running a tool, waiting for permission). The app's dev scenarios use this.
//!
//! Sessions report two fake models (`stub-model`, `stub-fast`) and thinking levels through
//! `_space/status`, and accept `_space/session/configure`. They offer the same session modes as
//! pi-agent (`write`, the default, and `read`) and accept `session/set_mode`; the mode only
//! shows in the stub's log.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use space_core::stub_script::{SCRIPT_ENV, Step, StubScript};
use space_protocol::{ConfigureSession, ModelInfo, SpaceStatus};
use space_protocol::acp::{self, Agent, ByteStreams, Client, Responder};
use space_protocol::acp_schema::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Conn = acp::v1::ConnectionTo<Client>;

#[derive(Default)]
struct State {
    next_session: AtomicU64,
    cancelled: Mutex<HashSet<String>>,
    sessions: Mutex<HashMap<String, StubSession>>,
    script: StubScript,
}

#[derive(Clone)]
struct StubSession {
    model: String,
    thinking: String,
    context_tokens: u64,
    mode: String,
}

const MODELS: [(&str, &str); 2] = [("stub-model", "Stub Model"), ("stub-fast", "Stub Fast")];
const LEVELS: [&str; 4] = ["low", "medium", "high", "max"];
/// Session modes, as pi-agent offers them.
const MODES: [(&str, &str, &str); 2] = [
    ("write", "Write", "The agent can edit files and run any command."),
    ("read", "Read-only", "The agent investigates without changing anything."),
];

fn mode_state(current: &str) -> SessionModeState {
    SessionModeState::new(
        current.to_string(),
        MODES.iter().map(|(id, name, desc)| SessionMode::new(*id, *name).description(desc.to_string())).collect(),
    )
}

impl State {
    fn status(&self, session_id: &str) -> SpaceStatus {
        let s = self.sessions.lock().unwrap().get(session_id).cloned().unwrap_or(StubSession {
            model: MODELS[0].0.into(),
            thinking: "low".into(),
            context_tokens: 0,
            mode: MODES[0].0.into(),
        });
        SpaceStatus {
            session_id: session_id.to_string(),
            provider: Some("stub".into()),
            model: Some(s.model),
            thinking_level: Some(s.thinking),
            available_models: Some(
                MODELS
                    .iter()
                    .map(|(id, name)| ModelInfo { provider: "stub".into(), id: (*id).into(), name: Some((*name).into()) })
                    .collect(),
            ),
            thinking_levels: Some(LEVELS.iter().map(|l| l.to_string()).collect()),
            context_tokens: Some(s.context_tokens),
            context_window: Some(200_000),
        }
    }
}

#[tokio::main]
async fn main() -> acp::Result<()> {
    let script = match std::env::var_os(SCRIPT_ENV) {
        Some(path) => match StubScript::load(std::path::Path::new(&path)) {
            Ok(script) => {
                eprintln!("space-stub-agent: loaded {} scripted turn(s)", script.turns.len());
                script
            }
            Err(e) => {
                eprintln!("space-stub-agent: bad {SCRIPT_ENV}: {e:#}");
                StubScript::default()
            }
        },
        None => StubScript::default(),
    };
    let state = Arc::new(State { script, ..State::default() });
    let transport = ByteStreams::new(tokio::io::stdout().compat_write(), tokio::io::stdin().compat());
    eprintln!("space-stub-agent: started");
    Agent
        .v1()
        .name("space-stub-agent")
        .on_receive_request(
            async |req: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_info(Implementation::new("space-stub-agent", env!("CARGO_PKG_VERSION"))),
                )
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |_req: NewSessionRequest, responder, cx: Conn| {
                    let id = format!("stub-{}", state.next_session.fetch_add(1, Ordering::Relaxed) + 1);
                    state.sessions.lock().unwrap().insert(
                        id.clone(),
                        StubSession {
                            model: MODELS[0].0.into(),
                            thinking: "low".into(),
                            context_tokens: 0,
                            mode: MODES[0].0.into(),
                        },
                    );
                    responder.respond(NewSessionResponse::new(id.clone()).modes(mode_state(MODES[0].0)))?;
                    cx.send_notification(state.status(&id))
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: PromptRequest, responder, cx: Conn| {
                    let state = state.clone();
                    let task_cx = cx.clone();
                    cx.spawn(async move { run_turn(task_cx, state, req, responder).await })
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: ConfigureSession, responder: Responder<SpaceStatus, acp::V1>, _cx: Conn| {
                    eprintln!("space-stub-agent: configure {req:?}");
                    {
                        let mut sessions = state.sessions.lock().unwrap();
                        let Some(s) = sessions.get_mut(&req.session_id) else {
                            return responder.respond_with_error(acp::Error::invalid_params());
                        };
                        if let Some(m) = req.model {
                            if !MODELS.iter().any(|(id, _)| *id == m) {
                                return responder.respond_with_error(acp::Error::invalid_params());
                            }
                            s.model = m;
                        }
                        if let Some(t) = req.thinking_level {
                            s.thinking = t;
                        }
                    }
                    responder.respond(state.status(&req.session_id))
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: SetSessionModeRequest, responder: Responder<SetSessionModeResponse, acp::V1>, _cx: Conn| {
                    eprintln!("space-stub-agent: set_mode {} {}", req.session_id.0, req.mode_id.0);
                    let mut sessions = state.sessions.lock().unwrap();
                    match sessions.get_mut(&*req.session_id.0) {
                        Some(s) if MODES.iter().any(|(id, ..)| *id == &*req.mode_id.0) => {
                            s.mode = req.mode_id.0.to_string();
                            responder.respond(SetSessionModeResponse::default())
                        }
                        _ => responder.respond_with_error(acp::Error::invalid_params()),
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let state = state.clone();
                async move |n: CancelNotification, _cx| {
                    eprintln!("space-stub-agent: cancel {}", n.session_id.0);
                    state.cancelled.lock().unwrap().insert(n.session_id.0.to_string());
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        )
        .connect_to(transport)
        .await
}

async fn run_turn(
    cx: Conn,
    state: Arc<State>,
    req: PromptRequest,
    responder: Responder<PromptResponse, acp::V1>,
) -> acp::Result<()> {
    let sid = req.session_id.clone();
    state.cancelled.lock().unwrap().remove(&*sid.0);
    let text: String = req
        .prompt
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    let mode = state.sessions.lock().unwrap().get(&*sid.0).map(|s| s.mode.clone()).unwrap_or_default();
    eprintln!("space-stub-agent: prompt {text:?} (mode {mode})");
    if let Some(turn) = state.script.turn(&text) {
        let steps = turn.steps.clone();
        return run_script(cx, state, sid, steps, responder).await;
    }
    let text = text.to_lowercase();
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));

    cx.send_notification(state.status(&sid.0))?;
    send(SessionUpdate::AgentThoughtChunk(chunk("Thinking about ")))?;
    send(SessionUpdate::AgentThoughtChunk(chunk("the request.")))?;

    if text.contains("crash") {
        tokio::time::sleep(Duration::from_millis(50)).await;
        eprintln!("space-stub-agent: crashing on purpose");
        std::process::exit(3);
    }

    let tool_id = "tool-1";
    send(SessionUpdate::ToolCall(
        ToolCall::new(tool_id, "bash: echo stub")
            .kind(ToolKind::Execute)
            .status(ToolCallStatus::Pending)
            .raw_input(serde_json::json!({"command": "echo stub"})),
    ))?;

    let mut allowed = true;
    if text.contains("permission") {
        let options = vec![
            PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        ];
        let resp = cx
            .send_request(RequestPermissionRequest::new(
                sid.clone(),
                ToolCallUpdate::new(tool_id, ToolCallUpdateFields::new().title("bash: echo stub")),
                options,
            ))
            .block_task()
            .await?;
        allowed = matches!(&resp.outcome, RequestPermissionOutcome::Selected(s) if &*s.option_id.0 == "allow");
    }

    if allowed {
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )))?;
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new("stub")))]),
        )))?;
    } else {
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Failed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new("rejected by user")))]),
        )))?;
    }

    let words: Vec<String> = if text.contains("slow") {
        (1..=50).map(|i| format!("{i} ")).collect()
    } else {
        ["Hello ", "from ", "the ", "stub ", "agent."].map(String::from).to_vec()
    };
    for w in words {
        if state.cancelled.lock().unwrap().contains(&*sid.0) {
            return responder.respond(PromptResponse::new(StopReason::Cancelled));
        }
        send(SessionUpdate::AgentMessageChunk(chunk(&w)))?;
        let delay = if text.contains("slow") { 100 } else { 10 };
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if let Some(s) = state.sessions.lock().unwrap().get_mut(&*sid.0) {
        s.context_tokens += 1800 + text.len() as u64 * 4;
    }
    cx.send_notification(state.status(&sid.0))?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// Play a scripted turn (see `space_core::stub_script`).
async fn run_script(
    cx: Conn,
    state: Arc<State>,
    sid: SessionId,
    steps: Vec<Step>,
    responder: Responder<PromptResponse, acp::V1>,
) -> acp::Result<()> {
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));
    let cancelled = || state.cancelled.lock().unwrap().contains(&*sid.0);
    let mut tools: HashSet<String> = HashSet::new();
    cx.send_notification(state.status(&sid.0))?;
    for step in steps {
        if cancelled() {
            return responder.respond(PromptResponse::new(StopReason::Cancelled));
        }
        match step {
            Step::Thought { text } | Step::Text { text } if text.is_empty() => {}
            Step::Thought { text } => {
                for w in text.split_inclusive(' ') {
                    send(SessionUpdate::AgentThoughtChunk(chunk(w)))?;
                    tokio::time::sleep(Duration::from_millis(8)).await;
                }
            }
            Step::Text { text } => {
                for w in text.split_inclusive(' ') {
                    send(SessionUpdate::AgentMessageChunk(chunk(w)))?;
                    tokio::time::sleep(Duration::from_millis(8)).await;
                }
            }
            Step::Tool { id, title, kind, status, output } => {
                let status: ToolCallStatus =
                    serde_json::from_value(serde_json::json!(status)).unwrap_or(ToolCallStatus::Pending);
                let content: Vec<ToolCallContent> = output
                    .map(|o| vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(o)))])
                    .unwrap_or_default();
                if tools.insert(id.clone()) {
                    let kind: ToolKind = serde_json::from_value(serde_json::json!(kind)).unwrap_or(ToolKind::Other);
                    send(SessionUpdate::ToolCall(ToolCall::new(id, title).kind(kind).status(status).content(content)))?;
                } else {
                    let mut fields = ToolCallUpdateFields::new().status(status).title(title);
                    if !content.is_empty() {
                        fields = fields.content(content);
                    }
                    send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id, fields)))?;
                }
            }
            Step::Permission { tool_id, title } => {
                let options = vec![
                    PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
                    PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
                ];
                cx.send_request(RequestPermissionRequest::new(
                    sid.clone(),
                    ToolCallUpdate::new(tool_id, ToolCallUpdateFields::new().title(title)),
                    options,
                ))
                .block_task()
                .await?;
            }
            Step::Context { tokens } => {
                if let Some(s) = state.sessions.lock().unwrap().get_mut(&*sid.0) {
                    s.context_tokens = tokens;
                }
                cx.send_notification(state.status(&sid.0))?;
            }
            Step::Sleep { ms } => tokio::time::sleep(Duration::from_millis(ms)).await,
            Step::Hold => loop {
                if cancelled() {
                    return responder.respond(PromptResponse::new(StopReason::Cancelled));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
            Step::Fail { message } => return responder.respond_with_error(acp::Error::new(-32603, message)),
        }
    }
    cx.send_notification(state.status(&sid.0))?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}
