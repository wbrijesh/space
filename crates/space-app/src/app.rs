//! The main window: thread sidebar, thread view with composer, logs panel.
//!
//! State lives here; rendering is in [`crate::view`]. Bridge: `space-core` runs on a tokio
//! runtime and its async methods spawn onto it internally, so GPUI tasks (`cx.spawn`) await
//! them directly. Host events arrive on a tokio mpsc receiver that a GPUI foreground task
//! drains in batches.
//!
//! Focus model: the root element tracks its own focus handle, so a mouse-down on anything
//! that is not itself focusable moves focus to the root. That clears the composer's focus
//! state (and any keyboard focus ring) the way clicking empty space does in a browser.
//! Thread selection and "new thread" explicitly focus the composer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use gpui::component::input::{InputEvent, TextareaState};
use gpui::*;
use space_core::protocol::{ConfigureSession, PluginKind};
use space_core::{
    AgentHandle, EventReceiver, Host, HostEvent, PermissionDecision, PluginStatus, Session, SessionEvent, SessionId,
    SessionState, TranscriptItem,
};

pub use space_ui::actions::{FocusComposer, GoBack, GoForward, NewThread, Quit, ToggleLogs, ToggleSidebar};
use space_ui::assets::SpaceAssets;
use space_ui::components::composer;
use space_ui::format;

#[cfg(feature = "dev")]
use crate::devscript;

const MAX_LOG_LINES: usize = 2000;
const DEFAULT_AGENT: &str = "pi-agent";

pub struct Options {
    pub project_dir: PathBuf,
    /// Plugin id of the agent to run (default `pi-agent`, else the first agent plugin).
    pub agent: Option<String>,
    /// Threads to open at launch (newest last); empty: one new thread.
    pub seed: Vec<SeedThread>,
    /// Index into `seed` of the thread to select (default: the last).
    pub select: Option<usize>,
}

/// A thread opened at launch: an optional prompt sent once its session exists and an
/// optional composer draft.
#[derive(Debug, Clone, Default)]
pub struct SeedThread {
    pub prompt: Option<String>,
    pub cwd: Option<PathBuf>,
    pub draft: Option<String>,
}

/// What the process opens.
pub enum Launch {
    App(Options),
    /// Dev only: just the component gallery.
    Gallery,
}

pub fn run(host: Host, events: EventReceiver, launch: Launch) {
    gpui::application().with_assets(SpaceAssets).run(move |cx: &mut App| {
        gpui::init(cx);
        space_ui::init(cx);
        #[cfg(feature = "dev")]
        crate::dev::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-n", NewThread, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-[", GoBack, None),
            KeyBinding::new("cmd-]", GoForward, None),
            // The composer would otherwise take these as indent/outdent.
            KeyBinding::new("cmd-[", GoBack, Some("Input")),
            KeyBinding::new("cmd-]", GoForward, Some("Input")),
            KeyBinding::new("cmd-j", ToggleLogs, None),
            KeyBinding::new("cmd-l", FocusComposer, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        #[allow(unused_mut)]
        let mut menus = vec![
            Menu {
                name: "Space".into(),
                items: vec![MenuItem::action("Quit Space", Quit)],
                disabled: false,
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("New Thread", NewThread)],
                disabled: false,
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::action("Plugin Logs", ToggleLogs),
                    MenuItem::separator(),
                    MenuItem::action("Back", GoBack),
                    MenuItem::action("Forward", GoForward),
                    MenuItem::separator(),
                    MenuItem::action("Focus Composer", FocusComposer),
                ],
                disabled: false,
            },
        ];
        #[cfg(feature = "dev")]
        menus.push(crate::dev::menu());
        cx.set_menus(menus);
        // Quit when the last window closes (the gallery is a second window).
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = match launch {
            Launch::App(options) => options,
            Launch::Gallery => {
                #[cfg(feature = "dev")]
                if let Some(handle) = space_ui::gallery::open(cx) {
                    devscript::spawn_from_env(handle, cx);
                }
                cx.activate(true);
                return;
            }
        };

        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Space".into()),
                appears_transparent: true,
                // Vertically centered in the 48px title bar (the buttons are ~14px tall).
                traffic_light_position: Some(point(px(18.), px(17.))),
            }),
            // The title bar is drawn by the app, which moves the window itself.
            app_owns_titlebar_drag: true,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(760.), px(520.))),
            ..Default::default()
        };
        let (handle, _) = gpui::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| SpaceApp::new(host, events, options, window, cx))
        })
        .expect("failed to open window");
        #[cfg(feature = "dev")]
        devscript::spawn_from_env(handle, cx);
        let _ = handle;
        cx.activate(true);
    });
}

