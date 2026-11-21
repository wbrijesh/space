//! The side chat: a conversation about a thread, in its right pane. It's a thread of its own,
//! kept with the saved threads but off the sidebar, whose session is a side chat of the thread's
//! (`space/sideOf`): a copy of the conversation that can only read, told before each message
//! what the thread added since. One per thread; ↺ starts it over.
//!
//! `/btw` and a space in the composer open it with its input focused at once, and whatever is
//! typed after goes there: keys that arrive before the side chat's first frame (its input isn't
//! drawn yet, so they land in the composer) are moved across (see [`SideChat::handoff`]).

use std::collections::HashMap;
use std::rc::Rc;

use gpui::component::input::{InputEvent, TextareaState};
use gpui::*;
use space_core::protocol::service::ThreadMeta;
use space_ui::components::composer::{Composer, ComposerHandlers, MAX_LINES};
use space_ui::components::follow_scroll::{FollowScroll, FollowScrollArea};
use space_ui::components::right_pane::{PaneView, side_chat_empty, side_chat_new};
use space_ui::components::message_actions::ActionHandlers;
use space_ui::components::transcript::{TranscriptView, transcript_items};
use space_ui::theme::{color, size, text};

use crate::app::SpaceApp;
use crate::threads::{Outgoing, Thread};

/// Key context around the side chat (its own ⌘W: hide the pane, not close the thread).
pub(crate) const SIDE_CHAT_CONTEXT: &str = "SideChat";

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

/// The side chats saved for threads not opened yet, by their thread's id.
pub(crate) type SavedSides = HashMap<String, ThreadMeta>;

impl SpaceApp {
    /// The session id of thread `main` (live, or the one it would load), which its side chat
    /// follows.
    pub(crate) fn main_session_id(&self, main: u64) -> Option<String> {
        let t = self.threads.iter().find(|t| t.key == main)?;
        t.session.as_ref().map(|s| s.id().to_string()).or_else(|| t.resume.as_ref().map(|r| r.1.clone()))
    }

