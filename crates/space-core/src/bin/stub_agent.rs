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
//! `SPACE_STUB_NO_IMAGES=1` makes it say it takes no images. A finished turn reports fixed usage
//! (`_meta["space/usage"]`).
//!
//! Every turn also sends a trace (`_space/trace`), like a real agent: the header before the
//! session's first request, each request with its new messages, first token, chunk timing,
//! responses with usage, tool calls, and the turn's end. The built-in turn makes two requests
//! (before and after its tool); a scripted turn makes one.
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
//! Steering (`_space/session/steer`, advertised in `initialize`): a message sent during a turn is
//! delivered at the next step: after the built-in turn's tool, every ten words of a `slow` reply,
//! while a scripted turn holds, or else at the end of the turn. Delivery sends `_space/steered`
//! and a reply chunk "Steered: <text>. ". A steer when no turn runs is refused.
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
use space_protocol::ext::{CONTEXT_META_KEY, DATA_DIR_META_KEY, PROTOCOL_VERSION_META_KEY, STEER_META_KEY, USAGE_META_KEY};
use space_protocol::{
    Complete, CompleteResult, Confine, ConfigureSession, ModelInfo, PROTOCOL_VERSION, PreExecute, SpaceStatus, SteerResult, SteerSession, Steered,
    UnsteerResult, UnsteerSession, Verdict,
};
use space_protocol::acp::{self, Agent, ByteStreams, Client, Responder, UntypedMessage};
use space_protocol::trace::{ChunkKind, HeaderReason, RequestOptions, TRACE_METHOD, ToolDef, TraceBatch, TraceEvent};
use space_protocol::acp_schema::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Conn = acp::v1::ConnectionTo<Client>;

#[derive(Default)]
struct State {
    next_session: AtomicU64,
    cancelled: Mutex<HashSet<String>>,
    sessions: Mutex<HashMap<String, StubSession>>,
    script: StubScript,
    /// Sessions with a turn running.
    running: Mutex<HashSet<String>>,
    /// Steered messages not delivered yet, by session: (id, text).
    steers: Mutex<HashMap<String, Vec<(String, String)>>>,
}

/// Marks a session's turn as running while it lives.
struct Running(Arc<State>, String);

impl Running {
    fn start(state: &Arc<State>, sid: &SessionId) -> Self {
        state.running.lock().unwrap().insert(sid.0.to_string());
        Self(state.clone(), sid.0.to_string())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.lock().unwrap().remove(&self.1);
        self.0.steers.lock().unwrap().remove(&self.1);
    }
}

/// Deliver the session's steered messages: `_space/steered` for each, then a reply chunk
/// naming them. Returns the chunk's text (empty when there were none).
fn deliver_steers(state: &State, cx: &Conn, sid: &SessionId) -> acp::Result<String> {
    let steers = state.steers.lock().unwrap().remove(&*sid.0).unwrap_or_default();
    let mut said = String::new();
    for (id, text) in steers {
        eprintln!("space-stub-agent: steered {id}");
        cx.send_notification(Steered { session_id: sid.0.to_string(), id })?;
        said.push_str(&format!("Steered: {text}. "));
    }
    if !said.is_empty() {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(said.clone())));
        cx.send_notification(SessionNotification::new(sid.clone(), SessionUpdate::AgentMessageChunk(chunk)))?;
    }
    Ok(said)
}

#[derive(Clone)]
struct StubSession {
    model: String,
    thinking: String,
    context_tokens: u64,
    /// Where the session's prompts are kept (in its data folder).
    history: Option<std::path::PathBuf>,
    /// Trace numbering: turns and requests so far, and whether the header went out.
    turns: u32,
    requests: u32,
    header_sent: bool,
}

