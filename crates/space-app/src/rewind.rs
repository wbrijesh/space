//! Going back to an earlier message of yours.
//!
//! ✎ Edit turns its bubble into a text field; ⏎ sends the edited message in its place. ↺ Rewind
//! goes back to just before it, and its text (with its pictures, pasted texts, files and quote)
//! goes back into the composer. Either way the message and everything after it leave the thread,
//! and the files Claude's edits changed since go back as they were then. A file you changed after
//! Claude did stays as you left it. ⌘Z (or Undo in the notice) brings the messages and the files
//! back, until you send another message. Esc Esc in an empty composer picks a message from the
//! keyboard (↑/↓, ⏎ edits, ⌘⏎ rewinds).
//!
//! The agent's conversation isn't touched until the next message goes out: that starts a new
//! session from a copy of the conversation up to the message ([`SessionState::fork`]), and the old
//! one stays as it was, for Undo. Files are put back by the agent at once (Claude Code's own file
//! checkpoints), so the thread's session is loaded first if it isn't.
//!
//! [`SessionState::fork`]: space_core::SessionState::fork

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::component::input::{InputEvent, TextareaState};
use gpui::*;
use sha2::{Digest, Sha256};
use space_client::{Session};
use space_core::{ImageRef, SessionEvent, TranscriptItem};

use crate::app::SpaceApp;
use crate::queue::{Pending as Queued, When};
use crate::threads::{Outgoing, Thread};

/// How long a notice stays (one listing files stays until it's closed).
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// The most the second Esc of Esc Esc may follow the first.
const ESC_ESC: Duration = Duration::from_millis(500);

/// What undoing the last rewind needs.
pub(crate) struct UndoEntry {
    /// The session the thread was in (kept alive while it can be undone).
    session: Option<Session>,
    session_id: String,
    /// The files the agent put back, as they were just before (`None`: there was no such file).
    backup: Vec<(PathBuf, Option<Vec<u8>>)>,
    /// The edited message sent in the rewound one's place (its turn's files go back too).
    resent: Option<String>,
    /// The composer as it was before the rewound message went into it, and what it put there.
    composer: Option<ComposerBefore>,
}

struct ComposerBefore {
    text: String,
    attachments: usize,
    images: usize,
    pastes: usize,
    quote: Option<String>,
    /// The composer's text right after the rewind.
    rewound: String,
}

/// Something that waits for the running turn to stop (or the session to load).
pub(crate) enum Waiting {
    Rewind { message_id: String, edit: Option<Outgoing> },
    Undo,
}

/// One of your messages being edited in place.
pub(crate) struct Editor {
    pub(crate) key: u64,
    pub(crate) message_id: String,
    pub(crate) input: Entity<TextareaState>,
    _events: Subscription,
}

