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
use space_ui::components::terminal::{FONT_SIZE, TerminalTab};

use crate::app::SpaceApp;
use crate::terminal_view::{TerminalView, TerminalViewEvent};

/// Shells: a tab whose title is one of these has nothing running in it.
const SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "login", "xonsh", "elvish"];

/// Pane width as a share of the main column.
const WIDTH_DEFAULT: f32 = 0.45;
const WIDTH_MIN: f32 = 0.2;
const WIDTH_MAX: f32 = 0.8;

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

/// A thread's pane.
#[derive(Default)]
pub(crate) struct Pane {
    pub(crate) open: bool,
    pub(crate) maximized: bool,
    /// Terminal ids, in tab order.
    pub(crate) tabs: Vec<String>,
    pub(crate) selected: usize,
    /// Shells being started.
    pub(crate) opening: usize,
    /// Why the last one couldn't start.
    pub(crate) error: Option<String>,
}

pub(crate) struct Terminals {
    /// By terminal id; shared with the output thread.
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    pub(crate) views: HashMap<String, Entity<TerminalView>>,
    /// By thread key.
    pub(crate) panes: HashMap<u64, Pane>,
    /// The pane's share of the main column.
    pub(crate) width: f32,
    /// The divider is being dragged.
    pub(crate) dragging: bool,
    /// Focus this terminal once its view exists.
    focus_next: Option<String>,
}

