//! Traces and the trajectory seam.
//!
//! Agents send `_space/trace` batches; the host adds its own facts (the prompt, the context it
//! sent, the tool pipeline's verdicts), tags each batch with the session's thread and hands it
//! to every running plugin that observes `trace` (and uses `read:trace`, which the manifest
//! requires). Each such plugin has its own unbounded queue, drained one `trace/record` at a
//! time in order: unlike other observed events, none is dropped, since a trajectory with holes
//! would mislead.
//!
//! The `trajectory` provider answers the app's questions ([`Host::trajectory_read`], ...) and
//! says when a thread's trajectory changed (relayed as [`HostEvent::TrajectoryChanged`]).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_plugin::rpc::Peer;
use space_protocol::CAP_TRAJECTORY;
use space_protocol::service::ThreadRef;
use space_protocol::trace::{
    TRACE_RECORD, TRAJECTORY_DETAIL, TRAJECTORY_READ, TRAJECTORY_STATS, TraceEvent, TraceRecord, TrajectoryDetail,
    TrajectoryDetailRequest, TrajectoryPage, TrajectoryRead, TrajectoryStats,
};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::{Host, HostInner};

/// How long one batch may take before the next is sent anyway.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);
/// At most this many batches are kept for plugins that haven't started yet.
const BACKLOG_MAX: usize = 50_000;
/// The app's trajectory calls (a long thread's page is built on the first read).
const TRAJECTORY_TIMEOUT: Duration = Duration::from_secs(20);

/// One plugin's trace queue.
pub(super) struct TraceSink {
    tx: mpsc::UnboundedSender<TraceRecord>,
}

impl TraceSink {
    /// Start draining a queue into `peer`; the task ends with the queue or the connection.
    pub(super) fn start(rt: &Handle, peer: Peer, plugin_id: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<TraceRecord>();
        rt.spawn(async move {
            while let Some(record) = rx.recv().await {
                if let Err(e) = peer.call::<_, serde_json::Value>(TRACE_RECORD, &record, Some(RECORD_TIMEOUT)).await {
                    if peer.is_closed() {
                        break;
                    }
                    tracing::warn!("`{plugin_id}` recording a trace: {e}");
                }
            }
        });
        Self { tx }
    }
}

impl HostInner {
    /// Hand `events` of `session_id` to every plugin that observes traces (and keep them for
    /// the ones still starting).
    pub(super) fn trace(&self, session_id: &str, events: Vec<TraceEvent>) {
        if events.is_empty() || !self.tracing() {
            return;
        }
        let thread_id = self.sessions.lock().unwrap().get(session_id).and_then(|m| m.thread_id.clone()).unwrap_or_default();
        let record = TraceRecord { thread_id, session_id: session_id.to_string(), events };
        // Held while sending, so a plugin that starts now gets each batch once (see `start_tracer`).
        let mut backlog = self.trace_backlog.lock().unwrap();
        if let Some(backlog) = backlog.as_mut()
            && backlog.len() < BACKLOG_MAX
        {
            backlog.push(record.clone());
        }
        let services: Vec<_> =
            self.services.lock().unwrap().values().filter(|s| s.tracer.is_some() && s.alive()).cloned().collect();
        for service in services {
            if let Some(sink) = &service.tracer {
                let _ = sink.tx.send(record.clone());
            }
        }
    }

    /// A plugin that observes traces is now running: it gets the batches sent before.
    pub(super) fn start_tracer(&self, sink: &TraceSink, insert: impl FnOnce()) {
        let backlog = self.trace_backlog.lock().unwrap();
        insert();
        for record in backlog.iter().flatten() {
            let _ = sink.tx.send(record.clone());
        }
    }

    /// Every plugin was started (or failed to): stop keeping batches for late ones.
    pub(super) fn end_trace_backlog(&self) {
        *self.trace_backlog.lock().unwrap() = None;
    }

    /// Whether any plugin would get a trace (so the host can skip building its facts): one is
    /// running, or the plugins are still starting.
    pub(super) fn tracing(&self) -> bool {
        self.trace_backlog.lock().unwrap().is_some() || self.services.lock().unwrap().values().any(|s| s.tracer.is_some() && s.alive())
    }

    async fn trajectory_call<P: Serialize, R: DeserializeOwned>(self: &Arc<Self>, method: &str, params: &P) -> Result<R> {
        let plugin = self.providers(CAP_TRAJECTORY).into_iter().next().ok_or_else(|| anyhow!("no trajectory plugin is installed"))?;
        self.call_service(plugin.id(), method, params, TRAJECTORY_TIMEOUT)
            .await
            .map_err(|e| anyhow!("{} ({method}): {e}", plugin.manifest.name))
    }
}

impl Host {
    /// Whether a trajectory plugin is installed (on and approved): the app shows its tab.
    pub fn has_trajectory(&self) -> bool {
        !self.inner.providers(CAP_TRAJECTORY).is_empty()
    }

    /// A page of a thread's trajectory (the newest rows unless `read.before`), or the rows
    /// matching `read.query`.
    pub async fn trajectory_read(&self, read: TrajectoryRead) -> Result<TrajectoryPage> {
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move { inner.trajectory_call(TRAJECTORY_READ, &read).await }).await?
    }

    /// Everything the inspector shows for a row (its id) or a request (`request:<n>`).
    pub async fn trajectory_detail(&self, request: TrajectoryDetailRequest) -> Result<TrajectoryDetail> {
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move { inner.trajectory_call(TRAJECTORY_DETAIL, &request).await }).await?
    }

    /// A thread's totals (turns, steps, usage, generation speed).
    pub async fn trajectory_stats(&self, thread_id: &str) -> Result<TrajectoryStats> {
        let inner = self.inner.clone();
        let thread = ThreadRef { thread_id: thread_id.to_string() };
        self.inner.rt.spawn(async move { inner.trajectory_call(TRAJECTORY_STATS, &thread).await }).await?
    }
}
