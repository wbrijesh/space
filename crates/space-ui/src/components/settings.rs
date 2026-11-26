//! Settings (⌘,): a few pages, listed in the sidebar while Settings is open. A page has no
//! heading (the title bar names it); it is cards of rows: Space's own first, then each plugin's
//! in a card of their own. Every row is built one way ([`row`]): a label, a description only
//! when it adds something, and the control on the right, all controls one width. A change takes
//! effect at once; only a typed plugin value (a key, text the plugin checks) has a Save button.
//!
//! Plugin settings are generic: each field a plugin declares goes on the page it names (what
//! it's about), or under the plugin's row on Plugins; no plugin is known here by its id.
//!
//! [`SettingsPage`] lays out one page from plain props; it has no scroll container of its own
//! (the app wraps it in one).

use std::rc::Rc;

use gpui::component::button::Button;
use gpui::component::input::{Input, InputState};
use gpui::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui::component::switch::Switch;
use gpui::component::{Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::ModelInfo;
use space_core::protocol::service::{CatalogEntry, DAYS, DAYS_NEVER, SettingPage};

use super::composer::{Handler, IndexHandler};
use super::widgets::pill_button;
use crate::format;
use crate::theme::{color, radius, size, text};

pub type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type ModelHandler = Rc<dyn Fn(Option<ModelInfo>, &mut Window, &mut App)>;
pub type LevelHandler = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
/// A number of days; `None`: never.
pub type DaysHandler = Rc<dyn Fn(Option<u32>, &mut Window, &mut App)>;
/// `(plugin id, key)`.
pub type FieldHandler = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type PluginToggleHandler = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;
/// `(plugin id, key, on)`.
pub type FieldToggleHandler = Rc<dyn Fn(String, String, bool, &mut Window, &mut App)>;
/// `(plugin id, key, value)`; `None` clears the setting.
pub type FieldChoiceHandler = Rc<dyn Fn(String, String, Option<String>, &mut Window, &mut App)>;

/// One plugin setting field.
#[derive(Clone)]
pub struct FieldView {
    pub plugin_id: String,
    pub key: String,
    pub label: SharedString,
    pub description: SharedString,
    /// The page it's on.
    pub page: SettingsTab,
    pub control: FieldControl,
}

/// A plugin setting's control, by its kind.
#[derive(Clone)]
pub enum FieldControl {
    /// A switch, and whether it's on.
    Switch(bool),
    /// A model picker.
    Model(ModelChoice),
    /// How long to keep something: one of the day choices, `None` for never.
    Days(Option<u32>),
    /// Typed, then saved once the plugin has checked it.
    Text(TextField),
}

/// A typed plugin setting.
#[derive(Clone)]
pub struct TextField {
    /// Masked, kept in the Keychain, never shown again once saved.
    pub secret: bool,
    pub set: bool,
    pub input: Entity<InputState>,
    /// The last save: being checked, or what the plugin said.
    pub check: FieldCheck,
    /// How the saved value is doing, from the plugin (e.g. "Working", or why not): `(text, ok)`.
    pub status: Option<(SharedString, bool)>,
}

/// A model setting: the thread's own model or one of the agent's models.
#[derive(Clone, Default)]
pub struct ModelChoice {
    /// `<provider>/<model id>`; `None`: the thread's own model.
    pub chosen: Option<String>,
    /// What the agent offers (empty: not known yet).
    pub models: Vec<ModelInfo>,
}

/// A save's progress: plugins check a value before it's stored.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FieldCheck {
    #[default]
    Idle,
    Checking,
    /// Accepted and stored.
    Saved,
    /// Rejected; nothing was stored.
    Invalid(SharedString),
    /// Couldn't be checked now; "Save anyway" stores it.
    Unverifiable(SharedString),
}

/// A row of the Plugins page.
#[derive(Clone)]
pub struct PluginRow {
    pub id: String,
    pub name: SharedString,
    pub description: SharedString,
    /// Agents can't be turned off here.
    pub agent: bool,
    /// A required guard: can't be turned off either.
    pub required: bool,
    pub enabled: bool,
    /// "Running", "Exited: …", "Off".
    pub status: SharedString,
    /// Set for a plugin from your home or project folder you haven't approved.
    pub approval: Option<PluginApproval>,
}

/// What a plugin from your home or project folder asks to do, and your answer so far.
#[derive(Clone)]
pub struct PluginApproval {
    /// Its permissions in plain words.
    pub permissions: Vec<SharedString>,
    /// You said no (you can still approve it).
    pub denied: bool,
}

/// Whether macOS lets Space post notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationPermission {
    /// Not known yet.
    Checking,
    Allowed,
    /// Turned off in System Settings.
    Denied,
    /// Never asked.
    NotAsked,
    /// Running as a bare binary (no bundle id): notifications can't work.
    Unbundled,
}

/// What the agent offers for new threads.
#[derive(Debug, Clone, PartialEq)]
pub enum Catalog {
    /// The agent is starting; no status has arrived yet.
    Loading,
    /// No agent, or it failed to start.
    Unavailable,
    Ready { models: Vec<ModelInfo>, levels: Vec<String> },
}

