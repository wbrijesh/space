//! `space-stub-agent`: a tiny scripted ACP agent for tests and offline development.
//!
//! Every prompt streams: a `_space/status` update, a thought chunk, a tool call with a
//! status update, then message chunks, and ends the turn with `end_turn`.
//! Prompt keywords change the script:
//! * `permission` — ask `session/request_permission` before running the tool.
//! * `slow` — stream 50 chunks 100 ms apart (use with cancel).
//! * `crash` — exit with code 3 mid-turn.
//! * `explain` — reply with a longer markdown answer.
//! * `wait` — keep the turn running (streaming nothing) until it is cancelled.
//! * `fail` — fail the turn with a JSON-RPC error.
//!
//! Sessions report two fake models (`stub-model`, `stub-fast`) and thinking levels through
//! `_space/status`, and accept `_space/session/configure`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
}

#[derive(Clone)]
struct StubSession {
    model: String,
    thinking: String,
    context_tokens: u64,
}

const MODELS: [(&str, &str); 2] = [("stub-model", "Stub Model"), ("stub-fast", "Stub Fast")];
const LEVELS: [&str; 4] = ["low", "medium", "high", "max"];

impl State {
    fn status(&self, session_id: &str) -> SpaceStatus {
        let s = self.sessions.lock().unwrap().get(session_id).cloned().unwrap_or(StubSession {
            model: MODELS[0].0.into(),
            thinking: "low".into(),
            context_tokens: 0,
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
    let state = Arc::new(State::default());
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
                        StubSession { model: MODELS[0].0.into(), thinking: "low".into(), context_tokens: 0 },
                    );
                    responder.respond(NewSessionResponse::new(id.clone()))?;
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
    let text = text.to_lowercase();
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));

    cx.send_notification(state.status(&sid.0))?;
    send(SessionUpdate::AgentThoughtChunk(chunk("Thinking about ")))?;
    send(SessionUpdate::AgentThoughtChunk(chunk("the request.")))?;

    if text.contains("fail") {
        tokio::time::sleep(Duration::from_millis(30)).await;
        return responder.respond_with_error(acp::Error::new(-32603, "The model request failed: overloaded (stub)"));
    }
    if text.contains("wait") {
        loop {
            if state.cancelled.lock().unwrap().contains(&*sid.0) {
                return responder.respond(PromptResponse::new(StopReason::Cancelled));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

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
    } else if text.contains("explain") {
        EXPLAIN.split_inclusive(' ').map(String::from).collect()
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

const EXPLAIN: &str = "Plugin discovery scans three roots in order: the bundled `plugins/` directory, \
`~/.space/plugins` and the project's `.space/plugins`. Each directory with a `space-plugin.toml` \
becomes a plugin; later roots win on duplicate ids.\n\n\
**What happens on start**\n\n\
- The manifest's `run.command` is resolved on `PATH` and spawned with piped stdio.\n\
- The harness performs the ACP `initialize` handshake.\n\
- stderr lines are kept in a ring buffer for the logs panel.\n\n\
```rust\nlet agent = host.start_agent(\"pi-agent\").await?;\n```\n\n\
Invalid manifests are reported, not fatal.";