impl Terminals {
    /// Route the host's terminal output to the screens.
    pub(crate) fn new(host: &Host) -> Self {
        let sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> = Arc::default();
        let (map, h) = (sessions.clone(), host.clone());
        host.set_terminal_sink(Arc::new(move |id, seq, data| {
            let session = map.lock().unwrap().get(id).cloned();
            session.is_some_and(|s| s.receive(seq, data, &h))
        }));
        Self { sessions, views: HashMap::new(), panes: HashMap::new(), width: WIDTH_DEFAULT, dragging: false, focus_next: None }
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

    /// The pane's selected terminal.
    pub(crate) fn selected(&self, key: u64) -> Option<Arc<Session>> {
        let pane = self.panes.get(&key)?;
        self.session(pane.tabs.get(pane.selected)?)
    }

    pub(crate) fn selected_view(&self, key: u64) -> Option<Entity<TerminalView>> {
        let pane = self.panes.get(&key)?;
        self.views.get(pane.tabs.get(pane.selected)?).cloned()
    }

    /// A thread has a terminal with something running, or output it hasn't seen.
    pub(crate) fn active(&self, key: u64) -> bool {
        self.panes.get(&key).is_some_and(|p| {
            p.tabs.iter().filter_map(|id| self.session(id)).any(|s| s.busy() || s.unread.load(Ordering::SeqCst))
        })
    }

    /// Mark which terminal is on screen (the rest wait to be drawn).
    pub(crate) fn set_visible(&self, shown: Option<&str>) {
        for s in self.all() {
            s.visible.store(shown == Some(s.id.as_str()), Ordering::SeqCst);
        }
    }

    pub(crate) fn set_width(&mut self, width: f32) {
        self.width = width.clamp(WIDTH_MIN, WIDTH_MAX);
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

    fn focus_terminal(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        match self.terminals.selected_view(key) {
            Some(view) => {
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
            }
            // Its view is made when the pane is drawn.
            None => self.terminals.focus_next = self.terminals.selected(key).map(|s| s.id.clone()),
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

    /// ⌘J (or ⌃`): open the thread's pane (a shell the first time) and focus it; from the terminal,
    /// hide it and go back to the composer.
    pub(crate) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings || !self.host.has_terminal() {
            return;
        }
        let Some(key) = self.selected else { return };
        let open = self.terminals.panes.get(&key).is_some_and(|p| p.open);
        if open && self.terminal_focused(key, window, cx) {
            self.hide_terminal(window, cx);
            return;
        }
        let pane = self.terminals.panes.entry(key).or_default();
        pane.open = true;
        if pane.tabs.is_empty() && pane.opening == 0 {
            self.open_terminal(key, None, window, cx);
        }
        self.focus_terminal(key, window, cx);
        cx.notify();
    }

    pub(crate) fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(pane) = self.terminals.panes.get_mut(&key) {
            pane.open = false;
            pane.maximized = false;
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// ⌘T or `+`: another shell in the thread's pane.
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings || !self.host.has_terminal() {
            return;
        }
        let Some(key) = self.selected else { return };
        self.terminals.panes.entry(key).or_default().open = true;
        self.open_terminal(key, None, window, cx);
        cx.notify();
    }

    /// Start a shell for thread `key` (in place of tab `replace`, else as a new tab), attach to
    /// it, select it and focus it.
    fn open_terminal(&mut self, key: u64, replace: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key) else { return };
        let (cols, rows) = self.terminals.selected(key).map_or((80, 24), |s| s.size());
        let open = TerminalOpen { thread_id: thread.id.clone(), cwd: thread.cwd.display().to_string(), cols, rows };
        let pane = self.terminals.panes.entry(key).or_default();
        pane.opening += 1;
        pane.error = None;
        let host = self.host.clone();
        cx.spawn_in(window, async move |this, cx| {
            let opened = host.terminal_open(open.clone()).await;
            let session = match opened {
                Ok(o) => {
                    let session = Arc::new(Session::new(o.terminal_id, o.title, cols, rows));
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.add_session(key, session.clone(), replace);
                        if let Some(pane) = this.terminals.panes.get_mut(&key) {
                            pane.opening = pane.opening.saturating_sub(1);
                        }
                        if this.selected == Some(key) && this.terminals.pane(key).is_some_and(|p| p.open) {
                            this.focus_terminal(key, window, cx);
                        }
                        cx.notify();
                    });
                    session
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        let pane = this.terminals.panes.entry(key).or_default();
                        pane.opening = pane.opening.saturating_sub(1);
                        pane.error = Some(format!("Couldn't start a terminal: {e:#}"));
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

    /// A session's tab (the output thread knows it from here on; its view is made when shown).
    fn add_session(&mut self, key: u64, session: Arc<Session>, replace: Option<usize>) {
        let id = session.id.clone();
        self.terminals.sessions.lock().unwrap().insert(id.clone(), session);
        let pane = self.terminals.panes.entry(key).or_default();
        match replace.filter(|ix| *ix < pane.tabs.len()) {
            Some(ix) => {
                pane.tabs[ix] = id;
                pane.selected = ix;
            }
            None => {
                pane.tabs.push(id);
                pane.selected = pane.tabs.len() - 1;
            }
        }
    }

    /// Forget a terminal here (its shell is closed separately).
    fn drop_session(&mut self, id: &str) {
        self.terminals.sessions.lock().unwrap().remove(id);
        self.terminals.views.remove(id);
    }

    /// A new shell in place of an ended (or stopped) one.
    pub(crate) fn restart_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, ix)) = self.terminals.panes.iter().find_map(|(k, p)| p.tabs.iter().position(|t| t == id).map(|ix| (*k, ix))) else {
            return;
        };
        let (host, old) = (self.host.clone(), id.to_string());
        cx.background_spawn(async move { host.terminal_close(&old).await }).detach();
        self.drop_session(id);
        self.open_terminal(key, Some(ix), window, cx);
        cx.notify();
    }

    /// Close tab `ix` of the current thread's pane, asking first when something runs in it.
    pub(crate) fn close_terminal(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(id) = self.terminals.panes.get(&key).and_then(|p| p.tabs.get(ix)).cloned() else { return };
        let Some(session) = self.terminals.session(&id) else {
            return self.remove_tab(key, &id, true, window, cx);
        };
        if !session.busy() {
            return self.remove_tab(key, &id, true, window, cx);
        }
        let title = session.state.lock().unwrap().title.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Close the terminal running {title}?"),
            Some("Closing it stops what's running in it."),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.remove_tab(key, &id, true, window, cx));
            }
        })
        .detach();
    }

    /// ⌘W in the terminal.
    pub(crate) fn close_current_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(ix) = self.terminals.panes.get(&key).map(|p| p.selected) {
            self.close_terminal(ix, window, cx);
        }
    }

    /// Close the tab and its shell; with `refocus`, focus goes to the pane's next tab, or the
    /// composer once the pane closes.
    fn remove_tab(&mut self, key: u64, id: &str, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (host, gone) = (self.host.clone(), id.to_string());
        cx.background_spawn(async move { host.terminal_close(&gone).await }).detach();
        self.drop_session(id);
        let Some(pane) = self.terminals.panes.get_mut(&key) else { return };
        if let Some(ix) = pane.tabs.iter().position(|t| t == id) {
            pane.tabs.remove(ix);
            if pane.selected >= ix && pane.selected > 0 {
                pane.selected -= 1;
            }
        }
        let closed = pane.tabs.is_empty();
        if closed {
            pane.open = false;
            pane.maximized = false;
        }
        if refocus && self.selected == Some(key) {
            if closed {
                self.focus_composer(window, cx);
            } else {
                self.focus_terminal(key, window, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn select_terminal(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(pane) = self.terminals.panes.get_mut(&key)
            && ix < pane.tabs.len()
        {
            pane.selected = ix;
            self.focus_terminal(key, window, cx);
            cx.notify();
        }
    }

    /// ⌃Tab / ⌃⇧Tab.
    pub(crate) fn step_terminal(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(pane) = self.terminals.panes.get(&key).filter(|p| !p.tabs.is_empty()) else { return };
        let n = pane.tabs.len() as isize;
        let ix = (pane.selected as isize + by).rem_euclid(n) as usize;
        self.select_terminal(ix, window, cx);
    }

    pub(crate) fn toggle_terminal_maximized(&mut self, cx: &mut Context<Self>) {
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
        let Some(key) = self.terminals.panes.iter().find_map(|(k, p)| p.tabs.iter().any(|t| t == id).then_some(*k)) else { return };
        // Focus moves only if it was in this terminal: a shell ending elsewhere doesn't take it.
        let had_focus = self.terminals.views.get(id).is_some_and(|v| v.read(cx).focus.is_focused(window));
        self.remove_tab(key, id, had_focus, window, cx);
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
        for id in pane.tabs {
            self.drop_session(&id);
            let host = self.host.clone();
            cx.background_spawn(async move { host.terminal_close(&id).await }).detach();
        }
    }

    /// The thread has terminals open.
    pub(crate) fn has_terminals(&self, key: u64) -> bool {
        self.terminals.panes.get(&key).is_some_and(|p| !p.tabs.is_empty())
    }

    /// ⌘Q: ask first when something runs in a terminal.
    pub(crate) fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let busy = self.terminals.busy();
        if busy.is_empty() {
            cx.quit();
            return;
        }
        let names: Vec<String> = busy.iter().map(|s| s.state.lock().unwrap().title.clone()).collect();
        let message = if names.len() == 1 {
            format!("{} is still running in a terminal. Quit anyway?", names[0])
        } else {
            format!("{} programs are still running in terminals ({}). Quit anyway?", names.len(), names.join(", "))
        };
        let answer = window.prompt(PromptLevel::Warning, &message, Some("Quitting stops them."), &["Quit", "Cancel"], cx);
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }
}
