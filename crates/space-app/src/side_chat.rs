//! Side chats: conversations about a thread, in its right pane. Each is a thread of its own,
//! kept with the saved threads but off the sidebar, whose session is a side chat of the
//! thread's (`space/sideOf`): a copy of the conversation that can only read, told before each
//! message what the thread added since. A thread can have any number; each is named after its
//! first message, like a thread, and kept (and resumable after a restart) until it's deleted
//! from the launcher, or its thread is. Closing its tab only closes the tab.
//!
//! `/btw` and a space in the composer open a new one with its input focused at once, and whatever
//! is typed after goes there: keys that arrive before the side chat's first frame (its input isn't
//! drawn yet, so they land in the composer) are moved across (see [`SideChat::handoff`]).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::component::input::{InputEvent, TextareaState};
use gpui::*;
use space_core::protocol::service::ThreadMeta;
use space_ui::components::composer::{Composer, ComposerHandlers, MAX_LINES};
use space_ui::components::follow_scroll::{FollowScroll, FollowScrollArea};
use space_ui::components::right_pane::side_chat_empty;
use space_ui::components::message_actions::ActionHandlers;
use space_ui::components::transcript::{TranscriptView, transcript_items};
use space_ui::theme::{color, size, text};

use crate::app::SpaceApp;
use crate::pane::PaneTab;
use crate::threads::{Outgoing, Thread};

/// Key context around the side chat.
pub(crate) const SIDE_CHAT_CONTEXT: &str = "SideChat";

/// How long a deleted side chat can be brought back.
const UNDO_FOR: Duration = Duration::from_secs(6);

pub(crate) struct SideChat {
    pub thread: Thread,
    pub input: Entity<TextareaState>,
    pub scroll: FollowScroll,
    /// Opened by `/btw` and not drawn yet: what's typed in the composer comes here.
    pub handoff: bool,
    /// Its replies' text views: none (they render on their own).
    replies: HashMap<usize, Entity<gpui::component::text::TextViewState>>,
    _input_events: Subscription,
}

impl SideChat {
    /// Its name: its title once it has one, else "New side chat".
    pub(crate) fn title(&self) -> String {
        if self.thread.is_empty() { "New side chat".into() } else { self.thread.title() }
    }
}

/// The side chats saved for threads whose side chats aren't loaded yet, by their thread's id.
pub(crate) type SavedSides = HashMap<String, Vec<ThreadMeta>>;

/// A side chat just deleted, kept a moment for Undo.
pub(crate) struct Deleted {
    pub(crate) side: SideChat,
    pub(crate) tab_at: Option<usize>,
    _timer: Task<()>,
}

impl SpaceApp {
    /// The session id of thread `main` (live, or the one it would load), which its side chats
    /// follow.
    pub(crate) fn main_session_id(&self, main: u64) -> Option<String> {
        let t = self.threads.iter().find(|t| t.key == main)?;
        t.session.as_ref().map(|s| s.id().to_string()).or_else(|| t.resume.clone())
    }

    /// Thread `main`'s side chats, newest first (the saved ones read in first).
    pub(crate) fn side_chats_of(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) -> Vec<u64> {
        self.load_side_chats(main, window, cx);
        let mut sides: Vec<_> = self.side_chats.values().filter(|s| s.thread.side_of == Some(main)).map(|s| (s.thread.created_ms, s.thread.key)).collect();
        sides.sort_by(|a, b| b.cmp(a));
        sides.into_iter().map(|(_, k)| k).collect()
    }

