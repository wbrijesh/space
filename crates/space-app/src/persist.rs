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
//!
//! A side chat is saved the same way, its summary naming the thread it's beside (`side_of`): it
//! isn't listed, opens with that thread, and is deleted with it.

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

    pub(crate) fn thread_meta(&self, thread: &Thread) -> ThreadMeta {
        let state = &thread.state;
        let session_id = thread.session.as_ref().map(|s| s.id().to_string());
        ThreadMeta {
            id: thread.id.clone(),
            title: thread.title(),
            project: thread.cwd.display().to_string(),
            mode: self.effective_mode(thread),
            // Not over what was saved while the thread (or its status) is still on its way.
            model: state.status.model.clone().or_else(|| thread.meta.as_ref().and_then(|m| m.model.clone())),
            session_id: session_id.or_else(|| thread.resume.clone()),
            created_ms: thread.created_ms,
            updated_ms: now_ms(),
            titled: thread.name.is_some(),
            renamed: thread.renamed,
            done_ms: None,
            pinned_ms: thread.pinned_ms,
            side_of: thread.side_of.and_then(|main| self.thread(main)).map(|t| t.id.clone()),
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
                // Marked done while this save was in flight (marking it wrote it).
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
            .chain(self.side_chats.values().map(|s| &s.thread))
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
                // Side chats wait for their thread to open them.
                let (sides, saved): (Vec<_>, Vec<_>) = saved.into_iter().partition(|m| m.side_of.is_some());
                for m in sides {
                    if let Some(main) = m.side_of.clone() {
                        this.saved_sides.entry(main).or_default().push(m);
                    }
                }
                let (done, open): (Vec<_>, Vec<_>) = saved.into_iter().partition(|m| m.done_ms.is_some());
                this.done_threads = done;
                this.purge_done_threads(cx);
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

    /// ⌘W, the row's archive button or its menu: mark a thread done. It leaves the sidebar at
    /// once, and the notice offers Undo. Only when threads aren't saved (it couldn't be reopened)
    /// does it ask first. A thread with nothing in it just goes.
    pub(crate) fn mark_done(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        if thread.is_empty() {
            // ⌘W on a new thread: nothing to keep (the empty thread would only come back).
            if self.threads.iter().any(|t| t.key != key) {
                self.finish_thread(key, window, cx);
            }
            return;
        }
        let title = thread.title();
        let running = thread.state.running;
        if !self.saving_threads() {
            let detail = if running { "Its turn stops · Can't be reopened" } else { "Can't be reopened" };
            let answer = window.prompt(PromptLevel::Warning, &format!("Mark \u{201c}{title}\u{201d} as done?"), Some(detail), &["Mark as Done", "Cancel"], cx);
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    let _ = this.update_in(cx, |this, window, cx| this.finish_thread(key, window, cx));
                }
            })
            .detach();
            return;
        }
        let id = thread.id.clone();
        let terminals = self.has_terminals(key);
        self.finish_thread(key, window, cx);
        let stopped = match (running, terminals) {
            (true, true) => Some("Its turn and terminals were stopped."),
            (true, false) => Some("Its turn was stopped."),
            (false, true) => Some("Its terminals were closed."),
            (false, false) => None,
        };
        self.done_notice(id, title, stopped, cx);
    }

    /// ⌘W: mark the selected thread done.
    pub(crate) fn mark_current_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.selected.filter(|_| !self.show_settings) {
            self.mark_done(key, window, cx);
        }
    }

    /// Done with a thread: stop its turn and terminals and take it off the sidebar. A saved
    /// thread is kept, marked done, to be reopened (until it's older than the setting says).
    pub(crate) fn finish_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
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
                meta.done_ms = Some(now);
                // Newer than a save still in flight, so that one doesn't reopen it.
                meta.updated_ms = meta.updated_ms.max(now);
                self.done_threads.push(meta.clone());
                self.purge_done_threads(cx);
                let (host, first) = (self.host.clone(), thread.saved);
                cx.spawn(async move |this, cx| {
                    if let Err(e) = host.save_thread(&meta.id, Some(meta.clone()), first, &events).await {
                        let _ = this.update(cx, |this, cx| {
                            this.banner = Some(format!("Couldn't save that the thread is done: {e:#}"));
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
        let id = self.threads.iter().find(|t| t.key == key)?.id.clone();
        // While the thread is still there: the side chat's summary names it.
        self.put_away_side_chats(key, &id, cx);
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

    /// Reopen a done thread and show it. It goes back to its place in the sidebar (the list is
    /// in the order threads were made), scrolled into view.
    pub(crate) fn reopen_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.done_threads.iter().position(|m| m.id == id) else { return };
        let mut meta = self.done_threads.remove(ix);
        meta.done_ms = None;
        // Newer than the save that marked it done, even one still in flight.
        meta.updated_ms = now_ms().max(meta.updated_ms + 1);
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
        let thread = self.saved_thread(meta);
        let key = thread.key;
        self.threads.push(thread);
        self.select_thread(key, window, cx);
        if let Some(row) = self.listed_threads().iter().position(|k| *k == key) {
            self.sidebar_scroll.scroll_to_item(row);
        }
    }

    /// A thread for a saved summary, its events read when it's opened.
    pub(crate) fn saved_thread(&mut self, meta: ThreadMeta) -> Thread {
        let key = self.next_key;
        self.next_key += 1;
        let mut thread = Thread::new(key, PathBuf::from(&meta.project));
        thread.id.clone_from(&meta.id);
        thread.created_ms = meta.created_ms;
        thread.mode.clone_from(&meta.mode);
        thread.resume = meta.session_id.clone();
        thread.name = (meta.titled || meta.renamed).then(|| meta.title.clone());
        thread.renamed = meta.renamed;
        thread.pinned_ms = meta.pinned_ms;
        thread.unloaded = true;
        thread.meta = Some(meta);
        thread
    }

    /// Delete the done threads kept longer than the setting says.
    pub(crate) fn purge_done_threads(&mut self, cx: &mut Context<Self>) {
        let cutoff = self.done_cutoff();
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.done_threads).into_iter().partition(|m| expired(m, cutoff));
        self.done_threads = kept;
        for meta in gone {
            self.remove_saved(meta.id, cx);
        }
    }

    /// Threads marked done before this (Unix ms) are past keeping.
    pub(crate) fn done_cutoff(&self) -> u64 {
        let days = u64::from(self.settings.settings.threads.keep_done_days.max(1));
        now_ms().saturating_sub(days * DAY_MS)
    }
}

const DAY_MS: u64 = 24 * 3600 * 1000;

/// A done thread kept past `cutoff`.
pub(crate) fn expired(meta: &ThreadMeta, cutoff: u64) -> bool {
    meta.done_ms.is_some_and(|done| done < cutoff)
}

impl SpaceApp {
    /// Delete what was saved of thread `id`, and of its side chat.
    pub(crate) fn remove_saved(&mut self, id: String, cx: &mut Context<Self>) {
        for side in self.saved_sides.remove(&id).unwrap_or_default() {
            self.remove_saved(side.id, cx);
        }
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
    fn threads_closed_before_done_existed_read_as_done() {
        let old = serde_json::from_value::<ThreadMeta>(serde_json::json!({"id": "a", "title": "t", "project": "/p", "createdMs": 1, "updatedMs": 2, "closedMs": 3})).unwrap();
        assert_eq!(old.done_ms, Some(3));
        assert_eq!(serde_json::to_value(&old).unwrap()["doneMs"], 3);
    }

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
    fn done_threads_expire_after_the_cutoff() {
        let closed = |at: Option<u64>| ThreadMeta { done_ms: at, ..Default::default() };
        let cutoff = 100 * DAY_MS;
        assert!(expired(&closed(Some(cutoff - 1)), cutoff));
        assert!(!expired(&closed(Some(cutoff)), cutoff));
        assert!(!expired(&closed(None), cutoff), "open threads never expire");
    }

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

    #[test]
    fn thread_ids_are_folder_names() {
        let (a, b) = (new_thread_id(), new_thread_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
