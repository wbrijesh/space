//! Settings page (⌘,): a heading and sections of rows. Each row is a label with a one-line
//! description on the left and its control on the right (dropdown, switch, status).
//!
//! Primitives ([`section`], [`row`], [`select`], [`toggle`]) plus [`SettingsPage`], which
//! lays out Space's settings from plain props. The page has no scroll container of its own;
//! the app wraps it in one.

use std::rc::Rc;

use gpui::component::button::Button;
use gpui::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui::component::switch::Switch;
use gpui::component::{Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::ModelInfo;

use super::composer::{Handler, IndexHandler};
use super::widgets::pill_button;
use crate::format;
use crate::theme::{color, radius, size, text};

pub type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type ModelHandler = Rc<dyn Fn(Option<ModelInfo>, &mut Window, &mut App)>;
pub type LevelHandler = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;

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
    pub on_notify: ToggleHandler,
    pub on_sound: ToggleHandler,
    /// Ask macOS for notification permission.
    pub on_allow: Handler,
    pub on_open_system_settings: Handler,
}

impl Default for SettingsHandlers {
    fn default() -> Self {
        Self {
            on_model: Rc::new(|_, _, _| {}),
            on_thinking: Rc::new(|_, _, _| {}),
            on_notify: Rc::new(|_, _, _| {}),
            on_sound: Rc::new(|_, _, _| {}),
            on_allow: Rc::new(|_, _| {}),
            on_open_system_settings: Rc::new(|_, _| {}),
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
    notify: bool,
    sound: bool,
    permission: NotificationPermission,
    handlers: SettingsHandlers,
}

impl SettingsPage {
    pub fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            model: None,
            thinking: None,
            notify: true,
            sound: true,
            permission: NotificationPermission::Checking,
            handlers: SettingsHandlers::default(),
        }
    }

    pub fn model(mut self, model: Option<(String, String)>) -> Self {
        self.model = model;
        self
    }

    pub fn thinking(mut self, level: Option<String>) -> Self {
        self.thinking = level;
        self
    }

    pub fn notify(mut self, on: bool) -> Self {
        self.notify = on;
        self
    }

    pub fn sound(mut self, on: bool) -> Self {
        self.sound = on;
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
        let Self { catalog, model, thinking, notify, sound, permission, handlers: h } = self;

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

        let (on_notify, on_sound) = (h.on_notify.clone(), h.on_sound.clone());
        let notifications = vec![
            row(
                "Notify when an agent finishes",
                "Shows a notification when you're not looking at the thread.",
                Some(toggle("settings-notify", notify, on_notify, window).into_any_element()),
            )
            .into_any_element(),
            row(
                "Play a sound",
                "When an agent finishes.",
                Some(toggle("settings-sound", sound, on_sound, window).into_any_element()),
            )
            .into_any_element(),
            permission_row(permission, &h).into_any_element(),
        ];

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_8()
            .child(div().text_size(text::HEADING).font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .child(section(
                "New threads",
                Some("Applies to threads you start from now on."),
                vec![model_row.into_any_element(), thinking_row.into_any_element()],
            ))
            .child(section("Notifications", None, notifications))
    }
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
        NotificationPermission::Unbundled => ("Unavailable outside the app bundle — run with `just run`.", None),
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

/// An on/off switch.
pub fn toggle(id: &'static str, checked: bool, on_change: ToggleHandler, window: &Window) -> Switch {
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

    vec![
        ComponentPreview::new(Area::Settings, "Settings page", "⌘, — defaults for new threads and notifications.")
            .stacked()
            .examples([
                page("models loaded, choice set", || {
                    SettingsPage::new(ready())
                        .model(sonnet())
                        .thinking(Some("high".into()))
                        .permission(NotificationPermission::Allowed)
                }),
                page("agent defaults, sound off", || {
                    SettingsPage::new(ready()).sound(false).permission(NotificationPermission::Allowed)
                }),
                page("loading models", || {
                    SettingsPage::new(Catalog::Loading).model(sonnet()).permission(NotificationPermission::Checking)
                }),
                page("agent unavailable, toggles off", || {
                    SettingsPage::new(Catalog::Unavailable)
                        .notify(false)
                        .sound(false)
                        .permission(NotificationPermission::Denied)
                }),
                page("saved model no longer offered", || {
                    SettingsPage::new(ready())
                        .model(Some(("claude-cli".into(), "claude-opus-4-1".into())))
                        .permission(NotificationPermission::NotAsked)
                }),
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
