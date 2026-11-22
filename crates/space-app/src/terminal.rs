//! Terminals: the right-hand pane of each thread, its tabs, and the screens behind them.
//!
//! The terminal plugin runs the shells; each has a [`Session`] here: its screen (fed by the
//! host's [`TerminalSink`](space_core::TerminalSink) on a background thread, in order) and what
//! the tab shows. A hidden terminal keeps running and its screen keeps up; only drawing waits.
//! Its tab gets a dot, and its thread a terminal mark in the sidebar. Each thread has its own
//! pane (open or not, maximized, tabs); the pane's width is shared.
//!
//! When the plugin (re)starts, the terminals it still runs are attached again (a replay of their
//! output rebuilds the screens); when it stops, its terminals say so and offer a new one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use gpui::*;
use space_core::Host;
use space_core::protocol::terminal::{TerminalOpen, TerminalReplay};
use space_terminal::{Screen, Size as TermSize};
use space_ui::components::right_pane::PaneView;
use space_ui::components::terminal::{FONT_SIZE, TerminalTab};

use crate::app::SpaceApp;
use crate::pane::{Pane, PaneTab, STARTING};
use crate::terminal_view::{TerminalView, TerminalViewEvent};

/// Shells: a tab whose title is one of these has nothing running in it.
const SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "login", "xonsh", "elvish"];

/// The pane's width in px: it stays put as the window resizes; dragging its edge sets it.
pub(crate) const WIDTH_DEFAULT: f32 = 520.;
const WIDTH_MIN: f32 = 320.;

#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) title: String,
    /// `Some` once the shell ended (its exit code when known).
    pub(crate) exited: Option<Option<i32>>,
    /// The last output on screen; `None` until the replay arrived (output is held till then).
    applied: Option<u64>,
    pending: Vec<(u64, Vec<u8>)>,
    /// Its plugin stopped (the shell is gone).
    pub(crate) stopped: bool,
}

/// One terminal's screen and state, shared with the output thread.
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) screen: Mutex<Screen>,
    pub(crate) state: Mutex<SessionState>,
    /// On screen now (its output asks for a redraw).
    pub(crate) visible: AtomicBool,
    /// A redraw was asked for and hasn't happened yet.
    pub(crate) redraw: AtomicBool,
    /// Output arrived while hidden.
    pub(crate) unread: AtomicBool,
    /// cols << 16 | rows, as last sent.
    pub(crate) last_size: AtomicU32,
}

impl Session {
    fn new(id: String, title: String, cols: u16, rows: u16) -> Self {
        let size = TermSize { cols, rows, cell_width: 7.5, cell_height: f32::from(FONT_SIZE) * 1.3 };
        Self {
            id,
            screen: Mutex::new(Screen::new(size, space_ui::theme::terminal_palette())),
            state: Mutex::new(SessionState { title, ..Default::default() }),
            visible: AtomicBool::new(false),
            redraw: AtomicBool::new(false),
            unread: AtomicBool::new(false),
            last_size: AtomicU32::new(((cols as u32) << 16) | rows as u32),
        }
    }

    /// Output from the plugin (background thread). Returns whether the app should be told.
    fn receive(&self, seq: u64, data: &[u8], host: &Host) -> bool {
        let mut state = self.state.lock().unwrap();
        match state.applied {
            None => {
                state.pending.push((seq, data.to_vec()));
                return false;
            }
            Some(applied) if seq <= applied => return false,
            Some(_) => {}
        }
        state.applied = Some(seq);
        drop(state);
        let reply = self.screen.lock().unwrap().feed(data);
        if !reply.is_empty() {
            host.terminal_input(&self.id, reply);
        }
        self.wants_redraw()
    }

    fn wants_redraw(&self) -> bool {
        if self.visible.load(Ordering::SeqCst) { !self.redraw.swap(true, Ordering::SeqCst) } else { !self.unread.swap(true, Ordering::SeqCst) }
    }