    /// Thread `main`'s side chat, made (or read from what was saved) the first time.
    pub(crate) fn ensure_side_chat(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) -> Option<u64> {
        if let Some(side) = self.side_chats.get(&main) {
            return Some(side.thread.key);
        }
        let (main_id, cwd) = self.threads.iter().find(|t| t.key == main).map(|t| (t.id.clone(), t.cwd.clone()))?;
        let mut thread = match self.saved_sides.remove(&main_id) {
            Some(meta) => self.saved_thread(meta),
            None => {
                let key = self.next_key;
                self.next_key += 1;
                Thread::new(key, cwd)
            }
        };
        thread.side_of = Some(main);
        let key = thread.key;
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("Ask about this thread…");
            state.set_auto_grow(1, MAX_LINES, cx);
            state.set_submit_on_enter(true, cx);
            state
        });
        let events = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { shift: false, .. } => this.send_side(main, window, cx),
            InputEvent::Change => {
                this.touch_agent();
                this.wake_agent(cx);
                cx.notify();
            }
            InputEvent::Focus | InputEvent::Blur => cx.notify(),
            _ => {}
        });
        let side = SideChat { thread, input, scroll: FollowScroll::new(), handoff: false, replies: HashMap::new(), _input_events: events };
        self.side_chats.insert(main, side);
        self.ensure_loaded(key, cx);
        // Its session starts now, so the first answer doesn't wait for it.
        self.start_side_session(key, cx);
        Some(key)
    }

    /// Start the side chat's session unless it has one (or a saved one, loaded when it's used).
    fn start_side_session(&mut self, key: u64, cx: &mut Context<Self>) {
        if self.thread(key).is_some_and(|t| t.session.is_none() && !t.creating && t.resume.is_none() && t.error.is_none()) {
            self.create_session(key, cx);
        }
    }

    /// ⌘⇧L or the pane's tab: open the side chat, focused.
    pub(crate) fn open_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_pane_view(PaneView::SideChat, window, cx);
    }

    /// Focus thread `main`'s side chat input (made if need be).
    pub(crate) fn focus_side_chat(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.ensure_side_chat(main, window, cx).is_none() {
            return;
        }
        if let Some(side) = self.side_chats.get(&main) {
            side.input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    pub(crate) fn side_chat_focused(&self, main: u64, window: &Window, cx: &App) -> bool {
        self.side_chats.get(&main).is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// `/btw ` typed in the composer: open the side chat focused, with what came after it.
    pub(crate) fn open_side_chat_with(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main) = self.selected else { return };
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.slash.edited("");
        self.open_side_chat(window, cx);
        let Some(side) = self.side_chats.get_mut(&main) else { return };
        side.handoff = true;
        if !rest.is_empty() {
            let input = side.input.clone();
            input.update(cx, |s, cx| {
                let text = format!("{}{rest}", s.value());
                s.set_value(text, window, cx);
            });
        }
        cx.notify();
    }

    /// The composer changed while the side chat opened by `/btw` isn't drawn yet: those keys
    /// were meant for it. `true`: they were moved across.
    pub(crate) fn hand_off_to_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(main) = self.selected else { return false };
        let Some(side) = self.side_chats.get(&main).filter(|s| s.handoff) else { return false };
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

    /// ⏎ in the side chat: send what's typed (not while it answers).
    pub(crate) fn send_side(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(side) = self.side_chats.get(&main) else { return };
        let (key, input, scroll) = (side.thread.key, side.input.clone(), side.scroll.clone());
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

    fn stop_side(&mut self, main: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.side_chats.get(&main).and_then(|s| s.thread.session.clone())
            && let Err(e) = session.cancel()
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    /// ↺: a new side chat, from the thread as it is now; the old one is deleted.
    pub(crate) fn new_side_chat(&mut self, main: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.take_side_chat(main)
            && old.thread.meta.is_some()
        {
            self.remove_saved(old.thread.id.clone(), cx);
        }
        self.focus_side_chat(main, window, cx);
        cx.notify();
    }

    /// Thread `main` is going (closed): its side chat stops, and is kept to open again with it.
    pub(crate) fn put_away_side_chat(&mut self, main: u64, main_id: &str, cx: &mut Context<Self>) {
        let Some(key) = self.side_chats.get(&main).map(|s| s.thread.key) else { return };
        self.save_thread(key, cx);
        let meta = self.thread(key).filter(|t| !t.state.items.is_empty() || t.meta.is_some()).map(|t| self.thread_meta(t));
        self.take_side_chat(main);
        if let Some(meta) = meta {
            self.saved_sides.insert(main_id.to_string(), meta);
        }
    }

    /// Take thread `main`'s side chat away, stopping its turn.
    fn take_side_chat(&mut self, main: u64) -> Option<SideChat> {
        let side = self.side_chats.remove(&main)?;
        if let Some(session) = &side.thread.session {
            if session.is_running() {
                let _ = session.cancel();
            }
            self.session_threads.remove(session.id());
        }
        Some(side)
    }

    /// The thread's session started: a side chat waiting for it can start too.
    pub(crate) fn main_session_started(&mut self, main: u64, cx: &mut Context<Self>) {
        let Some(t) = self.side_chats.get(&main).map(|s| &s.thread) else { return };
        if t.session.is_none() && !t.creating {
            let key = t.key;
            self.create_session(key, cx);
        }
    }
}

/// The side chat in the pane: its bar, the conversation (or what it's for), and its composer.
/// The side chat's ↺ for the pane's header, once it has something to start over from.
pub(crate) fn side_chat_header_action(app: &SpaceApp, main: u64, cx: &mut Context<SpaceApp>) -> Option<AnyElement> {
    let side = app.side_chats.get(&main)?;
    if side.thread.state.items.is_empty() {
        return None;
    }
    let this = cx.entity().downgrade();
    let on_new = Rc::new(move |w: &mut Window, cx: &mut App| {
        let _ = this.update(cx, |this, cx| this.new_side_chat(main, w, cx));
    });
    Some(side_chat_new(on_new).into_any_element())
}

pub(crate) fn side_chat_content(app: &mut SpaceApp, main: u64, window: &mut Window, cx: &mut Context<SpaceApp>) -> AnyElement {
    if app.ensure_side_chat(main, window, cx).is_none() {
        return div().flex_1().into_any_element();
    }
    let side = app.side_chats.get_mut(&main).expect("made above");
    // Drawn now: its input takes the keys from here on.
    side.handoff = false;
    let side = app.side_chats.get(&main).expect("made above");
    let thread = &side.thread;
    let weak = cx.entity().downgrade();
    let body = if thread.state.items.is_empty() && thread.queued.is_none() {
        side_chat_empty().into_any_element()
    } else {
        let toggle = |turns: bool| {
            let this = weak.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(side) = this.side_chats.get_mut(&main) {
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
        let content = div().w_full().px_4().pt_4().pb(size::SCROLL_FADE_H).flex().flex_col().children(items);
        FollowScrollArea::new("side-chat-transcript", &side.scroll, content).fade(true).button(true).into_any_element()
    };
    let handlers = ComposerHandlers {
        on_send: {
            let this = weak.clone();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |this, cx| this.send_side(main, w, cx));
            })
        },
        on_stop: {
            let this = weak.clone();
            Rc::new(move |_, cx| {
                let _ = this.update(cx, |this, cx| this.stop_side(main, cx));
            })
        },
        on_focus: {
            let this = weak.clone();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |this, cx| this.focus_side_chat(main, w, cx));
            })
        },
        ..Default::default()
    };
    let composer = Composer::new("side-chat-composer", &side.input).without_attach().running(thread.state.running).queued(thread.queued.is_some()).handlers(handlers);
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
                this.stop_side(main, cx);
                cx.stop_propagation();
            }
        }))
        .child(body)
        .child(div().flex_none().p_2().flex().flex_col().gap_1().children(error).child(composer))
        .into_any_element()
}
