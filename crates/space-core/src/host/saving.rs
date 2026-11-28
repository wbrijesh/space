//! Saving threads as they happen. Every event of a session made for a thread (`thread_id`) is
//! saved by the host soon after it happens ([`SAVE_DELAY`]), whether or not an app is connected;
//! events the app makes itself (going back to a message, a new session) come in through
//! [`Host::record_event`], and the thread's summary through [`Host::save_thread_meta`]. A new
//! thread's events wait for its first summary (the store keeps nothing without one). One write
//! at a time per thread; a failed write keeps its events for the next and is reported
//! ([`HostEvent::SaveFailed`]). Without a session store nothing is kept.
//!
//! What's saved is what the thread shows, chunks merged ([`record`]): not held calls or network
//! retries (nothing waits after a restart) and not the `Created` marker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use space_protocol::service::ThreadMeta;

use super::{Host, HostEvent, HostInner};
use crate::session::SessionEvent;

/// How soon after an event it's written: events this close together are one write.
pub const SAVE_DELAY: Duration = Duration::from_millis(250);
/// How long a failed write waits before trying again.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// What the host keeps to save one thread.
#[derive(Default)]
pub(super) struct ThreadSaver {
    /// A summary not yet written.
    meta: Option<ThreadMeta>,
    /// The store has a summary for it (events can be written without one).
    known: bool,
    /// How many events the store has; `None` until asked.
    saved: Option<u64>,
    unsaved: Vec<SessionEvent>,
    /// A write is in flight or scheduled.
    busy: bool,
    /// Being opened ([`Host::open_thread`]): no write starts.
    paused: bool,
}

/// Add `event` to the events waiting to be saved, merging streamed chunks.
pub fn record(unsaved: &mut Vec<SessionEvent>, event: &SessionEvent) {
    match (unsaved.last_mut(), event) {
        (_, SessionEvent::Created { .. } | SessionEvent::ToolCallHeld { .. } | SessionEvent::Retry(_)) => {}
        (Some(SessionEvent::AgentText { text: buf }), SessionEvent::AgentText { text })
        | (Some(SessionEvent::AgentThought { text: buf }), SessionEvent::AgentThought { text }) => buf.push_str(text),
        (Some(SessionEvent::Status(last)), SessionEvent::Status(status)) => last.merge(status),
        _ => unsaved.push(event.clone()),
    }
}

impl Host {
    /// Save thread `meta.id`'s summary (and from now on its events, as they happen).
    pub fn save_thread_meta(&self, meta: ThreadMeta) {
        if !self.has_session_store() {
            return;
        }
        let id = meta.id.clone();
        {
            let mut savers = self.inner.savers.lock().unwrap();
            let saver = savers.entry(id.clone()).or_default();
            saver.meta = Some(meta);
        }
        self.inner.schedule_save(&id, Duration::ZERO);
    }

    /// An event of thread `thread_id` the app made itself: saved like its sessions' events.
    pub fn record_event(&self, thread_id: &str, event: &SessionEvent) {
        self.inner.record_saved(thread_id, event);
    }

    /// Whether the host is saving thread `thread_id` (it was given a summary).
    pub fn is_saving(&self, thread_id: &str) -> bool {
        self.inner.savers.lock().unwrap().contains_key(thread_id)
    }

    /// [`Host::flush_saves`] from outside the runtime (Space is quitting).
    pub fn flush_saves_blocking(&self, timeout: Duration) {
        let host = self.clone();
        self.inner.rt.block_on(async move { host.flush_saves(timeout).await });
    }

