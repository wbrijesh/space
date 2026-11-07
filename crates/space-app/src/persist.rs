//! Saving and reopening threads, through the host's session store (`session-store` plugin).
//!
//! A thread is saved as its summary ([`ThreadMeta`]) and its events: the user's messages, the
//! agent's finished messages and thoughts (chunks merged), tool calls and results, permission
//! and guardrail decisions, status (model) changes and turn starts and ends. Not streaming
//! chunks one by one, and not held calls (nothing waits after a restart). Events collect on the
//! thread and are written when a turn ends, when its mode changes, and when Space quits; a
//! failed write keeps them for the next one and says so in the banner.
//!
//! At launch the sidebar lists the saved threads; a thread's events are read when it's opened
//! and replayed through the same reducer that shows live ones. Writing in it loads the agent's
//! session again (ACP `session/load`) when the agent can, else starts a new one.

use std::hash::{BuildHasher as _, Hasher as _};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, Window};
use space_core::protocol::service::ThreadMeta;
use space_core::SessionEvent;

use crate::app::{SpaceApp, now_ms};
use crate::threads::Thread;

/// How long quitting waits for the last saves.
const QUIT_SAVE_TIMEOUT: Duration = Duration::from_secs(2);

/// A new thread id: time-ordered and unique enough to name a folder.
pub(crate) fn new_thread_id() -> String {
    let mut random = std::collections::hash_map::RandomState::new().build_hasher();
    random.write_u64(now_ms());
    format!("{:x}-{:08x}", now_ms(), random.finish() as u32)
}

/// Add `event` to the events waiting to be saved, merging streamed chunks.
pub(crate) fn record(unsaved: &mut Vec<SessionEvent>, event: &SessionEvent) {
    match (unsaved.last_mut(), event) {
        (_, SessionEvent::Created { .. } | SessionEvent::ToolCallHeld { .. }) => {}
        (Some(SessionEvent::AgentText { text: buf }), SessionEvent::AgentText { text })
        | (Some(SessionEvent::AgentThought { text: buf }), SessionEvent::AgentThought { text }) => buf.push_str(text),
        (Some(SessionEvent::Status(last)), SessionEvent::Status(status)) => last.merge(status),
        _ => unsaved.push(event.clone()),
    }
}

impl SpaceApp {
    /// Threads are saved when a session store is installed.
    pub(crate) fn saving_threads(&self) -> bool {
        self.host.has_session_store()
    }

    fn thread_meta(&self, thread: &Thread) -> ThreadMeta {
        let state = &thread.state;
        let session_id = thread.session.as_ref().map(|s| s.id().to_string());
        ThreadMeta {
            id: thread.id.clone(),
            title: thread.title(),
            project: thread.cwd.display().to_string(),
            mode: self.effective_mode(thread),
            model: state.status.model.clone(),
            agent: Some(state.plugin_id.clone()).filter(|p| !p.is_empty()).or_else(|| thread.resume.as_ref().map(|r| r.0.clone())),
            session_id: session_id.or_else(|| thread.resume.as_ref().map(|r| r.1.clone())),
            created_ms: thread.created_ms,
            updated_ms: now_ms(),
        }
    }