impl StubSession {
    fn new(meta: Option<&Meta>) -> Self {
        let dir = meta.and_then(|m| m.get(DATA_DIR_META_KEY)).and_then(|d| d.as_str()).map(std::path::PathBuf::from);
        Self {
            model: MODELS[0].0.into(),
            thinking: "low".into(),
            context_tokens: 0,
            history: dir.map(|d| d.join("history.txt")),
            turns: 0,
            requests: 0,
            header_sent: false,
        }
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
                meta.insert(STEER_META_KEY.into(), true.into());
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
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: SteerSession, responder: Responder<SteerResult, acp::V1>, _cx: Conn| {
                    eprintln!("space-stub-agent: steer {} {:?}", req.id, req.text);
                    if !state.running.lock().unwrap().contains(&req.session_id) {
                        return responder.respond_with_error(acp::Error::new(-32600, "No turn is running"));
                    }
                    state.steers.lock().unwrap().entry(req.session_id).or_default().push((req.id, req.text));
                    responder.respond(SteerResult {})
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: UnsteerSession, responder: Responder<UnsteerResult, acp::V1>, _cx: Conn| {
                    let ids = state.steers.lock().unwrap().remove(&req.session_id).unwrap_or_default().into_iter().map(|(id, _)| id).collect();
                    responder.respond(UnsteerResult { ids })
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
    let _running = Running::start(&state, &sid);
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
        return run_script(cx, state, sid, &text, steps, responder).await;
    }
    let mut trace = Tracer::start(&state, &sid);
    trace.request(vec![serde_json::json!({"role": "user", "content": [{"type": "text", "text": text}]})]);
    trace.send(&cx)?;
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
    trace.chunk(ChunkKind::Thinking);
    send(SessionUpdate::AgentThoughtChunk(chunk("the request.")))?;
    trace.chunk(ChunkKind::Thinking);

    if text.contains("crash") {
        tokio::time::sleep(Duration::from_millis(50)).await;
        eprintln!("space-stub-agent: crashing on purpose");
        std::process::exit(3);
    }

    let tool_id = "tool-1";
    trace.respond("toolUse", "", "Thinking about the request.", Some(FIRST_USAGE));
    trace.tool_start(tool_id, "bash", serde_json::json!({"command": "echo stub"}));
    trace.send(&cx)?;
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
        trace.tool_end(tool_id, &format!("blocked: {reason}"), true);
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
        trace.tool_end(tool_id, &output, false);
        send(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            tool_id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .content(vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(output)))]),
        )))?;
    } else if !argv.is_empty() {
        trace.tool_end(tool_id, "rejected by user", true);
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
    trace.request(vec![
        serde_json::json!({"role": "assistant", "content": [{"type": "tool_use", "id": tool_id, "name": "bash", "input": {"command": "echo stub"}}]}),
        serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_id, "content": {"toolResultRef": tool_id}}]}),
    ]);
    trace.send(&cx)?;
    // A step ended (the tool): steered messages go in now.
    deliver_steers(&state, &cx, &sid)?;
    let words: Vec<String> = if text.contains("recall") {
        vec![format!("I remember {earlier} earlier prompt(s).")]
    } else if text.contains("slow") {
        (1..=50).map(|i| format!("{i} ")).collect()
    } else {
        ["Hello ", "from ", "the ", "stub ", "agent."].map(String::from).to_vec()
    };
    let reply: String = words.concat();
    for (i, w) in words.into_iter().enumerate() {
        if text.contains("slow") && i > 0 && i % 10 == 0 {
            deliver_steers(&state, &cx, &sid)?;
        }
        if state.cancelled.lock().unwrap().contains(&*sid.0) {
            trace.cancelled(&cx)?;
            return responder.respond(PromptResponse::new(StopReason::Cancelled));
        }
        send(SessionUpdate::AgentMessageChunk(chunk(&w)))?;
        trace.chunk(ChunkKind::Text);
        let delay = if text.contains("slow") { 100 } else { 10 };
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if let Some(s) = state.sessions.lock().unwrap().get_mut(&*sid.0) {
        s.context_tokens += 1800 + text.len() as u64 * 4;
    }
    // Steered messages not yet delivered: a last step.
    deliver_steers(&state, &cx, &sid)?;
    cx.send_notification(state.status(&sid.0))?;
    trace.respond("stop", &reply, "", Some(SECOND_USAGE));
    trace.end_turn("end_turn");
    trace.send(&cx)?;
    responder.respond(PromptResponse::new(StopReason::EndTurn).meta(usage_meta()))
}

