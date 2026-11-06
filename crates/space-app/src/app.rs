//! The main window: plugin sidebar, chat transcript, composer, log panel.
//!
//! Bridge: `space-core` runs on a tokio runtime. Its async methods spawn onto that runtime
//! internally, so they are awaited directly from GPUI tasks (`cx.spawn`). Host events arrive
//! on a tokio mpsc receiver which a GPUI foreground task drains and applies to this view.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use gpui::component::button::{Button, ButtonVariants as _};
use gpui::component::input::{InputEvent, Textarea, TextareaState};
use gpui::component::{Disableable as _, Sizable as _, Theme, ThemeMode};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::{
    AgentHandle, DiscoveredPlugin, EventReceiver, Host, HostEvent, PermissionDecision, PermissionRequest,
    PluginStatus, Session, SessionEvent, SessionId, SessionState, ToolCallInfo, ToolStatus, TranscriptItem,
};

use crate::theme;

const MAX_LOG_LINES: usize = 1000;

pub fn run(host: Host, events: EventReceiver, project_dir: PathBuf) {
    gpui::application().run(move |cx: &mut App| {
        gpui::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("Space".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui::open_window(options, cx, |window, cx| cx.new(|cx| SpaceApp::new(host, events, project_dir, window, cx)))
            .expect("failed to open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

struct PluginRow {
    plugin: DiscoveredPlugin,
    status: PluginStatus,
    agent: Option<AgentHandle>,
    logs: VecDeque<String>,
}

pub struct SpaceApp {
    host: Host,
    project_dir: PathBuf,
    plugins: Vec<PluginRow>,
    discovery_errors: Vec<String>,
    /// Session handles in creation order.
    sessions: Vec<Session>,
    /// Folded state per session id (built purely from events).
    states: HashMap<SessionId, SessionState>,
    active: Option<SessionId>,
    expanded_thoughts: HashSet<(SessionId, usize)>,
    input: Entity<TextareaState>,
    transcript_scroll: ScrollHandle,
    log_scroll: ScrollHandle,
    show_logs: bool,
    banner: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SpaceApp {
    fn new(
        host: Host,
        mut events: EventReceiver,
        project_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("Message the agent…  (Enter to send, Shift+Enter for newline)");
            state.set_auto_grow(2, 8, cx);
            state.set_submit_on_enter(true, cx);
            state
        });
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.send(window, cx);
            }
        });

        // Bridge host events into the view.
        cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(e) = events.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |this, cx| {
                    for e in batch {
                        this.on_host_event(e);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let report = host.discover();
        let plugins = report
            .plugins
            .into_iter()
            .map(|plugin| PluginRow { plugin, status: PluginStatus::Stopped, agent: None, logs: VecDeque::new() })
            .collect();
        let mut this = Self {
            host,
            project_dir,
            plugins,
            discovery_errors: report.errors.iter().map(|e| e.to_string()).collect(),
            sessions: Vec::new(),
            states: HashMap::new(),
            active: None,
            expanded_thoughts: HashSet::new(),
            input,
            transcript_scroll: ScrollHandle::new(),
            log_scroll: ScrollHandle::new(),
            show_logs: false,
            banner: None,
            _subscriptions: vec![sub],
        };

        if std::env::var_os("SPACE_AUTOSTART").is_some()
            && let Some(id) = this.first_agent_id()
        {
            let prompt = std::env::var("SPACE_AUTOPROMPT").ok().filter(|p| !p.is_empty());
            this.start_agent_and_session(id, prompt, cx);
        }
        this
    }

    fn first_agent_id(&self) -> Option<String> {
        self.plugins
            .iter()
            .find(|r| r.plugin.manifest.kind == space_core::protocol::PluginKind::Agent)
            .map(|r| r.plugin.manifest.id.clone())
    }

    fn row_mut(&mut self, plugin_id: &str) -> Option<&mut PluginRow> {
        self.plugins.iter_mut().find(|r| r.plugin.manifest.id == plugin_id)
    }

    fn on_host_event(&mut self, event: HostEvent) {
        match event {
            HostEvent::PluginStatus { plugin_id, status } => {
                if let Some(row) = self.row_mut(&plugin_id) {
                    if matches!(status, PluginStatus::Stopped | PluginStatus::Crashed(_)) {
                        row.agent = None;
                    }
                    row.status = status;
                }
            }
            HostEvent::PluginLog { plugin_id, line } => {
                if let Some(row) = self.row_mut(&plugin_id) {
                    if row.logs.len() == MAX_LOG_LINES {
                        row.logs.pop_front();
                    }
                    row.logs.push_back(line);
                }
                if self.show_logs {
                    self.log_scroll.scroll_to_bottom();
                }
            }
            HostEvent::Session { session_id, event } => {
                let state = self.states.entry(session_id.clone()).or_insert_with(|| SessionState::new(&session_id));
                state.apply(&event);
                if self.active.as_ref() == Some(&session_id) && !matches!(event, SessionEvent::Status(_)) {
                    self.transcript_scroll.scroll_to_bottom();
                }
            }
        }
    }

    // ----- actions -------------------------------------------------------------------------

    /// Start `plugin_id` if needed, create a session in the project dir, optionally prompt.
    fn start_agent_and_session(&mut self, plugin_id: String, prompt: Option<String>, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let cwd = self.project_dir.clone();
        let existing = self.plugins.iter().find(|r| r.plugin.manifest.id == plugin_id).and_then(|r| r.agent.clone());
        self.banner = None;
        cx.spawn(async move |this, cx| {
            let result = async {
                let agent = match existing {
                    Some(a) if a.is_running() => a,
                    _ => {
                        let agent = host.start_agent(&plugin_id).await?;
                        let a = agent.clone();
                        let id = plugin_id.clone();
                        this.update(cx, |this, cx| {
                            if let Some(row) = this.row_mut(&id) {
                                row.agent = Some(a);
                            }
                            cx.notify();
                        })?;
                        agent
                    }
                };
                let session = agent.new_session(cwd).await?;
                anyhow::Ok(session)
            }
            .await;
            match result {
                Ok(session) => {
                    let _ = this.update(cx, |this, cx| {
                        this.active = Some(session.id().to_string());
                        this.sessions.push(session.clone());
                        cx.notify();
                    });
                    if let Some(prompt) = prompt {
                        let _ = session.prompt(prompt).await;
                    }
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        this.banner = Some(format!("{e:#}"));
                        this.show_logs = true;
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn stop_agent(&mut self, plugin_id: &str, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(plugin_id)
            && let Some(agent) = row.agent.take()
        {
            agent.stop();
        }
        cx.notify();
    }

    fn active_session(&self) -> Option<&Session> {
        let id = self.active.as_ref()?;
        self.sessions.iter().find(|s| s.id() == id)
    }

    fn active_state(&self) -> Option<&SessionState> {
        self.states.get(self.active.as_ref()?)
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(session) = self.active_session().cloned() else {
            self.banner = Some("Start an agent session first.".into());
            cx.notify();
            return;
        };
        if session.is_running() || !session.agent().is_running() {
            return;
        }
        self.input.update(cx, |s, cx| s.clean(window, cx));
        cx.spawn(async move |_, _| {
            let _ = session.prompt(text).await;
        })
        .detach();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.active_session()
            && let Err(e) = session.cancel()
        {
            self.banner = Some(format!("{e:#}"));
            cx.notify();
        }
    }

    fn answer_permission(&mut self, request_id: u64, decision: PermissionDecision, cx: &mut Context<Self>) {
        if let Some(session) = self.active_session()
            && let Err(e) = session.resolve_permission(request_id, decision)
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    // ----- rendering -----------------------------------------------------------------------

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_2().px_3().py_3().flex_1().min_h_0();
        list = list.child(section_label("PLUGINS"));
        if self.plugins.is_empty() {
            list = list.child(div().text_sm().text_color(theme::muted()).child("No plugins found."));
        }
        for (ix, row) in self.plugins.iter().enumerate() {
            list = list.child(self.render_plugin_row(ix, row, cx));
        }
        for err in &self.discovery_errors {
            list = list.child(div().text_xs().text_color(theme::red()).child(err.clone()));
        }

        let project = self.project_dir.display().to_string();
        div()
            .flex()
            .flex_col()
            .w(px(260.))
            .h_full()
            .flex_none()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Space"))
                    .child(div().text_xs().text_color(theme::muted()).truncate().child(project)),
            )
            .child(list)
            .child(
                div().p_3().border_t_1().border_color(theme::border()).child(
                    Button::new("toggle-logs")
                        .ghost()
                        .small()
                        .label(if self.show_logs { "Hide plugin logs" } else { "Show plugin logs" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_logs = !this.show_logs;
                            this.log_scroll.scroll_to_bottom();
                            cx.notify();
                        })),
                ),
            )
    }

    fn render_plugin_row(&self, ix: usize, row: &PluginRow, cx: &mut Context<Self>) -> impl IntoElement {
        let m = &row.plugin.manifest;
        let id = m.id.clone();
        let (dot, label) = match &row.status {
            PluginStatus::Stopped => (theme::faint(), "stopped"),
            PluginStatus::Starting => (theme::yellow(), "starting"),
            PluginStatus::Running => (theme::green(), "running"),
            PluginStatus::Crashed(_) => (theme::red(), "crashed"),
        };
        let is_agent = m.kind == space_core::protocol::PluginKind::Agent;
        let running = row.status == PluginStatus::Running;
        let starting = row.status == PluginStatus::Starting;

        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2p5()
            .rounded_md()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(8.)).rounded_full().bg(dot))
                    .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(m.name.clone()))
                    .child(tag(m.kind.as_str())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(format!("{label} · v{} · {}", m.version, source_label(row.plugin.source))),
            );
        if let PluginStatus::Crashed(msg) = &row.status {
            card = card.child(div().text_xs().text_color(theme::red()).child(msg.clone()));
        }

        if is_agent {
            let mut buttons = div().flex().gap_2();
            let start_id = id.clone();
            buttons = buttons.child(
                Button::new(("new-session", ix))
                    .primary()
                    .xsmall()
                    .label(if running { "New session" } else { "Start" })
                    .loading(starting)
                    .disabled(starting)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_agent_and_session(start_id.clone(), None, cx);
                    })),
            );
            if running {
                let stop_id = id.clone();
                buttons = buttons.child(
                    Button::new(("stop-agent", ix))
                        .ghost()
                        .xsmall()
                        .label("Stop")
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(&stop_id, cx))),
                );
            }
            card = card.child(buttons);
        }

        // Sessions belonging to this plugin.
        let sessions: Vec<_> = self.sessions.iter().filter(|s| s.agent().plugin_id() == m.id).collect();
        if !sessions.is_empty() {
            let mut list = div().flex().flex_col().gap_0p5();
            for (n, s) in sessions.iter().enumerate() {
                let sid = s.id().to_string();
                let active = self.active.as_deref() == Some(sid.as_str());
                let running = self.states.get(&sid).is_some_and(|st| st.running);
                let title = self
                    .states
                    .get(&sid)
                    .and_then(|st| {
                        st.items.iter().find_map(|i| match i {
                            TranscriptItem::User(t) => Some(t.lines().next().unwrap_or("").to_string()),
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| format!("Session {}", n + 1));
                list = list.child(
                    div()
                        .id(SharedString::from(format!("session-{sid}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .text_sm()
                        .cursor_pointer()
                        .when(active, |d| d.bg(theme::panel_hover()))
                        .hover(|d| d.bg(theme::panel_hover()))
                        .child(div().text_color(if running { theme::yellow() } else { theme::faint() }).child("•"))
                        .child(div().flex_1().truncate().child(title))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active = Some(sid.clone());
                            this.transcript_scroll.scroll_to_bottom();
                            cx.notify();
                        })),
                );
            }
            card = card.child(list);
        }
        card
    }

    fn render_header(&self) -> impl IntoElement {
        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_5()
            .h(px(48.))
            .flex_none()
            .border_b_1()
            .border_color(theme::border());
        match self.active_state() {
            None => header.child(div().text_sm().text_color(theme::muted()).child("No session")),
            Some(state) => {
                header = header.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if state.plugin_id.is_empty() { "session".to_string() } else { state.plugin_id.clone() }),
                );
                let s = &state.status;
                for (k, v) in [("provider", &s.provider), ("model", &s.model), ("thinking", &s.thinking_level)] {
                    if let Some(v) = v {
                        header = header.child(chip(k, v));
                    }
                }
                header = header.child(div().flex_1());
                if state.running {
                    header = header.child(div().text_xs().text_color(theme::yellow()).child("● working…"));
                }
                header.child(
                    div()
                        .max_w(px(320.))
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::faint())
                        .truncate()
                        .child(state.cwd.display().to_string()),
                )
            }
        }
    }

    fn render_transcript(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.active_state() else {
            return self.render_empty(cx).into_any_element();
        };
        let mut col = div().flex().flex_col().gap_3().px_6().py_4().w_full();
        if state.items.is_empty() {
            col = col.child(div().text_sm().text_color(theme::muted()).child("Session ready. Say something."));
        }
        for (ix, item) in state.items.iter().enumerate() {
            col = col.child(self.render_item(&state.id, ix, item, cx));
        }
        div()
            .id("transcript")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.transcript_scroll)
            .child(col)
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Welcome to Space"))
            .child(div().text_sm().text_color(theme::muted()).child("Start an agent plugin to open a session."));
        if let Some(id) = self.first_agent_id() {
            let starting = self.plugins.iter().any(|r| r.status == PluginStatus::Starting);
            body = body.child(
                Button::new("start-first")
                    .primary()
                    .label(format!("Start {id}"))
                    .loading(starting)
                    .disabled(starting)
                    .on_click(cx.listener(move |this, _, _, cx| this.start_agent_and_session(id.clone(), None, cx))),
            );
        }
        body
    }

    fn render_item(&self, sid: &SessionId, ix: usize, item: &TranscriptItem, cx: &mut Context<Self>) -> AnyElement {
        match item {
            TranscriptItem::User(text) => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.8))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(theme::user_bubble())
                        .text_sm()
                        .child(text.clone()),
                )
                .into_any_element(),
            TranscriptItem::Agent(text) => {
                div().text_sm().line_height(relative(1.5)).child(text.trim_start().to_string()).into_any_element()
            }
            TranscriptItem::Thought(text) => {
                let key = (sid.clone(), ix);
                let open = self.expanded_thoughts.contains(&key);
                let words = text.split_whitespace().count();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id(("thought", ix))
                            .text_xs()
                            .text_color(theme::muted())
                            .cursor_pointer()
                            .hover(|d| d.text_color(theme::text()))
                            .child(format!("{} Thinking ({words} words)", if open { "▾" } else { "▸" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded_thoughts.remove(&key) {
                                    this.expanded_thoughts.insert(key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .pl_3()
                                .border_l_2()
                                .border_color(theme::border())
                                .text_xs()
                                .italic()
                                .text_color(theme::muted())
                                .child(text.trim().to_string()),
                        )
                    })
                    .into_any_element()
            }
            TranscriptItem::Tool(call) => render_tool(call).into_any_element(),
            TranscriptItem::Permission { request, resolved } => {
                self.render_permission(request, resolved.as_ref(), cx).into_any_element()
            }
            TranscriptItem::Plan(entries) => {
                let mut d = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(theme::panel())
                    .child(div().text_xs().text_color(theme::muted()).child("Plan"));
                for e in entries {
                    let mark = match e.status.as_str() {
                        "completed" => "✓",
                        "in_progress" => "◐",
                        _ => "○",
                    };
                    d = d.child(div().text_sm().child(format!("{mark} {}", e.content)));
                }
                d.into_any_element()
            }
            TranscriptItem::Error(msg) => div()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(theme::red())
                .text_sm()
                .text_color(theme::red())
                .child(msg.clone())
                .into_any_element(),
            TranscriptItem::TurnEnd(reason) => div()
                .text_xs()
                .text_color(theme::faint())
                .child(format!("— turn ended: {reason} —"))
                .into_any_element(),
        }
    }

    fn render_permission(
        &self,
        request: &PermissionRequest,
        resolved: Option<&PermissionDecision>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = request.tool_call.title.clone().unwrap_or_else(|| request.tool_call.id.clone());
        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(theme::panel())
            .border_1()
            .border_color(if resolved.is_none() { theme::yellow() } else { theme::border() })
            .child(div().text_xs().text_color(theme::yellow()).child("Permission requested"))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title));
        match resolved {
            Some(decision) => {
                let text = match decision {
                    PermissionDecision::Selected(id) => {
                        let name = request.options.iter().find(|o| &o.id == id).map_or(id.as_str(), |o| o.name.as_str());
                        format!("Answered: {name}")
                    }
                    PermissionDecision::Cancelled => "Cancelled".into(),
                };
                card = card.child(div().text_xs().text_color(theme::muted()).child(text));
            }
            None => {
                let mut row = div().flex().gap_2();
                for (n, opt) in request.options.iter().enumerate() {
                    let rid = request.request_id;
                    let oid = opt.id.clone();
                    let mut b = Button::new(SharedString::from(format!("perm-{rid}-{n}"))).small().label(opt.name.clone());
                    b = if opt.kind.is_allow() { b.success() } else { b.danger() };
                    row = row.child(b.on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_permission(rid, PermissionDecision::Selected(oid.clone()), cx)
                    })));
                }
                card = card.child(row);
            }
        }
        card
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.active_state();
        let has_session = state.is_some();
        let running = state.is_some_and(|s| s.running);
        div()
            .flex_none()
            .flex()
            .items_end()
            .gap_2()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(theme::border())
            .child(div().flex_1().min_w_0().child(Textarea::new(&self.input).disabled(!has_session)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .disabled(!has_session || running)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    )
                    .child(
                        Button::new("stop")
                            .danger()
                            .label("Stop")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ),
            )
    }

    fn render_logs(&self) -> impl IntoElement {
        let plugin_id = self.active_state().map(|s| s.plugin_id.clone());
        let row = self
            .plugins
            .iter()
            .find(|r| Some(&r.plugin.manifest.id) == plugin_id.as_ref())
            .or_else(|| self.plugins.iter().find(|r| !r.logs.is_empty()))
            .or(self.plugins.first());
        let mut col = div().flex().flex_col().px_4().py_2().font_family(theme::MONO_FONT).text_xs();
        let name = row.map(|r| r.plugin.manifest.id.clone()).unwrap_or_default();
        match row {
            Some(r) if !r.logs.is_empty() => {
                for line in r.logs.iter().skip(r.logs.len().saturating_sub(300)) {
                    col = col.child(div().text_color(theme::muted()).child(line.clone()));
                }
            }
            _ => col = col.child(div().text_color(theme::faint()).child("(no stderr output yet)")),
        }
        div()
            .flex_none()
            .flex()
            .flex_col()
            .h(px(180.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::sidebar())
            .child(
                div()
                    .px_4()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::faint())
                    .child(format!("stderr · {name}")),
            )
            .child(div().id("logs").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.log_scroll).child(col))
    }
}

