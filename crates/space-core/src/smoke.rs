//! Headless end-to-end run: discover → start agent → new session → prompt → print events.
//! Used by `space --smoke` and the `smoke` example.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use space_protocol::PluginKind;

use crate::host::{EventReceiver, Host, HostEvent};
use crate::session::{PermissionDecision, SessionEvent, SessionState};

#[derive(Debug, Clone)]
pub struct SmokeOptions {
    /// Agent plugin id; defaults to the first discovered agent.
    pub plugin_id: Option<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    /// Answer permission requests with the first allow option (else reject/cancel).
    pub allow_permissions: bool,
    /// Switch the session to this mode (`session/set_mode`, e.g. `read`) before prompting.
    pub mode: Option<String>,
    pub timeout: Duration,
}

/// Run one prompt and return the final folded session state.
pub async fn run(host: &Host, mut events: EventReceiver, opts: SmokeOptions) -> Result<SessionState> {
    let plugins = host.plugins();
    println!("discovered {} plugin(s):", plugins.len());
    for p in &plugins {
        println!("  - {} ({}, {}) at {}", p.manifest.id, p.manifest.kind, p.manifest.name, p.dir.display());
    }
    let plugin_id = match opts.plugin_id.clone() {
        Some(id) => id,
        None => plugins
            .iter()
            .find(|p| p.manifest.kind == PluginKind::Agent)
            .map(|p| p.manifest.id.clone())
            .context("no agent plugin discovered")?,
    };

    println!("starting {plugin_id} …");
    let agent = host.start_agent(&plugin_id).await?;
    println!("initialized (agent: {})", agent.agent_name().unwrap_or("?"));
    let session = agent.new_session(&opts.cwd).await?;
    println!("session {} in {}", session.id(), opts.cwd.display());
    if let Some(modes) = session.modes() {
        let ids: Vec<_> = modes.available.iter().map(|m| m.id.as_str()).collect();
        println!("modes: {} (current {})", ids.join(", "), modes.current);
    }
    if let Some(mode) = &opts.mode {
        session.set_mode(mode.clone()).await?;
        println!("mode set to {mode}");
    }
    println!("> {}", opts.prompt);

    let mut state = SessionState::new(session.id());
    let prompt = {
        let session = session.clone();
        let text = opts.prompt.clone();
        tokio::spawn(async move { session.prompt(text).await })
    };

    let deadline = tokio::time::sleep(opts.timeout);
    tokio::pin!(deadline);
    let mut in_text = false;
    loop {
        let event = tokio::select! {
            e = events.recv() => match e { Some(e) => e, None => break },
            _ = &mut deadline => {
                println!("\n[timeout after {:?}, cancelling]", opts.timeout);
                session.cancel()?;
                deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                continue;
            }
        };
        let event = match event {
            HostEvent::Session { session_id, event } if session_id == session.id() => event,
            HostEvent::PluginStatus { plugin_id, status } => {
                println!("[plugin {plugin_id}: {status:?}]");
                continue;
            }
            HostEvent::PluginLog { line, .. } => {
                if std::env::var_os("SPACE_SMOKE_LOGS").is_some() {
                    println!("[stderr] {line}");
                }
                continue;
            }
            _ => continue,
        };
        state.apply(&event);
        let is_text = matches!(event, SessionEvent::AgentText { .. });
        if in_text && !is_text {
            println!();
        }
        in_text = is_text;
        match &event {
            SessionEvent::AgentText { text } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            SessionEvent::AgentThought { text } => println!("[thought] {}", text.trim()),
            SessionEvent::ToolCall(t) => println!("[tool {} {} ({})] {}", t.id, t.kind, t.status.as_str(), t.title),
            SessionEvent::ToolCallUpdate(p) => println!(
                "[tool {} update] status={} output={:?}",
                p.id,
                p.status.map_or("-", |s| s.as_str()),
                p.output.as_deref().map(|o| o.chars().take(120).collect::<String>())
            ),
            SessionEvent::Status(s) => println!(
                "[status] provider={} model={} thinking={}",
                s.provider.as_deref().unwrap_or("-"),
                s.model.as_deref().unwrap_or("-"),
                s.thinking_level.as_deref().unwrap_or("-")
            ),
            SessionEvent::PermissionRequested(req) => {
                let pick = req
                    .options
                    .iter()
                    .find(|o| o.kind.is_allow() == opts.allow_permissions)
                    .map(|o| PermissionDecision::Selected(o.id.clone()))
                    .unwrap_or(PermissionDecision::Cancelled);
                println!("[permission #{} for {:?}] answering {pick:?}", req.request_id, req.tool_call.title);
                session.resolve_permission(req.request_id, pick)?;
            }
            SessionEvent::Plan(p) => println!("[plan] {} entries", p.len()),
            SessionEvent::Error { message } => println!("[error] {message}"),
            SessionEvent::TurnEnded { stop_reason } => {
                println!("[turn ended: {stop_reason}]");
                break;
            }
            _ => {}
        }
    }
    let _ = prompt.await;
    agent.stop();
    Ok(state)
}