/// Callbacks; every one defaults to a no-op.
#[derive(Clone)]
pub struct SettingsHandlers {
    /// `None`: the agent's default.
    pub on_model: ModelHandler,
    pub on_thinking: LevelHandler,
    /// Ask macOS for notification permission.
    pub on_allow: Handler,
    pub on_open_system_settings: Handler,
    /// Default permission mode for new threads (a catalog id).
    pub on_mode: LevelHandler,
    /// How long done threads are kept.
    pub on_keep_done: DaysHandler,
    /// Save the field's input as the setting's value.
    pub on_save_field: FieldHandler,
    /// Store the field's value although it couldn't be checked.
    pub on_save_anyway: FieldHandler,
    pub on_clear_field: FieldHandler,
    /// A switch setting was flipped.
    pub on_toggle_field: FieldToggleHandler,
    /// A model or days setting was picked.
    pub on_choose_field: FieldChoiceHandler,
    pub on_plugin_enabled: PluginToggleHandler,
    /// `(plugin id, approved)`.
    pub on_plugin_approval: PluginToggleHandler,
    /// What a two-finger swipe shows and hides (an index of [`SWIPE_CHOICES`]).
    pub on_swipe: IndexHandler,
}

/// The choices of "Swipe with two fingers to show or hide", in order.
pub const SWIPE_CHOICES: [&str; 3] = ["Right pane", "Sidebar", "Nothing"];

impl Default for SettingsHandlers {
    fn default() -> Self {
        Self {
            on_model: Rc::new(|_, _, _| {}),
            on_thinking: Rc::new(|_, _, _| {}),
            on_allow: Rc::new(|_, _| {}),
            on_open_system_settings: Rc::new(|_, _| {}),
            on_mode: Rc::new(|_, _, _| {}),
            on_keep_done: Rc::new(|_, _, _| {}),
            on_save_field: Rc::new(|_, _, _, _| {}),
            on_save_anyway: Rc::new(|_, _, _, _| {}),
            on_clear_field: Rc::new(|_, _, _, _| {}),
            on_toggle_field: Rc::new(|_, _, _, _, _| {}),
            on_choose_field: Rc::new(|_, _, _, _, _| {}),
            on_plugin_enabled: Rc::new(|_, _, _, _| {}),
            on_plugin_approval: Rc::new(|_, _, _, _| {}),
            on_swipe: Rc::new(|_, _, _| {}),
        }
    }
}

/// One page of Settings.
#[derive(IntoElement)]
pub struct SettingsPage {
    catalog: Catalog,
    /// Default model for new threads as (provider, id); `None`: the agent's default.
    model: Option<(String, String)>,
    thinking: Option<String>,
    permission: NotificationPermission,
    /// The `permission-modes` catalog (empty: no plugin publishes modes, so no row).
    modes: Vec<CatalogEntry>,
    /// Default mode for new threads; `None`: the catalog's default.
    mode: Option<String>,
    /// Every plugin's fields, grouped by plugin.
    fields: Vec<FieldView>,
    plugins: Vec<PluginRow>,
    handlers: SettingsHandlers,
    /// How many days done threads are kept (`Some(None)`: never deleted); `None`: threads
    /// aren't saved, so no row.
    keep_done: Option<Option<u32>>,
    /// What a two-finger swipe shows and hides (an index of [`SWIPE_CHOICES`]).
    swipe: usize,
    /// About: "0.1.6", or a local build's "0.1.6 +2 · a95ab81 · edited".
    version: SharedString,
    /// About: when a local build was made.
    built: Option<SharedString>,
    /// The page shown (the sidebar lists them).
    tab: SettingsTab,
}

/// A page of Settings: the sidebar lists them while Settings is open, and shows the chosen one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// Defaults for new threads, the trackpad.
    #[default]
    General,
    /// Deleting done threads (while threads are saved), and plugins' thread settings (titles).
    Threads,
    /// Whether macOS lets Space notify, and plugins' notification settings (alerts).
    Notifications,
    /// The plugins, and the settings of each that don't belong anywhere else.
    Plugins,
    /// Plugins' settings for working on Space (traces), when there are any.
    Developer,
    /// Which build this is.
    About,
}

impl From<SettingPage> for SettingsTab {
    fn from(page: SettingPage) -> Self {
        match page {
            SettingPage::General => SettingsTab::General,
            SettingPage::Threads => SettingsTab::Threads,
            SettingPage::Notifications => SettingsTab::Notifications,
            SettingPage::Plugins => SettingsTab::Plugins,
            SettingPage::Developer => SettingsTab::Developer,
        }
    }
}

impl SettingsTab {
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Threads => "Threads",
            SettingsTab::Notifications => "Notifications",
            SettingsTab::Plugins => "Plugins",
            SettingsTab::Developer => "Developer",
            SettingsTab::About => "About",
        }
    }

    /// The pages there are, in order; one with nothing on it isn't listed. `threads`: threads
    /// are saved (so done ones are kept a while); `plugins`: any are installed; `placed`: the
    /// pages plugin fields are on.
    pub fn all(threads: bool, plugins: bool, placed: impl IntoIterator<Item = SettingsTab>) -> Vec<SettingsTab> {
        let placed: Vec<SettingsTab> = placed.into_iter().collect();
        [
            SettingsTab::General,
            SettingsTab::Threads,
            SettingsTab::Notifications,
            SettingsTab::Plugins,
            SettingsTab::Developer,
            SettingsTab::About,
        ]
        .into_iter()
        .filter(|tab| match tab {
            SettingsTab::Threads => threads || placed.contains(tab),
            SettingsTab::Plugins => plugins || placed.contains(tab),
            SettingsTab::Developer => placed.contains(tab),
            SettingsTab::General | SettingsTab::Notifications | SettingsTab::About => true,
        })
        .collect()
    }
}

