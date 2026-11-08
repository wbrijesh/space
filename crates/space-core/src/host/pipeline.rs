//! The tool pipeline: `_space/tools/preExecute` runs the `tools/pre-execute` hook chain, and
//! holds a call while a listener can't decide.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use space_protocol::service::{self as svc, HistoryItem};
use space_protocol::trace::{GuardVerdict, TraceEvent};
use space_protocol::{
    CATALOG_PERMISSION_MODES, HOOK_TOOLS_PRE_EXECUTE, PreExecute, PreExecuteResult, USE_READ_TRANSCRIPT, Verdict,
};

use super::HostInner;
use super::agents::Shared;
use crate::session::{PermissionDecision, PermissionKind, PermissionOption, SessionEvent, ToolCallPatch, TranscriptItem};

/// How long one hook listener may take before the call is held. Generous: a listener may
/// retry a slow network call within it.
const HOOK_TIMEOUT: Duration = Duration::from_secs(15);

/// Thread items kept for hook listeners.
const MAX_HISTORY: usize = 200;
/// Option ids of the approval card the pipeline shows.
const ASK_ALLOW: &str = "allow";
const ASK_REJECT: &str = "reject";

/// Per-session facts for hook listeners.
pub(super) struct SessionMeta {
    cwd: PathBuf,
    /// The agent plugin the session runs on.
    pub(super) agent: String,
    /// The permission mode chosen for the session (`None`: the catalog's default).
    pub(super) mode: Option<String>,
    /// The thread it belongs to (its traces are filed under it).
    pub(super) thread_id: Option<String>,
    history: VecDeque<HistoryItem>,
    pub(super) signals: HoldSignals,
}

/// What a held call of a session waits on.
#[derive(Clone, Default)]
pub(super) struct HoldSignals {
    /// Run the chain again now.
    pub(super) wake: Arc<tokio::sync::Notify>,
    /// Bumped when the user stops the turn: held calls give up.
    pub(super) stops: Arc<AtomicU64>,
}

impl SessionMeta {
    pub(super) fn new(cwd: PathBuf, agent: String) -> Self {
        Self { cwd, agent, mode: None, thread_id: None, history: VecDeque::new(), signals: HoldSignals::default() }
    }
}

impl HostInner {
    /// Run every held call through the chain again.
    pub(super) fn wake_held(&self) {
        for meta in self.sessions.lock().unwrap().values() {
            meta.signals.wake.notify_waiters();
        }
    }

    pub(super) fn signals(&self, session_id: &str) -> HoldSignals {
        self.sessions.lock().unwrap().get(session_id).map(|m| m.signals.clone()).unwrap_or_default()
    }

    /// The session's mode (or the catalog default), project root and history.
    pub(super) fn session_facts(&self, session_id: &str) -> (String, String, Vec<HistoryItem>) {
        let default_mode = || {
            let catalogs = self.catalogs.lock().unwrap();
            let entries = catalogs.get(CATALOG_PERMISSION_MODES).cloned().unwrap_or_default();
            entries.iter().find(|e| e.default).or(entries.first()).map(|e| e.id.clone()).unwrap_or_default()
        };
        let sessions = self.sessions.lock().unwrap();
        match sessions.get(session_id) {
            Some(meta) => (
                meta.mode.clone().unwrap_or_else(default_mode),
                meta.cwd.display().to_string(),
                meta.history.iter().cloned().collect(),
            ),
            None => (default_mode(), self.options.project_dir.display().to_string(), Vec::new()),
        }
    }

    pub(super) fn record(&self, session_id: &str, item: HistoryItem) {
        if let Some(meta) = self.sessions.lock().unwrap().get_mut(session_id) {
            if meta.history.len() == MAX_HISTORY {
                meta.history.pop_front();
            }
            meta.history.push_back(item);
        }
    }