    /// The replay from `terminal/attach`, then whatever arrived meanwhile.
    fn apply_replay(&self, replay: TerminalReplay, host: &Host) {
        let mut state = self.state.lock().unwrap();
        let mut screen = self.screen.lock().unwrap();
        let mut reply = screen.feed(&replay.data.0);
        let mut applied = replay.seq;
        for (seq, data) in std::mem::take(&mut state.pending) {
            if seq > applied {
                reply.extend(screen.feed(&data));
                applied = seq;
            }
        }
        state.applied = Some(applied);
        if !replay.title.is_empty() {
            state.title = replay.title;
        }
        if replay.exited {
            state.exited = Some(replay.exit_code);
        }
        drop(screen);
        drop(state);
        // Answers to queries in old output are stale; only a live shell gets them.
        if !reply.is_empty() && !replay.exited {
            host.terminal_input(&self.id, reply);
        }
    }

    pub(crate) fn ended(&self) -> Option<Option<i32>> {
        self.state.lock().unwrap().exited
    }

    /// Something other than the shell is running.
    pub(crate) fn busy(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.exited.is_none() && !state.stopped && !SHELLS.contains(&state.title.as_str())
    }

    pub(crate) fn tab(&self) -> TerminalTab {
        let state = self.state.lock().unwrap();
        TerminalTab {
            title: state.title.clone().into(),
            unread: self.unread.load(Ordering::SeqCst),
            ended: state.exited.is_some() || state.stopped,
        }
    }

    fn size(&self) -> (u16, u16) {
        let s = self.last_size.load(Ordering::SeqCst);
        ((s >> 16) as u16, s as u16)
    }
}

pub(crate) struct Terminals {
    /// By terminal id; shared with the output thread.
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    pub(crate) views: HashMap<String, Entity<TerminalView>>,
    /// By thread key.
    pub(crate) panes: HashMap<u64, Pane>,
    /// The pane's width in px (beside the chat; maximized it fills the main column).
    pub(crate) width: f32,
    /// The divider is being dragged: where the pointer went down (window x) and the pane's
    /// width then. The pane follows the pointer from there, so a drag never jumps.
    pub(crate) drag: Option<(f32, f32)>,
    /// Focus this terminal once its view exists.
    focus_next: Option<String>,
    /// Terminals started, to name each starting tab.
    starting: u64,
}

impl Terminals {
    /// Route the host's terminal output to the screens.
    pub(crate) fn new(host: &Host, width: f32) -> Self {
        let sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> = Arc::default();
        let (map, h) = (sessions.clone(), host.clone());
        host.set_terminal_sink(Arc::new(move |id, seq, data| {
            let session = map.lock().unwrap().get(id).cloned();
            session.is_some_and(|s| s.receive(seq, data, &h))
        }));
        Self { sessions, views: HashMap::new(), panes: HashMap::new(), width: width.max(WIDTH_MIN), drag: None, focus_next: None, starting: 0 }
    }