impl SettingsPage {
    pub fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            model: None,
            thinking: None,
            permission: NotificationPermission::Checking,
            modes: Vec::new(),
            mode: None,
            fields: Vec::new(),
            plugins: Vec::new(),
            handlers: SettingsHandlers::default(),
            keep_done: None,
            swipe: 0,
            version: SharedString::default(),
            built: None,
            tab: SettingsTab::default(),
        }
    }

    pub fn tab(mut self, tab: SettingsTab) -> Self {
        self.tab = tab;
        self
    }

    /// The build, for About: its version line, and when a local build was made.
    pub fn build(mut self, version: impl Into<SharedString>, built: Option<String>) -> Self {
        self.version = version.into();
        self.built = built.map(Into::into);
        self
    }

    pub fn swipe(mut self, choice: usize) -> Self {
        self.swipe = choice;
        self
    }

    /// Done threads are deleted after this many days (`None`: never). Not called: threads
    /// aren't saved, so there's no row.
    pub fn keep_done(mut self, days: Option<u32>) -> Self {
        self.keep_done = Some(days);
        self
    }

    pub fn modes(mut self, modes: Vec<CatalogEntry>, default: Option<String>) -> Self {
        self.modes = modes;
        self.mode = default;
        self
    }

    /// Plugins' fields, each plugin's together.
    pub fn fields(mut self, fields: Vec<FieldView>) -> Self {
        self.fields = fields;
        self
    }

    pub fn plugins(mut self, plugins: Vec<PluginRow>) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn model(mut self, model: Option<(String, String)>) -> Self {
        self.model = model;
        self
    }

    pub fn thinking(mut self, level: Option<String>) -> Self {
        self.thinking = level;
        self
    }

    pub fn permission(mut self, permission: NotificationPermission) -> Self {
        self.permission = permission;
        self
    }

    pub fn handlers(mut self, handlers: SettingsHandlers) -> Self {
        self.handlers = handlers;
        self
    }
}

const AGENT_DEFAULT: &str = "Agent default";
/// gpui-component's popup menu items' height; a dropdown's trigger matches it.
const MENU_ITEM_H: Pixels = px(26.);
/// A trigger's padding, so its text lines up with the items' in the menu under it: their text
/// sits 12px in from the menu's edge (4px around the items, 8px inside them), less the
/// trigger's border and the menu's, which sits a pixel outside the trigger.
const MENU_TEXT_INSET: Pixels = px(10.);

/// "Claude Opus 5.5": the provider's display name without a "(latest)" suffix.
fn model_name(m: &ModelInfo) -> String {
    match m.name.as_deref().map(str::trim).map(|n| n.strip_suffix("(latest)").unwrap_or(n).trim_end()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format::model_label(&m.id, Some(m)),
    }
}

impl RenderOnce for SettingsPage {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let tab = self.tab;
        let h = &self.handlers;
        let any = |r: Div| r.into_any_element();
        // Space's own rows: one card, or several.
        let own: Vec<Vec<AnyElement>> = match tab {
            SettingsTab::General => {
                let mut defaults = defaults_rows(&self.catalog, self.model.as_ref(), self.thinking.as_deref(), h, window);
                if !self.modes.is_empty() {
                    defaults.push(any(mode_row(&self.modes, self.mode.as_deref(), h.on_mode.clone(), window)));
                }
                vec![defaults, vec![any(swipe_row(self.swipe, h.on_swipe.clone(), window))]]
            }
            SettingsTab::Threads => vec![self.keep_done.iter().map(|days| any(keep_done_row(*days, h.on_keep_done.clone(), window))).collect()],
            SettingsTab::Notifications => vec![vec![any(permission_row(self.permission, h))]],
            // Each plugin's own settings under its row.
            SettingsTab::Plugins => vec![
                self.plugins
                    .iter()
                    .flat_map(|p| {
                        let fields = self.fields.iter().filter(|f| f.page == tab && f.plugin_id == p.id);
                        std::iter::once(any(plugin_row(p, h, window))).chain(fields.map(|f| any(field_row(f, h, window))))
                    })
                    .collect(),
            ],
            SettingsTab::Developer => Vec::new(),
            SettingsTab::About => {
                let value = |s: SharedString| div().text_size(text::MD).text_color(color::text_2()).child(s).into_any_element();
                let mut rows = vec![any(row("Version", "", Some(value(self.version.clone()))))];
                if let Some(built) = self.built.clone() {
                    rows.push(any(row("Built", "", Some(value(built)))));
                }
                vec![rows]
            }
        };
        // Then each plugin's fields on this page, a card per plugin (on Plugins, only those of
        // a plugin not in the list).
        let listed = |f: &FieldView| tab == SettingsTab::Plugins && self.plugins.iter().any(|p| p.id == f.plugin_id);
        let mut cards = own;
        let mut last: Option<&str> = None;
        for f in self.fields.iter().filter(|f| f.page == tab && !listed(f)) {
            if last != Some(f.plugin_id.as_str()) {
                cards.push(Vec::new());
                last = Some(&f.plugin_id);
            }
            cards.last_mut().expect("pushed").push(any(field_row(f, h, window)));
        }
        div().flex().flex_col().gap_4().children(cards.into_iter().filter(|rows| !rows.is_empty()).map(card))
    }
}