/// The notice in the window's corner.
pub(crate) struct Notice {
    pub(crate) id: u64,
    pub(crate) key: u64,
    pub(crate) kind: NoticeKind,
    pub(crate) title: String,
    pub(crate) body: Option<String>,
    /// Files left as they are, and why.
    pub(crate) kept: Vec<(String, &'static str)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Rewound,
    Edited,
    Problem,
    /// A thread was marked done.
    Done,
}

/// A thread just marked done, which ⌘Z (or the notice's Undo) reopens.
pub(crate) struct DoneUndo {
    pub(crate) id: String,
    /// The thread shown after it went: ⌘Z reopens it only while that one is still shown.
    pub(crate) after: Option<u64>,
}

#[derive(Default)]
pub(crate) struct RewindUi {
    pub(crate) notice: Option<Notice>,
    next_notice: u64,
    /// The thread last marked done, until something else can be undone or you move on.
    pub(crate) done: Option<DoneUndo>,
    /// Rewind mode: the thread and the message picked.
    pub(crate) mode: Option<(u64, String)>,
    esc_at: Option<Instant>,
    pub(crate) editor: Option<Editor>,
    /// The agent can go back to an earlier message (known once it has started; kept while it
    /// sleeps).
    pub(crate) agent_can: bool,
}

/// A sha256 of a file's contents (`None`: no such file).
pub(crate) fn file_hash(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// The message at `ix` as it would be sent again: its text, quote, files, pictures and pasted texts.
fn outgoing_of(item: &TranscriptItem) -> Option<Outgoing> {
    let TranscriptItem::User { text, quote, attachments, images, texts, .. } = item else { return None };
    let images = images.iter().filter_map(prepared_of).collect();
    let pastes = texts.iter().filter_map(|t| std::fs::read_to_string(&t.path).ok()).map(crate::paste::Paste::new).collect();
    Some(Outgoing { text: text.clone(), quote: quote.clone(), attachments: attachments.clone(), images, pastes, id: String::new() })
}

/// A sent picture, ready to send again (it's saved under its sha256 already).
fn prepared_of(image: &ImageRef) -> Option<crate::images::Prepared> {
    let bytes = std::fs::read(&image.path).ok()?;
    let mime = if image.mime == "image/png" { "image/png" } else { "image/jpeg" };
    let sha256 = image.path.file_stem()?.to_string_lossy().into_owned();
    Some(crate::images::Prepared { bytes: bytes.into(), mime, width: image.width, height: image.height, sha256, name: image.name.clone() })
}

/// The files a subagent's edits wrote at or after `from` (Claude Code's checkpoints don't cover them).
fn subagent_files(items: &[TranscriptItem], from: usize) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    items[from..]
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Tool(t) if t.subagent && t.kind == "edit" => Some(t.locations.iter()),
            _ => None,
        })
        .flatten()
        .map(PathBuf::from)
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// `path` as the notice shows it: from the thread's folder when it's in it.
fn shown_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

impl SpaceApp {
    /// The transcript indices of `thread`'s messages that can be edited and rewound to.
    pub(crate) fn rewindable(&self, thread: &Thread) -> HashSet<usize> {
        if !self.rewind.agent_can || thread.side_of.is_some() || thread.loading || thread.unloaded || thread.queued.is_some() || thread.rewinding {
            return HashSet::new();
        }
        (0..thread.state.items.len()).filter(|&ix| thread.state.can_rewind_to(ix)).collect()
    }

    /// The message at `ix` of the selected thread, if it can be gone back to: its id.
    fn rewind_target(&self, ix: usize) -> Option<(u64, String)> {
        let key = self.selected?;
        let thread = self.thread(key)?;
        if !self.rewindable(thread).contains(&ix) {
            return None;
        }
        match &thread.state.items[ix] {
            TranscriptItem::User { id: Some(id), .. } => Some((key, id.clone())),
            _ => None,
        }
    }

