//! Settings page (⌘,): a heading and sections of rows. Each row is a label with a one-line
//! description on the left and its control on the right (dropdown, switch, status, a field).
//!
//! Plugin settings are generic: each service plugin with settings gets a section built from
//! the fields it declares (secrets are masked and never shown once saved), and the Plugins
//! section turns service plugins on and off.
//!
//! Primitives ([`section`], [`row`], [`select`], [`toggle`]) plus [`SettingsPage`], which
//! lays out Space's settings from plain props. The page has no scroll container of its own;
//! the app wraps it in one.

use std::rc::Rc;

use gpui::component::button::Button;
use gpui::component::input::{Input, InputState};
use gpui::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui::component::switch::Switch;
use gpui::component::{Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::ModelInfo;
use space_core::protocol::service::CatalogEntry;

use super::composer::{Handler, IndexHandler};
use super::widgets::pill_button;
use crate::format;
use crate::theme::{color, radius, size, text};

pub type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type ModelHandler = Rc<dyn Fn(Option<ModelInfo>, &mut Window, &mut App)>;
pub type LevelHandler = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
/// `(plugin id, key)`.
pub type FieldHandler = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type PluginToggleHandler = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;
/// `(plugin id, key, on)`.
pub type FieldToggleHandler = Rc<dyn Fn(String, String, bool, &mut Window, &mut App)>;

/// One plugin setting field.
#[derive(Clone)]
pub struct FieldView {
    pub plugin_id: String,
    pub key: String,
    pub label: SharedString,
    pub description: SharedString,
    /// Masked, kept in the Keychain, never shown again once saved.
    pub secret: bool,
    /// A switch, and whether it's on (then `input`, `check` and `status` aren't shown).
    pub switch: Option<bool>,
    pub set: bool,
    pub input: Entity<InputState>,
    /// The last save: being checked, or what the plugin said.
    pub check: FieldCheck,
    /// How the saved value is doing, from the plugin (e.g. "Working", or why not): `(text, ok)`.
    pub status: Option<(SharedString, bool)>,
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

/// A plugin's settings section.
#[derive(Clone)]
pub struct PluginSettingsView {
    pub title: SharedString,
    pub fields: Vec<FieldView>,
}

/// A row of the Plugins section.
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
    /// Save the field's input as the setting's value.
    pub on_save_field: FieldHandler,
    /// Store the field's value although it couldn't be checked.
    pub on_save_anyway: FieldHandler,
    pub on_clear_field: FieldHandler,
    /// A switch setting was flipped.
    pub on_toggle_field: FieldToggleHandler,
    pub on_plugin_enabled: PluginToggleHandler,
    /// `(plugin id, approved)`.
    pub on_plugin_approval: PluginToggleHandler,
}

impl Default for SettingsHandlers {
    fn default() -> Self {
        Self {
            on_model: Rc::new(|_, _, _| {}),
            on_thinking: Rc::new(|_, _, _| {}),
            on_allow: Rc::new(|_, _| {}),
            on_open_system_settings: Rc::new(|_, _| {}),
            on_mode: Rc::new(|_, _, _| {}),
            on_save_field: Rc::new(|_, _, _, _| {}),
            on_save_anyway: Rc::new(|_, _, _, _| {}),
            on_clear_field: Rc::new(|_, _, _, _| {}),
            on_toggle_field: Rc::new(|_, _, _, _, _| {}),
            on_plugin_enabled: Rc::new(|_, _, _, _| {}),
            on_plugin_approval: Rc::new(|_, _, _, _| {}),
        }
    }
}

/// Space's settings.
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
    plugin_settings: Vec<PluginSettingsView>,
    plugins: Vec<PluginRow>,
    handlers: SettingsHandlers,
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
            plugin_settings: Vec::new(),
            plugins: Vec::new(),
            handlers: SettingsHandlers::default(),
        }
    }

    pub fn modes(mut self, modes: Vec<CatalogEntry>, default: Option<String>) -> Self {
        self.modes = modes;
        self.mode = default;
        self
    }

    pub fn plugin_settings(mut self, sections: Vec<PluginSettingsView>) -> Self {
        self.plugin_settings = sections;
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

/// "Claude Opus 5.5": the provider's display name without a "(latest)" suffix.
fn model_name(m: &ModelInfo) -> String {
    match m.name.as_deref().map(str::trim).map(|n| n.strip_suffix("(latest)").unwrap_or(n).trim_end()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format::model_label(&m.id, Some(m)),
    }
}

impl RenderOnce for SettingsPage {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let Self { catalog, model, thinking, permission, modes, mode, plugin_settings, plugins, handlers: h } = self;

        // Model: "Agent default" + the agent's models.
        let (model_row, thinking_row) = match &catalog {
            Catalog::Ready { models, levels } => {
                let chosen = model.as_ref().map(|(p, id)| models.iter().position(|m| &m.provider == p && &m.id == id));
                let model_label = match (&model, chosen) {
                    (Some(_), Some(Some(ix))) => model_name(&models[ix]).into(),
                    (Some((_, id)), _) => format::model_label(id, None).into(),
                    (None, _) => SharedString::from(AGENT_DEFAULT),
                };
                let model_desc = if matches!(chosen, Some(None)) {
                    "Not offered by the agent right now, so new threads use its default."
                } else {
                    "The model new threads start with."
                };
                let names = models.iter().map(|m| model_name(m).into());
                let options = std::iter::once(AGENT_DEFAULT.into()).chain(names).collect();
                let selected = match chosen {
                    None => Some(0),
                    Some(ix) => ix.map(|i| i + 1),
                };
                let (list, f) = (models.clone(), h.on_model.clone());
                // Index 0 is "Agent default".
                let on_model: IndexHandler =
                    Rc::new(move |ix, w, cx| f(ix.checked_sub(1).map(|i| list[i].clone()), w, cx));
                let control = select("settings-model", model_label, options, selected, Some(on_model), window);
                let model_row = row("Model", model_desc, Some(control));

                let level_label = thinking.as_deref().map_or(AGENT_DEFAULT.into(), format::effort_label);
                let names = levels.iter().map(|l| format::effort_label(l).into());
                let options = std::iter::once(AGENT_DEFAULT.into()).chain(names).collect();
                let selected = match &thinking {
                    None => Some(0),
                    Some(t) => levels.iter().position(|l| l == t).map(|i| i + 1),
                };
                let (list, f) = (levels.clone(), h.on_thinking.clone());
                let on_level: IndexHandler =
                    Rc::new(move |ix, w, cx| f(ix.checked_sub(1).map(|i| list[i].clone()), w, cx));
                let control = select("settings-thinking", level_label, options, selected, Some(on_level), window);
                (model_row, row("Thinking", "How much the model reasons first.", Some(control)))
            }
            Catalog::Loading | Catalog::Unavailable => {
                let desc = if catalog == Catalog::Loading { "Loading models…" } else { "Agent unavailable" };
                let model_label = model
                    .as_ref()
                    .map_or(SharedString::from(AGENT_DEFAULT), |(_, id)| format::model_label(id, None).into());
                let level_label = thinking.as_deref().map_or(AGENT_DEFAULT.into(), format::effort_label);
                (
                    row("Model", desc, Some(select("settings-model", model_label, vec![], None, None, window))),
                    row("Thinking", desc, Some(select("settings-thinking", level_label, vec![], None, None, window))),
                )
            }
        };

        // What to be told about is the alerts plugin's (its own section); whether macOS lets
        // Space post notifications at all is Space's.
        let notifications = vec![permission_row(permission, &h).into_any_element()];

        let mut new_threads = vec![model_row.into_any_element(), thinking_row.into_any_element()];
        if !modes.is_empty() {
            new_threads.push(mode_row(&modes, mode.as_deref(), h.on_mode.clone(), window).into_any_element());
        }
        let plugin_sections = plugin_settings.into_iter().map(|p| {
            let rows = p.fields.iter().map(|f| field_row(f, &h, window).into_any_element()).collect();
            section(p.title, None, rows)
        });
        let plugin_rows: Vec<AnyElement> =
            plugins.iter().map(|p| plugin_row(p, &h, window).into_any_element()).collect();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_8()
            .child(div().text_size(text::HEADING).font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .child(section("New threads", Some("Applies to threads you start from now on."), new_threads))
            .children(plugin_sections)
            .child(section("Notifications", None, notifications))
            .when(!plugin_rows.is_empty(), |d| {
                d.child(section("Plugins", Some("Turning a plugin off removes what it adds, at once."), plugin_rows))
            })
    }
}

/// "Permission mode" for new threads: the catalog's entries; the default one is marked.
fn mode_row(modes: &[CatalogEntry], chosen: Option<&str>, on_mode: LevelHandler, window: &Window) -> Div {
    let default_ix = modes.iter().position(|e| e.default).unwrap_or(0);
    let selected = chosen.and_then(|c| modes.iter().position(|e| e.id == c)).unwrap_or(default_ix);
    let options: Vec<SharedString> = modes.iter().map(|e| e.name.clone().into()).collect();
    let label = options.get(selected).cloned().unwrap_or_default();
    let ids: Vec<String> = modes.iter().map(|e| e.id.clone()).collect();
    let on_select: IndexHandler = Rc::new(move |ix, w, cx| on_mode(ids.get(ix).cloned(), w, cx));
    let control = select_plain("settings-mode", label, options, Some(selected), Some(on_select), window);
    row("Permission mode", "What the agent may do in a new thread.", Some(control))
}

/// One plugin setting: its label, whether it's set, a field, Save and (when set) Remove; under
/// it, how the last save went (checking, rejected, "Save anyway") or how the value is doing.
/// A switch is just its label and the switch.
pub fn field_row(f: &FieldView, h: &SettingsHandlers, window: &Window) -> Div {
    if let Some(on) = f.switch {
        let (toggle_field, pid, key) = (h.on_toggle_field.clone(), f.plugin_id.clone(), f.key.clone());
        let on_change: ToggleHandler = Rc::new(move |on, w, cx| toggle_field(pid.clone(), key.clone(), on, w, cx));
        let id = SharedString::from(format!("switch-{}-{}", f.plugin_id, f.key));
        return row(f.label.clone(), f.description.clone(), Some(toggle(id, on, on_change, window).into_any_element()));
    }
    let stored = match (f.secret, f.set) {
        (true, true) => "Saved in your Keychain.",
        (true, false) => "Not set. Stored in your Keychain once saved.",
        (false, true) => "Saved.",
        (false, false) => "Not set.",
    };
    let description: SharedString = if f.description.is_empty() {
        stored.into()
    } else {
        format!("{} {stored}", f.description).into()
    };
    let checking = f.check == FieldCheck::Checking;
    let (save, clear, anyway) = (h.on_save_field.clone(), h.on_clear_field.clone(), h.on_save_anyway.clone());
    let id = |what: &str| SharedString::from(format!("{what}-{}-{}", f.plugin_id, f.key));
    let (pid, key) = (f.plugin_id.clone(), f.key.clone());
    let (pid2, key2, pid3, key3) = (pid.clone(), key.clone(), pid.clone(), key.clone());
    let control = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(size::SETTINGS_SELECT_W).child(Input::new(&f.input).small()))
        .child(
            pill_button(id("save"), if checking { "Checking…" } else { "Save" }, !checking)
                .when(!checking, |b| b.on_click(move |_, w, cx| save(pid.clone(), key.clone(), w, cx))),
        )
        .when(f.set && !checking, |d| {
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
    let note: Option<AnyElement> = match &f.check {
        FieldCheck::Checking => Some(line(IconName::Loader, color::text_3(), "Checking it with the service…".into()).into_any_element()),
        FieldCheck::Invalid(m) => Some(line(IconName::CircleX, color::red(), format!("{m} It wasn't saved.").into()).into_any_element()),
        FieldCheck::Unverifiable(m) => Some(
            line(IconName::TriangleAlert, color::yellow(), m.clone())
                .child(pill_button(id("anyway"), "Save anyway", false).on_click(move |_, w, cx| anyway(pid3.clone(), key3.clone(), w, cx)))
                .into_any_element(),
        ),
        FieldCheck::Saved | FieldCheck::Idle => f.status.as_ref().map(|(text, ok)| {
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
        .children(note.map(|n| div().pl_4().pb_3().child(n)))
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
        .px_4()
        .pb_3()
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
        NotificationPermission::Checking => {
            ("Lets Space show notifications.", Some(status("Checking…", color::text_3()).into_any_element()))
        }
        NotificationPermission::Allowed => {
            ("Lets Space show notifications.", Some(status("Allowed", color::text_2()).into_any_element()))
        }
        NotificationPermission::Denied => {
            let f = h.on_open_system_settings.clone();
            (
                "Off in System Settings",
                Some(
                    pill_button("settings-open-system", "Open System Settings", false)
                        .on_click(move |_, w, cx| f(w, cx))
                        .into_any_element(),
                ),
            )
        }
        NotificationPermission::NotAsked => {
            let f = h.on_allow.clone();
            (
                "Space hasn't asked yet.",
                Some(
                    pill_button("settings-allow", "Allow…", false)
                        .on_click(move |_, w, cx| f(w, cx))
                        .into_any_element(),
                ),
            )
        }
        NotificationPermission::Unbundled => ("Unavailable in this build. Launch Space with just run to turn them on.", None),
    };
    row("macOS permission", desc, control)
}

/// A titled card of rows, with an optional hint under the title.
pub fn section(title: impl Into<SharedString>, hint: Option<&'static str>, rows: Vec<AnyElement>) -> Div {
    // Hairlines between rows.
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ix, r)| div().when(ix > 0, |d| d.border_t_1().border_color(color::border())).child(r));
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .px_1()
                .child(div().text_size(text::MD).font_weight(FontWeight::SEMIBOLD).child(title.into()))
                .when_some(hint, |d, hint| d.child(div().text_size(text::SM).text_color(color::text_3()).child(hint))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(radius::CARD)
                .bg(color::surface())
                .border_1()
                .border_color(color::border())
                .children(rows),
        )
}

/// One setting: label and one-line description on the left, `control` on the right.
pub fn row(label: impl Into<SharedString>, description: impl Into<SharedString>, control: Option<AnyElement>) -> Div {
    div()
        .min_h(size::SETTINGS_ROW_H)
        .flex()
        .items_center()
        .gap_6()
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(text::MD).child(label.into()))
                .child(div().text_size(text::SM).text_color(color::text_2()).child(description.into())),
        )
        .children(control.map(|c| div().flex_none().flex().items_center().child(c)))
}