/// "Default model" and "Default thinking": the agent's choices once it has said, after "Agent
/// default".
fn defaults_rows(
    catalog: &Catalog,
    model: Option<&(String, String)>,
    thinking: Option<&str>,
    h: &SettingsHandlers,
    window: &Window,
) -> Vec<AnyElement> {
    let level_label = thinking.map_or(AGENT_DEFAULT.into(), format::effort_label);
    let Catalog::Ready { models, levels } = catalog else {
        let desc = if *catalog == Catalog::Loading { "Loading models…" } else { "Agent unavailable" };
        let model_label = model.map_or(SharedString::from(AGENT_DEFAULT), |(_, id)| format::model_label(id, None).into());
        return vec![
            row("Default model", desc, Some(select("settings-model", model_label, vec![], None, None, window))).into_any_element(),
            row("Default thinking", desc, Some(select("settings-thinking", level_label, vec![], None, None, window))).into_any_element(),
        ];
    };
    let chosen = model.map(|(p, id)| models.iter().position(|m| &m.provider == p && &m.id == id));
    // A saved model the agent no longer offers: marked, and new threads use the agent's default
    // (`session_defaults`).
    let model_label = match (model, chosen) {
        (Some(_), Some(Some(ix))) => model_name(&models[ix]).into(),
        (Some((_, id)), _) => format!("{} · unavailable", format::model_label(id, None)).into(),
        (None, _) => SharedString::from(AGENT_DEFAULT),
    };
    let names = models.iter().map(|m| model_name(m).into());
    let options = std::iter::once(AGENT_DEFAULT.into()).chain(names).collect();
    let selected = match chosen {
        None => Some(0),
        Some(ix) => ix.map(|i| i + 1),
    };
    let (list, f) = (models.clone(), h.on_model.clone());
    // Index 0 is "Agent default".
    let on_model: IndexHandler = Rc::new(move |ix, w, cx| f(ix.checked_sub(1).map(|i| list[i].clone()), w, cx));
    let model_row = row("Default model", "", Some(select("settings-model", model_label, options, selected, Some(on_model), window)));

    let names = levels.iter().map(|l| format::effort_label(l).into());
    let options = std::iter::once(AGENT_DEFAULT.into()).chain(names).collect();
    let selected = match thinking {
        None => Some(0),
        Some(t) => levels.iter().position(|l| l == t).map(|i| i + 1),
    };
    let (list, f) = (levels.clone(), h.on_thinking.clone());
    let on_level: IndexHandler = Rc::new(move |ix, w, cx| f(ix.checked_sub(1).map(|i| list[i].clone()), w, cx));
    let thinking_row = row("Default thinking", "", Some(select("settings-thinking", level_label, options, selected, Some(on_level), window)));
    vec![model_row.into_any_element(), thinking_row.into_any_element()]
}

/// "Default permission mode": the catalog's entries; the default one is marked.
fn mode_row(modes: &[CatalogEntry], chosen: Option<&str>, on_mode: LevelHandler, window: &Window) -> Div {
    let default_ix = modes.iter().position(|e| e.default).unwrap_or(0);
    let selected = chosen.and_then(|c| modes.iter().position(|e| e.id == c)).unwrap_or(default_ix);
    let options: Vec<SharedString> = modes.iter().map(|e| e.name.clone().into()).collect();
    let label = options.get(selected).cloned().unwrap_or_default();
    let ids: Vec<String> = modes.iter().map(|e| e.id.clone()).collect();
    let on_select: IndexHandler = Rc::new(move |ix, w, cx| on_mode(ids.get(ix).cloned(), w, cx));
    let control = select_plain("settings-mode", label, options, Some(selected), Some(on_select), window);
    row("Default permission mode", "", Some(control))
}

/// One plugin setting, by its kind.
pub fn field_row(f: &FieldView, h: &SettingsHandlers, window: &Window) -> Div {
    let (pid, key) = (f.plugin_id.clone(), f.key.clone());
    let id = |what: &str| SharedString::from(format!("{what}-{}-{}", f.plugin_id, f.key));
    match &f.control {
        FieldControl::Switch(on) => {
            let toggle_field = h.on_toggle_field.clone();
            let on_change: ToggleHandler = Rc::new(move |on, w, cx| toggle_field(pid.clone(), key.clone(), on, w, cx));
            row(f.label.clone(), f.description.clone(), Some(toggle(id("switch"), *on, on_change, window).into_any_element()))
        }
        FieldControl::Model(choice) => model_field_row(f, choice, h, window),
        FieldControl::Days(days) => {
            let choose = h.on_choose_field.clone();
            let on_pick: DaysHandler = Rc::new(move |days, w, cx| {
                let value = days.map_or(DAYS_NEVER.to_string(), |d| d.to_string());
                choose(pid.clone(), key.clone(), Some(value), w, cx)
            });
            row(f.label.clone(), f.description.clone(), Some(days_select(id("days"), *days, on_pick, window)))
        }
        FieldControl::Text(t) => text_field_row(f, t, h),
    }
}