/// Play a scripted turn (see `space_core::stub_script`).
async fn run_script(
    cx: Conn,
    state: Arc<State>,
    sid: SessionId,
    prompt: &str,
    steps: Vec<Step>,
    responder: Responder<PromptResponse, acp::V1>,
) -> acp::Result<()> {
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));
    let cancelled = || state.cancelled.lock().unwrap().contains(&*sid.0);
    let _running = Running::start(&state, &sid);
    let mut tools: HashSet<String> = HashSet::new();
    let mut trace = Tracer::start(&state, &sid);
    trace.request(vec![serde_json::json!({"role": "user", "content": [{"type": "text", "text": prompt}]})]);
    trace.send(&cx)?;
    let (mut reply, mut thought) = (String::new(), String::new());
    cx.send_notification(state.status(&sid.0))?;
    for step in steps {
        if cancelled() {
            trace.cancelled(&cx)?;
            return responder.respond(PromptResponse::new(StopReason::Cancelled));
        }
        match step {
            Step::Thought { text } | Step::Text { text } if text.is_empty() => {}
            Step::Thought { text } => {
                for w in text.split_inclusive(' ') {
                    send(SessionUpdate::AgentThoughtChunk(chunk(w)))?;
                    trace.chunk(ChunkKind::Thinking);
                    tokio::time::sleep(Duration::from_millis(8)).await;
                }
                thought.push_str(&text);
            }
            Step::Text { text } => {
                for w in text.split_inclusive(' ') {
                    send(SessionUpdate::AgentMessageChunk(chunk(w)))?;
                    trace.chunk(ChunkKind::Text);
                    tokio::time::sleep(Duration::from_millis(8)).await;
                }
                reply.push_str(&text);
            }
            Step::Tool { id, title, kind, status, output } => {
                let status: ToolCallStatus =
                    serde_json::from_value(serde_json::json!(status)).unwrap_or(ToolCallStatus::Pending);
                let content: Vec<ToolCallContent> = output
                    .clone()
                    .map(|o| vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(o)))])
                    .unwrap_or_default();
                if !tools.contains(&id) {
                    // "bash: cargo test" → tool `bash` with that command (or path, for file tools).
                    let (name, detail) = title.split_once(": ").unwrap_or((title.as_str(), ""));
                    let field = if kind == "execute" { "command" } else { "path" };
                    trace.tool_start(&id, &name.to_lowercase(), serde_json::json!({ field: detail }));
                }
                if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
                    trace.tool_end(&id, output.as_deref().unwrap_or_default(), status == ToolCallStatus::Failed);
                }
                trace.send(&cx)?;
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
                trace.tool_start(&id, &tool, args.clone());
                trace.send(&cx)?;
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
                trace.tool_end(&id, &text, status == ToolCallStatus::Failed);
                trace.send(&cx)?;
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
                    trace.cancelled(&cx)?;
                    return responder.respond(PromptResponse::new(StopReason::Cancelled));
                }
                // A held turn takes steered messages (and keeps holding).
                reply.push_str(&deliver_steers(&state, &cx, &sid)?);
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
            Step::Fail { message } => {
                trace.respond("error", "", "", None);
                trace.set_error(&message);
                trace.end_turn("error");
                trace.send(&cx)?;
                return responder.respond_with_error(acp::Error::new(-32603, message));
            }
        }
    }
    reply.push_str(&deliver_steers(&state, &cx, &sid)?);
    cx.send_notification(state.status(&sid.0))?;
    let usage = turn_usage();
    trace.respond("stop", &reply, &thought, Some((usage.input, usage.output, 8112, 88)));
    trace.end_turn("end_turn");
    trace.send(&cx)?;
    responder.respond(PromptResponse::new(StopReason::EndTurn).meta(usage_meta()))
}

/// The usage every finished turn reports.
fn turn_usage() -> space_protocol::TurnUsage {
    space_protocol::TurnUsage {
        input: 1204,
        output: 97,
        cache_read: Some(8112),
        cache_write: Some(88),
        reasoning: None,
        models: vec!["stub/stub-model".into()],
    }
}

fn usage_meta() -> Meta {
    let mut meta = Meta::new();
    meta.insert(USAGE_META_KEY.into(), serde_json::to_value(turn_usage()).unwrap_or_default());
    meta
}

/// Tokens (input, output, cache read, cache write) of the built-in turn's two requests: together,
/// [`turn_usage`].
const FIRST_USAGE: (u64, u64, u64, u64) = (600, 40, 4000, 88);
const SECOND_USAGE: (u64, u64, u64, u64) = (604, 57, 4112, 0);

/// Now, in Unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// One turn's trace. Events queue; [`Tracer::send`] sends them as one `_space/trace` batch.
struct Tracer {
    sid: String,
    model: String,
    turn: u32,
    step: u32,
    /// The request being answered, when it went out, and whether its first token came.
    request: u32,
    request_at: u64,
    first_token: bool,
    /// The current chunk run: its stream, first and last arrival, and the gaps.
    run: Option<(ChunkKind, u64, u64, Vec<u32>)>,
    started: HashMap<String, u64>,
    events: Vec<TraceEvent>,
    state: Arc<State>,
}

