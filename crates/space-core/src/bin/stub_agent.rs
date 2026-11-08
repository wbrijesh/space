//! `space-stub-agent`: a tiny scripted ACP agent for tests and offline development.
//!
//! Every prompt streams: a `_space/status` update, a thought chunk, a tool call with a
//! status update, then message chunks, and ends the turn with `end_turn`.
//! Prompt keywords change the script:
//! * `permission` — ask `session/request_permission` before running the tool.
//! * `slow` — stream 50 chunks 100 ms apart (use with cancel).
//! * `crash` — exit with code 3 mid-turn.
//! * `recall` — answer how many earlier prompts of the session it remembers.
//!
//! Images in a prompt are answered first: "Saw 2 images (image/png 132 bytes <uri>, …). ".
//! `SPACE_STUB_NO_IMAGES=1` makes it say it takes no images.
//!
//! `_space/complete` answers `"Title by <model>."` (quoted, as models sometimes do), with the
//! requested model or else the session's; a prompt with `titlefail` fails and one with
//! `titlehang` never answers.
//!
//! Given a data folder (`_meta["space/dataDir"]`), a session keeps its prompts there and can be
//! loaded again (`session/load`, which first replays a chunk, as agents replay history).
//!
//! With `SPACE_STUB_SCRIPT=<file>` (see `space_core::stub_script`), a prompt matching a
//! scripted turn plays that turn's steps instead, which can hold a turn open indefinitely
//! (thinking, running a tool, waiting for permission). The app's dev scenarios use this.
//!
//! Sessions report two fake models (`stub-model`, `stub-fast`) and thinking levels through
//! `_space/status`, and accept `_space/session/configure`.
//!
//! Like a real agent, the built-in turn asks the host's tool pipeline before its tool runs
//! (`_space/tools/preExecute`; a denial fails the tool with the reason) and has the command
//! confined (`_space/sandbox/confine`; the argv it would spawn ends the tool's output). Hidden
//! context sections from the prompt's `_meta` are logged.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use space_core::stub_script::{SCRIPT_ENV, Step, StubScript};
use space_protocol::ext::{CONTEXT_META_KEY, DATA_DIR_META_KEY, PROTOCOL_VERSION_META_KEY};
use space_protocol::{Complete, CompleteResult, Confine, ConfigureSession, ModelInfo, PROTOCOL_VERSION, PreExecute, SpaceStatus, Verdict};
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
    /// Where the session's prompts are kept (in its data folder).
    history: Option<std::path::PathBuf>,
}

impl StubSession {
    fn new(meta: Option<&Meta>) -> Self {
        let dir = meta.and_then(|m| m.get(DATA_DIR_META_KEY)).and_then(|d| d.as_str()).map(std::path::PathBuf::from);
        Self { model: MODELS[0].0.into(), thinking: "low".into(), context_tokens: 0, history: dir.map(|d| d.join("history.txt")) }
    }
}