/// A typed plugin setting: whether it's set, a field, Save and (when set) Remove; under it, how
/// the last save went (checking, rejected, "Save anyway") or how the value is doing.
fn text_field_row(f: &FieldView, t: &TextField, h: &SettingsHandlers) -> Div {
    let stored = match (t.secret, t.set) {
        (true, true) => "Saved in your Keychain",
        (_, false) => "Not set",
        (false, true) => "Saved",
    };
    let description: SharedString =
        if f.description.is_empty() { stored.into() } else { format!("{} · {stored}", f.description).into() };
    let checking = t.check == FieldCheck::Checking;
    let (save, clear, anyway) = (h.on_save_field.clone(), h.on_clear_field.clone(), h.on_save_anyway.clone());
    let id = |what: &str| SharedString::from(format!("{what}-{}-{}", f.plugin_id, f.key));
    let (pid, key) = (f.plugin_id.clone(), f.key.clone());
    let (pid2, key2, pid3, key3) = (pid.clone(), key.clone(), pid.clone(), key.clone());
    let control = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(size::SETTINGS_SELECT_W).child(Input::new(&t.input).small()))
        .child(
            pill_button(id("save"), if checking { "Checking…" } else { "Save" }, false)
                .when(checking, |b| b.opacity(0.6))
                .when(!checking, |b| b.on_click(move |_, w, cx| save(pid.clone(), key.clone(), w, cx))),
        )
        .when(t.set && !checking, |d| {
            d.child(pill_button(id("clear"), "Remove", false).on_click(move |_, w, cx| clear(pid2.clone(), key2.clone(), w, cx)))
        });
    let line = |icon: IconName, tone: Hsla, text: SharedString| {
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(text::SM)
            .text_color(tone)
            .child(Icon::new(icon).size(size::ICON_SM))
            .child(div().text_color(color::text_2()).child(text))
    };
    let note: Option<AnyElement> = match &t.check {
        FieldCheck::Checking => Some(line(IconName::Loader, color::text_3(), "Checking it with the service…".into()).into_any_element()),
        FieldCheck::Invalid(m) => Some(line(IconName::CircleX, color::red(), format!("{m} It wasn't saved.").into()).into_any_element()),
        FieldCheck::Unverifiable(m) => Some(
            line(IconName::TriangleAlert, color::yellow(), m.clone())
                .child(pill_button(id("anyway"), "Save anyway", false).on_click(move |_, w, cx| anyway(pid3.clone(), key3.clone(), w, cx)))
                .into_any_element(),
        ),
        FieldCheck::Saved | FieldCheck::Idle => t.status.as_ref().map(|(text, ok)| {
            if *ok {
                line(IconName::CircleCheck, color::green(), text.clone()).into_any_element()
            } else {
                line(IconName::TriangleAlert, color::yellow(), text.clone()).into_any_element()
            }
        }),
    };
    div()
        .flex()
        .flex_col()
        .child(row(f.label.clone(), description, Some(control.into_any_element())))
        .children(note.map(|n| div().pl_3().pb_2().child(n)))
}

const THREAD_MODEL: &str = "Thread's model";

/// A model setting: the thread's own model, then the agent's models.
fn model_field_row(f: &FieldView, choice: &ModelChoice, h: &SettingsHandlers, window: &Window) -> Div {
    let key = |m: &ModelInfo| format!("{}/{}", m.provider, m.id);
    let chosen = choice.chosen.as_ref().map(|c| choice.models.iter().position(|m| &key(m) == c));
    let label: SharedString = match (&choice.chosen, chosen) {
        (Some(_), Some(Some(ix))) => model_name(&choice.models[ix]).into(),
        (Some(c), _) => format::model_label(c.split_once('/').map_or(c.as_str(), |(_, id)| id), None).into(),
        (None, _) => THREAD_MODEL.into(),
    };
    let selected = match chosen {
        None => Some(0),
        Some(ix) => ix.map(|i| i + 1),
    };
    let options = std::iter::once(THREAD_MODEL.into()).chain(choice.models.iter().map(|m| model_name(m).into())).collect();
    let on_select: Option<IndexHandler> = (!choice.models.is_empty()).then(|| {
        let (values, f2, pid, k) = (choice.models.iter().map(key).collect::<Vec<_>>(), h.on_choose_field.clone(), f.plugin_id.clone(), f.key.clone());
        // Index 0 is the thread's model.
        Rc::new(move |ix: usize, w: &mut Window, cx: &mut App| f2(pid.clone(), k.clone(), ix.checked_sub(1).map(|i| values[i].clone()), w, cx))
            as IndexHandler
    });
    let id = SharedString::from(format!("model-{}-{}", f.plugin_id, f.key));
    row(f.label.clone(), f.description.clone(), Some(select(id, label, options, selected, on_select, window)))
}

/// A plugin: name, status and what it does; a switch for service plugins. One that waits for
/// your approval lists what it asks to do, with Approve and Deny.
pub fn plugin_row(p: &PluginRow, h: &SettingsHandlers, window: &Window) -> Div {
    let description: SharedString = if p.description.is_empty() {
        p.status.clone()
    } else {
        format!("{} · {}", p.status, p.description).into()
    };
    if let Some(approval) = &p.approval {
        return approval_row(p, approval, description, h);
    }
    let on_enabled = h.on_plugin_enabled.clone();
    let label = |t: &'static str| div().text_size(text::MD).text_color(color::text_3()).child(t).into_any_element();
    let control = if p.agent {
        label("Agent")
    } else if p.required {
        label("Required")
    } else {
        let id = p.id.clone();
        let element_id = SharedString::from(format!("plugin-{}", p.id));
        toggle(element_id, p.enabled, Rc::new(move |on, w, cx| on_enabled(id.clone(), on, w, cx)), window).into_any_element()
    };
    row(p.name.clone(), description, Some(control))
}