impl Tracer {
    /// A new turn in the session, with the header before the session's first request.
    fn start(state: &Arc<State>, sid: &SessionId) -> Self {
        let mut sessions = state.sessions.lock().unwrap();
        let session = sessions.entry(sid.0.to_string()).or_insert_with(|| StubSession::new(None));
        session.turns += 1;
        let mut events = vec![TraceEvent::TurnStart { turn: session.turns, at: now_ms() }];
        if !session.header_sent {
            session.header_sent = true;
            events.push(TraceEvent::Header {
                header_id: 1,
                reason: HeaderReason::Initial,
                system_prompt: "You are the stub agent. You run `echo stub` and say hello.".into(),
                tools: vec![ToolDef {
                    name: "bash".into(),
                    description: "Run a shell command.".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
                }],
                options: RequestOptions {
                    provider: "stub".into(),
                    model: session.model.clone(),
                    max_tokens: Some(8192),
                    effort: Some(session.thinking.clone()),
                    ..RequestOptions::default()
                },
            });
        }
        Self {
            sid: sid.0.to_string(),
            model: session.model.clone(),
            turn: session.turns,
            step: 0,
            request: 0,
            request_at: 0,
            first_token: false,
            run: None,
            started: HashMap::new(),
            events,
            state: state.clone(),
        }
    }

    fn request(&mut self, messages: Vec<serde_json::Value>) {
        self.end_run();
        let number = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let session = sessions.entry(self.sid.clone()).or_insert_with(|| StubSession::new(None));
            session.requests += 1;
            session.requests
        };
        self.step += 1;
        self.request = number;
        self.request_at = now_ms();
        self.first_token = false;
        self.events.push(TraceEvent::Request {
            request: number,
            turn: self.turn,
            step: self.step,
            at: self.request_at,
            header_id: 1,
            messages,
            rewritten: false,
        });
    }

    /// A streamed chunk arrived.
    fn chunk(&mut self, stream: ChunkKind) {
        let at = now_ms();
        if !self.first_token {
            self.first_token = true;
            self.events.push(TraceEvent::FirstToken { request: self.request, at });
        }
        match &mut self.run {
            Some((kind, _, last, dt)) if *kind == stream => {
                dt.push(at.saturating_sub(*last) as u32);
                *last = at;
            }
            _ => {
                self.end_run();
                self.run = Some((stream, at, at, Vec::new()));
            }
        }
    }

    fn end_run(&mut self) {
        if let Some((stream, t0, _, dt)) = self.run.take() {
            self.events.push(TraceEvent::Chunks { request: self.request, stream, t0, dt });
        }
    }

    /// The request's answer, with its tokens (input, output, cache read, cache write).
    fn respond(&mut self, stop: &str, text: &str, thinking: &str, tokens: Option<(u64, u64, u64, u64)>) {
        self.end_run();
        let at = now_ms();
        let model = format!("stub/{}", self.model);
        let usage = tokens.map(|(input, output, read, write)| space_protocol::TurnUsage {
            input,
            output,
            cache_read: Some(read),
            cache_write: Some(write),
            reasoning: None,
            models: vec![model.clone()],
        });
        self.events.push(TraceEvent::Response {
            request: self.request,
            at,
            duration_ms: at.saturating_sub(self.request_at),
            usage,
            stop_reason: stop.into(),
            response_id: Some(format!("stub-{}-{}", self.sid, self.request)),
            model: Some(model),
            text: text.into(),
            thinking: thinking.into(),
            error: None,
        });
    }

    /// The last response failed with `message`.
    fn set_error(&mut self, message: &str) {
        if let Some(TraceEvent::Response { error, .. }) = self.events.last_mut() {
            *error = Some(message.into());
        }
    }

    fn tool_start(&mut self, call_id: &str, name: &str, input: serde_json::Value) {
        self.end_run();
        let at = now_ms();
        self.started.insert(call_id.into(), at);
        self.events.push(TraceEvent::ToolStart { request: self.request, call_id: call_id.into(), name: name.into(), input, at });
    }

    fn tool_end(&mut self, call_id: &str, output: &str, is_error: bool) {
        let at = now_ms();
        let started = self.started.remove(call_id).unwrap_or(at);
        self.events.push(TraceEvent::ToolEnd {
            request: self.request,
            call_id: call_id.into(),
            at,
            duration_ms: at.saturating_sub(started),
            output: output.into(),
            is_error,
            truncated: false,
        });
    }

    fn end_turn(&mut self, reason: &str) {
        self.end_run();
        self.events.push(TraceEvent::TurnEnd { turn: self.turn, at: now_ms(), reason: reason.into() });
    }

    /// The turn was cancelled: the answer stops and the turn ends; sent at once.
    fn cancelled(&mut self, cx: &Conn) -> acp::Result<()> {
        self.respond("aborted", "", "", None);
        self.end_turn("cancelled");
        self.send(cx)
    }

    /// Send what's queued as one `_space/trace` batch.
    fn send(&mut self, cx: &Conn) -> acp::Result<()> {
        if self.events.is_empty() {
            return Ok(());
        }
        let batch = TraceBatch { session_id: self.sid.clone(), events: std::mem::take(&mut self.events) };
        cx.send_notification(UntypedMessage::new(TRACE_METHOD, batch)?)
    }
}