    pub(crate) fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    fn all(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    pub(crate) fn pane(&self, key: u64) -> Option<&Pane> {
        self.panes.get(&key)
    }

    /// The terminal the pane shows (its shown tab is a terminal).
    pub(crate) fn selected(&self, key: u64) -> Option<Arc<Session>> {
        match self.panes.get(&key)?.active_tab()? {
            PaneTab::Terminal(id) => self.session(id),
            _ => None,
        }
    }

    pub(crate) fn selected_view(&self, key: u64) -> Option<Entity<TerminalView>> {
        let id = self.selected(key)?.id.clone();
        self.views.get(&id).cloned()
    }

    /// A thread has a terminal with something running, or output it hasn't seen.
    pub(crate) fn active(&self, key: u64) -> bool {
        self.panes.get(&key).is_some_and(|p| p.terminals().filter_map(|id| self.session(id)).any(|s| s.busy() || s.unread.load(Ordering::SeqCst)))
    }

    /// Mark which terminal is on screen (the rest wait to be drawn).
    pub(crate) fn set_visible(&self, shown: Option<&str>) {
        for s in self.all() {
            s.visible.store(shown == Some(s.id.as_str()), Ordering::SeqCst);
        }
    }

    /// The divider dragged to window x `x`, with `room` the widest the pane can be and leave the
    /// chat its minimum. The pane follows the pointer, from [`WIDTH_MIN`] up to `room`, or up to
    /// its width when the drag began if that's wider (the chat was already too narrow: dragging
    /// shrinks the pane smoothly, and never snaps it).
    pub(crate) fn drag_to(&mut self, x: f32, room: f32) {
        let Some((from_x, from_width)) = self.drag else { return };
        let max = room.max(from_width).max(WIDTH_MIN);
        self.width = (from_width + from_x - x).clamp(WIDTH_MIN, max).round();
    }

    /// Terminals with something running (asked about before quitting).
    pub(crate) fn busy(&self) -> Vec<Arc<Session>> {
        self.all().into_iter().filter(|s| s.busy()).collect()
    }
}

impl SpaceApp {
    fn terminal_focused(&self, key: u64, window: &Window, cx: &App) -> bool {
        self.terminals.selected_view(key).is_some_and(|v| v.read(cx).focus.contains_focused(window, cx))
    }

    pub(crate) fn focus_terminal(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        match self.terminals.selected_view(key) {
            Some(view) => {
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
            }
            // Its view is made when the pane is drawn; a terminal still starting has none.
            None => {
                self.terminals.focus_next = self.terminals.selected(key).map(|s| s.id.clone());
                let focus = self.pane_focus.clone();
                window.focus(&focus, cx);
            }
        }
    }

    /// A terminal's view, made the first time it's shown.
    pub(crate) fn terminal_view(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        if let Some(view) = self.terminals.views.get(id) {
            return Some(view.clone());
        }
        let session = self.terminals.session(id)?;
        let host = self.host.clone();
        let view = cx.new(|cx| TerminalView::new(session, host, window, cx));
        let sub = cx.subscribe_in(&view, window, move |this, view, event: &TerminalViewEvent, window, cx| match event {
            TerminalViewEvent::Restart => {
                let id = view.read(cx).session.id.clone();
                this.restart_terminal(&id, window, cx);
            }
        });
        self._subscriptions.push(sub);
        self.terminals.views.insert(id.to_string(), view.clone());
        if self.terminals.focus_next.as_deref() == Some(id) {
            self.terminals.focus_next = None;
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        }
        Some(view)
    }

    pub(crate) fn pane_open(&self, key: u64) -> bool {
        self.terminals.panes.get(&key).is_some_and(|p| p.open)
    }

    /// The title bar's button: hide the pane, or show it as it was (a new tab the first time).
    pub(crate) fn toggle_right_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        let Some(key) = self.selected else { return };
        if self.pane_open(key) {
            self.hide_terminal(window, cx);
            return;
        }
        let pane = self.terminals.panes.entry(key).or_default();
        pane.open = true;
        if pane.tabs.is_empty() {
            pane.add(PaneTab::Launcher);
        }
        self.pane_tab_shown(key, window, cx);
    }