fn approval_row(p: &PluginRow, approval: &PluginApproval, description: SharedString, h: &SettingsHandlers) -> Div {
    let button = |what: &str, label: &'static str, approved: bool| {
        let (f, id) = (h.on_plugin_approval.clone(), p.id.clone());
        pill_button(SharedString::from(format!("plugin-{what}-{}", p.id)), label, approved)
            .on_click(move |_, w, cx| f(id.clone(), approved, w, cx))
    };
    let controls = div()
        .flex()
        .items_center()
        .gap_2()
        .when(!approval.denied, |d| d.child(button("deny", "Deny", false)))
        .child(button("approve", "Approve", true));
    let permission = |text: SharedString| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(Icon::new(IconName::Check).size(size::ICON_SM).text_color(color::text_3()))
            .child(text)
    };
    let details = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .pb_2()
        .text_size(text::SM)
        .text_color(color::text_2())
        .children(approval.permissions.iter().cloned().map(permission))
        .child(
            div()
                .pt_1()
                .text_color(color::text_3())
                .child("Like any program you run, it can also use your files and the network."),
        );
    div().flex().flex_col().child(row(p.name.clone(), description, Some(controls.into_any_element()))).child(details)
}

/// The "macOS permission" row: status text and, when it helps, one action.
pub fn permission_row(permission: NotificationPermission, h: &SettingsHandlers) -> Div {
    let status = |label: &'static str, c: Hsla| div().text_size(text::MD).text_color(c).child(label);
    let (desc, control): (&str, Option<AnyElement>) = match permission {
        NotificationPermission::Checking => ("", Some(status("Checking…", color::text_3()).into_any_element())),
        NotificationPermission::Allowed => ("", Some(status("Allowed", color::text_2()).into_any_element())),
        NotificationPermission::Denied => {
            let f = h.on_open_system_settings.clone();
            let button = pill_button("settings-open-system", "Open System Settings", false).on_click(move |_, w, cx| f(w, cx));
            ("Off in System Settings", Some(button.into_any_element()))
        }
        NotificationPermission::NotAsked => {
            let f = h.on_allow.clone();
            ("", Some(pill_button("settings-allow", "Allow…", false).on_click(move |_, w, cx| f(w, cx)).into_any_element()))
        }
        NotificationPermission::Unbundled => ("Unavailable in this build (launch it with just run)", None),
    };
    row("macOS permission", desc, control)
}

/// "Delete done threads after [30 days]".
pub fn keep_done_row(days: Option<u32>, on_pick: DaysHandler, window: &Window) -> Div {
    row("Delete done threads after", "", Some(days_select("settings-keep-done", days, on_pick, window)))
}

/// "7 days", "30 days", "90 days", "Never": how long something is kept (`None`: never).
pub fn days_select(id: impl Into<ElementId>, days: Option<u32>, on_pick: DaysHandler, window: &Window) -> AnyElement {
    let choices: Vec<Option<u32>> = DAYS.into_iter().map(Some).chain([None]).collect();
    let name = |d: &Option<u32>| SharedString::from(d.map_or("Never".to_string(), |d| format!("{d} days")));
    let options = choices.iter().map(name).collect();
    let selected = choices.iter().position(|c| *c == days);
    let on_select: IndexHandler = Rc::new(move |ix, w, cx| on_pick(choices[ix], w, cx));
    select_plain(id, name(&days), options, selected, Some(on_select), window)
}

/// "Swipe with two fingers to show or hide [Right pane]".
pub fn swipe_row(choice: usize, on_swipe: IndexHandler, window: &Window) -> Div {
    let label = SWIPE_CHOICES.get(choice).copied().unwrap_or(SWIPE_CHOICES[0]);
    let options = SWIPE_CHOICES.iter().map(|c| SharedString::from(*c)).collect();
    let control = select_plain("settings-swipe", label, options, Some(choice), Some(on_swipe), window);
    row("Swipe with two fingers to show or hide", "", Some(control))
}

/// Rows in one card, with hairlines between them.
pub fn card(rows: Vec<AnyElement>) -> Div {
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ix, r)| div().when(ix > 0, |d| d.border_t_1().border_color(color::border())).child(r));
    div()
        .flex()
        .flex_col()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .children(rows)
}

/// One setting: label and an optional one-line description on the left, `control` on the right.
pub fn row(label: impl Into<SharedString>, description: impl Into<SharedString>, control: Option<AnyElement>) -> Div {
    let description: SharedString = description.into();
    div()
        .min_h(size::SETTINGS_ROW_H)
        .flex()
        .items_center()
        .gap_6()
        .px_3()
        .py_1p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(text::MD).child(label.into()))
                .when(!description.is_empty(), |d| d.child(div().text_size(text::SM).text_color(color::text_2()).child(description))),
        )
        .children(control.map(|c| div().flex_none().flex().items_center().child(c)))
}

/// A dropdown showing `label`, with a separator after the first option (the "default" entry).
/// `options[selected]` is checked in the menu; `on_select: None` disables it.
pub fn select(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    options: Vec<SharedString>,
    selected: Option<usize>,
    on_select: Option<IndexHandler>,
    window: &Window,
) -> AnyElement {
    dropdown(id, label.into(), options, selected, on_select, true, window)
}

