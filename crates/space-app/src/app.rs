//! The main window: thread sidebar, thread view with composer.
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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use gpui::component::input::{self, InputEvent, InputState, TextareaState};
use gpui::*;
use space_core::protocol::service::{BlockerAction, CatalogEntry};
use space_core::protocol::{CATALOG_PERMISSION_MODES, ConfigureSession, ModelInfo};
use space_core::{
    AgentHandle, Approval, DiscoveredPlugin, EventReceiver, Host, HostEvent, PermissionDecision, PluginStatus, SessionEvent,
    SessionId, SettingState,
};
use space_ui::components::settings::FieldCheck;

pub use space_ui::actions::{
    CloseSettings, CloseThread, CyclePermissionMode, FocusComposer, NextThread, PreviousThread, GoBack, GoForward, NewThread, OpenSettings, Quit, RecallNext, RecallPrevious,
    ScrollToBottom,
    ToggleSidebar,
    ToggleTrajectory,
    CloseTerminal, MaximizeTerminal, NewTerminal, NextTerminal, PreviousTerminal, TerminalClear, TerminalCopy, TerminalPaste,
    ToggleTerminal, OpenFind, FindNext, FindPrevious, CloseFind, RenameThread, TogglePinThread,
};
use space_ui::assets::SpaceAssets;
use space_ui::components::composer;
use space_ui::components::follow_scroll::FollowScroll;
use space_ui::format;

#[cfg(feature = "dev")]
use crate::devscript;
use crate::notify::{self, NotificationPermission};
use crate::settings::SettingsStore;
use crate::threads::Thread;

/// Where ⌘↓ means Scroll to Bottom inside the composer: a single-line textarea. Bound after
/// gpui-component's own `cmd-down` (MoveToEnd, context `Input`), so it wins at equal depth.
const SCROLL_IN_COMPOSER: &str = "Input && !ComposerMultiline";
const DEFAULT_AGENT: &str = "pi-agent";

pub struct Options {
    pub project_dir: PathBuf,
    /// No folder came with the launch: once the saved threads are listed, new threads start in
    /// the folder of the one used last (instead of `project_dir`, the home folder).
    pub last_folder: bool,
    /// Plugin id of the agent to run (default `pi-agent`, else the first agent plugin).
    pub agent: Option<String>,
    /// Threads to open at launch (newest last); empty: one new thread.
    pub seed: Vec<SeedThread>,
    /// Index into `seed` of the thread to select (default: the last).
    pub select: Option<usize>,
    /// Post the notifications plugins ask for (the alerts plugin: when a turn finishes). Off for
    /// scenarios; `SPACE_NOTIFY=0` turns it off too.
    pub notify: bool,
    /// Settings file (`None`: in memory only). Scenarios use a temp file.
    pub settings_path: Option<PathBuf>,
    /// Open on the Settings page.
    pub open_settings: bool,
    /// Dev scenarios: open the selected thread's Trajectory tab.
    pub open_trajectory: bool,
    /// Dev scenarios: open the selected thread's terminal pane.
    pub open_terminal: bool,
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
    /// Dev scenarios: images sent with `prompt`, and images in the draft.
    pub images: Vec<PathBuf>,
    pub draft_images: Vec<PathBuf>,
    /// Dev scenarios: pinned (listed at the top of the sidebar).
    pub pinned: bool,
}

/// What the process opens.
pub enum Launch {
    App(Options),
    /// Dev only: just the component gallery.
    Gallery,
}

/// Shortcuts fire once per press: while a ⌘ shortcut is held, macOS sends repeated key-downs,
/// and each would run its action again (⌘B toggling the sidebar on and off). Swallowing them
/// here, before key bindings and the menu bar see them, covers every shortcut. Typing and
/// plain editing keys (arrows, delete) still repeat.
fn ignore_held_shortcuts(cx: &mut App) {
    cx.intercept_keystrokes(|e, _, cx| {
        if e.keystroke.modifiers.platform && current_key_down_is_repeat() {
            cx.stop_propagation();
        }
    })
    .detach();
}

