//! Saving and reopening threads, through the host's session store (`session-store` plugin).
//!
//! A thread is saved as its summary ([`ThreadMeta`]) and its events: the user's messages, the
//! agent's finished messages and thoughts (chunks merged), tool calls and results, permission
//! and guardrail decisions, status (model) changes and turn starts and ends. The host saves the
//! events itself as they happen (`space_core::host::saving`), so a turn is kept even while no
//! app runs; the app hands it the summary (when a turn ends, on a rename, pin, mode change or
//! Done, and when Space quits) and the events it makes itself (a new session, going back). A
//! failed write is tried again and says so in the banner.
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
            // A done thread opened to read stays done whatever saves it.
            done_ms: thread.done_ms,
            pinned_ms: thread.pinned_ms,
            side_of: thread.side_of.and_then(|main| self.thread(main)).map(|t| t.id.clone()),
        }
    }

    /// Write the thread's waiting events and its summary (once it has something to save). One
    /// save at a time per thread: asked for during one (or while the thread loads), it follows.
    pub(crate) fn save_thread(&mut self, key: u64, _cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.unloaded {
            return;
        }
        if thread.loading {
            thread.save_again = true;
            return;
        }
        if thread.state.items.is_empty() && thread.meta.is_none() {
            return;
        }
        let meta = self.thread_meta(self.thread(key).expect("thread exists"));
        self.host.save_thread_meta(meta.clone());
        if let Some(thread) = self.thread_mut(key) {
            thread.meta = Some(meta);
        }
    }

    /// Before quitting: every thread's summary as it is now, and everything the host has
    /// waiting written. Blocks briefly.
    pub(crate) fn save_all_now(&mut self) {
        if !self.saving_threads() {
            return;
        }
        let metas: Vec<_> = self
            .threads
            .iter()
            .chain(self.side_chats.values().map(|s| &s.thread))
            .filter(|t| !t.unloaded && !t.loading && !t.state.items.is_empty())
            .map(|t| self.thread_meta(t))
            .collect();
        for meta in metas {
            self.host.save_thread_meta(meta);
        }
        self.host.flush_saves_blocking(QUIT_SAVE_TIMEOUT);
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
                // Drafts of threads that are gone go too.
                this.drafts.keep_only(&saved.iter().map(|m| m.id.as_str()).collect());
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
        let fresh: Vec<u64> = self.threads.iter().filter(|t| t.is_empty() && !t.has_draft() && t.cwd == home).map(|t| t.key).collect();
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
                    Ok((_, events, _)) => {
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
        // A done thread opened to read is done already.
        let Some(thread) = self.thread(key).filter(|t| t.done_ms.is_none()) else { return };
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
            let meta = if thread.unloaded || thread.loading { thread.meta.clone() } else { Some(self.thread_meta(&thread)) };
            if let Some(mut meta) = meta {
                let now = now_ms();
                meta.done_ms = Some(now);
                // Newer than a save still in flight, so that one doesn't reopen it.
                meta.updated_ms = meta.updated_ms.max(now);
                self.done_threads.push(meta.clone());
                self.purge_done_threads(cx);
                self.host.save_thread_meta(meta);
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
        // Its draft waits for it to be opened again (while it's still the shown one, if it is).
        self.keep_draft(&thread, cx);
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

    /// Open a done thread to read: it stays done, listed under Done (its row selected), and
    /// goes when you leave it. Sending a message in it reopens it (`reopen_done`).
    pub(crate) fn view_done_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.threads.iter().find(|t| t.id == id).map(|t| t.key) {
            self.select_thread(key, window, cx);
            return;
        }
        let Some(meta) = self.done_threads.iter().find(|m| m.id == id).cloned() else { return };
        let mut thread = self.saved_thread(meta);
        thread.done_ms = thread.meta.as_ref().and_then(|m| m.done_ms);
        let key = thread.key;
        self.threads.push(thread);
        self.select_thread(key, window, cx);
    }

    /// The row's reopen button, or ⌘Z after marking it done: reopen a done thread and show it.
    pub(crate) fn reopen_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.reopen_done(id, cx) {
            self.select_thread(key, window, cx);
        }
    }

    /// Reopen done thread `id`: it goes back to its place in the sidebar (the list is in the
    /// order threads were made), scrolled into view, and that it's open is saved. The thread
    /// opened to read, if it's that one, is the one reopened. Its key, or `None` if it isn't done.
    pub(crate) fn reopen_done(&mut self, id: &str, cx: &mut Context<Self>) -> Option<u64> {
        let ix = self.done_threads.iter().position(|m| m.id == id)?;
        let mut meta = self.done_threads.remove(ix);
        let viewed = self.threads.iter().position(|t| t.id == id);
        // Newer than the save that marked it done, and than any save made while it was read,
        // even one still in flight.
        let last = viewed.and_then(|ix| self.threads[ix].meta.as_ref()).map_or(meta.updated_ms, |m| m.updated_ms.max(meta.updated_ms));
        meta.done_ms = None;
        meta.updated_ms = now_ms().max(last + 1);
        self.host.save_thread_meta(meta.clone());
        let key = match viewed {
            Some(ix) => {
                let thread = &mut self.threads[ix];
                thread.done_ms = None;
                thread.meta = Some(meta);
                thread.key
            }
            None => {
                let thread = self.saved_thread(meta);
                let key = thread.key;
                self.threads.push(thread);
                key
            }
        };
        if let Some(row) = self.listed_threads().iter().position(|k| *k == key) {
            self.sidebar_scroll.scroll_to_item(row);
        }
        cx.notify();
        Some(key)
    }

    /// The id of the done thread open to read, if that's what's shown.
    pub(crate) fn reading_done(&self) -> Option<String> {
        self.current().filter(|t| t.done_ms.is_some() && !self.show_settings).map(|t| t.id.clone())
    }

    /// ⌘⌫: delete the done thread being read (nothing anywhere else).
    pub(crate) fn delete_current_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.reading_done() {
            self.delete_done(id, window, cx);
        }
    }

    /// A done row's menu, or ⌘⌫ while reading it: delete done thread `id` for good, once you
    /// confirm (there's no undo).
    pub(crate) fn delete_done(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meta) = self.done_threads.iter().find(|m| m.id == id) else { return };
        let title = &meta.title;
        let answer = window.prompt(PromptLevel::Warning, &format!("Delete \u{201c}{title}\u{201d}?"), Some("This can't be undone."), &["Delete", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.delete_done_now(&id, window, cx));
            }
        })
        .detach();
    }

    /// Delete done thread `id`: off the Done list, its saved files (and its side chat's) gone.
    /// Read when it went, it's left as any thread being read is, for the thread you came from
    /// (or the last one listed, or a new one).
    pub(crate) fn delete_done_now(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.done_threads.iter().position(|m| m.id == id) else { return };
        self.done_threads.remove(ix);
        // Marked done just now: nothing is left to undo.
        if self.rewind.done.as_ref().is_some_and(|d| d.id == id) {
            self.rewind.done = None;
            self.rewind.notice = self.rewind.notice.take().filter(|n| n.kind != crate::rewind::NoticeKind::Done);
        }
        if let Some(key) = self.threads.iter().find(|t| t.id == id).map(|t| t.key) {
            self.drop_thread(key, cx);
            if self.selected.is_none() {
                // The thread you came from (taken off Back, so Back doesn't lead to it again),
                // else the last one listed, else a new one.
                let mut back = None;
                while let Some(k) = self.back.pop() {
                    if self.thread(k).is_some() {
                        back = Some(k);
                        break;
                    }
                }
                match back.or_else(|| self.threads.last().map(|t| t.key)) {
                    Some(next) => self.select_inner(next, false, window, cx),
                    None => self.new_thread(None, window, cx),
                }
            }
        }
        self.remove_saved(id.to_string(), cx);
        cx.notify();
    }

    /// The composer's placeholder says sending reopens the thread while a done one is open to read.
    pub(crate) fn sync_composer_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reopens = self.current().is_some_and(|t| t.done_ms.is_some());
        if reopens != self.composer_reopens {
            self.composer_reopens = reopens;
            use space_ui::components::composer::{PLACEHOLDER, REOPEN_PLACEHOLDER};
            self.input.update(cx, |s, cx| s.set_placeholder(if reopens { REOPEN_PLACEHOLDER } else { PLACEHOLDER }, window, cx));
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
        if let Some(draft) = self.drafts.take(&meta.id) {
            crate::drafts::restore(&mut thread, draft, &mut self.next_image_id);
        }
        thread.meta = Some(meta);
        thread
    }

    /// Delete the done threads kept longer than the setting says.
    pub(crate) fn purge_done_threads(&mut self, cx: &mut Context<Self>) {
        let Some(cutoff) = self.done_cutoff() else { return };
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.done_threads).into_iter().partition(|m| expired(m, cutoff));
        self.done_threads = kept;
        for meta in gone {
            self.remove_saved(meta.id, cx);
        }
    }

    /// Threads marked done before this (Unix ms) are past keeping; `None`: they're kept.
    pub(crate) fn done_cutoff(&self) -> Option<u64> {
        let days = u64::from(self.settings.settings.threads.keep_done()?);
        Some(now_ms().saturating_sub(days * DAY_MS))
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
        self.drafts.forget(&id);
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
    fn thread_ids_are_folder_names() {
        let (a, b) = (new_thread_id(), new_thread_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
