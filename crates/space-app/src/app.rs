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
//!
//! Settings (⌘,) replace the thread view in the main column; the sidebar stays. Esc, ⌘, again,
//! Back, ⌘N or picking a thread leave them.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::component::input::{InputEvent, InputState, TextareaState};
use gpui::*;
use space_core::protocol::service::CatalogEntry;
use space_core::protocol::{CATALOG_PERMISSION_MODES, ConfigureSession, ModelInfo};
use space_core::{
    AgentHandle, EventReceiver, Host, HostEvent, PermissionDecision, PluginStatus, Session, SessionEvent, SessionId,
    SessionState, SettingError, SettingState, TranscriptItem,
};
use space_ui::components::settings::FieldCheck;

pub use space_ui::actions::{
    CloseSettings, CyclePermissionMode, FocusComposer, GoBack, GoForward, NewThread, OpenSettings, Quit, ScrollToBottom,
    ToggleLogs, ToggleSidebar,
};
use space_ui::assets::SpaceAssets;
use space_ui::components::composer;
use space_ui::components::follow_scroll::FollowScroll;
use space_ui::format;

#[cfg(feature = "dev")]
use crate::devscript;
use crate::notify::{self, NotificationPermission};
use crate::settings::{ModelRef, SettingsStore};

const MAX_LOG_LINES: usize = 2000;
/// Where ⌘↓ means Scroll to Bottom inside the composer: a single-line textarea. Bound after
/// gpui-component's own `cmd-down` (MoveToEnd, context `Input`), so it wins at equal depth.
const SCROLL_IN_COMPOSER: &str = "Input && !ComposerMultiline";
const DEFAULT_AGENT: &str = "pi-agent";

pub struct Options {
    pub project_dir: PathBuf,
    /// Plugin id of the agent to run (default `pi-agent`, else the first agent plugin).
    pub agent: Option<String>,
    /// Threads to open at launch (newest last); empty: one new thread.
    pub seed: Vec<SeedThread>,
    /// Index into `seed` of the thread to select (default: the last).
    pub select: Option<usize>,
    /// Notify (sound, plus a system notification when not watching) when a turn finishes.
    /// Off for scenarios; `SPACE_NOTIFY=0` turns it off too. Settings can turn either off.
    pub notify: bool,
    /// Settings file (`None`: in memory only). Scenarios use a temp file.
    pub settings_path: Option<PathBuf>,
    /// Open on the Settings page.
    pub open_settings: bool,
}