/// A dropdown showing `label`. `options[selected]` is checked in the menu; a separator follows
/// the first option (the "default" entry). `on_select: None` disables it.
pub fn select(
    id: &'static str,
    label: impl Into<SharedString>,
    options: Vec<SharedString>,
    selected: Option<usize>,
    on_select: Option<IndexHandler>,
    window: &Window,
) -> AnyElement {
    let enabled = on_select.is_some();
    let trigger = Button::new(id)
        .outline()
        .small()
        // Menus hand focus back to their trigger on close; only show a ring for keyboard users.
        .focus_ring(window.last_input_was_keyboard())
        .w(size::SETTINGS_SELECT_W)
        .rounded(radius::MD)
        .text_size(text::MD)
        .disabled(!enabled)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(if enabled { color::text() } else { color::text_2() })
                        .child(label.into()),
                )
                .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3())),
        );
    let Some(on_select) = on_select else {
        return trigger.into_any_element();
    };
    trigger
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(size::SETTINGS_SELECT_W).max_h(px(420.)).scrollable(true);
            for (ix, label) in options.iter().enumerate() {
                let f = on_select.clone();
                let item = PopupMenuItem::new(label.clone()).checked(selected == Some(ix));
                menu = menu.item(item.on_click(move |_, w, cx| f(ix, w, cx)));
                if ix == 0 && options.len() > 1 {
                    menu = menu.separator();
                }
            }
            menu
        })
        .into_any_element()
}