/// Whether the key event macOS is delivering right now is an auto-repeat. GPUI's interceptors
/// don't get the event's `is_held`, but they run while AppKit is handling it.
fn current_key_down_is_repeat() -> bool {
    use objc2_app_kit::{NSApplication, NSEventType};
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else { return false };
    NSApplication::sharedApplication(mtm)
        .currentEvent()
        .is_some_and(|e| e.r#type() == NSEventType::KeyDown && e.isARepeat())
}

pub fn run(host: Host, events: EventReceiver, launch: Launch) {
    gpui::application().with_assets(SpaceAssets).run(move |cx: &mut App| {
        gpui::init(cx);
        space_ui::init(cx);
        // Images in replies that come from the web (https only; see `reply_image_source`).
        match reqwest_client::ReqwestClient::user_agent(&format!("Space/{}", env!("CARGO_PKG_VERSION"))) {
            Ok(client) => cx.set_http_client(std::sync::Arc::new(client)),
            Err(e) => tracing::warn!("no HTTP client for images: {e:#}"),
        }
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
            KeyBinding::new("cmd-l", FocusComposer, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-shift-r", CyclePermissionMode, None),
            KeyBinding::new("cmd-shift-t", ToggleTrajectory, None),
            // ⌘↓ scrolls the transcript to the bottom, also from the composer (where the
            // textarea would move the caret to the end) unless its text spans several lines.
            // Not context-free: a `None` binding counts as the deepest match and would beat
            // the textarea's own MoveToEnd even in a multi-line composer.
            KeyBinding::new("cmd-down", ScrollToBottom, Some("Space")),
            KeyBinding::new("cmd-down", ScrollToBottom, Some(SCROLL_IN_COMPOSER)),
            KeyBinding::new("escape", CloseSettings, Some("Space")),
            // ↑ on the composer's first line / ↓ on its last step through the thread's sent
            // messages; otherwise they reach the text field (its MoveUp/MoveDown).
            KeyBinding::new("up", RecallPrevious, Some("Composer > Input")),
            KeyBinding::new("down", RecallNext, Some("Composer > Input")),
            KeyBinding::new("cmd-q", Quit, None),
            // The terminal pane. ⌘J (or ⌃`, as in VS Code) from anywhere; the rest while a
            // terminal has focus. ⌘J is bound last, so menus and tooltips show it.
            KeyBinding::new("ctrl-`", ToggleTerminal, None),
            KeyBinding::new("cmd-j", ToggleTerminal, None),
            KeyBinding::new("cmd-shift-enter", MaximizeTerminal, Some("Terminal")),
            KeyBinding::new("cmd-t", NewTerminal, Some("Terminal")),
            KeyBinding::new("cmd-w", CloseThread, None),
            KeyBinding::new("cmd-r", RenameThread, None),
            KeyBinding::new("cmd-alt-down", NextThread, None),
            KeyBinding::new("cmd-alt-up", PreviousThread, None),
            // In the terminal, ⌘W closes the terminal tab instead.
            KeyBinding::new("cmd-w", CloseTerminal, Some("Terminal")),
            KeyBinding::new("ctrl-tab", NextTerminal, Some("Terminal")),
            KeyBinding::new("ctrl-shift-tab", PreviousTerminal, Some("Terminal")),
            KeyBinding::new("cmd-c", TerminalCopy, Some("Terminal")),
            KeyBinding::new("cmd-v", TerminalPaste, Some("Terminal")),
            KeyBinding::new("cmd-k", TerminalClear, Some("Terminal")),
            // These go to the shell: the window root takes Tab and Shift-Tab to move focus, and
            // Esc closes things in the app.
            KeyBinding::new("tab", gpui::NoAction, Some("Terminal")),
            KeyBinding::new("shift-tab", gpui::NoAction, Some("Terminal")),
            KeyBinding::new("escape", gpui::NoAction, Some("Terminal")),
            // Find in thread, from anywhere (beats the text field's own ⌘F); Esc in its field
            // closes it (bound after the field's Escape, so it wins at equal depth).
            KeyBinding::new("cmd-f", OpenFind, None),
            KeyBinding::new("cmd-g", FindNext, None),
            KeyBinding::new("cmd-shift-g", FindPrevious, None),
            KeyBinding::new("escape", CloseFind, Some("FindBar > Input")),
        ]);
        // Without a window to ask in (the window's own handler asks about running terminals).
        cx.on_action(|_: &Quit, cx| cx.quit());
        ignore_held_shortcuts(cx);
        crate::native_edit::init();
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
                items: vec![MenuItem::action("New Thread", NewThread),
                    MenuItem::action("Rename Thread…", RenameThread),
                    MenuItem::action("Close Thread", CloseThread),
                ],
                disabled: false,
            },
            Menu {
                name: "Edit".into(),
                // The standard items carry AppKit's own actions (`cut:`, `selectAll:`…), so native
                // text fields (the rename sheet, open panels) get them too: AppKit only sends ⌘X,
                // ⌘C, ⌘V and ⌘A to a native field through these menu items. In Space's own
                // windows the keys reach the focused input first, as before. Undo and Redo in
                // native fields: see `native_edit`.
                items: vec![
                    MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                    MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                    MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                    MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                    MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                    MenuItem::separator(),
                    MenuItem::action("Find…", OpenFind),
                    MenuItem::action("Find Next", FindNext),
                    MenuItem::action("Find Previous", FindPrevious),
                ],
                disabled: false,
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::separator(),
                    MenuItem::action("Back", GoBack),
                    MenuItem::action("Forward", GoForward),
                    MenuItem::action("Next Thread", NextThread),
                    MenuItem::action("Previous Thread", PreviousThread),
                    MenuItem::separator(),
                    MenuItem::action("Trajectory", ToggleTrajectory),
                    MenuItem::action("Terminal", ToggleTerminal),
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

        // A window an input script drives (test runs) never takes the keyboard: scripted keys
        // go through GPUI, and your typing stays where you are.
        #[cfg(feature = "dev")]
        let scripted = devscript::active();
        #[cfg(not(feature = "dev"))]
        let scripted = false;
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let window_options = WindowOptions {
            focus: !scripted,
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
        if !scripted {
            cx.activate(true);
        }
    });
}

pub struct SpaceApp {
    pub(crate) host: Host,
    /// Opened without a folder: start in the last used one once the saved threads are listed.
    pub(crate) last_folder: bool,
    /// The window it's in, for work that arrives without one (a shell that exits).
    pub(crate) window_handle: AnyWindowHandle,
    pub(crate) project_dir: PathBuf,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent: Option<AgentHandle>,
    pub(crate) agent_status: PluginStatus,
    pub(crate) notify: bool,
    pub(crate) threads: Vec<Thread>,
    pub(crate) next_key: u64,
    pub(crate) selected: Option<u64>,
    pub(crate) back: Vec<u64>,
    pub(crate) forward: Vec<u64>,
    pub(crate) session_threads: HashMap<SessionId, u64>,
    pub(crate) orphans: HashMap<SessionId, Vec<SessionEvent>>,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) focus: FocusHandle,
    /// The selected thread's transcript; every thread opens at the bottom.
    pub(crate) transcript_scroll: FollowScroll,
    pub(crate) sidebar_open: bool,
    /// The sidebar's shown fraction, sliding toward `sidebar_open`.
    pub(crate) sidebar_shown: space_ui::theme::motion::Spring,
    /// How far the current thread's terminal pane is shown (0..1), and the thread it's for:
    /// switching threads snaps it instead of sliding.
    pub(crate) terminal_shown: space_ui::theme::motion::Spring,
    pub(crate) terminal_shown_for: Option<u64>,
    pub(crate) banner: Option<String>,
    /// The image viewer: a message's pictures and the one shown.
    pub(crate) viewer: Option<(Vec<space_ui::components::images::Pic>, usize)>,
    pub(crate) viewer_focus: FocusHandle,
    /// The Trajectory tab: which threads show it, the shown one's data, the search field.
    pub(crate) trajectories: crate::trajectory::Trajectories,
    /// The message row that shows "Copied" for a moment, and the one whose usage panel is open.
    pub(crate) copied: Option<String>,
    /// The shown thread's streaming reply, revealed at an even pace.
    pub(crate) stream_reveal: std::rc::Rc<std::cell::RefCell<space_ui::components::turns::StreamReveal>>,
    pub(crate) usage_open: Option<String>,
    /// Draft images are told apart by this (they're prepared in the background).
    pub(crate) next_image_id: u64,
    /// Settings: the days closed threads are kept (made when the page is first shown).
    pub(crate) keep_closed_input: Option<Entity<gpui::component::input::InputState>>,
    /// Find in thread (⌘F), made when first opened.
    pub(crate) find: Option<crate::find::Find>,
    /// The shown thread's replies' text views (see [`crate::find`]).
    pub(crate) replies: crate::find::Replies,
    /// The sidebar's thread list (which rows are in view, for ⌘ numbers).
    pub(crate) sidebar_scroll: ScrollHandle,
    /// ⌘ alone is held: the sidebar shows row numbers.
    pub(crate) cmd_alone: bool,
    /// Digits typed while ⌘ is held: the row to go to when it's let go.
    pub(crate) cmd_digits: String,
    /// Another ⌘ shortcut was pressed while ⌘ is held: no numbers until it's pressed again.
    pub(crate) cmd_other: bool,
    /// Closed threads kept to be resumed (their saved summaries).
    pub(crate) closed_threads: Vec<space_core::protocol::service::ThreadMeta>,
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
    /// When something last kept the agent busy (see `idle`), and whether it was stopped for
    /// being idle (it starts again when you write).
    pub(crate) agent_activity: std::time::Instant,
    pub(crate) agent_asleep: bool,
    /// The `claude` CLI pi-agent needs was found (checked at launch and on each restart).
    pub(crate) claude_cli: bool,
    pub(crate) _subscriptions: Vec<Subscription>,
    /// Numbers queued messages.
    pub(crate) next_pending: u64,
    /// ⌘ is held (the composer's send button shows "after this step").
    pub(crate) cmd_held: bool,
    pub(crate) terminals: crate::terminal::Terminals,
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
                InputEvent::PressEnter { shift: false, secondary } => {
                    if *secondary {
                        this.send_after_step(window, cx)
                    } else {
                        this.send(window, cx)
                    }
                }
                // Typing ends ↑/↓ recall (a recalled message is set without a change event), and
                // typing while the agent sleeps starts it, so it's up when you send.
                InputEvent::Change => {
                    this.end_recall();
                    if !this.input.read(cx).value().trim().is_empty() {
                        this.wake_agent(cx);
                    }
                    cx.notify();
                }
                // Send button state and the card's focus border follow the input.
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
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

        // Quitting: save what the threads haven't yet, then stop the plugins, each before what
        // it requires.
        let quit = cx.on_app_quit(|this, _| {
            this.save_all_now();
            this.host.shutdown();
            async {}
        });

        // Back from System Settings (or anywhere): the notification permission may have changed.
        // Either way, re-render so the host knows whether you're watching a thread.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_permission(cx);
            }
            cx.notify();
        });

        // A ⌘ shortcut that isn't a thread number (⌘J, ⌘N…): the sidebar's numbers go at once,
        // before the shortcut runs (bindings run before the window's key listeners see it).
        let shortcuts = {
            let this = cx.entity().downgrade();
            cx.intercept_keystrokes(move |e, _, cx| {
                if e.keystroke.modifiers.platform && !crate::jump::is_number_key(&e.keystroke) {
                    let _ = this.update(cx, |this, cx| this.cmd_shortcut(cx));
                }
            })
        };

        cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(e) = events.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |this, cx| {
                    let mut app_changed = false;
                    for e in batch {
                        match e {
                            // Output of a terminal on screen redraws just that terminal.
                            HostEvent::TerminalOutput { terminal_id } => app_changed |= this.terminal_output(&terminal_id, cx),
                            e => {
                                this.on_host_event(e, cx);
                                app_changed = true;
                            }
                        }
                    }
                    if app_changed {
                        cx.notify();
                    }
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

        let host_for_terminals = host.clone();
        let mut this = Self {
            window_handle: window.window_handle(),
            agent_id: agent_plugin.map(|p| p.id().to_string()),
            notify: options.notify,
            host,
            project_dir: options.project_dir.clone(),
            last_folder: options.last_folder,
            agent: None,
            agent_status: PluginStatus::Stopped,
            threads: Vec::new(),
            next_key: 1,
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            session_threads: HashMap::new(),
            orphans: HashMap::new(),
            trajectories: crate::trajectory::Trajectories::new(window, cx),
            input,
            focus: cx.focus_handle(),
            viewer: None,
            viewer_focus: cx.focus_handle(),
            next_image_id: 0,
            copied: None,
            stream_reveal: Default::default(),
            usage_open: None,
            transcript_scroll: FollowScroll::new(),
            sidebar_open: true,
            sidebar_shown: space_ui::theme::motion::Spring::new(1., space_ui::theme::motion::SIDEBAR_SLIDE),
            terminal_shown: space_ui::theme::motion::Spring::new(0., space_ui::theme::motion::TERMINAL_SLIDE),
            terminal_shown_for: None,
            banner,
            closed_threads: Vec::new(),
            sidebar_scroll: ScrollHandle::new(),
            cmd_alone: false,
            cmd_digits: String::new(),
            cmd_other: false,
            keep_closed_input: None,
            find: None,
            replies: Default::default(),
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
            agent_activity: std::time::Instant::now(),
            agent_asleep: false,
            claude_cli: claude_cli_installed(),
            _subscriptions: vec![sub, activation, quit, shortcuts],
            terminals: crate::terminal::Terminals::new(&host_for_terminals),
            next_pending: 0,
            cmd_held: false,
        };
        this.watch_agent_idle(cx);
        #[cfg(feature = "dev")]
        crate::reload::start(cx);
        this.seed_threads(options.seed, options.select, window, cx);
        this.start_services(cx);
        this.load_saved_threads(cx);
        this.ensure_agent(cx);
        this.refresh_permission(cx);
        if options.open_trajectory
            && let Some(key) = this.selected
        {
            this.trajectories.open.insert(key);
        }
        if options.open_settings {
            this.open_settings(window, cx);
        } else {
            let input = this.input.clone();
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        if options.open_terminal {
            this.toggle_terminal(window, cx);
        }
        this
    }

    // ----- lookup -----------------------------------------------------------------------

    pub(crate) fn thread(&self, key: u64) -> Option<&Thread> {
        self.threads.iter().find(|t| t.key == key)
    }

    pub(crate) fn thread_mut(&mut self, key: u64) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.key == key)
    }

    pub(crate) fn current(&self) -> Option<&Thread> {
        self.thread(self.selected?)
    }

    pub(crate) fn agent_running(&self) -> bool {
        self.agent.as_ref().is_some_and(AgentHandle::is_running)
    }

    /// The agent requires something `plugin_id` provides.
    fn agent_requires(&self, plugin_id: &str) -> bool {
        let (Some(agent), Some(plugin)) = (self.agent_id.as_deref().and_then(|id| self.host.plugin(id)), self.host.plugin(plugin_id))
        else {
            return false;
        };
        agent.manifest.requires.iter().any(|c| plugin.manifest.provides(c))
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
                if self.host.terminal_plugin().as_deref() == Some(plugin_id.as_str()) {
                    match status {
                        PluginStatus::Running => self.terminal_plugin_status(true, cx),
                        PluginStatus::Stopped | PluginStatus::Crashed(_) | PluginStatus::NotStarted(_) => self.terminal_plugin_status(false, cx),
                        PluginStatus::Starting => {}
                    }
                }
                if Some(&plugin_id) != self.agent_id.as_ref() {
                    // The agent was waiting for this plugin (`requires`): start it now.
                    if status == PluginStatus::Running
                        && matches!(self.agent_status, PluginStatus::NotStarted(_))
                        && self.agent_requires(&plugin_id)
                    {
                        self.ensure_agent(cx);
                    }
                    self.plugin_status.insert(plugin_id, status);
                    return;
                }
                if matches!(status, PluginStatus::Stopped | PluginStatus::Crashed(_) | PluginStatus::NotStarted(_)) {
                    self.agent = None;
                }
                // The agent failing may be the Claude CLI gone missing: look again.
                if matches!(status, PluginStatus::Crashed(_)) {
                    self.claude_cli = claude_cli_installed();
                }
                self.agent_status = status;
            }
            // Plugins' stderr isn't shown in the window; the host keeps a tail for error messages.
            HostEvent::PluginLog { plugin_id, line } => tracing::debug!(plugin = %plugin_id, "{line}"),
            HostEvent::Catalog { name, entries } => {
                if name == CATALOG_PERMISSION_MODES {
                    self.permission_modes = entries;
                    self.sync_session_modes();
                }
            }
            HostEvent::PluginSettings { plugin_id, settings } => {
                self.plugin_settings.insert(plugin_id, settings);
            }
            HostEvent::Notify { notify: n, .. } => {
                if !self.notify {
                    return;
                }
                let thread = n.session_id.as_ref().and_then(|sid| self.session_threads.get(sid)).and_then(|k| self.thread(*k));
                let body = n.body.clone().or_else(|| thread.map(Thread::title)).unwrap_or_default();
                notify::post(thread.map(|t| t.key), &n.title, &body, n.banner, n.sound);
            }
            HostEvent::OpenUrl { url, .. } => open_url(url),
            // The Trajectory tab isn't wired yet.
            HostEvent::TrajectoryChanged { thread_id, .. } => self.trajectory_changed(thread_id, cx),
            // Handled before the batch (see the event loop); here only when it needs the app.
            HostEvent::TerminalOutput { .. } => {}
            HostEvent::TerminalTitle { terminal_id, title } => self.terminal_title(&terminal_id, title),
            HostEvent::TerminalExited { terminal_id, exit_code } => self.terminal_exited(&terminal_id, exit_code, cx),
            HostEvent::Steered { session_id, steer_id } => self.steered(&session_id, &steer_id, cx),
            HostEvent::Session { session_id, event } => {
                self.touch_agent();
                let Some(&key) = self.session_threads.get(&session_id) else {
                    // Arrived before `new_session` returned; applied once the thread knows its id.
                    self.orphans.entry(session_id).or_default().push(event);
                    return;
                };
                self.apply_session_event(key, &event, cx);
            }
        }
    }

    // ----- user actions -----------------------------------------------------------------

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.current().and_then(|t| t.session.clone())
            && let Err(e) = session.cancel()
        {
            self.banner = Some(format!("{e:#}"));
        }
        cx.notify();
    }

    pub(crate) fn answer_permission(&mut self, request_id: u64, decision: PermissionDecision, cx: &mut Context<Self>) {
        self.touch_agent();
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

    pub(crate) fn select_inner(&mut self, key: u64, record: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread(key).is_none() {
            return;
        }
        self.show_settings = false;
        if self.selected != Some(key) {
            self.close_find(false, window, cx);
            let left = self.selected;
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
            // A new thread left untouched (nothing typed, attached or open in a terminal) goes:
            // it isn't listed, so nothing would lead back to it.
            if let Some(prev) = left
                && self.thread(prev).is_some_and(|t| t.is_empty() && t.draft.trim().is_empty() && t.attachments.is_empty() && t.images.is_empty())
                && !self.has_terminals(prev)
            {
                self.drop_thread(prev, cx);
            }
        }
        if let Some(t) = self.thread_mut(key) {
            t.unread = 0;
        }
        self.ensure_loaded(key, cx);
        self.thread_selected_for_trajectory(cx);
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
        // A new thread in this folder you started typing in and left: back to it.
        if let Some(key) = self.threads.iter().rev().find(|t| t.is_empty() && t.cwd == dir).map(|t| t.key) {
            self.select_thread(key, window, cx);
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
        if self.move_empty_thread(key, dir, cx) {
            self.focus_composer(window, cx);
        }
    }

    /// Start an empty thread over in `dir` (a new session there), keeping its draft and mode.
    /// False when it's already there.
    pub(crate) fn move_empty_thread(&mut self, key: u64, dir: PathBuf, cx: &mut Context<Self>) -> bool {
        let Some(thread) = self.thread_mut(key).filter(|t| t.is_empty() && t.cwd != dir) else { return false };
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
        true
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
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| this.add_files(paths, cx));
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
        self.touch_agent();
        // No live session (a saved thread not loaded yet, or the agent sleeps): keep the choice on
        // the thread; its session is switched to it when it's loaded (`session_defaults`).
        if self.current().is_some_and(|t| t.live_session().is_none()) {
            if let Some(t) = self.selected.and_then(|k| self.thread_mut(k)) {
                let s = &mut t.state.status;
                if request.model.is_some() {
                    s.provider = request.provider.clone().or(s.provider.take());
                    s.model = request.model.clone();
                }
                if request.thinking_level.is_some() {
                    s.thinking_level = request.thinking_level.clone();
                }
            }
            if self.agent_asleep {
                self.wake_agent(cx);
            }
            cx.notify();
            return;
        }
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

    /// Open or fold a finished turn's work; the row stays where it is on screen.
    pub(crate) fn toggle_turn(&mut self, user_ix: usize, cx: &mut Context<Self>) {
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && !t.open_turns.remove(&user_ix)
        {
            t.open_turns.insert(user_ix);
        }
        self.transcript_scroll.hold();
        cx.notify();
    }

    pub(crate) fn toggle_expanded(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.transcript_scroll.hold();
        if let Some(key) = self.selected
            && let Some(t) = self.thread_mut(key)
            && !t.expanded.remove(&ix)
        {
            t.expanded.insert(ix);
        }
        cx.notify();
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

    /// Why the thread can't send right now (a plugin waits for your approval, or the thread's
    /// mode isn't set up), if so.
    pub(crate) fn blocker(&self, thread: &Thread) -> Option<space_core::protocol::service::Blocker> {
        let waiting = self.awaiting_approval();
        if let Some(first) = waiting.first() {
            let text = match waiting.len() {
                1 => format!("{} needs your approval to run.", first.manifest.name),
                n => format!("{n} plugins need your approval to run."),
            };
            let review = BlockerAction::new("Review in Settings", format!("open-settings:{}", first.id()));
            return Some(space_core::protocol::service::Blocker { text, actions: vec![review], ..Default::default() });
        }
        if let Some(blocker) = self.resume_blocker(thread) {
            return Some(blocker);
        }
        if !thread.images.is_empty()
            && let Some(agent) = self.agent.as_ref().filter(|a| a.is_running() && !a.accepts_images())
        {
            let name = self.host.plugin(agent.plugin_id()).map_or_else(|| agent.plugin_id().to_string(), |p| p.manifest.name);
            let text = format!("{name} can't read images. Remove them to send.");
            return Some(space_core::protocol::service::Blocker { text, ..Default::default() });
        }
        let entry = self.mode_entry(thread).filter(|e| !e.ready)?;
        Some(entry.blocker.clone().unwrap_or_else(|| space_core::protocol::service::Blocker {
            text: format!("{} isn't ready yet.", entry.name),
            ..Default::default()
        }))
    }

    /// A saved conversation the running agent can't continue (it can't load sessions, or it
    /// isn't the agent that had it): it reads, and continues only in a new session.
    fn resume_blocker(&self, thread: &Thread) -> Option<space_core::protocol::service::Blocker> {
        let (plugin, _) = thread.resume.as_ref().filter(|_| thread.session.is_none() && thread.queued.is_none())?;
        let agent = self.agent.as_ref().filter(|a| a.is_running())?;
        if agent.plugin_id() == plugin && agent.can_load_session() {
            return None;
        }
        let name = self.host.plugin(plugin).map_or_else(|| plugin.clone(), |p| p.manifest.name);
        let text = if agent.plugin_id() == plugin {
            format!("{name} can't continue a saved conversation.")
        } else {
            format!("This conversation was with {name}.")
        };
        let fresh = BlockerAction::new("Continue in new session", format!("new-session:{}", thread.key));
        Some(space_core::protocol::service::Blocker { text, actions: vec![fresh], ..Default::default() })
    }

    /// Plugins (turned on) that wait for you to approve or deny them.
    pub(crate) fn awaiting_approval(&self) -> Vec<DiscoveredPlugin> {
        let pending = |p: &DiscoveredPlugin| self.host.approval(p.id()) == Approval::Pending && !self.host.is_disabled(p.id());
        self.host.plugins().into_iter().filter(pending).collect()
    }

    /// Tell the host each live session's permission mode.
    pub(crate) fn sync_session_modes(&self) {
        for t in &self.threads {
            if let (Some(session), Some(mode)) = (&t.session, self.effective_mode(t)) {
                session.set_permission_mode(mode);
            }
        }
    }

    /// Switch the selected thread to mode `id`. A mode that asks for confirmation (Full access)
    /// does so once per thread first. Applies from the agent's next tool call.
    pub(crate) fn set_mode(&mut self, id: String, cx: &mut Context<Self>) {
        self.touch_agent();
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
        let saved = thread.meta.is_some() || thread.saving;
        self.sync_session_modes();
        if saved {
            self.save_thread(key, cx);
        }
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

    /// A blocker link: `open-settings:<plugin>`, `open-url:<url>`, `retry:<plugin>`,
    /// `use-mode:<mode>` or `new-session:<thread key>`.
    pub(crate) fn blocker_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (verb, arg) = action.split_once(':').unwrap_or((action, ""));
        match verb {
            "open-settings" => self.open_settings(window, cx),
            "open-url" if arg.starts_with("https://") => open_url(arg.to_string()),
            "retry" => {
                let (host, id) = (self.host.clone(), arg.to_string());
                cx.spawn(async move |_, _| host.retry_plugin(&id).await).detach();
            }
            "use-mode" => self.set_mode(arg.to_string(), cx),
            "new-session" => {
                if let Some(t) = arg.parse().ok().and_then(|key| self.thread_mut(key)) {
                    t.resume = None;
                }
                cx.notify();
            }
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
        self.claude_cli = claude_cli_installed();
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
        let target = if self.sidebar_open { 1. } else { 0. };
        if cx.reduce_motion() {
            self.sidebar_shown.snap(target);
        } else {
            let clock = space_ui::clock::AnimationClock::global(cx);
            let (now, pinned) = (clock.now(), clock.pinned());
            self.sidebar_shown.set(target, now, pinned);
        }
        cx.notify();
    }

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


/// Open a URL in the browser (on a thread, so the `open` child is reaped).
fn open_url(url: String) {
    std::thread::spawn(move || {
        if let Err(e) = std::process::Command::new("open").arg(&url).status() {
            tracing::warn!("opening {url}: {e}");
        }
    });
}

impl Render for SpaceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Whatever changed what's on screen re-renders, so this is where the host learns which
        // thread you're watching (Settings and other apps: none).
        let watching = self.current().and_then(|t| t.session.as_ref()).map(|s| s.id().to_string());
        self.host.set_watching(watching.filter(|_| !self.show_settings && window.is_window_active()));
        crate::view::render_root(self, window, cx)
    }
}

/// Whether the Claude CLI is installed where pi-agent looks for it: on `PATH`, or in the usual
/// install folders (a GUI app gets a minimal `PATH`).
pub(crate) fn claude_cli_installed() -> bool {
    let home = dirs_home();
    let fallbacks = [home.join(".local/bin"), home.join(".claude/local"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()];
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(fallbacks)
        .any(|dir| dir.join("claude").is_file())
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
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