impl Render for SpaceApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header())
            .child(self.render_transcript(cx));
        if let Some(banner) = &self.banner {
            main = main.child(
                div()
                    .id("banner")
                    .mx_5()
                    .mb_2()
                    .p_2()
                    .rounded_md()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::red())
                    .text_xs()
                    .text_color(theme::red())
                    .cursor_pointer()
                    .child(format!("{banner}   (click to dismiss)"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.banner = None;
                        cx.notify();
                    })),
            );
        }
        main = main.child(self.render_composer(cx));
        if self.show_logs {
            main = main.child(self.render_logs());
        }

        div()
            .flex()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .child(self.render_sidebar(cx))
            .child(main)
    }
}

fn render_tool(call: &ToolCallInfo) -> impl IntoElement {
    let (color, label) = match call.status {
        ToolStatus::Pending => (theme::faint(), "pending"),
        ToolStatus::InProgress => (theme::yellow(), "running"),
        ToolStatus::Completed => (theme::green(), "done"),
        ToolStatus::Failed => (theme::red(), "failed"),
    };
    let output: Vec<&str> = call.output.lines().collect();
    let shown = output.iter().take(8).copied().collect::<Vec<_>>().join("\n");
    let more = output.len().saturating_sub(8);
    let title = if call.title.is_empty() { call.id.clone() } else { call.title.clone() };
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_2p5()
        .rounded_md()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(7.)).rounded_full().bg(color))
                .child(tag(&call.kind))
                .child(div().flex_1().min_w_0().text_sm().font_family(theme::MONO_FONT).truncate().child(title))
                .child(div().text_xs().text_color(color).child(label)),
        )
        .when(!call.locations.is_empty(), |d| {
            d.child(div().text_xs().text_color(theme::faint()).child(call.locations.join(", ")))
        })
        .when(!shown.is_empty(), |d| {
            d.child(
                div()
                    .px_2()
                    .py_1p5()
                    .rounded_sm()
                    .bg(theme::bg())
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(shown)
                    .when(more > 0, |d| d.child(div().text_color(theme::faint()).child(format!("… {more} more lines")))),
            )
        })
}

fn section_label(text: &'static str) -> impl IntoElement {
    div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme::faint()).child(text)
}

fn tag(text: &str) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_sm()
        .bg(theme::panel_hover())
        .text_xs()
        .text_color(theme::muted())
        .child(text.to_string())
}

fn chip(key: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .text_xs()
        .child(div().text_color(theme::faint()).child(key.to_string()))
        .child(div().text_color(theme::accent()).child(value.to_string()))
}

fn source_label(source: space_core::PluginSource) -> &'static str {
    match source {
        space_core::PluginSource::Bundled => "bundled",
        space_core::PluginSource::User => "user",
        space_core::PluginSource::Project => "project",
    }
}
