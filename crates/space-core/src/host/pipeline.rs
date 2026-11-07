//! The tool pipeline: `_space/tools/preExecute` runs the `tools/pre-execute` hook chain, and
//! holds a call while a listener can't decide.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use space_protocol::service::{self as svc, HistoryItem};
use space_protocol::{CATALOG_PERMISSION_MODES, HOOK_TOOLS_PRE_EXECUTE, PreExecute, PreExecuteResult, Verdict};

use super::HostInner;
use super::agents::Shared;
use crate::session::{PermissionDecision, PermissionKind, PermissionOption, SessionEvent, ToolCallPatch};

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
    /// The permission mode chosen for the session (`None`: the catalog's default).
    pub(super) mode: Option<String>,
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
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self { cwd, mode: None, history: VecDeque::new(), signals: HoldSignals::default() }
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
}

/// One pass through the `tools/pre-execute` chain.
enum Pass {
    Allow,
    Deny(String),
    /// Ask the user, with the listeners' reasons.
    Ask(Vec<String>),
    /// A listener can't decide right now: wait.
    Hold(String),
}

/// Every listener, in order. A deny wins over a hold, a hold over asks.
async fn run_chain(host: &Arc<HostInner>, hook: &svc::HookCall) -> Pass {
    let (mut asks, mut hold) = (Vec::new(), None);
    for plugin in host.hook_listeners(HOOK_TOOLS_PRE_EXECUTE) {
        let name = &plugin.manifest.name;
        match host.call_service::<_, svc::HookDecision>(plugin.id(), svc::HOOK_PRE_EXECUTE, hook, HOOK_TIMEOUT).await {
            Ok(d) => match d.decision {
                svc::HookVerdict::Deny => {
                    tracing::info!("{} {}: denied by {}", hook.call.tool, hook.call.id, plugin.id());
                    return Pass::Deny(d.reason.unwrap_or_else(|| format!("Blocked by {name}.")));
                }
                svc::HookVerdict::Ask => asks.push(d.reason.unwrap_or_else(|| format!("{name} wants you to check this call."))),
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
        None => Pass::Ask(asks),
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
    let (pass, call) = loop {
        // Listen before running the chain, so a wake-up during it isn't lost.
        let wake = signals.wake.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        // Facts are read on every pass: the mode may have changed while the call was held.
        let (mode, root, history) = host.session_facts(&session_id);
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
    let asks = match pass {
        Pass::Allow => {
            record(true);
            return PreExecuteResult { decision: Verdict::Allow, reason: None };
        }
        Pass::Deny(reason) => {
            record(false);
            return block(reason);
        }
        // Only when the user stopped the turn while the call was held.
        Pass::Hold(_) => return PreExecuteResult { decision: Verdict::Deny, reason: Some("The turn was stopped.".into()) },
        Pass::Ask(asks) => asks,
    };
    let options = vec![
        PermissionOption { id: ASK_ALLOW.into(), name: "Allow once".into(), kind: PermissionKind::AllowOnce },
        PermissionOption { id: ASK_REJECT.into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
    ];
    let patch = ToolCallPatch { id: req.tool_call_id.clone(), title: Some(title.clone()), kind: Some(req.kind.clone()), ..Default::default() };
    let rx = shared.request_permission(&session_id, patch, options, Some(asks.join(" ")));
    let allowed = matches!(rx.await, Ok(PermissionDecision::Selected(id)) if id == ASK_ALLOW);
    record(allowed);
    if allowed { PreExecuteResult { decision: Verdict::Allow, reason: None } } else { block("The user didn't allow this call.".into()) }
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
