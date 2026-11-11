//! Observed events: what happens in sessions, told to the service plugins that `observe` it.
//!
//! Fire-and-forget: each observing plugin has its own bounded queue, drained by a task that
//! sends one event at a time and waits for the answer. Nothing else waits on it, so a slow
//! plugin only falls behind on its own events; once its queue is full its newest events are
//! dropped (with a warning).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use space_plugin::rpc::Peer;
use space_protocol::USE_READ_TRANSCRIPT;
use space_protocol::service::{self as svc, Observed};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::{Host, HostInner};
use crate::session::{SessionEvent, SessionId, ToolStatus};

/// Events waiting for one plugin.
const QUEUE: usize = 256;
/// How long one event may take before the next is sent anyway.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);

/// One plugin's event queue.
pub(super) struct Observer {
    tx: mpsc::Sender<Observed>,
    dropped: AtomicU64,
}

impl Observer {
    /// Start draining a queue into `peer`; the task ends with the queue (when the service is
    /// dropped) or the connection.
    pub(super) fn start(rt: &Handle, peer: Peer, plugin_id: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<Observed>(QUEUE);
        rt.spawn(async move {
            while let Some(event) = rx.recv().await {
                let name = event.name();
                if let Err(e) = peer.call::<_, serde_json::Value>(svc::EVENTS_OBSERVE, &event, Some(OBSERVE_TIMEOUT)).await {
                    if peer.is_closed() {
                        break;
                    }
                    tracing::warn!("`{plugin_id}` observing {name}: {e}");
                }
            }
        });
        Self { tx, dropped: AtomicU64::new(0) }
    }
}

impl Host {
    /// The session Space is frontmost on (`None`: Settings, another app, no thread). Observers
    /// are told it (`watching`) with the events where it matters.
    pub fn set_watching(&self, session_id: Option<SessionId>) {
        *self.inner.watching.lock().unwrap() = session_id;
    }
}

impl HostInner {
    /// Tell observers what a session event means to them, if anything.
    pub(super) fn observe_session(&self, session_id: &str, event: &SessionEvent) {
        let id = || session_id.to_string();
        let watching = || self.watching.lock().unwrap().as_deref() == Some(session_id);
        let finished = |status: Option<ToolStatus>| matches!(status, Some(ToolStatus::Completed | ToolStatus::Failed));
        let observed = match event {
            SessionEvent::Created { plugin_id, cwd } => Observed::SessionCreated {
                session_id: id(),
                agent: plugin_id.clone(),
                project_root: cwd.display().to_string(),
            },
            SessionEvent::TurnStarted => Observed::TurnStarted { session_id: id() },
            SessionEvent::TurnEnded { stop_reason, .. } => {
                Observed::TurnEnded { session_id: id(), stop_reason: stop_reason.clone(), watching: watching() }
            }
            SessionEvent::ToolCallHeld { id: call_id, reason: Some(reason), .. } => Observed::ToolHeld {
                session_id: id(),
                call_id: call_id.clone(),
                reason: reason.clone(),
                watching: watching(),
            },
            SessionEvent::PermissionRequested(request) => Observed::PermissionRequested {
                session_id: id(),
                call_id: request.tool_call.id.clone(),
                reason: request.reason.clone(),
                watching: watching(),
            },
            SessionEvent::ToolCall(info) if finished(Some(info.status)) => Observed::ToolResult {
                session_id: id(),
                call_id: info.id.clone(),
                failed: info.status == ToolStatus::Failed,
                output: Some(info.output.clone()),
            },
            SessionEvent::ToolCallUpdate(patch) if finished(patch.status) => Observed::ToolResult {
                session_id: id(),
                call_id: patch.id.clone(),
                failed: patch.status == Some(ToolStatus::Failed),
                output: patch.output.clone(),
            },
            _ => return,
        };
        self.observe(observed);
    }

    /// Queue `event` for every running plugin that observes it (without the transcript for
    /// those that don't use `read:transcript`).
    pub(super) fn observe(&self, event: Observed) {
        let services: Vec<_> = self.services.lock().unwrap().values().cloned().collect();
        for service in services {
            let (Some(observer), manifest) = (&service.observer, &service.plugin.manifest) else { continue };
            if !manifest.observes(event.name()) || !service.alive() {
                continue;
            }
            let event = if manifest.uses(USE_READ_TRANSCRIPT) { event.clone() } else { event.clone().without_transcript() };
            if observer.tx.try_send(event).is_err() && observer.dropped.fetch_add(1, Ordering::Relaxed) % 100 == 0 {
                let dropped = observer.dropped.load(Ordering::Relaxed);
                tracing::warn!("`{}` isn't keeping up with events; dropped {dropped} so far", service.plugin.id());
            }
        }
    }
}