    /// ⌘J / ⌃` (a terminal), ⌘P (the tree), ⌘⇧L (a side chat) and ⌃⇧G (Changes): show that
    /// tab; when it's shown and focused already, hide the pane and go back to the composer.
    /// ⌘J goes to the terminal shown last, or starts one; ⌘⇧L starts a new side chat.
    pub(crate) fn show_pane_view(&mut self, view: PaneView, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        let Some(key) = self.selected else { return };
        let pane = self.terminals.panes.get(&key).filter(|p| p.open);
        let active = pane.and_then(|p| p.active_tab()).cloned();
        let in_use = match (view, &active) {
            (PaneView::Files, Some(PaneTab::Files)) => self.files.focus.contains_focused(window, cx),
            (PaneView::Terminal, Some(PaneTab::Terminal(_))) => self.terminal_focused(key, window, cx),
            (PaneView::SideChat, Some(PaneTab::SideChat(side))) => self.side_chat_focused(*side, window, cx),
            (PaneView::Changes, Some(PaneTab::Changes)) => self.changes_focus.contains_focused(window, cx),
            _ => false,
        };
        if in_use {
            self.hide_terminal(window, cx);
            return;
        }
        let tab = match view {
            PaneView::Files => PaneTab::Files,
            PaneView::SideChat => {
                self.new_side_chat(window, cx);
                return;
            }
            PaneView::Changes => PaneTab::Changes,
            PaneView::Terminal => {
                let pane = self.terminals.panes.get(&key);
                let shown = active.clone().filter(|t| matches!(t, PaneTab::Terminal(_)));
                let last = pane.and_then(|p| p.last_terminal.clone()).map(PaneTab::Terminal).filter(|t| pane.is_some_and(|p| p.position(t).is_some()));
                let first = pane.and_then(|p| p.terminals().next()).map(|id| PaneTab::Terminal(id.to_string()));
                match shown.or(last).or(first) {
                    Some(tab) => tab,
                    None => return self.new_terminal(window, cx),
                }
            }
        };
        self.show_pane_tab(key, tab, window, cx);
    }

    /// Point the pane's slide at whether the current thread's pane is open: it slides in and
    /// out as it opens and hides, and snaps when the thread changes (another thread's pane is
    /// just there) or under reduced motion. Called where a pane opens or closes, so the slide
    /// starts then, and on every render.
    pub(crate) fn terminal_slide(&mut self, cx: &mut Context<Self>) {
        let open = self.selected.is_some_and(|k| {
            !self.show_settings && self.terminals.pane(k).is_some_and(|p| p.open)
        });
        let target = if open { 1. } else { 0. };
        if self.terminal_shown_for != self.selected {
            self.terminal_shown.snap(target);
            self.terminal_shown_for = self.selected;
        } else if self.terminal_shown.target() != target {
            // It travels its own width.
            crate::app::slide(&mut self.terminal_shown, target, px(self.terminals.width), cx);
        }
    }

    /// Bring the maximize slide in line with the current thread's pane: it grows over the chat
    /// as it maximizes and shrinks back as it's restored, travelling the chat's width (timed like
    /// every panel); it snaps when the thread changes. Called on every render.
    pub(crate) fn maximize_slide(&mut self, window: &Window, cx: &mut Context<Self>) {
        let maximized = self.selected.and_then(|k| self.terminals.pane(k)).is_some_and(|p| p.open && p.maximized);
        let target = if maximized { 1. } else { 0. };
        if self.pane_max_for != self.selected {
            self.pane_max.snap(target);
            self.pane_max_for = self.selected;
        } else if self.pane_max.target() != target {
            let chat = self.main_width(window) - self.pane_width(window);
            crate::app::slide(&mut self.pane_max, target, px(chat.max(0.)), cx);
        }
    }

    /// How far the current thread's pane is maximized now (0..1), and whether it's moving.
    pub(crate) fn pane_max_at(&self, cx: &App) -> (f32, bool) {
        let clock = space_ui::clock::AnimationClock::global(cx);
        self.pane_max.at(clock.now(), clock.pinned())
    }

    pub(crate) fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(pane) = self.terminals.panes.get_mut(&key) {
            pane.open = false;
            pane.maximized = false;
        }
        self.terminal_slide(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// A new terminal tab in the thread's pane (the launcher's Terminal, or ⌘J with none open).
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        let Some(key) = self.selected else { return };
        self.terminals.panes.entry(key).or_default().open = true;
        self.open_terminal(key, None, window, cx);
        self.pane_tab_shown(key, window, cx);
    }