    /// Start a session's history with what the thread already shows (a saved thread whose
    /// session was loaded again): its messages and tool calls, oldest first.
    pub(super) fn seed_history(&self, session_id: &str, items: &[TranscriptItem]) {
        let earlier: Vec<HistoryItem> = items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::User { text, .. } => Some(HistoryItem::User { text: text.clone() }),
                // The saved call keeps its title (`bash: ls`), not its arguments.
                TranscriptItem::Tool(call) => Some(HistoryItem::Tool {
                    id: call.id.clone(),
                    tool: call.title.split(':').next().unwrap_or_default().trim().to_string(),
                    kind: call.kind.clone(),
                    args: serde_json::json!({ "title": call.title }),
                    allowed: call.blocked.is_none(),
                }),
                _ => None,
            })
            .collect();
        if let Some(meta) = self.sessions.lock().unwrap().get_mut(session_id) {
            let mut history: VecDeque<_> = earlier.into_iter().chain(meta.history.drain(..)).collect();
            while history.len() > MAX_HISTORY {
                history.pop_front();
            }
            meta.history = history;
        }
    }
}

/// One pass through the `tools/pre-execute` chain.
enum Pass {
    Allow,
    /// Denied, why, and by which plugin.
    Deny(String, String),
    /// Ask the user, with the listeners' reasons (and the first one that asked).
    Ask(Vec<String>, String),
    /// A listener can't decide right now: wait.
    Hold(String),
}

/// Every listener, in order. A deny wins over a hold, a hold over asks. Listeners without
/// `read:transcript` get the history without what the user wrote.
async fn run_chain(host: &Arc<HostInner>, hook: &svc::HookCall) -> Pass {
    let (mut asks, mut hold, mut asker) = (Vec::new(), None, None);
    let stripped = without_transcript(hook);
    for plugin in host.hook_listeners(HOOK_TOOLS_PRE_EXECUTE) {
        let name = &plugin.manifest.name;
        let hook = if plugin.manifest.uses(USE_READ_TRANSCRIPT) { hook } else { &stripped };
        match host.call_service::<_, svc::HookDecision>(plugin.id(), svc::HOOK_PRE_EXECUTE, hook, HOOK_TIMEOUT).await {
            Ok(d) => match d.decision {
                svc::HookVerdict::Deny => {
                    tracing::info!("{} {}: denied by {}", hook.call.tool, hook.call.id, plugin.id());
                    return Pass::Deny(d.reason.unwrap_or_else(|| format!("Blocked by {name}.")), plugin.id().to_string());
                }
                svc::HookVerdict::Ask => {
                    asker.get_or_insert_with(|| plugin.id().to_string());
                    asks.push(d.reason.unwrap_or_else(|| format!("{name} wants you to check this call.")));
                }
                svc::HookVerdict::Unavailable => {
                    hold.get_or_insert(d.reason.unwrap_or_else(|| format!("{name} can't check tool calls right now.")));
                }
                svc::HookVerdict::Allow | svc::HookVerdict::Next => {}
            },
            Err(e) => {
                let reason = if host.keeps_crashing(plugin.id()) {
                    format!("The {name} plugin keeps crashing, so tool calls can't be checked. See the logs.")
                } else {
                    format!("The {name} plugin couldn't check this call ({e}).")
                };
                hold.get_or_insert(reason);
            }
        }
    }
    match hold {
        Some(reason) => Pass::Hold(reason),
        None if asks.is_empty() => Pass::Allow,
        None => Pass::Ask(asks, asker.unwrap_or_default()),
    }
}