/// A thread opened at launch: an optional prompt sent once its session exists and an
/// optional composer draft.
#[derive(Debug, Clone, Default)]
pub struct SeedThread {
    pub prompt: Option<String>,
    pub cwd: Option<PathBuf>,
    pub draft: Option<String>,
    /// Permission mode for the thread (a `permission-modes` id).
    pub mode: Option<String>,
    /// Dev scenarios: hold the transcript this far above the bottom, as if scrolled up.
    pub scrolled_up: Option<f32>,
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
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-shift-r", CyclePermissionMode, None),
            // ⌘↓ scrolls the transcript to the bottom, also from the composer (where the
            // textarea would move the caret to the end) unless its text spans several lines.
            // Not context-free: a `None` binding counts as the deepest match and would beat
            // the textarea's own MoveToEnd even in a multi-line composer.
            KeyBinding::new("cmd-down", ScrollToBottom, Some("Space")),
            KeyBinding::new("cmd-down", ScrollToBottom, Some(SCROLL_IN_COMPOSER)),
            KeyBinding::new("escape", CloseSettings, Some("Space")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        #[allow(unused_mut)]
        let mut menus = vec![
            Menu {
                name: "Space".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Space", Quit),
                ],
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
                    MenuItem::action("Scroll to Bottom", ScrollToBottom),
                    MenuItem::action("Focus Composer", FocusComposer),
                    MenuItem::action("Next Permission Mode", CyclePermissionMode),
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

        // Dock icon (and notification registration when bundled), for the gallery too.
        let clicks = notify::init();

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
            cx.new(|cx| SpaceApp::new(host, events, options, clicks, window, cx))
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
    /// Permission mode picked for this thread (a `permission-modes` id); `None`: the default
    /// for new threads. Applied to every session the thread gets.
    pub mode: Option<String>,
    /// Modes that need confirming (Full access) and were confirmed in this thread.
    pub confirmed: HashSet<String>,
    /// This turn already alerted that a tool call is paused.
    paused_alerted: bool,
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
            mode: None,
            confirmed: HashSet::new(),
            paused_alerted: false,
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
    notify: bool,
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
    /// The selected thread's transcript; every thread opens at the bottom.
    pub(crate) transcript_scroll: FollowScroll,
    pub(crate) log_scroll: FollowScroll,
    pub(crate) sidebar_open: bool,
    pub(crate) show_logs: bool,
    pub(crate) banner: Option<String>,
    pub(crate) settings: SettingsStore,
    /// The Settings page replaces the thread view.
    pub(crate) show_settings: bool,
    /// Models and thinking levels from the latest `_space/status` that listed them.
    pub(crate) agent_models: Option<Vec<ModelInfo>>,
    pub(crate) agent_levels: Option<Vec<String>>,
    pub(crate) permission: NotificationPermission,
    /// The `permission-modes` catalog (empty until a plugin publishes it).
    pub(crate) permission_modes: Vec<CatalogEntry>,
    /// Service plugins' settings, by plugin id.
    pub(crate) plugin_settings: BTreeMap<String, Vec<SettingState>>,
    /// Service plugins' process status, by plugin id.
    pub(crate) plugin_status: HashMap<String, PluginStatus>,
    /// Settings-page fields, by (plugin id, key), with the placeholder they show.
    pub(crate) setting_inputs: HashMap<(String, String), (Entity<InputState>, Option<String>)>,
    /// How each field's last save went, by (plugin id, key).
    pub(crate) field_checks: HashMap<(String, String), FieldCheck>,
    /// A mode that needs confirming before the thread switches to it: (thread, mode id).
    pub(crate) pending_confirm: Option<(u64, String)>,
    _subscriptions: Vec<Subscription>,
}

impl SpaceApp {
    fn new(
        host: Host,
        mut events: EventReceiver,
        options: Options,
        mut clicks: futures::channel::mpsc::UnboundedReceiver<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

        // A clicked "finished" notification: bring the window forward on that thread.
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(key) = clicks.next().await {
                let alive = this.update_in(cx, |this, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    if this.thread(key).is_some() {
                        this.select_thread(key, window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // "Last tried … ago · next try in …" counts live: redraw each second while shown.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.permission_modes.iter().any(|e| e.blocker.as_ref().is_some_and(|b| b.last_attempt_ms.is_some())) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Back from System Settings (or anywhere): the notification permission may have changed.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_permission(cx);
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
        let agents: Vec<_> = plugins.iter().filter(|p| p.manifest.is_agent()).collect();
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
            notify: options.notify,
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
            transcript_scroll: FollowScroll::new(),
            log_scroll: FollowScroll::new(),
            sidebar_open: true,
            show_logs: false,
            banner,
            settings: SettingsStore::load(options.settings_path),
            show_settings: false,
            agent_models: None,
            agent_levels: None,
            permission: NotificationPermission::Checking,
            permission_modes: Vec::new(),
            plugin_settings: BTreeMap::new(),
            plugin_status: HashMap::new(),
            setting_inputs: HashMap::new(),
            field_checks: HashMap::new(),
            pending_confirm: None,
            _subscriptions: vec![sub, activation],
        };
        this.seed_threads(options.seed, options.select, window, cx);
        this.start_services(cx);
        this.ensure_agent(cx);
        this.refresh_permission(cx);
        if options.open_settings {
            this.open_settings(window, cx);
        } else {
            let input = this.input.clone();
            input.update(cx, |s, cx| s.focus(window, cx));
        }
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
                    self.plugin_status.insert(plugin_id, status);
                    return;
                }
                if matches!(status, PluginStatus::Stopped | PluginStatus::Crashed(_)) {
                    self.agent = None;
                }
                self.agent_status = status;
            }
            HostEvent::PluginLog { plugin_id, line } => {
                // Service plugins' lines are tagged with their id; the agent's are not.
                let line = if Some(&plugin_id) == self.agent_id.as_ref() { line } else { format!("[{plugin_id}] {line}") };
                if self.logs.len() == MAX_LOG_LINES {
                    self.logs.pop_front();
                }
                self.logs.push_back(line);
            }
            HostEvent::Catalog { name, entries } => {
                if name == CATALOG_PERMISSION_MODES {
                    self.permission_modes = entries;
                    self.sync_session_modes();
                }
            }
            HostEvent::PluginSettings { plugin_id, settings } => {
                self.plugin_settings.insert(plugin_id, settings);
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
        if let SessionEvent::Status(status) = event {
            // What the agent offers, for the Settings page.
            if let Some(models) = &status.available_models {
                self.agent_models = Some(models.clone());
            }
            if let Some(levels) = &status.thinking_levels {
                self.agent_levels = Some(levels.clone());
            }
        }
        // On the Settings page nothing is being watched.
        let selected = self.selected == Some(key) && !self.show_settings;
        let Some(thread) = self.thread_mut(key) else { return };
        thread.apply(event);
        // A tool call waiting for the guardrail: alert once per turn (it may stay paused long).
        let paused = match event {
            SessionEvent::TurnStarted => {
                thread.paused_alerted = false;
                None
            }
            SessionEvent::ToolCallHeld { reason: Some(reason), .. } if !thread.paused_alerted => {
                thread.paused_alerted = true;
                Some(reason.clone())
            }
            _ => None,
        };
        let attention = paused.is_some()
            || matches!(
                event,
                SessionEvent::TurnEnded { .. } | SessionEvent::PermissionRequested(_) | SessionEvent::Error { .. }
            );
        if !selected && attention && !matches!(event, SessionEvent::TurnEnded { stop_reason } if stop_reason == "error") {
            thread.unread += 1;
        }
        let prefs = &self.settings.settings.notifications;
        let alerts = notify::Alerts { notification: prefs.on_finish, sound: prefs.sound };
        if let SessionEvent::TurnEnded { stop_reason } = event
            && self.notify
            && (alerts.notification || alerts.sound)
            && stop_reason != "cancelled"
            && let Some(thread) = self.thread(key)
        {
            // Headline says what happened; the second line says which thread.
            let headline = if stop_reason == "error" { "Agent stopped with an error" } else { "Agent finished" };
            notify::turn_finished(key, headline, &thread.title(), selected && notify::app_active(), alerts);
        }
        if let Some(reason) = paused
            && self.notify
            && (alerts.notification || alerts.sound)
            && let Some(thread) = self.thread(key)
        {
            let headline = format!("Paused: {}", reason.trim_end_matches('.'));
            notify::turn_finished(key, &headline, &thread.title(), selected && notify::app_active(), alerts);
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
        let Some(thread) = self.thread(key) else { return };
        let defaults = self.session_defaults(thread);
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
            if let Some(request) = defaults {
                apply_defaults(&session, request).await;
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

    /// What a new session should be switched to: for a new thread, the defaults from
    /// Settings; for a thread whose agent exited, the model and level it was using.
    fn session_defaults(&self, thread: &Thread) -> Option<ConfigureSession> {
        let request = if thread.state.items.is_empty() {
            let d = &self.settings.settings.new_threads;
            let (provider, model) =
                d.model.clone().map_or((None, None), |ModelRef { provider, id }| (Some(provider), Some(id)));
            ConfigureSession { provider, model, thinking_level: d.thinking_level.clone(), ..Default::default() }
        } else {
            let s = &thread.state.status;
            ConfigureSession {
                provider: s.provider.clone(),
                model: s.model.clone(),
                thinking_level: s.thinking_level.clone(),
                ..Default::default()
            }
        };
        (request.model.is_some() || request.thinking_level.is_some()).then_some(request)
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
        self.sync_session_modes();
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
        let mut scrolled_up = Vec::new();
        for s in seed {
            let key = self.add_thread(s.cwd.unwrap_or_else(|| self.project_dir.clone()));
            if let Some(t) = self.thread_mut(key) {
                t.queued = s.prompt.map(|p| (p, Vec::new()));
                t.draft = s.draft.unwrap_or_default();
                t.mode = s.mode;
            }
            keys.push(key);
            scrolled_up.push(s.scrolled_up);
        }
        let ix = select.filter(|i| *i < keys.len()).unwrap_or(keys.len() - 1);
        let key = keys.get(ix).copied();
        self.selected = key;
        if let Some(distance) = scrolled_up[ix] {
            self.transcript_scroll.scroll_up_from_bottom(px(distance));
        }
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
        // The thread's mode isn't set up yet (the composer says so, with a link).
        if let Some(t) = self.thread(key)
            && self.blocker(t).is_some()
        {
            return;
        }
        let Some(thread) = self.thread_mut(key) else { return };
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
        self.transcript_scroll.jump_to_bottom();
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
        self.show_settings = false;
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
        self.transcript_scroll.jump_to_bottom();
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Settings sit on top of the current thread: Back just leaves them.
        if self.show_settings {
            self.close_settings(window, cx);
            return;
        }
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
            if self.show_settings {
                self.close_settings(window, cx);
            }
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
        let (mode, confirmed) = (thread.mode.take(), std::mem::take(&mut thread.confirmed));
        *thread = fresh;
        thread.draft = draft;
        thread.mode = mode;
        thread.confirmed = confirmed;
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

    /// Switch the selected thread's model and/or effort (new threads use Settings' defaults).
    pub(crate) fn configure(&mut self, request: ConfigureSession, cx: &mut Context<Self>) {
        let Some(session) = self.current().and_then(|t| t.live_session().cloned()) else { return };
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

    /// Start the service plugins (permissions and the like) with the saved plugin settings;
    /// failures show in the Plugins section and the logs.
    fn start_services(&mut self, cx: &mut Context<Self>) {
        let plugins = &self.settings.settings.plugins;
        self.host.set_disabled(plugins.disabled.iter().cloned());
        for (id, values) in &plugins.settings {
            self.host.set_text_settings(id, values.clone());
        }
        let host = self.host.clone();
        cx.spawn(async move |_, _| host.start_services().await).detach();
    }

    // ----- permission modes -------------------------------------------------------------

    /// The thread's permission mode: its own choice, else the default for new threads (if the
    /// catalog has it), else the catalog's default. `None` while there is no catalog.
    pub(crate) fn effective_mode(&self, thread: &Thread) -> Option<String> {
        let default = self.settings.settings.new_threads.permission_mode.as_deref();
        resolve_mode(&self.permission_modes, thread.mode.as_deref(), default)
    }

    pub(crate) fn mode_entry(&self, thread: &Thread) -> Option<&CatalogEntry> {
        let id = self.effective_mode(thread)?;
        self.permission_modes.iter().find(|e| e.id == id)
    }

    /// Why the thread can't send right now (its mode isn't set up), if so.
    pub(crate) fn blocker(&self, thread: &Thread) -> Option<space_core::protocol::service::Blocker> {
        let entry = self.mode_entry(thread).filter(|e| !e.ready)?;
        Some(entry.blocker.clone().unwrap_or_else(|| space_core::protocol::service::Blocker {
            text: format!("{} isn't ready yet.", entry.name),
            ..Default::default()
        }))
    }

    /// Tell the host each live session's permission mode.
    fn sync_session_modes(&self) {
        for t in &self.threads {
            if let (Some(session), Some(mode)) = (&t.session, self.effective_mode(t)) {
                session.set_permission_mode(mode);
            }
        }
    }

    /// Switch the selected thread to mode `id`. A mode that asks for confirmation (Full access)
    /// does so once per thread first. Applies from the agent's next tool call.
    pub(crate) fn set_mode(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(entry) = self.permission_modes.iter().find(|e| e.id == id).cloned() else { return };
        let Some(thread) = self.thread(key) else { return };
        if entry.confirm.is_some() && !thread.confirmed.contains(&id) && self.effective_mode(thread).as_ref() != Some(&id) {
            self.pending_confirm = Some((key, id));
            cx.notify();
            return;
        }
        self.apply_mode(key, id, cx);
    }

    fn apply_mode(&mut self, key: u64, id: String, cx: &mut Context<Self>) {
        self.pending_confirm = None;
        let Some(thread) = self.thread_mut(key) else { return };
        thread.mode = Some(id);
        self.sync_session_modes();
        cx.notify();
    }

    /// The confirmation's answer.
    pub(crate) fn confirm_mode(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let Some((key, id)) = self.pending_confirm.take() else { return };
        if confirmed {
            if let Some(t) = self.thread_mut(key) {
                t.confirmed.insert(id.clone());
            }
            self.apply_mode(key, id, cx);
        }
        cx.notify();
    }

    /// ⇧⌘R: the selected thread's next mode, in catalog order.
    pub(crate) fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        if self.show_settings || self.permission_modes.is_empty() {
            return;
        }
        let Some(current) = self.current().and_then(|t| self.effective_mode(t)) else { return };
        let ix = self.permission_modes.iter().position(|e| e.id == current).map_or(0, |i| i + 1);
        let next = self.permission_modes[ix % self.permission_modes.len()].id.clone();
        self.set_mode(next, cx);
    }

    /// A blocker link: `open-settings:<plugin>`, `open-url:<url>`, `retry:<plugin>` or
    /// `use-mode:<mode>`.
    pub(crate) fn blocker_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (verb, arg) = action.split_once(':').unwrap_or((action, ""));
        match verb {
            "open-settings" => self.open_settings(window, cx),
            "open-url" if arg.starts_with("https://") => {
                let url = arg.to_string();
                std::thread::spawn(move || {
                    if let Err(e) = std::process::Command::new("open").arg(&url).status() {
                        tracing::warn!("opening {url}: {e}");
                    }
                });
            }
            "retry" => {
                let (host, id) = (self.host.clone(), arg.to_string());
                cx.spawn(async move |_, _| host.retry_plugin(&id).await).detach();
            }
            "use-mode" => self.set_mode(arg.to_string(), cx),
            _ => tracing::warn!("unknown blocker action {action:?}"),
        }
    }

    /// How a plugin's saved settings are doing, for its Settings section: the blocker of a
    /// catalog it publishes (with retry times), or "Working." when nothing blocks.
    pub(crate) fn plugin_health(&self, plugin_id: &str) -> Option<(SharedString, bool)> {
        let plugin = self.host.plugin(plugin_id)?;
        if !plugin.manifest.catalogs().any(|c| c == CATALOG_PERMISSION_MODES) || self.permission_modes.is_empty() {
            return None;
        }
        Some(match self.permission_modes.iter().find_map(|e| e.blocker.as_ref()) {
            Some(b) => {
                let times = format::attempt_times(b.last_attempt_ms, b.next_attempt_ms, now_ms());
                (times.map_or_else(|| b.text.clone(), |t| format!("{} {t}", b.text)).into(), false)
            }
            None => ("Working.".into(), true),
        })
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
        self.log_scroll.jump_to_bottom();
        cx.notify();
    }

    /// ⌘↓ or the button: glide the transcript to the bottom and follow again.
    pub(crate) fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        self.transcript_scroll.glide_to_bottom();
        cx.notify();
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    // ----- settings ---------------------------------------------------------------------

    pub(crate) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = true;
        // Off the composer (it's hidden), so Esc reaches the page.
        window.focus(&self.focus, cx);
        self.refresh_permission(cx);
        cx.notify();
    }

    /// Back to the current thread.
    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = false;
        match self.selected {
            Some(key) => self.select_inner(key, false, window, cx),
            None => cx.notify(),
        }
    }

    /// Ask macOS whether notifications are allowed (answers on another thread).
    pub(crate) fn refresh_permission(&mut self, cx: &mut Context<Self>) {
        let answer = notify::permission();
        cx.spawn(async move |this, cx| {
            if let Ok(permission) = answer.await {
                let _ = this.update(cx, |this, cx| {
                    if this.permission != permission {
                        this.permission = permission;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    pub(crate) fn allow_notifications(&mut self, cx: &mut Context<Self>) {
        let answered = notify::request_permission();
        cx.spawn(async move |this, cx| {
            let _ = answered.await;
            let _ = this.update(cx, |this, cx| this.refresh_permission(cx));
        })
        .detach();
    }

    pub(crate) fn set_default_model(&mut self, model: Option<ModelInfo>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.model = model.map(|m| ModelRef { provider: m.provider, id: m.id }));
        cx.notify();
    }

    pub(crate) fn set_default_thinking(&mut self, level: Option<String>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.thinking_level = level);
        cx.notify();
    }

    pub(crate) fn set_notify_on_finish(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.update(|s| s.notifications.on_finish = on);
        cx.notify();
    }

    pub(crate) fn set_sound(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.update(|s| s.notifications.sound = on);
        cx.notify();
    }

    pub(crate) fn set_default_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        self.settings.update(|s| s.new_threads.permission_mode = mode);
        self.sync_session_modes();
        cx.notify();
    }

    /// Save a plugin setting from its Settings field. The plugin checks a new value first:
    /// the field shows "Checking…", then the outcome (a rejected value isn't stored; one that
    /// couldn't be checked can be saved anyway).
    pub(crate) fn save_plugin_setting(
        &mut self,
        plugin_id: String,
        key: String,
        kind: SaveKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = (plugin_id.clone(), key.clone());
        let input = self.setting_inputs.get(&field).map(|(i, _)| i.clone());
        let value = match kind {
            SaveKind::Clear => None,
            SaveKind::Checked | SaveKind::Anyway => {
                let v = input.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                if v.is_empty() {
                    return;
                }
                Some(v)
            }
        };
        let secret = self
            .plugin_settings
            .get(&plugin_id)
            .and_then(|fields| fields.iter().find(|f| f.field.key == key))
            .is_some_and(|f| f.field.kind == space_core::protocol::service::SettingKind::Secret);
        self.field_checks.insert(field.clone(), if value.is_some() { FieldCheck::Checking } else { FieldCheck::Idle });
        let host = self.host.clone();
        let force = kind == SaveKind::Anyway;
        cx.spawn_in(window, async move |this, cx| {
            let result = host.set_setting(&plugin_id, &key, value.clone(), force).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let check = match result {
                    Ok(()) => {
                        // Text values live in the settings file (secrets only in the Keychain).
                        if !secret {
                            let (id, k, v) = (plugin_id.clone(), key.clone(), value.clone());
                            this.settings.update(|s| {
                                let map = s.plugins.settings.entry(id).or_default();
                                match v {
                                    Some(v) => map.insert(k, v),
                                    None => map.remove(&k),
                                };
                            });
                        }
                        if let Some(input) = &input {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        if value.is_some() { FieldCheck::Saved } else { FieldCheck::Idle }
                    }
                    Err(SettingError::Invalid(m)) => FieldCheck::Invalid(m.into()),
                    Err(SettingError::Unverifiable(m)) => FieldCheck::Unverifiable(m.into()),
                    Err(SettingError::Failed(e)) => {
                        this.banner = Some(format!("Couldn't save the setting: {e}"));
                        FieldCheck::Idle
                    }
                };
                this.field_checks.insert(field, check);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Turn a service plugin on or off (saved; takes effect at once).
    pub(crate) fn set_plugin_enabled(&mut self, plugin_id: String, enabled: bool, cx: &mut Context<Self>) {
        if self.host.plugin(&plugin_id).is_some_and(|p| p.manifest.required) {
            return; // required guards stay on
        }
        self.settings.update(|s| {
            s.plugins.disabled.retain(|id| *id != plugin_id);
            if !enabled {
                s.plugins.disabled.push(plugin_id.clone());
            }
        });
        self.host.set_disabled(self.settings.settings.plugins.disabled.iter().cloned());
        if enabled {
            let host = self.host.clone();
            cx.spawn(async move |_, _| host.start_services().await).detach();
        } else {
            self.plugin_settings.remove(&plugin_id);
            self.plugin_status.insert(plugin_id, PluginStatus::Stopped);
        }
        cx.notify();
    }
}

/// What a Settings field's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveKind {
    /// Save the field's value once the plugin has checked it.
    Checked,
    /// Save it although it couldn't be checked.
    Anyway,
    /// Remove the setting.
    Clear,
}

/// Now, in Unix milliseconds (for blockers' retry times).
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A thread's permission mode: its own choice, else the default for new threads, else the
/// catalog's default entry, else its first; ids the catalog doesn't have are skipped. Before
/// the catalog has arrived, the choices are taken as they are (so a Read Only thread is never
/// sent off in the default mode while the permissions plugin starts).
pub(crate) fn resolve_mode(entries: &[CatalogEntry], thread: Option<&str>, default: Option<&str>) -> Option<String> {
    if entries.is_empty() {
        return thread.or(default).map(str::to_string);
    }
    let known = |id: &&str| entries.iter().any(|e| e.id == *id);
    thread
        .filter(known)
        .or(default.filter(known))
        .map(str::to_string)
        .or_else(|| entries.iter().find(|e| e.default).or(entries.first()).map(|e| e.id.clone()))
}

/// Switch a just-created session to `request`. When that fails (say the model is no longer
/// offered), keep the agent's model but still try the thinking level; otherwise stay on the
/// agent's defaults.
async fn apply_defaults(session: &Session, request: ConfigureSession) {
    let level = request.thinking_level.clone().filter(|_| request.model.is_some());
    if let Err(e) = session.configure(request).await {
        tracing::info!("new thread keeps the agent's model: {e:#}");
        if let Some(level) = level {
            let request = ConfigureSession { thinking_level: Some(level), ..Default::default() };
            if let Err(e) = session.configure(request).await {
                tracing::info!("new thread keeps the agent's thinking level: {e:#}");
            }
        }
    }
}

impl Render for SpaceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::view::render_root(self, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogEntry, resolve_mode};

    #[test]
    fn mode_resolution() {
        let e = |id: &str, default: bool| CatalogEntry { id: id.into(), name: id.into(), default, ..Default::default() };
        let modes = vec![e("read-only", false), e("workspace-write", true), e("full-access", false)];
        assert_eq!(resolve_mode(&[], None, None), None);
        assert_eq!(resolve_mode(&[], Some("read-only"), None).as_deref(), Some("read-only"));
        assert_eq!(resolve_mode(&[], None, Some("full-access")).as_deref(), Some("full-access"));
        assert_eq!(resolve_mode(&modes, None, None).as_deref(), Some("workspace-write"));
        assert_eq!(resolve_mode(&modes, None, Some("read-only")).as_deref(), Some("read-only"));
        assert_eq!(resolve_mode(&modes, Some("full-access"), Some("read-only")).as_deref(), Some("full-access"));
        // Unknown ids fall through.
        assert_eq!(resolve_mode(&modes, Some("gone"), Some("also-gone")).as_deref(), Some("workspace-write"));
        let no_default = vec![e("a", false), e("b", false)];
        assert_eq!(resolve_mode(&no_default, None, None).as_deref(), Some("a"));
    }
}