    /// Write the thread's waiting events and its summary (once it has something to save).
    pub(crate) fn save_thread(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        if thread.saving || thread.unloaded || (thread.state.items.is_empty() && thread.meta.is_none()) {
            return;
        }
        let meta = self.thread_meta(thread);
        let host = self.host.clone();
        let thread = self.thread_mut(key).expect("thread exists");
        let events = std::mem::take(&mut thread.unsaved);
        thread.saving = true;
        let id = thread.id.clone();
        cx.spawn(async move |this, cx| {
            let result = host.save_thread(&id, Some(meta.clone()), &events).await;
            let _ = this.update(cx, |this, cx| {
                let Some(thread) = this.thread_mut(key) else { return };
                thread.saving = false;
                match result {
                    Ok(()) => {
                        thread.meta = Some(meta);
                        // More arrived meanwhile (a turn ended): save those too.
                        if matches!(thread.unsaved.last(), Some(SessionEvent::TurnEnded { .. })) {
                            this.save_thread(key, cx);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("saving thread {id}: {e:#}");
                        thread.unsaved.splice(0..0, events);
                        this.banner = Some(format!("Couldn't save this thread: {e:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Before quitting: save every thread's waiting events (blocks briefly).
    pub(crate) fn save_all_now(&mut self) {
        if !self.saving_threads() {
            return;
        }
        let saves: Vec<_> = self
            .threads
            .iter()
            .filter(|t| !t.unloaded && !t.saving && !t.state.items.is_empty() && (!t.unsaved.is_empty() || t.meta.is_none()))
            .map(|t| (t.id.clone(), self.thread_meta(t), t.unsaved.clone()))
            .collect();
        self.host.save_threads_blocking(saves, QUIT_SAVE_TIMEOUT);
    }

    /// At launch: list the saved threads in the sidebar, below the launch threads.
    pub(crate) fn load_saved_threads(&mut self, cx: &mut Context<Self>) {
        if !self.saving_threads() {
            return;
        }
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let saved = match host.list_threads().await {
                Ok(saved) => saved,
                Err(e) => {
                    tracing::warn!("listing saved threads: {e:#}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                // Oldest first, like `threads`; before the ones opened at launch.
                for (ix, meta) in saved.into_iter().rev().enumerate() {
                    let key = this.next_key;
                    this.next_key += 1;
                    let mut thread = Thread::new(key, PathBuf::from(&meta.project));
                    thread.id.clone_from(&meta.id);
                    thread.created_ms = meta.created_ms;
                    thread.mode.clone_from(&meta.mode);
                    thread.resume = meta.agent.clone().zip(meta.session_id.clone());
                    thread.unloaded = true;
                    thread.meta = Some(meta);
                    this.threads.insert(ix, thread);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read a saved thread's events (when it's opened). A turn that was cut short ends as
    /// cancelled, so no permission card waits.
    pub(crate) fn ensure_loaded(&mut self, key: u64, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let Some(thread) = self.thread_mut(key).filter(|t| t.unloaded) else { return };
        thread.unloaded = false;
        thread.loading = true;
        let id = thread.id.clone();
        cx.spawn(async move |this, cx| {
            let result = host.load_thread(&id).await;
            let _ = this.update(cx, |this, cx| {
                let Some(thread) = this.thread_mut(key) else { return };
                thread.loading = false;
                match result {
                    Ok((_, events)) => {
                        for event in &events {
                            thread.apply(event);
                        }
                        if thread.state.running || thread.state.pending_permission().is_some() {
                            thread.apply(&SessionEvent::TurnEnded { stop_reason: "cancelled".into() });
                        }
                    }
                    Err(e) => thread.error = Some(format!("Couldn't open this thread: {e:#}")),
                }
                this.transcript_scroll.jump_to_bottom();
                cx.notify();
            });
        })
        .detach();
    }

    /// Delete a thread: stop its turn, forget it, and remove what was saved of it.
    pub(crate) fn delete_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.threads.iter().position(|t| t.key == key) else { return };
        let thread = self.threads.remove(ix);
        if let Some(session) = &thread.session {
            // Stops a running turn and answers any open permission card.
            if session.is_running() {
                let _ = session.cancel();
            }
            self.session_threads.remove(session.id());
        }
        if self.saving_threads() {
            let (host, id) = (self.host.clone(), thread.id.clone());
            cx.spawn(async move |this, cx| {
                if let Err(e) = host.delete_thread(&id).await {
                    let _ = this.update(cx, |this, cx| {
                        this.banner = Some(format!("Couldn't delete the saved thread: {e:#}"));
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        self.back.retain(|k| *k != key);
        self.forward.retain(|k| *k != key);
        if self.selected == Some(key) {
            self.selected = None;
            match self.threads.last().map(|t| t.key) {
                Some(next) => self.select_inner(next, false, window, cx),
                None => self.new_thread(None, window, cx),
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use space_core::SessionState;
    use space_core::protocol::SpaceStatus;

    use super::*;

    #[test]
    fn chunks_merge_and_holds_are_left_out() {
        let mut unsaved = Vec::new();
        let events = [
            SessionEvent::UserMessage { text: "hi".into(), attachments: vec![] },
            SessionEvent::TurnStarted,
            SessionEvent::AgentThought { text: "a".into() },
            SessionEvent::AgentThought { text: "b".into() },
            // A hold isn't saved; the agent's report of the call is.
            SessionEvent::ToolCallHeld { id: "t".into(), title: "ls".into(), reason: Some("down".into()) },
            SessionEvent::ToolCall(space_core::ToolCallInfo {
                id: "t".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: space_core::ToolStatus::Completed,
                output: "a".into(),
                locations: vec![],
                blocked: None,
                held: None,
            }),
            SessionEvent::AgentText { text: "Hel".into() },
            SessionEvent::AgentText { text: "lo".into() },
            SessionEvent::Status(SpaceStatus { model: Some("m".into()), ..Default::default() }),
            SessionEvent::Status(SpaceStatus { thinking_level: Some("high".into()), ..Default::default() }),
            SessionEvent::TurnEnded { stop_reason: "end_turn".into() },
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

    #[test]
    fn thread_ids_are_folder_names() {
        let (a, b) = (new_thread_id(), new_thread_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