/// A dropdown like [`select`], without the separator after the first option.
pub fn select_plain(
    id: &'static str,
    label: impl Into<SharedString>,
    options: Vec<SharedString>,
    selected: Option<usize>,
    on_select: Option<IndexHandler>,
    window: &Window,
) -> AnyElement {
    let Some(on_select) = on_select else {
        return select(id, label, options, selected, None, window);
    };
    Button::new(id)
        .outline()
        .small()
        .focus_ring(window.last_input_was_keyboard())
        .w(size::SETTINGS_SELECT_W)
        .rounded(radius::MD)
        .text_size(text::MD)
        .child(select_label(label.into(), true))
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(size::SETTINGS_SELECT_W).max_h(px(420.)).scrollable(true);
            for (ix, label) in options.iter().enumerate() {
                let f = on_select.clone();
                menu = menu.item(PopupMenuItem::new(label.clone()).checked(selected == Some(ix)).on_click(move |_, w, cx| f(ix, w, cx)));
            }
            menu
        })
        .into_any_element()
}

fn select_label(label: SharedString, enabled: bool) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(div().min_w_0().truncate().text_color(if enabled { color::text() } else { color::text_2() }).child(label))
        .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3()))
}

/// An on/off switch.
pub fn toggle(id: impl Into<ElementId>, checked: bool, on_change: ToggleHandler, window: &Window) -> Switch {
    Switch::new(id)
        .checked(checked)
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
    fn page(name: &'static str, f: fn() -> SettingsPage) -> Example {
        Example::new(name, move |_, _| f().into_any_element()).width(size::CONTENT_MAX_W)
    }
    fn permission(name: &'static str, p: NotificationPermission) -> Example {
        Example::new(name, move |_, _| {
            section("Notifications", None, vec![permission_row(p, &SettingsHandlers::default()).into_any_element()])
                .into_any_element()
        })
        .width(size::CONTENT_MAX_W)
    }
    fn sonnet() -> Option<(String, String)> {
        Some(("claude-cli".into(), "claude-sonnet-5-5".into()))
    }
    fn modes() -> Vec<CatalogEntry> {
        super::composer::fixtures::modes("workspace-write", true).entries
    }
    /// The permissions plugin's section, its key field over a persistent input.
    fn key_section(name: &'static str, set: bool, check: fn() -> FieldCheck, status: Option<(&'static str, bool)>) -> Example {
        Example::new(name, move |window, cx| {
            let input = window.use_keyed_state(SharedString::from(format!("p-key-{name}")), cx, |window, cx| {
                InputState::new(window, cx).masked(true).placeholder(if set { "Enter a new key to replace it" } else { "sk-…" })
            });
            let field = FieldView {
                plugin_id: "permissions".into(),
                key: "openai_api_key".into(),
                label: "OpenAI API key".into(),
                description: "For the decision model (gpt-6-luna) that checks every operation in Workspace Write and Full access."
                    .into(),
                secret: true,
                switch: None,
                set,
                input,
                check: check(),
                status: status.map(|(t, ok)| (t.into(), ok)),
            };
            section("Permissions", None, vec![field_row(&field, &SettingsHandlers::default(), window).into_any_element()])
                .into_any_element()
        })
        .width(size::CONTENT_MAX_W)
    }
    fn plugins() -> Vec<PluginRow> {
        vec![
            PluginRow {
                id: "pi-agent".into(),
                name: "Pi Agent".into(),
                description: "Coding agent on the pi SDK, using the Claude CLI subscription.".into(),
                agent: true,
                required: false,
                enabled: true,
                status: "Running".into(),
                approval: None,
            },
            PluginRow {
                id: "permissions".into(),
                name: "Permissions".into(),
                description: "Read Only runs commands in the macOS sandbox; Workspace Write and Full access check every \
                              operation with a decision model first."
                    .into(),
                agent: false,
                required: true,
                enabled: true,
                status: "Running".into(),
                approval: None,
            },
        ]
    }

    vec![
        ComponentPreview::new(Area::Settings, "Settings page", "⌘, — defaults for new threads, plugin settings, notifications.")
            .stacked()
            .examples([
                page("models loaded, choice set", || {
                    SettingsPage::new(ready())
                        .model(sonnet())
                        .thinking(Some("high".into()))
                        .permission(NotificationPermission::Allowed)
                        .modes(modes(), Some("read-only".into()))
                        .plugins(plugins())
                }),
                page("agent defaults", || SettingsPage::new(ready()).permission(NotificationPermission::Allowed)),
                page("loading models", || {
                    SettingsPage::new(Catalog::Loading).model(sonnet()).permission(NotificationPermission::Checking)
                }),
                page("agent unavailable, notifications denied", || {
                    SettingsPage::new(Catalog::Unavailable).permission(NotificationPermission::Denied)
                }),
                page("saved model no longer offered", || {
                    SettingsPage::new(ready())
                        .model(Some(("claude-cli".into(), "claude-opus-4-1".into())))
                        .permission(NotificationPermission::NotAsked)
                }),
            ]),
        ComponentPreview::new(Area::Settings, "Plugin settings", "Fields a plugin declares; secrets are masked.")
            .stacked()
            .examples([
                Example::new("switches (alerts)", |window, cx| {
                    let input = window.use_keyed_state("alerts-unused", cx, InputState::new);
                    let switch = |key: &str, label: &'static str, description: &'static str, on: bool| FieldView {
                        plugin_id: "alerts".into(),
                        key: key.into(),
                        label: label.into(),
                        description: description.into(),
                        secret: false,
                        switch: Some(on),
                        set: !on,
                        input: input.clone(),
                        check: FieldCheck::Idle,
                        status: None,
                    };
                    let fields = [
                        switch("notify", "Notify when an agent finishes", "Shows a notification when you're not looking at the thread.", true),
                        switch("sound", "Play a sound", "When an agent finishes.", false),
                    ];
                    let h = SettingsHandlers::default();
                    section("Alerts", None, fields.iter().map(|f| field_row(f, &h, window).into_any_element()).collect())
                        .into_any_element()
                })
                .width(size::CONTENT_MAX_W),
                key_section("key not set", false, || FieldCheck::Idle, None),
                key_section("checking a new key", false, || FieldCheck::Checking, None),
                key_section("rejected", false, || FieldCheck::Invalid("OpenAI rejected this key.".into()), None),
                key_section(
                    "couldn't check (save anyway)",
                    false,
                    || FieldCheck::Unverifiable("Couldn't reach OpenAI to check it (can't connect; offline?).".into()),
                    None,
                ),
                key_section("saved and working", true, || FieldCheck::Saved, Some(("Working.", true))),
                key_section(
                    "saved, stopped working",
                    true,
                    || FieldCheck::Idle,
                    Some(("OpenAI rejected your API key, so actions are paused. Last tried 2m 14s ago · next try in 2m 46s", false)),
                ),
            ]),
        ComponentPreview::new(
            Area::Settings,
            "Plugins",
            "Service plugins can be turned off; agents and required guards can't. Plugins from your home or project \
             folder wait for your approval.",
        )
            .stacked()
            .examples([Example::new("plugins", |window, _| {
                let mut rows = plugins();
                rows.push(PluginRow {
                    id: "audit".into(),
                    name: "Audit log".into(),
                    description: "Writes every tool call to a file.".into(),
                    agent: false,
                    required: false,
                    enabled: false,
                    status: "Off".into(),
                    approval: None,
                });
                let asks = |id: &str, name: &str, status: &str, denied: bool| PluginRow {
                    id: id.into(),
                    name: name.to_string().into(),
                    description: "From ~/.space/plugins.".into(),
                    agent: false,
                    required: false,
                    enabled: true,
                    status: status.to_string().into(),
                    approval: Some(PluginApproval {
                        permissions: vec![
                            "Is told when the agent finishes".into(),
                            "Shows notifications".into(),
                            "Reads what you and the agent write".into(),
                        ],
                        denied,
                    }),
                };
                rows.push(asks("alerts", "Alerts", "Needs your approval", false));
                rows.push(asks("titles", "Titles", "Not approved", true));
                let h = SettingsHandlers::default();
                let rows = rows.iter().map(|p| plugin_row(p, &h, window).into_any_element()).collect();
                section("Plugins", None, rows).into_any_element()
            })
            .width(size::CONTENT_MAX_W)]),
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