/// A dropdown like [`select`], without the separator.
pub fn select_plain(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    options: Vec<SharedString>,
    selected: Option<usize>,
    on_select: Option<IndexHandler>,
    window: &Window,
) -> AnyElement {
    dropdown(id, label.into(), options, selected, on_select, false, window)
}

fn dropdown(
    id: impl Into<ElementId>,
    label: SharedString,
    options: Vec<SharedString>,
    selected: Option<usize>,
    on_select: Option<IndexHandler>,
    separator: bool,
    window: &Window,
) -> AnyElement {
    let enabled = on_select.is_some();
    // As tall as the menu's items, its text their size and lined up with theirs, so trigger and
    // menu read as one.
    let trigger = Button::new(id)
        .outline()
        .small()
        .h(MENU_ITEM_H)
        .px(MENU_TEXT_INSET)
        // Menus hand focus back to their trigger on close; only show a ring for keyboard users.
        .focus_ring(window.last_input_was_keyboard())
        .w(size::SETTINGS_SELECT_W)
        .rounded(radius::MD)
        .disabled(!enabled)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().min_w_0().truncate().text_color(if enabled { color::text() } else { color::text_2() }).child(label))
                .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3())),
        );
    let Some(on_select) = on_select else {
        return trigger.into_any_element();
    };
    trigger
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(size::SETTINGS_SELECT_W).max_h(px(420.)).scrollable(true).check_side(gpui::component::Side::Right);
            for (ix, label) in options.iter().enumerate() {
                let f = on_select.clone();
                let item = PopupMenuItem::new(label.clone()).checked(selected == Some(ix));
                menu = menu.item(item.on_click(move |_, w, cx| f(ix, w, cx)));
                if separator && ix == 0 && options.len() > 1 {
                    menu = menu.separator();
                }
            }
            menu
        })
        .into_any_element()
}

