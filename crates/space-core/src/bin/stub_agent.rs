//! `space-stub-agent`: a tiny scripted ACP agent for tests and offline development.
//!
//! Every prompt streams: a `_space/status` update, a thought chunk, a tool call with a
//! status update, then message chunks, and ends the turn with `end_turn`.
//! Prompt keywords change the script:
//! * `permission` — ask `session/request_permission` before running the tool.
//! * `slow` — stream 50 chunks 100 ms apart (use with cancel).
//! * `crash` — exit with code 3 mid-turn.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use space_protocol::SpaceStatus;
use space_protocol::acp::{self, Agent, ByteStreams, Client, Responder};
use space_protocol::acp_schema::*;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Conn = acp::v1::ConnectionTo<Client>;

#[derive(Default)]
struct State {
    next_session: AtomicU64,
    cancelled: Mutex<HashSet<String>>,
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
                    responder.respond(NewSessionResponse::new(id.clone()))?;
                    cx.send_notification(SpaceStatus {
                        session_id: id,
                        provider: Some("stub".into()),
                        model: Some("stub-model".into()),
                        thinking_level: Some("off".into()),
                    })
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
    let send = |update: SessionUpdate| cx.send_notification(SessionNotification::new(sid.clone(), update));
    let chunk = |s: &str| ContentChunk::new(ContentBlock::Text(TextContent::new(s)));

    cx.send_notification(SpaceStatus {
        session_id: sid.0.to_string(),
        thinking_level: Some("low".into()),
        ..Default::default()
    })?;
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
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}