    /// Start a shell for thread `key` (in place of terminal tab `replace`, else as a new tab,
    /// shown), attach to it, and focus it if it's shown.
    fn open_terminal(&mut self, key: u64, replace: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let (cols, rows) = self.terminals.selected(key).map_or((80, 24), |s| s.size());
        let open = TerminalOpen { thread_id: thread.id.clone(), cwd: thread.cwd.display().to_string(), cols, rows };
        self.terminals.starting += 1;
        let placeholder = PaneTab::Terminal(format!("{STARTING}{}", self.terminals.starting));
        let pane = self.terminals.panes.entry(key).or_default();
        match replace {
            Some(old) => {
                if !pane.replace(&PaneTab::Terminal(old), placeholder.clone()) {
                    pane.add(placeholder.clone());
                }
            }
            None => pane.add(placeholder.clone()),
        }
        if !self.host.has_terminal() {
            pane.failed.insert(tab_id(&placeholder), "The terminal plugin isn't running.".into());
            return;
        }
        let host = self.host.clone();
        cx.spawn_in(window, async move |this, cx| {
            let opened = host.terminal_open(open.clone()).await;
            let session = match opened {
                Ok(o) => {
                    let session = Arc::new(Session::new(o.terminal_id, o.title, cols, rows));
                    let _ = this.update_in(cx, |this, window, cx| {
                        match this.add_session(key, session.clone(), Some(&placeholder)) {
                            Some(true) if this.selected == Some(key) && this.pane_open(key) => this.focus_terminal(key, window, cx),
                            Some(_) => {}
                            // Its tab closed while it started.
                            None => {
                                let (host, id) = (this.host.clone(), session.id.clone());
                                cx.background_spawn(async move { host.terminal_close(&id).await }).detach();
                            }
                        }
                        cx.notify();
                    });
                    session
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pane) = this.terminals.panes.get_mut(&key) {
                            pane.failed.insert(tab_id(&placeholder), format!("Couldn't start a terminal: {e:#}"));
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            match host.terminal_attach(&session.id).await {
                Ok(replay) => session.apply_replay(replay, &host),
                Err(e) => tracing::warn!("terminal attach: {e:#}"),
            }
            let _ = this.update(cx, |this, cx| {
                this.notify_terminal(&session.id, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A session's tab (the output thread knows it from here on; its view is made when shown):
    /// in place of its starting tab, or at the end (found running when the plugin started).
    /// Returns whether it's the tab shown; `None` when its starting tab was closed meanwhile.
    fn add_session(&mut self, key: u64, session: Arc<Session>, replace: Option<&PaneTab>) -> Option<bool> {
        let id = session.id.clone();
        let pane = self.terminals.panes.entry(key).or_default();
        let tab = PaneTab::Terminal(id.clone());
        match replace {
            Some(old) => {
                if !pane.replace(old, tab.clone()) {
                    return None;
                }
            }
            None => {
                pane.tabs.push(tab.clone());
                if pane.tabs.len() == 1 {
                    pane.select(0);
                }
            }
        }
        let shown = pane.active_tab() == Some(&tab);
        self.terminals.sessions.lock().unwrap().insert(id, session);
        Some(shown)
    }

    /// Forget a terminal here (its shell is closed separately).
    fn drop_session(&mut self, id: &str) {
        self.terminals.sessions.lock().unwrap().remove(id);
        self.terminals.views.remove(id);
    }

    /// A new shell in place of an ended (or stopped) one, or one that couldn't start.
    pub(crate) fn restart_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.terminals.panes.iter().find_map(|(k, p)| p.terminals().any(|t| t == id).then_some(*k)) else {
            return;
        };
        self.close_shell(id, cx);
        if let Some(pane) = self.terminals.panes.get_mut(&key) {
            pane.failed.remove(id);
        }
        self.open_terminal(key, Some(id.to_string()), window, cx);
        cx.notify();
    }

    /// Close a terminal's shell and forget it here.
    pub(crate) fn close_shell(&mut self, id: &str, cx: &mut Context<Self>) {
        if id.starts_with(STARTING) {
            return;
        }
        let (host, gone) = (self.host.clone(), id.to_string());
        cx.background_spawn(async move { host.terminal_close(&gone).await }).detach();
        self.drop_session(id);
    }

    /// Maximize the open pane (it fills the window beside the sidebar), or restore it.
    pub(crate) fn toggle_pane_maximized(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(pane) = self.terminals.panes.get_mut(&key).filter(|p| p.open) {
            pane.maximized = !pane.maximized;
            cx.notify();
        }
    }

    /// Draw a terminal again (its output arrived, or it ended).
    pub(crate) fn notify_terminal(&self, id: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.views.get(id) {
            view.update(cx, |_, cx| cx.notify());
        }
    }

    /// `HostEvent::TerminalOutput`: redraw it if it's on screen; else its tab and thread show
    /// there's something new (returns whether the rest of the app needs a redraw).
    pub(crate) fn terminal_output(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.terminals.session(id) else { return false };
        if session.visible.load(Ordering::SeqCst) {
            self.notify_terminal(id, cx);
            false
        } else {
            true
        }
    }

    pub(crate) fn terminal_title(&mut self, id: &str, title: String) {
        if let Some(session) = self.terminals.session(id) {
            session.state.lock().unwrap().title = title;
        }
    }

    /// The shell ended (`exit`, ⌃D): its tab closes, as closing it would, and the pane with
    /// its last tab. Ended shells found when attaching (`replay`) keep their notice instead.
    pub(crate) fn terminal_exited(&mut self, id: &str, exit_code: Option<i32>, cx: &mut Context<Self>) {
        let Some(session) = self.terminals.session(id) else { return };
        session.state.lock().unwrap().exited = Some(exit_code);
        self.notify_terminal(id, cx);
        // After this event's update: closing needs the window (focus moves).
        let (handle, id) = (self.window_handle, id.to_string());
        cx.spawn(async move |this, cx| {
            let _ = handle.update(cx, |_, window, cx| this.update(cx, |this, cx| this.close_exited_terminal(&id, window, cx)));
        })
        .detach();
    }

    fn close_exited_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.terminals.panes.iter().find_map(|(k, p)| p.terminals().any(|t| t == id).then_some(*k)) else { return };
        // Focus moves only if it was in this terminal: a shell ending elsewhere doesn't take it.
        let had_focus = self.terminals.views.get(id).is_some_and(|v| v.read(cx).focus.is_focused(window));
        self.remove_pane_tab(key, &PaneTab::Terminal(id.to_string()), had_focus, window, cx);
    }

    /// The terminal plugin's status changed: attach to what it runs once it's up; mark its
    /// terminals stopped when it goes.
    pub(crate) fn terminal_plugin_status(&mut self, running: bool, cx: &mut Context<Self>) {
        if !running {
            for s in self.terminals.all() {
                s.state.lock().unwrap().stopped = true;
            }
            for id in self.terminals.views.keys().cloned().collect::<Vec<_>>() {
                self.notify_terminal(&id, cx);
            }
            return;
        }
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let Ok(live) = host.terminal_list().await else { return };
            let ids: HashSet<String> = live.iter().map(|t| t.terminal_id.clone()).collect();
            let mut attach = Vec::new();
            let _ = this.update(cx, |this, cx| {
                // Ours that it no longer runs: stopped.
                for s in this.terminals.all().into_iter().filter(|s| !ids.contains(&s.id)) {
                    s.state.lock().unwrap().stopped = true;
                }
                for info in live {
                    if this.terminals.session(&info.terminal_id).is_some() {
                        continue;
                    }
                    let Some(key) = this.threads.iter().find(|t| t.id == info.thread_id).map(|t| t.key) else {
                        // A thread that's gone: nothing will show it.
                        let (host, id) = (this.host.clone(), info.terminal_id.clone());
                        cx.background_spawn(async move { host.terminal_close(&id).await }).detach();
                        continue;
                    };
                    let session = Arc::new(Session::new(info.terminal_id, info.title, 80, 24));
                    attach.push((key, session));
                }
                cx.notify();
            });
            for (key, session) in attach {
                let _ = this.update(cx, |this, _| this.add_session(key, session.clone(), None));
                if let Ok(replay) = host.terminal_attach(&session.id).await {
                    session.apply_replay(replay, &host);
                }
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// A thread was deleted: its pane goes (the plugin closes its shells).
    /// Stop a thread's terminals (it was closed) and forget them.
    pub(crate) fn close_thread_terminals(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(pane) = self.terminals.panes.remove(&key) else { return };
        for id in pane.terminals() {
            self.close_shell(id, cx);
        }
    }

    /// The thread has terminals open.
    pub(crate) fn has_terminals(&self, key: u64) -> bool {
        self.terminals.panes.get(&key).is_some_and(|p| p.terminals().next().is_some())
    }

    /// ⌘Q and Space ▸ Quit Space: always ask first, saying what quitting stops. A second ⌘Q
    /// while the alert is up does nothing. (The Dock, logging out and shutting down don't come
    /// through here.)
    pub(crate) fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.asking_to_quit {
            return;
        }
        let working = self.threads.iter().filter(|t| t.state.running).count();
        let programs: Vec<String> = self.terminals.busy().iter().map(|s| s.state.lock().unwrap().title.clone()).collect();
        let detail = quit_detail(working, &programs);
        let level = if detail.is_some() { PromptLevel::Warning } else { PromptLevel::Info };
        let answer = window.prompt(level, "Quit Space?", detail.as_deref(), &["Quit", "Cancel"], cx);
        self.asking_to_quit = true;
        cx.spawn(async move |this, cx| {
            let quit = answer.await == Ok(0);
            let _ = this.update(cx, |this, _| this.asking_to_quit = false);
            if quit {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }
}

/// The quit alert's detail line: what quitting stops, most important first, in one sentence
/// (`None` when nothing is running).
pub(crate) fn quit_detail(working_threads: usize, programs: &[String]) -> Option<String> {
    let threads = match working_threads {
        0 => None,
        1 => Some("1 thread is still working".to_string()),
        n => Some(format!("{n} threads are still working")),
    };
    let terminals = match programs {
        [] => None,
        [one] => Some(format!("{one} is still running in a terminal")),
        many => Some(format!("{} programs are still running in terminals ({})", many.len(), many.join(", "))),
    };
    match (threads, terminals) {
        (None, None) => None,
        (Some(t), None) | (None, Some(t)) => Some(format!("{t}.")),
        (Some(t), Some(p)) => Some(format!("{t}, and {p}.")),
    }
}

/// A terminal tab's id.
fn tab_id(tab: &PaneTab) -> String {
    match tab {
        PaneTab::Terminal(id) => id.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod quit_tests {
    use super::quit_detail;

    #[test]
    fn the_quit_alert_says_what_stops() {
        let progs = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(quit_detail(0, &[]), None);
        assert_eq!(quit_detail(2, &[]).as_deref(), Some("2 threads are still working."));
        assert_eq!(quit_detail(1, &[]).as_deref(), Some("1 thread is still working."));
        assert_eq!(quit_detail(0, &progs(&["vim"])).as_deref(), Some("vim is still running in a terminal."));
        assert_eq!(
            quit_detail(1, &progs(&["vim", "cargo"])).as_deref(),
            Some("1 thread is still working, and 2 programs are still running in terminals (vim, cargo).")
        );
    }
}