    /// Bring thread `main`'s saved side chats in (not their conversations: those load when shown).
    fn load_side_chats(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main_id) = self.threads.iter().find(|t| t.key == main).map(|t| t.id.clone()) else { return };
        for meta in self.saved_sides.remove(&main_id).unwrap_or_default() {
            let thread = self.saved_thread(meta);
            self.add_side_chat(main, thread, window, cx);
        }
    }

    fn add_side_chat(&mut self, main: u64, mut thread: Thread, window: &mut Window, cx: &mut Context<Self>) -> u64 {
        thread.side_of = Some(main);
        let key = thread.key;
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("Ask about this thread…");
            state.set_auto_grow(1, MAX_LINES, cx);
            state.set_submit_on_enter(true, cx);
            state
        });
        let events = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { shift: false, .. } => this.send_side(key, window, cx),
            InputEvent::Change => {
                this.touch_agent();
                this.wake_agent(cx);
                cx.notify();
            }
            InputEvent::Focus | InputEvent::Blur => cx.notify(),
            _ => {}
        });
        let side = SideChat { thread, input, scroll: FollowScroll::new(), handoff: false, replies: HashMap::new(), _input_events: events };
        self.side_chats.insert(key, side);
        key
    }

    /// Start the side chat's session unless it has one (or a saved one, loaded when it's used).
    fn start_side_session(&mut self, key: u64, cx: &mut Context<Self>) {
        if self.thread(key).is_some_and(|t| t.session.is_none() && !t.creating && t.resume.is_none() && t.error.is_none()) {
            self.create_session(key, cx);
        }
    }

    /// ⌘⇧L, the launcher or `/btw`: a new side chat about the current thread, in a tab of its
    /// own, focused. One that's still empty is reused rather than another made.
    pub(crate) fn new_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<u64> {
        let main = self.selected?;
        let empty = self.side_chats_of(main, window, cx).into_iter().find(|k| self.side_chats.get(k).is_some_and(|s| s.thread.is_empty() && !s.thread.state.running));
        let key = match empty {
            Some(key) => key,
            None => {
                let cwd = self.threads.iter().find(|t| t.key == main)?.cwd.clone();
                let key = self.next_key;
                self.next_key += 1;
                let key = self.add_side_chat(main, Thread::new(key, cwd), window, cx);
                // Its session starts now, so the first answer doesn't wait for it.
                self.start_side_session(key, cx);
                key
            }
        };
        self.show_pane_tab(main, PaneTab::SideChat(key), window, cx);
        Some(key)
    }

    /// Show side chat `key` in its thread's pane (a launcher row).
    pub(crate) fn open_side_chat(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main) = self.side_chats.get(&key).and_then(|s| s.thread.side_of) else { return };
        self.show_pane_tab(main, PaneTab::SideChat(key), window, cx);
    }

    /// Focus side chat `key`'s input.
    pub(crate) fn focus_side_chat(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(side) = self.side_chats.get(&key) {
            side.input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    pub(crate) fn side_chat_focused(&self, key: u64, window: &Window, cx: &App) -> bool {
        self.side_chats.get(&key).is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// `/btw ` typed in the composer: open a new side chat focused, with what came after it.
    pub(crate) fn open_side_chat_with(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<u64> {
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.slash.edited("");
        let key = self.new_side_chat(window, cx)?;
        let side = self.side_chats.get_mut(&key)?;
        side.handoff = true;
        if !rest.is_empty() {
            let input = side.input.clone();
            input.update(cx, |s, cx| {
                let text = format!("{}{rest}", s.value());
                s.set_value(text, window, cx);
            });
        }
        cx.notify();
        Some(key)
    }

    /// The composer changed while a side chat opened by `/btw` isn't drawn yet: those keys
    /// were meant for it. `true`: they were moved across.
    pub(crate) fn hand_off_to_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(main) = self.selected else { return false };
        let Some(side) = self.side_chats.values().find(|s| s.handoff && s.thread.side_of == Some(main)) else { return false };
        let typed = self.input.read(cx).value().to_string();
        if typed.is_empty() {
            return true;
        }
        let input = side.input.clone();
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        input.update(cx, |s, cx| {
            let text = format!("{}{typed}", s.value());
            s.set_value(text, window, cx);
            s.focus(window, cx);
        });
        true
    }

    /// ⏎ in side chat `key`: send what's typed (not while it answers).
    pub(crate) fn send_side(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(side) = self.side_chats.get(&key) else { return };
        let (input, scroll) = (side.input.clone(), side.scroll.clone());
        let text = input.read(cx).value().trim().to_string();
        let t = &side.thread;
        if text.is_empty() || t.state.running || t.queued.is_some() || t.loading || t.unloaded {
            return;
        }
        self.touch_agent();
        input.update(cx, |s, cx| s.clean(window, cx));
        scroll.jump_to_bottom();
        self.dispatch(key, crate::queue::When::Done, false, Outgoing { text, ..Default::default() }, cx);
    }

    fn stop_side(&mut self, key: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.side_chats.get(&key).and_then(|s| s.thread.session.clone())
            && let Err(e) = session.cancel()
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    /// The launcher's 🗑: delete side chat `key` (its tab closes). It can be brought back for a
    /// moment ([`Self::undo_delete_side_chat`]); then it's gone for good.
    pub(crate) fn delete_side_chat(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_deleting_side_chat(cx);
        let Some(main) = self.side_chats.get(&key).and_then(|s| s.thread.side_of) else { return };
        let tab = PaneTab::SideChat(key);
        let tab_at = self.terminals.pane(main).and_then(|p| p.position(&tab));
        if tab_at.is_some() {
            self.remove_pane_tab(main, &tab, true, window, cx);
        }
        let Some(side) = self.take_side_chat(key) else { return };
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            let _ = this.update(cx, |this, cx| this.finish_deleting_side_chat(cx));
        });
        self.deleted_side = Some(Deleted { side, tab_at, _timer: timer });
        cx.notify();
    }

    /// The deleted side chat's Undo: it's back, in its tab if it had one.
    pub(crate) fn undo_delete_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Deleted { side, tab_at, .. }) = self.deleted_side.take() else { return };
        let (key, main) = (side.thread.key, side.thread.side_of);
        if let Some(session) = &side.thread.session {
            self.session_threads.insert(session.id().to_string(), key);
        }
        self.side_chats.insert(key, side);
        if let (Some(main), Some(at)) = (main, tab_at)
            && let Some(pane) = self.terminals.panes.get_mut(&main)
        {
            pane.tabs.insert(at.min(pane.tabs.len()), PaneTab::SideChat(key));
            pane.select(at.min(pane.tabs.len() - 1));
            pane.open = true;
            self.pane_tab_shown(main, window, cx);
        }
        cx.notify();
    }

    /// The deleted side chat's moment passed: delete what was saved of it.
    fn finish_deleting_side_chat(&mut self, cx: &mut Context<Self>) {
        let Some(Deleted { side, .. }) = self.deleted_side.take() else { return };
        if side.thread.meta.is_some() || side.thread.saved > 0 {
            self.remove_saved(side.thread.id.clone(), cx);
        }
        cx.notify();
    }

    /// Thread `main` is going (closed): its side chats stop, and are kept to open again with it.
    pub(crate) fn put_away_side_chats(&mut self, main: u64, main_id: &str, cx: &mut Context<Self>) {
        if self.deleted_side.as_ref().is_some_and(|d| d.side.thread.side_of == Some(main)) {
            self.finish_deleting_side_chat(cx);
        }
        let keys: Vec<u64> = self.side_chats.values().filter(|s| s.thread.side_of == Some(main)).map(|s| s.thread.key).collect();
        for key in keys {
            self.save_thread(key, cx);
            let meta = self.thread(key).filter(|t| !t.state.items.is_empty() || t.meta.is_some()).map(|t| self.thread_meta(t));
            self.take_side_chat(key);
            if let Some(meta) = meta {
                self.saved_sides.entry(main_id.to_string()).or_default().push(meta);
            }
        }
    }

    /// Take side chat `key` away, stopping its turn.
    fn take_side_chat(&mut self, key: u64) -> Option<SideChat> {
        let side = self.side_chats.remove(&key)?;
        if let Some(session) = &side.thread.session {
            if session.is_running() {
                let _ = session.cancel();
            }
            self.session_threads.remove(session.id());
        }
        Some(side)
    }

    /// The thread's session started: side chats waiting for it can start too.
    pub(crate) fn main_session_started(&mut self, main: u64, cx: &mut Context<Self>) {
        let waiting: Vec<u64> = self
            .side_chats
            .values()
            .filter(|s| s.thread.side_of == Some(main) && !s.thread.unloaded && s.thread.session.is_none() && !s.thread.creating)
            .map(|s| s.thread.key)
            .collect();
        for key in waiting {
            self.create_session(key, cx);
        }
    }
}