/// One conversation with the agent (an ACP session once it has been created).
pub struct Thread {
    pub key: u64,
    pub cwd: PathBuf,
    pub branch: Option<String>,
    pub session: Option<Session>,
    pub state: SessionState,
    /// `session/new` is in flight.
    pub creating: bool,
    /// Creating the session failed.
    pub error: Option<String>,
    /// Turns that finished (or need input) while the thread was not selected.
    pub unread: usize,
    pub draft: String,
    pub attachments: Vec<PathBuf>,
    /// Message sent before the session existed; sent once it does.
    pub queued: Option<(String, Vec<PathBuf>)>,
    /// Expanded thought/tool items (transcript indices).
    pub expanded: HashSet<usize>,
    thought_started: HashMap<usize, Instant>,
    pub thought_secs: HashMap<usize, u64>,
}

impl Thread {
    fn new(key: u64, cwd: PathBuf) -> Self {
        Self {
            key,
            branch: format::git_branch(&cwd),
            cwd,
            session: None,
            state: SessionState::default(),
            creating: false,
            error: None,
            unread: 0,
            draft: String::new(),
            attachments: Vec::new(),
            queued: None,
            expanded: HashSet::new(),
            thought_started: HashMap::new(),
            thought_secs: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.state.items.is_empty() && self.queued.is_none()
    }

    pub fn title(&self) -> String {
        format::thread_title(&self.state)
            .or_else(|| self.queued.as_ref().map(|(t, _)| format::truncate_chars(&format::one_line(t), 120)))
            .unwrap_or_else(|| "New thread".into())
    }

    /// The session can take a prompt now.
    pub fn live_session(&self) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.agent().is_running())
    }

    fn apply(&mut self, event: &SessionEvent) {
        self.state.apply(event);
        let now = Instant::now();
        let last = self.state.items.len().checked_sub(1);
        match (last, self.state.items.last()) {
            (Some(ix), Some(TranscriptItem::Thought(_))) => {
                self.thought_started.entry(ix).or_insert(now);
            }
            _ => {
                for (ix, start) in &self.thought_started {
                    self.thought_secs.entry(*ix).or_insert_with(|| now.duration_since(*start).as_secs());
                }
            }
        }
        if matches!(event, SessionEvent::TurnEnded { .. }) {
            self.branch = format::git_branch(&self.cwd);
        }
    }
}

