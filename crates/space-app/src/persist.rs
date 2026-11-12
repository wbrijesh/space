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
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, PromptLevel, Window};
use space_core::protocol::service::ThreadMeta;
use space_core::{SessionEvent, ThreadSave};

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
        (_, SessionEvent::Created { .. } | SessionEvent::ToolCallHeld { .. } | SessionEvent::Retry(_)) => {}
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
            // Not over what was saved while the thread (or its status) is still on its way.
            model: state.status.model.clone().or_else(|| thread.meta.as_ref().and_then(|m| m.model.clone())),
            agent: Some(state.plugin_id.clone()).filter(|p| !p.is_empty()).or_else(|| thread.resume.as_ref().map(|r| r.0.clone())),
            session_id: session_id.or_else(|| thread.resume.as_ref().map(|r| r.1.clone())),
            created_ms: thread.created_ms,
            updated_ms: now_ms(),
            titled: thread.name.is_some(),
            renamed: thread.renamed,
            closed_ms: None,
            pinned_ms: thread.pinned_ms,
        }
    }

    /// Write the thread's waiting events and its summary (once it has something to save). One
    /// save at a time per thread: asked for during one (or while the thread loads), it follows.
    pub(crate) fn save_thread(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.unloaded {
            return;
        }
        if thread.saving || thread.loading {
            thread.save_again = true;
            return;
        }
        if thread.state.items.is_empty() && thread.meta.is_none() {
            return;
        }
        let meta = self.thread_meta(self.thread(key).expect("thread exists"));
        let host = self.host.clone();
        let thread = self.thread_mut(key).expect("thread exists");
        thread.inflight = std::mem::take(&mut thread.unsaved);
        thread.saving = true;
        thread.save_again = false;
        let (id, first, events) = (thread.id.clone(), thread.saved, thread.inflight.clone());
        cx.spawn(async move |this, cx| {
            let result = host.save_thread(&id, Some(meta.clone()), first, &events).await;
            let _ = this.update(cx, |this, cx| {
                // Closed while this save was in flight (the close wrote it).
                let Some(thread) = this.thread_mut(key) else { return };
                thread.saving = false;
                let sent = std::mem::take(&mut thread.inflight);
                match result {
                    Ok(()) => {
                        thread.saved += sent.len() as u64;
                        thread.meta = Some(meta);
                    }
                    Err(e) => {
                        tracing::warn!("saving thread {id}: {e:#}");
                        thread.unsaved.splice(0..0, sent);
                        this.banner = Some(format!("Couldn't save this thread: {e:#}"));
                    }
                }
                if this.thread(key).is_some_and(|t| t.save_again) {
                    this.save_thread(key, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Before quitting: save what every thread has waiting, the events of a save still in
    /// flight included (the store skips what it already has). Blocks briefly.
    pub(crate) fn save_all_now(&mut self) {
        if !self.saving_threads() {
            return;
        }
        let saves: Vec<_> = self
            .threads
            .iter()
            .filter(|t| !t.unloaded && !t.loading && !t.state.items.is_empty())
            .filter(|t| t.saving || !t.unsaved.is_empty() || t.meta.is_none())
            .map(|t| ThreadSave {
                id: t.id.clone(),
                meta: self.thread_meta(t),
                first: t.saved,
                events: t.inflight.iter().chain(&t.unsaved).cloned().collect(),
            })
            .collect();
        self.host.save_threads_blocking(saves, Vec::new(), QUIT_SAVE_TIMEOUT);
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
                if std::mem::take(&mut this.last_folder) {
                    this.use_last_folder(&saved, cx);
                }
                let (closed, open): (Vec<_>, Vec<_>) = saved.into_iter().partition(|m| m.closed_ms.is_some());
                this.closed_threads = closed;
                this.purge_closed_threads(cx);
                // Oldest first, like `threads`; before the ones opened at launch.
                for (ix, meta) in open.into_iter().rev().enumerate() {
                    let thread = this.saved_thread(meta);
                    this.threads.insert(ix, thread);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opened without a folder: work where you last did. The folder of the thread used last
    /// (that still exists) becomes the one new threads start in, and the new thread opened at
    /// launch moves there if nothing's been typed in it.
    fn use_last_folder(&mut self, saved: &[ThreadMeta], cx: &mut Context<Self>) {
        let Some(dir) = saved.iter().filter(|m| Path::new(&m.project).is_dir()).max_by_key(|m| m.updated_ms).map(|m| PathBuf::from(&m.project)) else {
            return;
        };
        let home = std::mem::replace(&mut self.project_dir, dir.clone());
        let fresh: Vec<u64> = self.threads.iter().filter(|t| t.is_empty() && t.draft.is_empty() && t.cwd == home).map(|t| t.key).collect();
        for key in fresh {
            self.move_empty_thread(key, dir.clone(), cx);
        }
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
                    Ok((_, events, count)) => {
                        thread.saved = count;
                        thread.error = None;
                        for event in &events {
                            thread.apply(event);
                        }
                        if thread.state.running || thread.state.pending_permission().is_some() {
                            thread.apply(&SessionEvent::turn_ended("cancelled"));
                        }
                        if thread.save_again {
                            this.save_thread(key, cx);
                        }
                    }
                    Err(e) => {
                        // Read again when it's next opened; until then nothing is sent or saved.
                        thread.unloaded = true;
                        thread.error = Some(format!("Couldn't open this thread: {e:#}"));
                    }
                }
                this.transcript_scroll.jump_to_bottom();
                cx.notify();
            });
        })
        .detach();
    }

    /// ⌘W or the row's menu: close a thread, after asking (a thread with nothing in it just
    /// goes).
    pub(crate) fn confirm_close_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        if thread.is_empty() {
            // ⌘W on a new thread: nothing to close (the empty thread would only come back).
            if self.threads.iter().any(|t| t.key != key) {
                self.close_thread(key, window, cx);
            }
            return;
        }
        let title = thread.title();
        let folder = space_ui::format::dir_name(&thread.cwd);
        let mut detail = if self.saving_threads() {
            let days = self.settings.settings.threads.keep_closed_days;
            format!("You can resume it from a new thread in {folder} for {days} {}.", if days == 1 { "day" } else { "days" })
        } else {
            "Threads aren't being saved, so it can't be resumed.".to_string()
        };
        if thread.state.running {
            detail = format!("Its turn is stopped. {detail}");
        }
        let answer = window.prompt(PromptLevel::Warning, &format!("Close \u{201c}{title}\u{201d}?"), Some(&detail), &["Close Thread", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.close_thread(key, window, cx));
            }
        })
        .detach();
    }

    /// ⌘W: close the selected thread.
    pub(crate) fn close_current_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.selected.filter(|_| !self.show_settings) {
            self.confirm_close_thread(key, window, cx);
        }
    }

    /// Close a thread: stop its turn and terminals and take it off the sidebar. A saved thread
    /// is kept, marked closed, to be resumed (until it's older than the setting says).
    pub(crate) fn close_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let keep = self.saving_threads() && self.thread(key).is_some_and(|t| !t.is_empty());
        let Some(thread) = self.drop_thread(key, cx) else { return };
        if keep {
            // A thread whose events haven't been read keeps the summary it was saved with.
            let (meta, events) = if thread.unloaded || thread.loading {
                (thread.meta.clone(), Vec::new())
            } else {
                (Some(self.thread_meta(&thread)), thread.inflight.iter().chain(&thread.unsaved).cloned().collect())
            };
            if let Some(mut meta) = meta {
                let now = now_ms();
                meta.closed_ms = Some(now);
                // Newer than a save still in flight, so that one doesn't reopen it.
                meta.updated_ms = meta.updated_ms.max(now);
                self.closed_threads.push(meta.clone());
                self.purge_closed_threads(cx);
                let (host, first) = (self.host.clone(), thread.saved);
                cx.spawn(async move |this, cx| {
                    if let Err(e) = host.save_thread(&meta.id, Some(meta.clone()), first, &events).await {
                        let _ = this.update(cx, |this, cx| {
                            this.banner = Some(format!("Couldn't save that the thread was closed: {e:#}"));
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
        if self.selected.is_none() {
            match self.threads.last().map(|t| t.key) {
                Some(next) => self.select_inner(next, false, window, cx),
                None => self.new_thread(None, window, cx),
            }
        }
        cx.notify();
    }

    /// Take a thread off the list: stop its turn (answering any open permission card) and its
    /// terminals, and forget it.
    pub(crate) fn drop_thread(&mut self, key: u64, cx: &mut Context<Self>) -> Option<Thread> {
        let ix = self.threads.iter().position(|t| t.key == key)?;
        let thread = self.threads.remove(ix);
        self.close_thread_terminals(key, cx);
        if let Some(session) = &thread.session {
            if session.is_running() {
                let _ = session.cancel();
            }
            self.session_threads.remove(session.id());
        }
        self.back.retain(|k| *k != key);
        self.forward.retain(|k| *k != key);
        if self.selected == Some(key) {
            self.selected = None;
        }
        Some(thread)
    }

    /// The closed threads of `dir` that can be resumed, last closed first.
    pub(crate) fn closed_threads_in(&self, dir: &std::path::Path) -> Vec<&ThreadMeta> {
        let dir = dir.display().to_string();
        let cutoff = self.closed_cutoff();
        let mut closed: Vec<_> = self.closed_threads.iter().filter(|m| m.project == dir && !expired(m, cutoff)).collect();
        closed.sort_by_key(|m| std::cmp::Reverse(m.closed_ms));
        closed
    }

    /// Reopen a closed thread, in place of the new thread it was resumed from (unless that one
    /// has terminals open).
    pub(crate) fn resume_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.closed_threads.iter().position(|m| m.id == id) else { return };
        let mut meta = self.closed_threads.remove(ix);
        meta.closed_ms = None;
        meta.updated_ms = now_ms();
        let host = self.host.clone();
        let saved = meta.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = host.save_thread(&saved.id, Some(saved.clone()), 0, &[]).await {
                let _ = this.update(cx, |this, cx| {
                    this.banner = Some(format!("Couldn't save that the thread was reopened: {e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
        let replace = self.selected.filter(|k| self.thread(*k).is_some_and(Thread::is_empty) && !self.has_terminals(*k));
        let at = replace.and_then(|k| self.threads.iter().position(|t| t.key == k)).unwrap_or(self.threads.len());
        let thread = self.saved_thread(meta);
        let key = thread.key;
        self.threads.insert(at, thread);
        if let Some(old) = replace {
            self.drop_thread(old, cx);
        }
        self.select_thread(key, window, cx);
    }

    /// A thread for a saved summary, its events read when it's opened.
    fn saved_thread(&mut self, meta: ThreadMeta) -> Thread {
        let key = self.next_key;
        self.next_key += 1;
        let mut thread = Thread::new(key, PathBuf::from(&meta.project));
        thread.id.clone_from(&meta.id);
        thread.created_ms = meta.created_ms;
        thread.mode.clone_from(&meta.mode);
        thread.resume = meta.agent.clone().zip(meta.session_id.clone());
        thread.name = (meta.titled || meta.renamed).then(|| meta.title.clone());
        thread.renamed = meta.renamed;
        thread.pinned_ms = meta.pinned_ms;
        thread.unloaded = true;
        thread.meta = Some(meta);
        thread
    }

    /// Delete the closed threads kept longer than the setting says.
    pub(crate) fn purge_closed_threads(&mut self, cx: &mut Context<Self>) {
        let cutoff = self.closed_cutoff();
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.closed_threads).into_iter().partition(|m| expired(m, cutoff));
        self.closed_threads = kept;
        for meta in gone {
            self.remove_saved(meta.id, cx);
        }
    }

    /// Threads closed before this (Unix ms) are past keeping.
    fn closed_cutoff(&self) -> u64 {
        let days = u64::from(self.settings.settings.threads.keep_closed_days.max(1));
        now_ms().saturating_sub(days * DAY_MS)
    }
}

const DAY_MS: u64 = 24 * 3600 * 1000;

/// A closed thread kept past `cutoff`.
fn expired(meta: &ThreadMeta, cutoff: u64) -> bool {
    meta.closed_ms.is_some_and(|closed| closed < cutoff)
}

impl SpaceApp {
    /// Delete what was saved of thread `id`.
    fn remove_saved(&mut self, id: String, cx: &mut Context<Self>) {
        let host = self.host.clone();
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
}

#[cfg(test)]
mod tests {
    use space_core::SessionState;
    use space_core::protocol::SpaceStatus;

    use super::*;

    #[test]
    fn a_rename_is_saved_as_yours() {
        let meta = ThreadMeta { title: "Mine".into(), titled: true, renamed: true, ..Default::default() };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["renamed"], true);
        assert_eq!(serde_json::from_value::<ThreadMeta>(json).unwrap(), meta);
        // Pinned: written, and read back.
        let pinned = ThreadMeta { pinned_ms: Some(42), ..Default::default() };
        let json = serde_json::to_value(&pinned).unwrap();
        assert_eq!(json["pinnedMs"], 42);
        assert_eq!(serde_json::from_value::<ThreadMeta>(json).unwrap().pinned_ms, Some(42));
        // Saved before renaming existed (or never renamed): not renamed, and not written.
        let json = serde_json::to_value(ThreadMeta::default()).unwrap();
        assert!(json.get("renamed").is_none());
        let old = serde_json::from_value::<ThreadMeta>(serde_json::json!({"id": "a", "title": "t", "project": "/p", "createdMs": 1, "updatedMs": 2})).unwrap();
        assert!(!old.renamed);
        assert_eq!(old.pinned_ms, None);
        assert!(serde_json::to_value(&old).unwrap().get("pinnedMs").is_none());
    }

    #[test]
    fn closed_threads_expire_after_the_cutoff() {
        let closed = |at: Option<u64>| ThreadMeta { closed_ms: at, ..Default::default() };
        let cutoff = 100 * DAY_MS;
        assert!(expired(&closed(Some(cutoff - 1)), cutoff));
        assert!(!expired(&closed(Some(cutoff)), cutoff));
        assert!(!expired(&closed(None), cutoff), "open threads never expire");
    }

    #[test]
    fn chunks_merge_and_holds_are_left_out() {
        let mut unsaved = Vec::new();
        let events = [
            SessionEvent::UserMessage { text: "hi".into(), quote: None, attachments: vec![], images: vec![], texts: vec![], at_ms: None },
            SessionEvent::TurnStarted,
            SessionEvent::AgentThought { text: "a".into() },
            SessionEvent::AgentThought { text: "b".into() },
            // A hold isn't saved; the agent's report of the call is.
            SessionEvent::ToolCallHeld { id: "t".into(), title: "ls".into(), reason: Some("down".into()) },
            SessionEvent::Retry(Some(space_core::protocol::RetryState {
                kind: space_core::protocol::RetryKind::Offline,
                attempt: 1,
                reason: "ECONNRESET".into(),
            })),
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

    #[test]
    fn thread_ids_are_folder_names() {
        let (a, b) = (new_thread_id(), new_thread_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