    /// Write everything waiting, waiting at most `timeout` (before a planned exit).
    pub async fn flush_saves(&self, timeout: Duration) {
        let inner = self.inner.clone();
        let flush = async move {
            loop {
                let pending: Vec<String> = inner.savers.lock().unwrap().iter().filter(|(_, s)| s.busy || !s.unsaved.is_empty() || s.meta.is_some()).map(|(id, _)| id.clone()).collect();
                if pending.is_empty() {
                    return;
                }
                for id in pending {
                    inner.schedule_save(&id, Duration::ZERO);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = self.inner.rt.spawn(async move { tokio::time::timeout(timeout, flush).await }).await;
    }

    /// A saved thread's summary and every event so far (the store's, and those not written
    /// yet), with a [`HostEvent::ThreadOpened`] marker reported at that exact point: its events
    /// before the marker are in what's returned, those after it are reported after it. For a
    /// thread that's still going (its agent kept working while no app showed it).
    pub async fn open_thread(&self, thread_id: &str) -> Result<(ThreadMeta, Vec<SessionEvent>)> {
        let tracked = {
            let mut savers = self.inner.savers.lock().unwrap();
            savers.get_mut(thread_id).map(|s| s.paused = true).is_some()
        };
        if !tracked {
            let (meta, events, _) = self.load_thread(thread_id).await?;
            self.inner.emit(HostEvent::ThreadOpened { thread_id: thread_id.to_string() });
            return Ok((meta, events));
        }
        // Let a write in flight finish; none starts while paused.
        while self.inner.savers.lock().unwrap().get(thread_id).is_some_and(|s| s.busy) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let loaded = self.load_thread(thread_id).await;
        let pending = {
            let mut savers = self.inner.savers.lock().unwrap();
            let saver = savers.get_mut(thread_id);
            let pending = saver.map(|s| {
                s.paused = false;
                s.unsaved.clone()
            });
            self.inner.emit(HostEvent::ThreadOpened { thread_id: thread_id.to_string() });
            pending.unwrap_or_default()
        };
        self.inner.schedule_save(thread_id, Duration::ZERO);
        let (meta, mut events, _) = match loaded {
            Ok(loaded) => loaded,
            // A thread not written yet (its first summary hasn't come): only what waits.
            Err(e) if format!("{e:#}").contains("no saved thread") && !pending.is_empty() => (ThreadMeta { id: thread_id.to_string(), ..Default::default() }, Vec::new(), 0),
            Err(e) => return Err(e),
        };
        events.extend(pending);
        Ok((meta, events))
    }

    /// Stop saving thread `thread_id` and drop what waits (it's deleted).
    pub(super) fn forget_saves(&self, thread_id: &str) {
        self.inner.savers.lock().unwrap().remove(thread_id);
    }
}

impl HostInner {
    /// A session was made for thread `thread_id`: its events are saved from now on.
    pub(super) fn track_thread(&self, thread_id: &str) {
        if self.providers(space_protocol::CAP_SESSION_STORE).is_empty() {
            return;
        }
        self.savers.lock().unwrap().entry(thread_id.to_string()).or_default();
    }

    /// Report a session event, recording it for saving if its thread is saved. Both happen under
    /// the savers' lock, so [`Host::open_thread`]'s marker splits the events exactly: those before
    /// it are in what it returns, those after it are reported after it.
    pub(super) fn emit_saved_session(&self, session_id: &str, event: SessionEvent) {
        let thread_id = self.sessions.lock().unwrap().get(session_id).and_then(|m| m.thread_id.clone());
        let mut save = None;
        {
            let mut savers = self.savers.lock().unwrap();
            if let Some(thread_id) = thread_id
                && let Some(saver) = savers.get_mut(&thread_id)
            {
                let before = saver.unsaved.len();
                record(&mut saver.unsaved, &event);
                if saver.unsaved.len() != before || matches!(event, SessionEvent::AgentText { .. } | SessionEvent::AgentThought { .. } | SessionEvent::Status(_)) {
                    save = Some(thread_id);
                }
            }
            self.emit(HostEvent::Session { session_id: session_id.to_string(), event });
        }
        if let Some(thread_id) = save {
            self.schedule_save(&thread_id, SAVE_DELAY);
        }
    }

    /// The thread a session was made for.
    pub(super) fn session_thread(&self, session_id: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(session_id).and_then(|m| m.thread_id.clone())
    }

    fn record_saved(&self, thread_id: &str, event: &SessionEvent) {
        {
            let mut savers = self.savers.lock().unwrap();
            let Some(saver) = savers.get_mut(thread_id) else { return };
            let before = saver.unsaved.len();
            record(&mut saver.unsaved, event);
            if saver.unsaved.len() == before && !matches!(event, SessionEvent::AgentText { .. } | SessionEvent::AgentThought { .. } | SessionEvent::Status(_)) {
                return;
            }
        }
        self.schedule_save(thread_id, SAVE_DELAY);
    }

    /// Write thread `thread_id` after `delay`, unless a write is already on its way.
    fn schedule_save(&self, thread_id: &str, delay: Duration) {
        let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade) else { return };
        {
            let mut savers = self.savers.lock().unwrap();
            let Some(saver) = savers.get_mut(thread_id) else { return };
            if saver.busy || saver.paused {
                return;
            }
            saver.busy = true;
        }
        let id = thread_id.to_string();
        self.rt.spawn(async move {
            tokio::time::sleep(delay).await;
            me.write_thread(id).await;
        });
    }

    /// Write what waits for one thread, then again while more came meanwhile.
    async fn write_thread(self: Arc<Self>, id: String) {
        let host = Host { inner: self.clone() };
        loop {
            let need_count = matches!(self.savers.lock().unwrap().get(&id), Some(s) if s.saved.is_none());
            if need_count {
                // Not loaded by this host: ask the store how many events it has.
                let count = match host.load_thread(&id).await {
                    Ok((_, _, count)) => Some((count, true)),
                    Err(e) if format!("{e:#}").contains("no saved thread") => Some((0, false)),
                    Err(e) => {
                        tracing::warn!("reading thread {id} before saving: {e:#}");
                        None
                    }
                };
                let mut savers = self.savers.lock().unwrap();
                let Some(saver) = savers.get_mut(&id) else { return };
                match count {
                    Some((count, known)) => {
                        saver.saved = Some(count);
                        saver.known |= known;
                    }
                    None => {
                        saver.busy = false;
                        drop(savers);
                        self.retry_later(&id);
                        return;
                    }
                }
            }
            let (meta, first, events) = {
                let mut savers = self.savers.lock().unwrap();
                let Some(saver) = savers.get_mut(&id) else { return };
                if saver.meta.is_none() && saver.unsaved.is_empty() {
                    saver.busy = false;
                    return;
                }
                if saver.meta.is_none() && !saver.known {
                    // Nothing can be written before its summary.
                    saver.busy = false;
                    return;
                }
                (saver.meta.take(), saver.saved.unwrap_or(0), std::mem::take(&mut saver.unsaved))
            };
            let result = host.save_thread(&id, meta.clone(), first, &events).await;
            let mut savers = self.savers.lock().unwrap();
            let Some(saver) = savers.get_mut(&id) else { return };
            match result {
                Ok(()) => {
                    saver.saved = Some(first + events.len() as u64);
                    saver.known |= meta.is_some();
                }
                Err(e) => {
                    tracing::warn!("saving thread {id}: {e:#}");
                    // Put them back in front of what came meanwhile; a newer summary wins.
                    let later = std::mem::replace(&mut saver.unsaved, events);
                    for event in &later {
                        record(&mut saver.unsaved, event);
                    }
                    if saver.meta.is_none() {
                        saver.meta = meta;
                    }
                    saver.busy = false;
                    drop(savers);
                    self.emit(HostEvent::SaveFailed { thread_id: id.clone(), message: format!("{e:#}") });
                    self.retry_later(&id);
                    return;
                }
            }
        }
    }

    fn retry_later(self: &Arc<Self>, id: &str) {
        let (inner, id) = (self.clone(), id.to_string());
        self.rt.spawn(async move {
            tokio::time::sleep(RETRY_DELAY).await;
            inner.schedule_save(&id, Duration::ZERO);
        });
    }
}

/// The savers by thread id.
pub(super) type Savers = std::sync::Mutex<HashMap<String, ThreadSaver>>;

#[cfg(test)]
mod tests {
    use space_protocol::SpaceStatus;

    use super::*;
    use crate::session::{SessionState, ToolCallInfo, ToolStatus};

    #[test]
    fn chunks_merge_and_holds_are_left_out() {
        let mut unsaved = Vec::new();
        let events = [
            SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None, id: None, steered: false },
            SessionEvent::TurnStarted,
            SessionEvent::AgentThought { text: "a".into() },
            SessionEvent::AgentThought { text: "b".into() },
            // A hold isn't saved; the agent's report of the call is.
            SessionEvent::ToolCallHeld { id: "t".into(), title: "ls".into(), reason: Some("down".into()) },
            SessionEvent::Retry(Some(space_protocol::RetryState {
                kind: space_protocol::RetryKind::Offline,
                attempt: 1,
                reason: "ECONNRESET".into(),
            })),
            SessionEvent::ToolCall(ToolCallInfo {
                id: "t".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: ToolStatus::Completed,
                output: "a".into(),
                locations: vec![],
                blocked: None,
                held: None,
                subagent: false,
            }),
            SessionEvent::AgentText { text: "Hel".into() },
            SessionEvent::AgentText { text: "lo".into() },
            SessionEvent::Status(SpaceStatus { model: Some("m".into()), ..Default::default() }),
            SessionEvent::Status(SpaceStatus { thinking_level: Some("high".into()), ..Default::default() }),
            SessionEvent::turn_ended("end_turn"),
        ];
        let mut live = SessionState::default();
        for e in &events {
            live.apply(e);
            record(&mut unsaved, e);
        }
        assert_eq!(unsaved.len(), 7);
        assert_eq!(unsaved[2], SessionEvent::AgentThought { text: "ab".into() });
        // Replaying what's saved shows what was shown live.
        let json = serde_json::to_string(&unsaved).unwrap();
        let mut replayed = SessionState::default();
        for e in serde_json::from_str::<Vec<SessionEvent>>(&json).unwrap() {
            replayed.apply(&e);
        }
        assert_eq!(replayed.items, live.items);
        assert_eq!(replayed.status.model.as_deref(), Some("m"));
        assert_eq!(replayed.status.thinking_level.as_deref(), Some("high"));
    }
}