/// Overrides the Space protocol version the stub reports.
const PROTOCOL_ENV: &str = "SPACE_STUB_PROTOCOL";
/// Set: the stub says it takes no images.
const NO_IMAGES_ENV: &str = "SPACE_STUB_NO_IMAGES";
const MODELS: [(&str, &str); 2] = [("stub-model", "Stub Model"), ("stub-fast", "Stub Fast")];
const LEVELS: [&str; 4] = ["low", "medium", "high", "max"];
impl State {
    fn status(&self, session_id: &str) -> SpaceStatus {
        let s = self.sessions.lock().unwrap().get(session_id).cloned().unwrap_or_else(|| StubSession::new(None));
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
                // The Space protocol it speaks; tests set another to see it refused.
                let version = std::env::var(PROTOCOL_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(PROTOCOL_VERSION);
                let mut meta = Meta::new();
                meta.insert(PROTOCOL_VERSION_META_KEY.into(), version.into());
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(
                            AgentCapabilities::new()
                                .load_session(true)
                                .prompt_capabilities(PromptCapabilities::new().image(std::env::var_os(NO_IMAGES_ENV).is_none())),
                        )
                        .agent_info(Implementation::new("space-stub-agent", env!("CARGO_PKG_VERSION")))
                        .meta(meta),
                )
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: NewSessionRequest, responder, cx: Conn| {
                    // Unique across runs, so a loaded session can't clash with a new one.
                    let run = std::process::id();
                    let id = format!("stub-{run}-{}", state.next_session.fetch_add(1, Ordering::Relaxed) + 1);
                    state.sessions.lock().unwrap().insert(id.clone(), StubSession::new(req.meta.as_ref()));
                    responder.respond(NewSessionResponse::new(id.clone()))?;
                    cx.send_notification(state.status(&id))
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: LoadSessionRequest, responder, cx: Conn| {
                    let id = req.session_id.clone();
                    eprintln!("space-stub-agent: load {}", id.0);
                    state.sessions.lock().unwrap().insert(id.0.to_string(), StubSession::new(req.meta.as_ref()));
                    // Agents replay the conversation while loading.
                    let replay = ContentChunk::new(ContentBlock::Text(TextContent::new("(replayed history)")));
                    cx.send_notification(SessionNotification::new(id.clone(), SessionUpdate::AgentMessageChunk(replay)))?;
                    responder.respond(LoadSessionResponse::new())?;
                    cx.send_notification(state.status(&id.0))
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
                async move |req: Complete, responder: Responder<CompleteResult, acp::V1>, cx: Conn| {
                    eprintln!("space-stub-agent: complete {req:?}");
                    let session_model = state.sessions.lock().unwrap().get(&req.session_id).map(|s| s.model.clone());
                    let Some(model) = req.model.or(session_model) else {
                        return responder.respond_with_error(acp::Error::invalid_params());
                    };
                    if req.prompt.contains("titlefail") {
                        return responder.respond_with_error(acp::Error::new(-32603, "no title today"));
                    }
                    let hang = req.prompt.contains("titlehang");
                    cx.spawn(async move {
                        if hang {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                        }
                        responder.respond(CompleteResult { text: format!("\"Title by {model}.\"\n") })
                    })
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
    eprintln!("space-stub-agent: prompt {text:?}");
    if let Some(context) = req.meta.as_ref().and_then(|m| m.get(CONTEXT_META_KEY)) {
        eprintln!("space-stub-agent: context {context}");
    }
    if let Some(turn) = state.script.turn(&text) {
        let steps = turn.steps.clone();
        return run_script(cx, state, sid, steps, responder).await;
    }
    let text = text.to_lowercase();
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));

    cx.send_notification(state.status(&sid.0))?;
    let images: Vec<String> = req
        .prompt
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Image(i) => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD.decode(&i.data).map_or(0, |b| b.len());
                Some(format!("{} {bytes} bytes {}", i.mime_type, i.uri.as_deref().unwrap_or("-")))
            }
            _ => None,
        })
        .collect();
    if !images.is_empty() {
        let s = if images.len() == 1 { "" } else { "s" };
        send(SessionUpdate::AgentMessageChunk(chunk(&format!("Saw {} image{s} ({}). ", images.len(), images.join(", ")))))?;
    }
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

    // The host's tool pipeline decides first, then the command is confined.
    let guard = cx
        .send_request(PreExecute {
            session_id: sid.0.to_string(),
            tool_call_id: tool_id.into(),
            tool: "bash".into(),
            kind: "execute".into(),
            args: serde_json::json!({"command": "echo stub"}),
            cwd: None,
        })
        .block_task()
        .await;
    let blocked = match guard {
        Ok(r) if r.decision == Verdict::Allow => None,
        Ok(r) => Some(r.reason.unwrap_or_else(|| "denied".into())),
        Err(e) => Some(format!("the tool pipeline failed: {e}")),
    };
    let confined = match &blocked {
        Some(_) => None,
        None => {
            let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "echo stub".to_string()];
            let confine = Confine { session_id: sid.0.to_string(), argv, cwd: "/".into() };
            Some(cx.send_request(confine).block_task().await.map(|r| r.argv.join(" ")))
        }
    };
    let blocked = blocked.or_else(|| match &confined {
        Some(Err(e)) => Some(format!("couldn't confine the command: {e}")),
        _ => None,
    });
    if let Some(reason) = blocked {
        eprintln!("space-stub-agent: tool blocked: {reason}");
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Failed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(format!("blocked: {reason}"))))]),
        )))?;
    }
    let argv = match confined {
        Some(Ok(argv)) => argv,
        _ => String::new(),
    };

    let mut allowed = !argv.is_empty();
    if allowed && text.contains("permission") {
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
        let output = format!("stub\nargv: {argv}");
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(output)))]),
        )))?;
    } else if !argv.is_empty() {
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Failed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new("rejected by user")))]),
        )))?;
    }

    // Earlier prompts of this session (kept in its data folder), then this one.
    let history = state.sessions.lock().unwrap().get(&*sid.0).and_then(|s| s.history.clone());
    let earlier = history.as_ref().and_then(|h| std::fs::read_to_string(h).ok()).map_or(0, |t| t.lines().count());
    if let Some(h) = &history {
        let line = format!("{}\n", text.replace('\n', " "));
        let kept = std::fs::create_dir_all(h.parent().unwrap())
            .and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(h))
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = kept {
            eprintln!("space-stub-agent: can't keep the session in {}: {e}", h.display());
        }
    }
    let words: Vec<String> = if text.contains("recall") {
        vec![format!("I remember {earlier} earlier prompt(s).")]
    } else if text.contains("slow") {
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
            Step::Guard { id, title, kind, tool, args, output } => {
                let kind_value: ToolKind = serde_json::from_value(serde_json::json!(kind)).unwrap_or(ToolKind::Other);
                send(SessionUpdate::ToolCall(ToolCall::new(id.clone(), title).kind(kind_value).status(ToolCallStatus::Pending)))?;
                let guard = cx
                    .send_request(PreExecute {
                        session_id: sid.0.to_string(),
                        tool_call_id: id.clone(),
                        tool,
                        kind,
                        args,
                        cwd: None,
                    })
                    .block_task()
                    .await;
                let (status, text) = match guard {
                    Ok(r) if r.decision == Verdict::Allow => (ToolCallStatus::Completed, output.unwrap_or_default()),
                    Ok(r) => (ToolCallStatus::Failed, r.reason.unwrap_or_else(|| "denied".into())),
                    Err(e) => (ToolCallStatus::Failed, format!("the tool pipeline failed: {e}")),
                };
                let content = vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(text)))];
                send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id, ToolCallUpdateFields::new().status(status).content(content))))?;
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