/// An on/off switch, gpui-component's small one.
pub fn toggle(id: impl Into<ElementId>, checked: bool, on_change: ToggleHandler, window: &Window) -> Switch {
    Switch::new(id)
        .checked(checked)
        .small()
        .focus_ring(window.last_input_was_keyboard())
        .on_click(move |on, w, cx| on_change(*on, w, cx))
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};

    fn ready() -> Catalog {
        let status = super::composer::fixtures::status(0);
        Catalog::Ready { models: status.available_models.unwrap(), levels: status.thinking_levels.unwrap() }
    }
    fn models() -> Vec<ModelInfo> {
        let Catalog::Ready { models, .. } = ready() else { unreachable!() };
        models
    }
    fn page(name: &'static str, f: impl Fn(&mut Window, &mut App) -> SettingsPage + 'static) -> Example {
        Example::new(name, move |w, cx| f(w, cx).into_any_element()).width(size::CONTENT_MAX_W)
    }
    fn permission(name: &'static str, p: NotificationPermission) -> Example {
        Example::new(name, move |_, _| card(vec![permission_row(p, &SettingsHandlers::default()).into_any_element()]).into_any_element())
            .width(size::CONTENT_MAX_W)
    }
    fn sonnet() -> Option<(String, String)> {
        Some(("claude-cli".into(), "claude-sonnet-5-5".into()))
    }
    fn modes() -> Vec<CatalogEntry> {
        super::composer::fixtures::modes("guarded", true).entries
    }
    fn field(plugin: &str, key: &str, label: &str, description: &str, page: SettingsTab, control: FieldControl) -> FieldView {
        FieldView {
            plugin_id: plugin.into(),
            key: key.into(),
            label: label.to_string().into(),
            description: description.to_string().into(),
            page,
            control,
        }
    }
    /// The bundled plugins' fields, as they declare them.
    fn bundled() -> Vec<FieldView> {
        let notifications = SettingsTab::Notifications;
        vec![
            field("alerts", "notify", "Notify when an agent finishes or needs you", "When you're not looking at the thread", notifications, FieldControl::Switch(true)),
            field("alerts", "sound", "Play a sound", "", notifications, FieldControl::Switch(false)),
            field("titles", "model", "Model for titles", "", SettingsTab::Threads, FieldControl::Model(ModelChoice { chosen: None, models: models() })),
            field("trajectory", "keep_days", "Delete traces after", "", SettingsTab::Developer, FieldControl::Days(None)),
            field("trajectory", "chunk_timing", "Record streamed chunk timing", "For each request's timing graph", SettingsTab::Developer, FieldControl::Switch(true)),
        ]
    }
    /// A guard's API key (no page of its own, so under its row on Plugins), over a persistent input.
    fn key_field(name: &str, set: bool, check: FieldCheck, status: Option<(&'static str, bool)>, window: &mut Window, cx: &mut App) -> FieldView {
        let input = window.use_keyed_state(SharedString::from(format!("p-key-{name}")), cx, |window, cx| {
            InputState::new(window, cx).masked(true).placeholder(if set { "Enter a new key to replace it" } else { "sk-…" })
        });
        let text = TextField { secret: true, set, input, check, status: status.map(|(t, ok)| (t.into(), ok)) };
        field("guard", "api_key", "API key", "For the service that checks each operation in Guarded mode.", SettingsTab::Plugins, FieldControl::Text(text))
    }
    fn key_example(name: &'static str, set: bool, check: fn() -> FieldCheck, status: Option<(&'static str, bool)>) -> Example {
        Example::new(name, move |window, cx| {
            let f = key_field(name, set, check(), status, window, cx);
            card(vec![field_row(&f, &SettingsHandlers::default(), window).into_any_element()]).into_any_element()
        })
        .width(size::CONTENT_MAX_W)
    }
    fn plugin(id: &str, name: &str, description: &str, agent: bool, enabled: bool, status: &str) -> PluginRow {
        PluginRow {
            id: id.into(),
            name: name.to_string().into(),
            description: description.to_string().into(),
            agent,
            required: false,
            enabled,
            status: status.to_string().into(),
            approval: None,
        }
    }
    fn plugins() -> Vec<PluginRow> {
        let asks = |id: &str, name: &str, status: &str, denied: bool| PluginRow {
            description: "From ~/.space/plugins.".into(),
            approval: Some(PluginApproval {
                permissions: vec!["Is told when the agent finishes".into(), "Shows notifications".into(), "Reads what you and the agent write".into()],
                denied,
            }),
            ..plugin(id, name, "", false, true, status)
        };
        vec![
            plugin("claude-agent", "Claude Agent", "Coding agent on Claude Code (the Claude Agent SDK), using your Claude subscription.", true, true, "Running"),
            plugin("guard", "Guard", "Checks each operation in Guarded mode.", false, true, "Running"),
            plugin("audit", "Audit log", "Writes every tool call to a file.", false, false, "Off"),
            asks("notes", "Notes", "Needs your approval", false),
            asks("digest", "Digest", "Not approved", true),
        ]
    }

    vec![
        ComponentPreview::new(Area::Settings, "Settings page", "⌘, — one page per subject; plugin settings on the page for what they do.")
            .stacked()
            .examples([
                page("general", |_, _| {
                    SettingsPage::new(ready()).model(sonnet()).thinking(Some("high".into())).modes(modes(), Some("trusted".into())).swipe(0)
                }),
                page("general, agent defaults", |_, _| SettingsPage::new(ready())),
                page("general, loading models", |_, _| SettingsPage::new(Catalog::Loading).model(sonnet())),
                page("general, agent unavailable", |_, _| SettingsPage::new(Catalog::Unavailable)),
                page("general, saved model no longer offered", |_, _| {
                    SettingsPage::new(ready()).model(Some(("claude-cli".into(), "claude-opus-4-1".into())))
                }),
                page("threads", |_, _| SettingsPage::new(ready()).tab(SettingsTab::Threads).keep_done(Some(30)).fields(bundled())),
                page("notifications", |_, _| {
                    SettingsPage::new(ready()).tab(SettingsTab::Notifications).permission(NotificationPermission::Allowed).fields(bundled())
                }),
                page("plugins", |window, cx| {
                    let mut fields = bundled();
                    fields.push(key_field("page", true, FieldCheck::Saved, Some(("Working.", true)), window, cx));
                    SettingsPage::new(ready()).tab(SettingsTab::Plugins).plugins(plugins()).fields(fields)
                }),
                page("developer", |_, _| SettingsPage::new(ready()).tab(SettingsTab::Developer).fields(bundled())),
                page("about", |_, _| {
                    SettingsPage::new(ready()).tab(SettingsTab::About).build("0.1.6 +2 · a95ab81 · edited", Some("Today at 14:02".into()))
                }),
            ]),
        ComponentPreview::new(Area::Settings, "Typed plugin setting", "A value the plugin checks before it's saved; secrets are masked.")
            .stacked()
            .examples([
                key_example("key not set", false, || FieldCheck::Idle, None),
                key_example("checking a new key", false, || FieldCheck::Checking, None),
                key_example("rejected", false, || FieldCheck::Invalid("The service rejected this key.".into()), None),
                key_example(
                    "couldn't check (save anyway)",
                    false,
                    || FieldCheck::Unverifiable("Couldn't reach the service to check it (can't connect; offline?).".into()),
                    None,
                ),
                key_example("saved and working", true, || FieldCheck::Saved, Some(("Working.", true))),
                key_example(
                    "saved, stopped working",
                    true,
                    || FieldCheck::Idle,
                    Some(("The service rejected your key, so actions are paused. Last tried 2m 14s ago · next try in 2m 46s", false)),
                ),
            ]),
        ComponentPreview::new(Area::Settings, "Permission row", "macOS notification permission, per state.")
            .stacked()
            .examples([
                permission("allowed", NotificationPermission::Allowed),
                permission("denied", NotificationPermission::Denied),
                permission("not asked", NotificationPermission::NotAsked),
                permission("unbundled", NotificationPermission::Unbundled),
                permission("checking", NotificationPermission::Checking),
            ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::{SettingPage, SettingsTab};

    #[test]
    fn pages_with_nothing_on_them_arent_listed() {
        use SettingsTab::*;
        assert_eq!(SettingsTab::all(false, false, []), [General, Notifications, About]);
        assert_eq!(SettingsTab::all(true, true, []), [General, Threads, Notifications, Plugins, About]);
        // Plugin fields bring their page, in the pages' order.
        assert_eq!(SettingsTab::all(false, true, [Developer, Threads, Notifications]), [General, Threads, Notifications, Plugins, Developer, About]);
    }

    #[test]
    fn plugin_pages_map_onto_tabs() {
        assert_eq!(SettingsTab::from(SettingPage::Developer), SettingsTab::Developer);
        assert_eq!(SettingsTab::from(SettingPage::default()), SettingsTab::Plugins);
    }
}