pub struct SpaceApp {
    pub(crate) host: Host,
    pub(crate) project_dir: PathBuf,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent: Option<AgentHandle>,
    pub(crate) agent_status: PluginStatus,
    pub(crate) agent_title: String,
    pub(crate) logs: VecDeque<String>,
    pub(crate) threads: Vec<Thread>,
    next_key: u64,
    pub(crate) selected: Option<u64>,
    pub(crate) back: Vec<u64>,
    pub(crate) forward: Vec<u64>,
    session_threads: HashMap<SessionId, u64>,
    orphans: HashMap<SessionId, Vec<SessionEvent>>,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) focus: FocusHandle,
    pub(crate) transcript_scroll: ScrollHandle,
    pub(crate) log_scroll: ScrollHandle,
    pub(crate) sidebar_open: bool,
    pub(crate) show_logs: bool,
    pub(crate) banner: Option<String>,
    preferred_model: Option<(String, String)>,
    preferred_effort: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SpaceApp {
    fn new(host: Host, mut events: EventReceiver, options: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| composer::new_input(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            #[cfg(feature = "dev")]
            if devscript::active() {
                match event {
                    InputEvent::Focus => eprintln!("script: composer focused"),
                    InputEvent::Blur => eprintln!("script: composer blurred"),
                    _ => {}
                }
            }
            match event {
                InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                // Send button state and the card's focus border follow the input.
                InputEvent::Change | InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }
        });

        cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(e) = events.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |this, cx| {
                    for e in batch {
                        this.on_host_event(e, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let plugins = host.plugins();
        let agents: Vec<_> = plugins.iter().filter(|p| p.manifest.kind == PluginKind::Agent).collect();
        let wanted = options.agent.clone().unwrap_or_else(|| DEFAULT_AGENT.to_string());
        let agent_plugin = agents.iter().find(|p| p.id() == wanted).or(agents.first()).copied();
        let banner = match (&agent_plugin, &options.agent) {
            (None, _) => Some("No agent plugin found. Build plugins/pi-agent (just build) or set SPACE_PLUGIN_DIRS.".into()),
            (Some(p), Some(w)) if p.id() != w => Some(format!("Agent plugin `{w}` not found; using `{}`.", p.id())),
            _ => None,
        };

        let mut this = Self {
            agent_id: agent_plugin.map(|p| p.id().to_string()),
            agent_title: agent_plugin.map(|p| p.manifest.name.clone()).unwrap_or_else(|| "No agent".into()),
            host,
            project_dir: options.project_dir.clone(),
            agent: None,
            agent_status: PluginStatus::Stopped,
            logs: VecDeque::new(),
            threads: Vec::new(),
            next_key: 1,
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            session_threads: HashMap::new(),
            orphans: HashMap::new(),
            input,
            focus: cx.focus_handle(),
            transcript_scroll: ScrollHandle::new(),
            log_scroll: ScrollHandle::new(),
            sidebar_open: true,
            show_logs: false,
            banner,
            preferred_model: None,
            preferred_effort: None,
            _subscriptions: vec![sub],
        };
        this.seed_threads(options.seed, options.select, window, cx);
        this.ensure_agent(cx);
        let input = this.input.clone();
        input.update(cx, |s, cx| s.focus(window, cx));
        this
    }

    // ----- lookup -----------------------------------------------------------------------

    pub(crate) fn thread(&self, key: u64) -> Option<&Thread> {
        self.threads.iter().find(|t| t.key == key)
    }

    fn thread_mut(&mut self, key: u64) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.key == key)
    }

    pub(crate) fn current(&self) -> Option<&Thread> {
        self.thread(self.selected?)
    }

    pub(crate) fn agent_running(&self) -> bool {
        self.agent.as_ref().is_some_and(AgentHandle::is_running)
    }

    /// Project directories seen so far (for the folder menu), project dir first.
    pub(crate) fn recent_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.project_dir.clone()];
        for t in self.threads.iter().rev() {
            if !dirs.contains(&t.cwd) {
                dirs.push(t.cwd.clone());
            }
        }
        dirs
    }

    // ----- host events ------------------------------------------------------------------

    fn on_host_event(&mut self, event: HostEvent, cx: &mut Context<Self>) {
        match event {
            HostEvent::PluginStatus { plugin_id, status } => {
                if Some(&plugin_id) != self.agent_id.as_ref() {
                    return;
                }
                if matches!(status, PluginStatus::Stopped | PluginStatus::Crashed(_)) {
                    self.agent = None;
                }
                self.agent_status = status;
            }
            HostEvent::PluginLog { plugin_id, line } => {
                if Some(&plugin_id) == self.agent_id.as_ref() {
                    if self.logs.len() == MAX_LOG_LINES {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                    if self.show_logs {
                        self.log_scroll.scroll_to_bottom();
                    }
                }
            }
            HostEvent::Session { session_id, event } => {
                let Some(&key) = self.session_threads.get(&session_id) else {
                    // Arrived before `new_session` returned; applied once the thread knows its id.
                    self.orphans.entry(session_id).or_default().push(event);
                    return;
                };
                self.apply_session_event(key, &event, cx);
            }
        }
    }

    fn apply_session_event(&mut self, key: u64, event: &SessionEvent, _cx: &mut Context<Self>) {
        let selected = self.selected == Some(key);
        let Some(thread) = self.thread_mut(key) else { return };
        thread.apply(event);
        let attention = matches!(
            event,
            SessionEvent::TurnEnded { .. } | SessionEvent::PermissionRequested(_) | SessionEvent::Error { .. }
        );
        if !selected && attention && !matches!(event, SessionEvent::TurnEnded { stop_reason } if stop_reason == "error") {
            thread.unread += 1;
        }
        if selected && !matches!(event, SessionEvent::Status(_)) {
            self.transcript_scroll.scroll_to_bottom();
        }
    }

    // ----- agent and sessions -----------------------------------------------------------

    fn add_thread(&mut self, cwd: PathBuf) -> u64 {
        let key = self.next_key;
        self.next_key += 1;
        self.threads.push(Thread::new(key, cwd));
        key
    }

    /// Start the agent plugin if it is not running; sessions are created once it is up.
    pub(crate) fn ensure_agent(&mut self, cx: &mut Context<Self>) {
        if self.agent_running() || self.agent_status == PluginStatus::Starting {
            return;
        }
        let Some(id) = self.agent_id.clone() else { return };
        self.agent_status = PluginStatus::Starting;
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let result = host.start_agent(&id).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(agent) => {
                        if let Some(name) = agent.agent_name() {
                            this.agent_title = name.to_string();
                        }
                        this.agent = Some(agent);
                        this.agent_status = PluginStatus::Running;
                        this.create_pending_sessions(cx);
                    }
                    Err(e) => {
                        this.agent_status = PluginStatus::Crashed(format!("{e:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn create_pending_sessions(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<u64> = self
            .threads
            .iter()
            .filter(|t| t.session.is_none() && !t.creating && t.error.is_none())
            .map(|t| t.key)
            .collect();
        for key in keys {
            self.create_session(key, cx);
        }
    }

    fn create_session(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent.clone().filter(AgentHandle::is_running) else {
            self.ensure_agent(cx);
            return;
        };
        let prefs = (self.preferred_model.clone(), self.preferred_effort.clone());
        let Some(thread) = self.thread_mut(key) else { return };
        thread.creating = true;
        thread.error = None;
        let cwd = thread.cwd.clone();
        cx.spawn(async move |this, cx| {
            let result = agent.new_session(cwd).await;
            let session = match result {
                Ok(s) => s,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(t) = this.thread_mut(key) {
                            t.creating = false;
                            t.error = Some(format!("{e:#}"));
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| this.attach_session(key, session.clone(), cx));
            // Carry the model / effort the user picked last over to new sessions.
            if prefs.0.is_some() || prefs.1.is_some() {
                let (provider, model) = prefs.0.map_or((None, None), |(p, m)| (Some(p), Some(m)));
                let request = ConfigureSession { provider, model, thinking_level: prefs.1, ..Default::default() };
                if let Err(e) = session.configure(request).await {
                    tracing::warn!("applying model preference failed: {e:#}");
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(t) = this.thread_mut(key) {
                    t.creating = false;
                    if let Some((text, attachments)) = t.queued.take() {
                        this.prompt(key, text, attachments, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn attach_session(&mut self, key: u64, session: Session, cx: &mut Context<Self>) {
        let sid = session.id().to_string();
        let Some(thread) = self.thread_mut(key) else { return };
        if thread.state.id.is_empty() {
            thread.state.id.clone_from(&sid);
        }
        thread.state.status.session_id.clone_from(&sid);
        thread.state.cwd.clone_from(&thread.cwd);
        thread.state.plugin_id = session.agent().plugin_id().to_string();
        thread.session = Some(session);
        self.session_threads.insert(sid.clone(), key);
        for event in self.orphans.remove(&sid).unwrap_or_default() {
            self.apply_session_event(key, &event, cx);
        }
        cx.notify();
    }

    /// Open the launch threads (one empty thread when `seed` is empty). Prompts are queued
    /// and sent once the agent is up.
    fn seed_threads(&mut self, seed: Vec<SeedThread>, select: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let seed = if seed.is_empty() { vec![SeedThread::default()] } else { seed };
        let mut keys = Vec::new();
        for s in seed {
            let key = self.add_thread(s.cwd.unwrap_or_else(|| self.project_dir.clone()));
            if let Some(t) = self.thread_mut(key) {
                t.queued = s.prompt.map(|p| (p, Vec::new()));
                t.draft = s.draft.unwrap_or_default();
            }
            keys.push(key);
        }
        let key = select.and_then(|i| keys.get(i).copied()).or(keys.last().copied());
        self.selected = key;
        if let Some(draft) = key.and_then(|k| self.thread(k)).map(|t| t.draft.clone()).filter(|d| !d.is_empty()) {
            self.input.update(cx, |s, cx| s.set_value(draft, window, cx));
        }
    }

    // ----- user actions -----------------------------------------------------------------

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let text = self.input.read(cx).value().trim().to_string();
        let Some(thread) = self.thread_mut(key) else { return };
        if text.is_empty() && thread.attachments.is_empty() {
            return;
        }
        if thread.state.running || thread.queued.is_some() {
            return;
        }
        let attachments = std::mem::take(&mut thread.attachments);
        thread.draft.clear();
        self.input.update(cx, |s, cx| s.clean(window, cx));

        let thread = self.thread_mut(key).expect("thread exists");
        if thread.live_session().is_some() {
            self.prompt(key, text, attachments, cx);
        } else {
            // No session yet (agent starting) or its agent exited: send once there is one.
            if thread.session.take().is_some() {
                thread.state.items.push(TranscriptItem::TurnEnd("new-session".into()));
            }
            thread.queued = Some((text, attachments));
            thread.error = None;
            if !thread.creating {
                self.create_session(key, cx);
            }
        }
        self.transcript_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn prompt(&mut self, key: u64, text: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(session) = self.thread(key).and_then(|t| t.session.clone()) else { return };
        cx.spawn(async move |_, _| {
            // Failures are reported as session events (Error + TurnEnded).
            let _ = session.prompt_with(text, attachments).await;
        })
        .detach();
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.current().and_then(|t| t.session.clone())
            && let Err(e) = session.cancel()
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    pub(crate) fn answer_permission(&mut self, request_id: u64, decision: PermissionDecision, cx: &mut Context<Self>) {
        if let Some(session) = self.current().and_then(|t| t.session.clone())
            && let Err(e) = session.resolve_permission(request_id, decision)
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    pub(crate) fn select_thread(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.select_inner(key, true, window, cx);
    }

    fn select_inner(&mut self, key: u64, record: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread(key).is_none() {
            return;
        }
        if self.selected != Some(key) {
            let draft = self.input.read(cx).value().to_string();
            if let Some(prev) = self.selected {
                if let Some(t) = self.thread_mut(prev) {
                    t.draft = draft;
                }
                if record {
                    self.back.push(prev);
                    self.forward.clear();
                }
            }
            self.selected = Some(key);
            let draft = self.thread(key).map(|t| t.draft.clone()).unwrap_or_default();
            self.input.update(cx, |s, cx| s.set_value(draft, window, cx));
        }
        if let Some(t) = self.thread_mut(key) {
            t.unread = 0;
        }
        self.transcript_scroll.scroll_to_bottom();
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(key) = self.back.pop() {
            if self.thread(key).is_some() {
                if let Some(cur) = self.selected {
                    self.forward.push(cur);
                }
                self.select_inner(key, false, window, cx);
                return;
            }
        }
    }

    pub(crate) fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(key) = self.forward.pop() {
            if self.thread(key).is_some() {
                if let Some(cur) = self.selected {
                    self.back.push(cur);
                }
                self.select_inner(key, false, window, cx);
                return;
            }
        }
    }

    /// New thread in `dir` (default: the current thread's folder). An empty current
    /// thread in the same folder is reused instead of piling up blank threads.
    pub(crate) fn new_thread(&mut self, dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let dir = dir.or_else(|| self.current().map(|t| t.cwd.clone())).unwrap_or_else(|| self.project_dir.clone());
        if let Some(t) = self.current()
            && t.is_empty()
            && t.cwd == dir
        {
            self.focus_composer(window, cx);
            return;
        }
        let key = self.add_thread(dir);
        self.select_thread(key, window, cx);
        self.create_session(key, cx);
    }

    /// Move the selected (still empty) thread to another folder: its session is replaced.
    pub(crate) fn set_thread_dir(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(thread) = self.thread_mut(key) else { return };
        if !thread.is_empty() {
            self.new_thread(Some(dir), window, cx);
            return;
        }
        if thread.cwd == dir {
            return;
        }
        if let Some(old) = thread.session.take() {
            self.session_threads.remove(old.id());
        }
        let fresh = Thread::new(key, dir);
        let thread = self.thread_mut(key).expect("thread exists");
        let draft = std::mem::take(&mut thread.draft);
        *thread = fresh;
        thread.draft = draft;
        self.create_session(key, cx);
        self.focus_composer(window, cx);
    }

    pub(crate) fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(dir) = paths.pop()
            {
                let _ = this.update_in(cx, |this, window, cx| this.set_thread_dir(dir, window, cx));
            }
        })
        .detach();
    }

    pub(crate) fn attach_files(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(t) = this.thread_mut(key) {
                        for p in paths {
                            if !t.attachments.contains(&p) {
                                t.attachments.push(p);
                            }
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn remove_attachment(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && ix < t.attachments.len()
        {
            t.attachments.remove(ix);
        }
        cx.notify();
    }

    /// Switch the selected thread's model and/or effort; also remembered for new threads.
    pub(crate) fn configure(&mut self, request: ConfigureSession, cx: &mut Context<Self>) {
        let Some(session) = self.current().and_then(|t| t.live_session().cloned()) else { return };
        if let Some(model) = &request.model {
            let provider = request
                .provider
                .clone()
                .or_else(|| self.current().and_then(|t| t.state.status.provider.clone()))
                .unwrap_or_default();
            self.preferred_model = Some((provider, model.clone()));
        }
        if let Some(level) = &request.thinking_level {
            self.preferred_effort = Some(level.clone());
        }
        cx.spawn(async move |this, cx| {
            if let Err(e) = session.configure(request).await {
                let _ = this.update(cx, |this, cx| {
                    this.banner = Some(format!("{e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn toggle_expanded(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && !t.expanded.remove(&ix)
        {
            t.expanded.insert(ix);
        }
        cx.notify();
    }

    pub(crate) fn restart_agent(&mut self, cx: &mut Context<Self>) {
        if let Some(agent) = self.agent.take() {
            agent.stop();
        }
        self.agent_status = PluginStatus::Stopped;
        for t in &mut self.threads {
            t.error = None;
        }
        self.ensure_agent(cx);
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    pub(crate) fn toggle_logs(&mut self, cx: &mut Context<Self>) {
        self.show_logs = !self.show_logs;
        self.log_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }
}

impl Render for SpaceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::view::render_root(self, window, cx)
    }
}