/// The side chat in the pane: the conversation (or what it's for), and its composer.
pub(crate) fn side_chat_content(app: &mut SpaceApp, key: u64, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    // A saved one's conversation loads the first time it's shown.
    app.ensure_loaded(key, cx);
    let Some(side) = app.side_chats.get_mut(&key) else { return div().flex_1().into_any_element() };
    // Drawn now: its input takes the keys from here on.
    side.handoff = false;
    let side = app.side_chats.get(&key).expect("just there");
    let thread = &side.thread;
    let weak = cx.entity().downgrade();
    let body = if thread.state.items.is_empty() && thread.queued.is_none() {
        side_chat_empty().into_any_element()
    } else {
        let toggle = |turns: bool| {
            let this = weak.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(side) = this.side_chats.get_mut(&key) {
                        let set = if turns { &mut side.thread.open_turns } else { &mut side.thread.expanded };
                        if !set.remove(&ix) {
                            set.insert(ix);
                        }
                        side.scroll.hold();
                    }
                    cx.notify();
                });
            })
        };
        let answer = {
            let this = weak.clone();
            Rc::new(move |rid, decision, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.answer_permission(rid, decision, cx));
            })
        };
        let items = transcript_items(
            TranscriptView {
                state: &thread.state,
                key: thread.key,
                expanded: &thread.expanded,
                thought_secs: &thread.thought_secs,
                queued: thread.queued.as_ref().map(|q| (q.text.as_str(), Default::default())),
                on_toggle: toggle(false),
                on_answer: answer,
                on_open_image: None,
                on_open_text: None,
                now_ms: crate::app::now_ms(),
                copied: None,
                usage_open: None,
                actions: ActionHandlers {
                    on_copy: Rc::new(|_, text, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()))),
                    on_usage: Rc::new(|_, _, _| {}),
                },
                open_turns: &thread.open_turns,
                on_toggle_turn: toggle(true),
                streaming: None,
                replies: &side.replies,
                find: None,
                on_reveal: None,
                on_continue: None,
            },
            window,
            cx,
        );
        // One column, as wide as the main chat's at most, centred in a wider pane.
        let content = div().w_full().max_w(size::CONTENT_MAX_W).mx_auto().px_4().pt_4().pb(size::SCROLL_FADE_H).flex().flex_col().children(items);
        FollowScrollArea::new("side-chat-transcript", &side.scroll, content).fade(true).button(true).into_any_element()
    };
    let handlers = ComposerHandlers {
        on_send: {
            let this = weak.clone();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |this, cx| this.send_side(key, w, cx));
            })
        },
        on_stop: {
            let this = weak.clone();
            Rc::new(move |_, cx| {
                let _ = this.update(cx, |this, cx| this.stop_side(key, cx));
            })
        },
        on_focus: {
            let this = weak.clone();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |this, cx| this.focus_side_chat(key, w, cx));
            })
        },
        ..Default::default()
    };
    let composer = Composer::new("side-chat-composer", &side.input).without_attach().docked().running(thread.state.running).queued(thread.queued.is_some()).handlers(handlers);
    let error = thread.error.clone().map(|e| {
        div().px_1().text_size(text::SM).text_color(color::red()).child(space_ui::format::clean_error(&e))
    });
    let running = thread.state.running;
    div()
        .key_context(SIDE_CHAT_CONTEXT)
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        // Esc stops its answer.
        .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
            if running && e.keystroke.key == "escape" && !e.keystroke.modifiers.modified() {
                this.stop_side(key, cx);
                cx.stop_propagation();
            }
        }))
        .child(body)
        // Docked along the bottom: a hairline across the pane, the input in the transcript's column.
        .child(
            div().flex_none().border_t_1().border_color(color::border()).child(
                // px_3: the editor's own 4px inset lines its text up with the transcript's (px_4).
                div().w_full().max_w(size::CONTENT_MAX_W).mx_auto().px_3().flex().flex_col().gap_1().children(error).child(composer),
            ),
        )
        .into_any_element()
}