/// `_space/tools/preExecute`: run the hook chain; hold the call while a listener can't decide;
/// ask the user if a listener said so.
pub(super) async fn pre_execute(shared: Arc<Shared>, req: PreExecute) -> PreExecuteResult {
    let host = shared.host.clone();
    let session_id = req.session_id.clone();
    let title = call_title(&req.tool, &req.args);
    let signals = host.signals(&session_id);
    let stops = signals.stops.load(Ordering::SeqCst);
    let stopped = || signals.stops.load(Ordering::SeqCst) != stops;
    let held = |reason: Option<String>| {
        host.emit_session(&session_id, SessionEvent::ToolCallHeld { id: req.tool_call_id.clone(), title: title.clone(), reason })
    };
    let mut held_for: Option<String> = None;
    // For the trace: when the call was first held, and the mode it was last checked under.
    let mut held_since: Option<Instant> = None;
    let mut last_mode = String::new();
    let (pass, call) = loop {
        // Listen before running the chain, so a wake-up during it isn't lost.
        let wake = signals.wake.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        // Facts are read on every pass: the mode may have changed while the call was held.
        let (mode, root, history) = host.session_facts(&session_id);
        last_mode.clone_from(&mode);
        let call = svc::ToolCallFacts {
            id: req.tool_call_id.clone(),
            tool: req.tool.clone(),
            kind: req.kind.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone().unwrap_or_else(|| root.clone()),
        };
        let hook = svc::HookCall { session_id: session_id.clone(), mode, project_root: root, call: call.clone(), history };
        let pass = run_chain(&host, &hook).await;
        let Pass::Hold(reason) = pass else { break (pass, call) };
        if stopped() {
            break (Pass::Hold(reason), call);
        }
        held_since.get_or_insert_with(Instant::now);
        if held_for.as_ref() != Some(&reason) {
            tracing::info!("{} {}: held: {reason}", req.tool, req.tool_call_id);
            held(Some(reason.clone()));
            held_for = Some(reason);
        }
        tokio::select! {
            () = &mut wake => {}
            () = tokio::time::sleep(host.options.held_recheck) => {}
        }
        if stopped() {
            break (Pass::Hold(String::new()), call);
        }
    };
    if held_for.is_some() {
        held(None);
    }
    let held_ms = held_since.map_or(0, |t| t.elapsed().as_millis() as u64);
    let guard = |verdict: GuardVerdict, plugin: Option<String>, reason: Option<String>| {
        if host.tracing() {
            let event = TraceEvent::Guard {
                call_id: req.tool_call_id.clone(),
                at: crate::now_ms(),
                plugin: plugin.filter(|p| !p.is_empty()),
                mode: last_mode.clone(),
                verdict,
                reason,
                held_ms,
            };
            host.trace(&session_id, vec![event]);
        }
    };
    let record = |allowed: bool| {
        host.record(
            &session_id,
            HistoryItem::Tool { id: call.id.clone(), tool: call.tool.clone(), kind: call.kind.clone(), args: call.args.clone(), allowed },
        )
    };
    let block = |reason: String| {
        host.emit_session(&session_id, SessionEvent::ToolCallBlocked { id: req.tool_call_id.clone(), reason: reason.clone() });
        PreExecuteResult { decision: Verdict::Deny, reason: Some(reason) }
    };
    let (asks, asker) = match pass {
        Pass::Allow => {
            record(true);
            guard(GuardVerdict::Allow, None, None);
            return PreExecuteResult { decision: Verdict::Allow, reason: None };
        }
        Pass::Deny(reason, plugin) => {
            record(false);
            guard(GuardVerdict::Deny, Some(plugin), Some(reason.clone()));
            return block(reason);
        }
        // Only when the user stopped the turn while the call was held.
        Pass::Hold(reason) => {
            guard(GuardVerdict::Held, None, Some(reason).filter(|r| !r.is_empty()));
            return PreExecuteResult { decision: Verdict::Deny, reason: Some("The turn was stopped.".into()) };
        }
        Pass::Ask(asks, asker) => (asks, asker),
    };
    let options = vec![
        PermissionOption { id: ASK_ALLOW.into(), name: "Allow once".into(), kind: PermissionKind::AllowOnce },
        PermissionOption { id: ASK_REJECT.into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
    ];
    let patch = ToolCallPatch { id: req.tool_call_id.clone(), title: Some(title.clone()), kind: Some(req.kind.clone()), ..Default::default() };
    let rx = shared.request_permission(&session_id, patch, options, Some(asks.join(" ")));
    let allowed = matches!(rx.await, Ok(PermissionDecision::Selected(id)) if id == ASK_ALLOW);
    record(allowed);
    let answer = if allowed { "You allowed it." } else { "You didn't allow it." };
    guard(if allowed { GuardVerdict::Allow } else { GuardVerdict::Deny }, Some(asker), Some(format!("{} {answer}", asks.join(" "))));
    if allowed { PreExecuteResult { decision: Verdict::Allow, reason: None } } else { block("The user didn't allow this call.".into()) }
}

/// `hook` with the user's messages emptied (each still counts as an item).
fn without_transcript(hook: &svc::HookCall) -> svc::HookCall {
    let mut hook = hook.clone();
    for item in &mut hook.history {
        if let HistoryItem::User { text } = item {
            text.clear();
        }
    }
    hook
}

/// `bash: ls -la`, `read: src/main.rs`.
fn call_title(tool: &str, args: &serde_json::Value) -> String {
    let detail = ["command", "path", "file_path", "pattern", "url"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "));
    match detail {
        Some(d) if !d.is_empty() => format!("{tool}: {}", d.chars().take(120).collect::<String>()),
        _ => tool.to_string(),
    }
}