    /// ✎: edit the message at `ix` in place.
    pub(crate) fn start_edit(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, message_id)) = self.rewind_target(ix) else { return };
        let Some(TranscriptItem::User { text, .. }) = self.thread(key).and_then(|t| t.state.items.get(ix)).cloned() else { return };
        self.rewind.mode = None;
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("Edit your message");
            state.set_auto_grow(1, space_ui::components::composer::MAX_LINES, cx);
            state.set_submit_on_enter(true, cx);
            state.set_value(text, window, cx);
            state
        });
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { shift: false, .. } => this.confirm_edit(window, cx),
            InputEvent::Change | InputEvent::Focus | InputEvent::Blur => cx.notify(),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.rewind.editor = Some(Editor { key, message_id, input, _events: events });
        cx.notify();
    }

    /// Esc in the editor: the message stays as it was.
    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewind.editor.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    /// ⏎ in the editor: back to just before the message, and the edited one goes out in its place.
    pub(crate) fn confirm_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.rewind.editor.take() else { return };
        let text = editor.input.read(cx).value().trim().to_string();
        let item = self.thread(editor.key).and_then(|t| t.state.message_index(&editor.message_id).map(|ix| t.state.items[ix].clone()));
        let Some(mut message) = item.as_ref().and_then(outgoing_of) else { return };
        message.text = text;
        if message.text.is_empty() && message.attachments.is_empty() && message.images.is_empty() && message.pastes.is_empty() && message.quote.is_none() {
            self.focus_composer(window, cx);
            return;
        }
        message.id = space_core::new_message_id();
        self.focus_composer(window, cx);
        self.rewind_to(editor.key, editor.message_id, Some(message), cx);
    }

    /// ↺: back to just before the message at `ix`.
    pub(crate) fn rewind_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, message_id)) = self.rewind_target(ix) else { return };
        self.rewind.mode = None;
        self.rewind.editor = None;
        self.focus_composer(window, cx);
        self.rewind_to(key, message_id, None, cx);
    }

    /// Go back to just before message `message_id` of thread `key`; with `edit`, send that in its
    /// place. A running turn is stopped first, and the thread's session loaded if it isn't.
    pub(crate) fn rewind_to(&mut self, key: u64, message_id: String, edit: Option<Outgoing>, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.rewinding || thread.waiting.is_some() {
            return;
        }
        thread.rewinding = true;
        thread.waiting = Some(Waiting::Rewind { message_id, edit });
        self.run_waiting(key, cx);
    }

    /// ⌘Z or the notice's Undo: undo the thread's last rewind.
    pub(crate) fn undo_rewind(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.undo.is_empty() || thread.rewinding || thread.waiting.is_some() {
            return;
        }
        thread.rewinding = true;
        thread.waiting = Some(Waiting::Undo);
        self.run_waiting(key, cx);
    }

    /// Go on with what waits on thread `key` once its turn has stopped and its session is up
    /// (called again when either happens).
    pub(crate) fn run_waiting(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        if thread.waiting.is_none() {
            return;
        }
        // A running turn stops first (its queued messages go back into the composer, as Stop does).
        if thread.state.running {
            if let Some(session) = thread.session.clone()
                && let Err(e) = session.cancel()
            {
                tracing::warn!("stopping the turn to go back: {e:#}");
            }
            return;
        }
        let undo = matches!(thread.waiting, Some(Waiting::Undo));
        // Undoing an edit puts back what its turn wrote: in the session it went to.
        let session = match (undo, thread.undo.last()) {
            (true, Some(entry)) if entry.resent.is_some() && thread.state.fork.is_none() => thread.live_session().cloned(),
            (true, _) => None,
            (false, _) => thread.live_session().cloned(),
        };
        if session.is_none() && !undo {
            // The agent is asleep (or the thread wasn't loaded): load the session, then go on.
            if self.agent.as_ref().is_none_or(|a| !a.is_running()) || !self.thread(key).is_some_and(|t| t.creating) {
                let Some(thread) = self.thread_mut(key) else { return };
                if let Some(old) = thread.session.take() {
                    thread.resume = Some(old.id().to_string());
                }
                if thread.resume.is_none() {
                    thread.resume = thread.state.fork.as_ref().map(|f| f.session_id.clone());
                }
                if !thread.creating {
                    self.create_session(key, cx);
                }
            }
            return;
        }
        let Some(thread) = self.thread_mut(key) else { return };
        match thread.waiting.take() {
            Some(Waiting::Rewind { message_id, edit }) => self.rewind_now(key, message_id, edit, session.expect("checked"), cx),
            Some(Waiting::Undo) => self.undo_now(key, session, cx),
            None => {}
        }
    }

    /// The thread's session is up and nothing runs: ask the agent, back up and put back the
    /// files, then cut the transcript.
    fn rewind_now(&mut self, key: u64, message_id: String, edit: Option<Outgoing>, session: Session, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let Some(at) = thread.state.message_index(&message_id) else {
            self.rewind_failed(key, "That message isn't in the thread any more.".into(), cx);
            return;
        };
        let steered = matches!(thread.state.items[at], TranscriptItem::User { steered: true, .. });
        let written = thread.state.written.clone();
        let subagent = subagent_files(&thread.state.items, at);
        let cwd = thread.cwd.clone();
        cx.spawn(async move |this, cx| {
            let preview = match session.rewind(&message_id, false).await {
                Ok(p) => p,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.rewind_failed(key, format!("{e:#}"), cx));
                    return;
                }
            };
            if !preview.found {
                let _ = this.update(cx, |this, cx| this.rewind_failed(key, "Claude's conversation doesn't have it any more.".into(), cx));
                return;
            }
            let changed: Vec<PathBuf> = preview.changed.iter().map(PathBuf::from).collect();
            let restore = preview.files && !changed.is_empty();
            // Each file as it is now (for Undo), and the ones you changed since Claude last
            // wrote them (they stay as you left them).
            let (backup, yours) = cx
                .background_spawn({
                    let changed = changed.clone();
                    async move {
                        let backup: Vec<(PathBuf, Option<Vec<u8>>)> = changed.iter().map(|p| (p.clone(), std::fs::read(p).ok())).collect();
                        let yours: Vec<PathBuf> = backup
                            .iter()
                            .filter(|(p, bytes)| {
                                let now = bytes.as_ref().map(|b| Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>());
                                written.get(p).is_some_and(|last| *last != now)
                            })
                            .map(|(p, _)| p.clone())
                            .collect();
                        (backup, yours)
                    }
                })
                .await;
            let mut problem = None;
            let mut skipped = 0;
            if restore {
                match session.rewind(&message_id, true).await {
                    Ok(r) if r.files => skipped = r.skipped.unwrap_or(0),
                    Ok(r) => problem = Some(r.reason.unwrap_or_else(|| "Claude couldn't put the files back.".into())),
                    Err(e) => problem = Some(format!("{e:#}")),
                }
                if problem.is_none() {
                    // Yours go back to how you left them.
                    let keep: Vec<(PathBuf, Option<Vec<u8>>)> = backup.iter().filter(|(p, _)| yours.contains(p)).cloned().collect();
                    cx.background_spawn(async move { put_back(&keep) }).await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(reason) = problem {
                    // Nothing changed: say why, and stay.
                    this.rewind_failed(key, format!("Claude couldn't put the files back: {reason}"), cx);
                    return;
                }
                let restored = if restore { changed.len().saturating_sub(yours.len()) } else { 0 };
                let mut kept: Vec<(String, &'static str)> = yours.iter().map(|p| (shown_path(p, &cwd), "you changed it since")).collect();
                kept.extend(subagent.iter().filter(|p| !changed.contains(p)).map(|p| (shown_path(p, &cwd), "written by a subagent")));
                let _ = skipped;
                let body = match (restored > 0, preview.files, steered) {
                    (true, _, _) if edit.is_some() => Some("Claude's file changes after it were undone.".to_string()),
                    (true, _, _) => Some("Claude's file changes after this point were undone.".to_string()),
                    (false, false, true) => Some("Files are as they were: the message went in while Claude was working.".to_string()),
                    _ => None,
                };
                let backup = if restore { backup } else { Vec::new() };
                this.finish_rewind(key, message_id, edit, backup, NoticeText { body, kept }, cx);
            });
        })
        .detach();
    }

    /// Cut the transcript, keep what Undo needs, and send the edit (or put the message back into
    /// the composer).
    fn finish_rewind(&mut self, key: u64, message_id: String, edit: Option<Outgoing>, backup: Vec<(PathBuf, Option<Vec<u8>>)>, text: NoticeText, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let Some(at) = thread.state.message_index(&message_id) else { return };
        let session = thread.session.clone();
        let session_id = session.as_ref().map(|s| s.id().to_string()).or_else(|| thread.resume.clone()).unwrap_or_default();
        let rewound = thread.state.items.get(at).and_then(outgoing_of);
        self.emit_local(key, SessionEvent::Rewound { message_id, session_id: session_id.clone() }, cx);
        let selected = self.selected == Some(key);
        let Some(thread) = self.thread_mut(key) else { return };
        thread.rewinding = false;
        // What was shown by transcript index past the cut is gone.
        thread.expanded.retain(|ix| *ix < at);
        thread.open_turns.retain(|ix| *ix < at);
        thread.forget_thoughts_from(at);
        thread.recall.end();
        let resent = edit.as_ref().map(|m| m.id.clone());
        thread.undo.push(UndoEntry { session, session_id, backup, resent, composer: None });
        if selected {
            self.replies = Default::default();
            self.copied = None;
            self.usage_open = None;
            self.transcript_scroll.jump_to_bottom();
        }
        let kind = if edit.is_some() { NoticeKind::Edited } else { NoticeKind::Rewound };
        match edit {
            Some(message) => self.dispatch(key, When::Done, false, message, cx),
            None => {
                if let Some(thread) = self.thread_mut(key) {
                    thread.to_composer = rewound;
                }
            }
        }
        let title = if kind == NoticeKind::Edited { "Edited." } else { "Rewound." };
        self.show_notice(key, kind, title.into(), text.body, text.kept, cx);
        self.refresh_after_files(key, cx);
        cx.notify();
    }

    /// Thread `key` couldn't go back: nothing changed; the notice says why.
    fn rewind_failed(&mut self, key: u64, reason: String, cx: &mut Context<Self>) {
        if let Some(thread) = self.thread_mut(key) {
            thread.rewinding = false;
            thread.waiting = None;
        }
        self.show_notice(key, NoticeKind::Problem, "Couldn't go back to that message.".into(), Some(reason), Vec::new(), cx);
        cx.notify();
    }

    /// Undo the last rewind: the files as they were, the messages back, the old session again.
    fn undo_now(&mut self, key: u64, fork: Option<Session>, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let Some(entry) = thread.undo.last() else { return };
        let (resent, backup) = (entry.resent.clone(), entry.backup.clone());
        cx.spawn(async move |this, cx| {
            // What the edited message's turn wrote goes first (the backup is from before it).
            if let (Some(fork), Some(resent)) = (fork, resent)
                && let Err(e) = fork.rewind(&resent, true).await
            {
                tracing::warn!("putting back the edited turn's files: {e:#}");
            }
            cx.background_spawn(async move { put_back(&backup) }).await;
            let _ = this.update(cx, |this, cx| this.finish_undo(key, cx));
        })
        .detach();
    }

    fn finish_undo(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        let Some(entry) = thread.undo.pop() else { return };
        thread.rewinding = false;
        // Back in the session it was in (the fork, if one was made, is left as it is).
        let alive = entry.session.filter(|s| s.agent().is_running());
        match alive {
            Some(session) => {
                thread.resume = None;
                thread.session = Some(session);
            }
            None => {
                thread.session = None;
                thread.resume = Some(entry.session_id);
            }
        }
        let composer = entry.composer;
        self.emit_local(key, SessionEvent::RewindUndone, cx);
        let selected = self.selected == Some(key);
        if let Some(thread) = self.thread_mut(key) {
            thread.to_composer = None;
            thread.composer_undo = composer.map(|c| (c.text, c.attachments, c.images, c.pastes, c.quote, c.rewound));
        }
        if selected {
            self.replies = Default::default();
        }
        if self.rewind.notice.as_ref().is_some_and(|n| n.key == key) {
            self.rewind.notice = None;
        }
        self.refresh_after_files(key, cx);
        cx.notify();
    }

    /// When the thread is drawn (with the window): the rewound message into the composer, or
    /// the composer back as it was after an Undo.
    pub(crate) fn apply_composer_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(message) = self.thread_mut(key).and_then(|t| t.to_composer.take()) {
            let before = {
                let thread = self.thread(key).expect("thread exists");
                (self.input.read(cx).value().to_string(), thread.attachments.len(), thread.images.len(), thread.pastes.len(), thread.quote.clone())
            };
            self.restore_to_composer(key, vec![Queued { id: String::new(), message, when: When::Done }], window, cx);
            let rewound = self.input.read(cx).value().to_string();
            if let Some(entry) = self.thread_mut(key).and_then(|t| t.undo.last_mut()) {
                let (text, attachments, images, pastes, quote) = before;
                entry.composer = Some(ComposerBefore { text, attachments, images, pastes, quote, rewound });
            }
        }
        if let Some((text, attachments, images, pastes, quote, rewound)) = self.thread_mut(key).and_then(|t| t.composer_undo.take()) {
            // Only if the composer still holds what the rewind put there.
            if self.input.read(cx).value() == rewound {
                if let Some(thread) = self.thread_mut(key) {
                    thread.attachments.truncate(attachments);
                    thread.images.truncate(images);
                    thread.pastes.truncate(pastes);
                    thread.quote = quote;
                }
                self.input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
    }

    /// ⌘Z: undo the selected thread's last rewind, if the composer holds nothing of yours since
    /// (else it's the text field's own undo). `true`: taken.
    /// First a thread just marked done, while the thread shown after it still is (from the
    /// composer only while it's empty).
    pub(crate) fn undo_key(&mut self, composer_focused: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.rewind.done.as_ref().is_some_and(|d| d.after == self.selected) {
            if composer_focused && !self.input.read(cx).value().trim().is_empty() {
                return false;
            }
            self.undo_done(window, cx);
            return true;
        }
        let Some(key) = self.selected else { return false };
        let Some(thread) = self.thread(key) else { return false };
        let Some(entry) = thread.undo.last() else { return false };
        if composer_focused {
            let value = self.input.read(cx).value().to_string();
            let untouched = match &entry.composer {
                Some(c) => value == c.rewound,
                None => value.trim().is_empty(),
            };
            if !untouched {
                return false;
            }
        }
        self.undo_rewind(key, cx);
        true
    }

    /// Reopen the thread just marked done (it goes back to its place in the list).
    pub(crate) fn undo_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(done) = self.rewind.done.take() else { return };
        if self.rewind.notice.as_ref().is_some_and(|n| n.kind == NoticeKind::Done) {
            self.rewind.notice = None;
        }
        self.reopen_thread(&done.id, window, cx);
    }

    /// The notice for a thread just marked done (`stopped`: what of it was stopped), with Undo.
    pub(crate) fn done_notice(&mut self, id: String, title: String, stopped: Option<&str>, cx: &mut Context<Self>) {
        let after = self.selected;
        self.show_notice(after.unwrap_or(0), NoticeKind::Done, format!("Done: {title}"), stopped.map(Into::into), Vec::new(), cx);
        self.rewind.done = Some(DoneUndo { id, after });
    }

    /// You sent another message: the thread's rewinds can't be undone any more, nor the thread
    /// marked done before it.
    pub(crate) fn forget_undo(&mut self, key: u64) {
        if let Some(thread) = self.thread_mut(key) {
            thread.undo.clear();
        }
        self.rewind.done = None;
        if self.rewind.notice.as_ref().is_some_and(|n| n.key == key && n.kind != NoticeKind::Problem) {
            self.rewind.notice = None;
        }
    }

    /// Apply and save an event Space makes itself.
    fn emit_local(&mut self, key: u64, event: SessionEvent, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_mut(key) else { return };
        thread.apply(&event);
        let id = thread.id.clone();
        self.host.record_event(&id, &event);
        self.save_thread(key, cx);
    }

    /// A turn ended: what its edits wrote, as it is now (to tell your later changes from Claude's).
    pub(crate) fn note_written(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let items = &thread.state.items;
        let start = items.iter().rposition(|i| matches!(i, TranscriptItem::User { steered: false, .. })).unwrap_or(0);
        let mut paths: Vec<PathBuf> = Vec::new();
        for item in &items[start..] {
            if let TranscriptItem::Tool(t) = item
                && t.kind == "edit"
            {
                for l in &t.locations {
                    let p = PathBuf::from(l);
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
            }
        }
        if paths.is_empty() {
            return;
        }
        let files = paths.into_iter().map(|p| {
            let hash = file_hash(&p);
            (p, hash)
        });
        self.emit_local(key, SessionEvent::Written { files: files.collect() }, cx);
    }

    /// After files changed under the thread: the Changes and Files views look again.
    fn refresh_after_files(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let cwd = thread.cwd.clone();
        if let Some(thread) = self.thread_mut(key) {
            thread.branch = space_ui::format::git_branch(&cwd);
        }
        self.load_changes(cwd, cx);
    }

    fn show_notice(&mut self, key: u64, kind: NoticeKind, title: String, body: Option<String>, kept: Vec<(String, &'static str)>, cx: &mut Context<Self>) {
        self.rewind.next_notice += 1;
        let id = self.rewind.next_notice;
        // Only the newest thing can be undone: a rewind after a thread was marked done wins.
        self.rewind.done = None;
        let sticky = !kept.is_empty() || kind == NoticeKind::Problem;
        self.rewind.notice = Some(Notice { id, key, kind, title, body, kept });
        if !sticky {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTICE_FOR).await;
                let _ = this.update(cx, |this, cx| {
                    if this.rewind.notice.as_ref().is_some_and(|n| n.id == id) {
                        this.rewind.notice = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn close_notice(&mut self, cx: &mut Context<Self>) {
        self.rewind.notice = None;
        cx.notify();
    }

    // ----- rewind mode (Esc Esc) ----------------------------------------------------------

    /// Esc in an empty composer with nothing running: the second within a moment enters rewind
    /// mode on your last message. `true`: taken.
    pub(crate) fn esc_for_rewind(&mut self, cx: &mut Context<Self>) -> bool {
        if self.rewind.mode.is_some() {
            self.rewind.mode = None;
            cx.notify();
            return true;
        }
        let Some(key) = self.selected else { return false };
        let Some(thread) = self.thread(key) else { return false };
        let empty = self.input.read(cx).value().trim().is_empty() && thread.attachments.is_empty() && thread.images.is_empty() && thread.pastes.is_empty() && thread.quote.is_none();
        let can = self.rewindable(thread);
        if !empty || can.is_empty() {
            self.rewind.esc_at = None;
            return false;
        }
        let now = Instant::now();
        if self.rewind.esc_at.is_some_and(|at| now.duration_since(at) <= ESC_ESC) {
            let last = can.iter().max().and_then(|&ix| match thread.state.items.get(ix) {
                Some(TranscriptItem::User { id: Some(id), .. }) => Some(id.clone()),
                _ => None,
            });
            self.rewind.esc_at = None;
            self.rewind.mode = last.map(|id| (key, id));
            cx.notify();
        } else {
            self.rewind.esc_at = Some(now);
        }
        true
    }

    /// The message picked in rewind mode: its index.
    pub(crate) fn picked(&self, thread: &Thread) -> Option<usize> {
        let (key, id) = self.rewind.mode.as_ref()?;
        (*key == thread.key).then(|| thread.state.message_index(id)).flatten()
    }

    /// ↑/↓ in rewind mode: the message before or after. `true`: taken.
    pub(crate) fn step_picked(&mut self, back: bool, cx: &mut Context<Self>) -> bool {
        let Some(thread) = self.current() else { return false };
        let Some(at) = self.picked(thread) else { return false };
        let mut can: Vec<usize> = self.rewindable(thread).into_iter().collect();
        can.sort_unstable();
        let next = if back { can.iter().rev().find(|&&ix| ix < at) } else { can.iter().find(|&&ix| ix > at) };
        if let Some(TranscriptItem::User { id: Some(id), .. }) = next.and_then(|&ix| thread.state.items.get(ix)) {
            self.rewind.mode = Some((thread.key, id.clone()));
            cx.notify();
        }
        true
    }

    /// ⏎ (edit) or ⌘⏎ (rewind) in rewind mode. `true`: taken.
    pub(crate) fn act_on_picked(&mut self, rewind: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(at) = self.current().and_then(|t| self.picked(t)) else { return false };
        self.rewind.mode = None;
        if rewind {
            self.rewind_at(at, window, cx);
        } else {
            self.start_edit(at, window, cx);
        }
        true
    }

    /// Typing, a click elsewhere or another thread: out of rewind mode.
    pub(crate) fn leave_rewind_mode(&mut self, cx: &mut Context<Self>) {
        if self.rewind.mode.take().is_some() {
            cx.notify();
        }
    }
}

#[cfg(feature = "dev")]
impl SpaceApp {
    /// The selected thread's going-back state, for dev scripts.
    pub(crate) fn rewind_summary(&self, cx: &App) -> String {
        let Some(thread) = self.current() else { return "no thread".into() };
        let can = self.rewindable(thread);
        let messages: Vec<String> = thread
            .state
            .items
            .iter()
            .enumerate()
            .filter_map(|(ix, i)| match i {
                TranscriptItem::User { text, id, steered, .. } => {
                    Some(format!("{ix}:{:?}{}{}{}", text, id.as_ref().map_or("", |_| " id"), if *steered { " steered" } else { "" }, if can.contains(&ix) { " ↺" } else { "" }))
                }
                _ => None,
            })
            .collect();
        let editor = self.rewind.editor.as_ref().map(|e| e.input.read(cx).value().to_string());
        let notice = self.rewind.notice.as_ref().map(|n| format!("{} {} kept={:?}", n.title, n.body.as_deref().unwrap_or(""), n.kept));
        format!(
            "messages=[{}] fork={:?} undo={} rewinding={} editor={editor:?} mode={:?} picked={:?} notice={notice:?} session={:?} composer={:?}",
            messages.join(", "),
            thread.state.fork.as_ref().map(|f| &f.message_id),
            thread.undo.len(),
            thread.rewinding,
            self.rewind.mode.as_ref().map(|(_, id)| id),
            self.picked(thread),
            thread.session.as_ref().map(|s| s.id().to_string()),
            self.input.read(cx).value().to_string(),
        )
    }
}

/// What the notice says besides its title.
struct NoticeText {
    body: Option<String>,
    kept: Vec<(String, &'static str)>,
}

/// Write files back as `backup` has them (`None`: remove it).
fn put_back(backup: &[(PathBuf, Option<Vec<u8>>)]) {
    for (path, bytes) in backup {
        let result = match bytes {
            Some(bytes) => path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, bytes)),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(e) = result {
            tracing::warn!("putting back {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use space_core::TranscriptItem;

    use super::{file_hash, put_back, shown_path, subagent_files};

    #[test]
    fn files_go_back_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("sub/b.txt"));
        std::fs::write(&a, "now").unwrap();
        put_back(&[(a.clone(), Some(b"then".to_vec())), (b.clone(), Some(b"new".to_vec()))]);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "then");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "new");
        put_back(&[(b.clone(), None), (dir.path().join("never"), None)]);
        assert!(!b.exists());
        assert_eq!(file_hash(&a).map(|h| h.len()), Some(64));
        assert_eq!(file_hash(&b), None);
    }

    #[test]
    fn a_subagents_edits_are_named_once() {
        let tool = |id: &str, path: &str, subagent: bool| {
            TranscriptItem::Tool(space_core::ToolCallInfo {
                id: id.into(),
                title: String::new(),
                kind: "edit".into(),
                status: space_core::ToolStatus::Completed,
                output: String::new(),
                locations: vec![path.into()],
                blocked: None,
                held: None,
                subagent,
            })
        };
        let items = vec![tool("1", "/p/old.rs", true), TranscriptItem::Agent("x".into()), tool("2", "/p/a.rs", true), tool("3", "/p/a.rs", true), tool("4", "/p/main.rs", false)];
        assert_eq!(subagent_files(&items, 1), vec![PathBuf::from("/p/a.rs")]);
        assert_eq!(shown_path(Path::new("/p/src/a.rs"), Path::new("/p")), "src/a.rs");
        assert_eq!(shown_path(Path::new("/elsewhere/a.rs"), Path::new("/p")), "/elsewhere/a.rs");
    }
}
