//! Composer: the input card (+ · text · send/stop) and the control row below it (project
//! folder, permission mode, model, effort, context ring). A mode that isn't set up yet blocks
//! sending and says why, with a link to fix it.
//!
//! One visual line: a single row. More (a newline, or text wider than the single-row text
//! column): two zones, text across the full width over a row with + and send.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::component::input::{Textarea, TextareaState};
use gpui::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui::assets::IconName as Lucide;
use gpui::component::{Disableable as _, Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::service::CatalogEntry;
use space_core::protocol::{ConfigureSession, ModelInfo, SpaceStatus};

use super::loaders::ContextRing;
use super::transcript::attachment_chip;
use super::widgets::{icon_button, menu_trigger, tip};
use crate::format;
use crate::theme::{color, radius, size, space, text};

/// Card padding.
const PAD: f32 = space::MD;
const BUTTON: f32 = size::ROUND_BUTTON;
/// The textarea's line height: 1.25rem.
const LINE: f32 = 20.;
const SEND_GAP: f32 = space::SM;
/// Expanded mode: wrapper inset so text (inset + the editor's 4px) starts at the "+" icon.
const TEXT_INSET: f32 = space::XS;
/// The editor's own horizontal padding at XSmall size (each side).
const EDITOR_PAD: f32 = space::XS;
/// Control-row insets: the folder icon centers under "+" (border 1 + padding + half the
/// button, minus half the 14px icon and the menu button's 8px padding), and the context
/// ring's right edge meets the send button's (border 1 + padding).
const ROW_INSET_LEFT: f32 = 1. + PAD + BUTTON / 2. - 7. - space::MD;
const ROW_INSET_RIGHT: f32 = 1. + PAD;
pub const MAX_LINES: usize = 10;
/// Key context around the composer while its text spans more than one line.
pub const MULTILINE_CONTEXT: &str = "ComposerMultiline";

/// A textarea configured for the composer (placeholder, 1–10 line auto-grow, Enter submits).
pub fn new_input(window: &mut Window, cx: &mut Context<TextareaState>) -> TextareaState {
    let mut state = TextareaState::new(window, cx).placeholder("Describe a task or ask a question");
    state.set_auto_grow(1, MAX_LINES, cx);
    state.set_submit_on_enter(true, cx);
    state
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type PathHandler = Rc<dyn Fn(PathBuf, &mut Window, &mut App)>;
pub type ConfigureHandler = Rc<dyn Fn(ConfigureSession, &mut Window, &mut App)>;
pub type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type StringHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// Callbacks; every one defaults to a no-op.
#[derive(Clone)]
pub struct ComposerHandlers {
    pub on_send: Handler,
    pub on_stop: Handler,
    pub on_attach: Handler,
    pub on_focus: Handler,
    pub on_remove_attachment: IndexHandler,
    pub on_pick_dir: PathHandler,
    pub on_choose_folder: Handler,
    pub on_configure: ConfigureHandler,
    /// A permission mode was picked (its catalog id).
    pub on_mode: StringHandler,
    /// The blocker's link was clicked.
    pub on_blocker_action: Handler,
}

impl Default for ComposerHandlers {
    fn default() -> Self {
        let noop: Handler = Rc::new(|_, _| {});
        Self {
            on_send: noop.clone(),
            on_stop: noop.clone(),
            on_attach: noop.clone(),
            on_focus: noop.clone(),
            on_remove_attachment: Rc::new(|_, _, _| {}),
            on_pick_dir: Rc::new(|_, _, _| {}),
            on_choose_folder: noop,
            on_configure: Rc::new(|_, _, _| {}),
            on_mode: Rc::new(|_, _, _| {}),
            on_blocker_action: Rc::new(|_, _| {}),
        }
    }
}

/// Data for the control row.
#[derive(Clone, Default)]
pub struct ComposerControls {
    pub cwd: PathBuf,
    /// Folders offered in the folder menu.
    pub dirs: Vec<PathBuf>,
    /// The thread has no messages yet (the folder can still change).
    pub thread_empty: bool,
    pub status: SpaceStatus,
    /// Model and effort can't change now (turn running or no session).
    pub locked: bool,
    /// The permission mode menu; `None` hides it (no plugin publishes modes).
    pub mode: Option<ModeChoice>,
    /// Sending is blocked until this is resolved (e.g. a mode that needs setting up).
    pub blocker: Option<ComposerBlocker>,
}

/// The thread's permission mode and the modes it can switch to (the `permission-modes` catalog).
#[derive(Clone, Default)]
pub struct ModeChoice {
    pub entries: Vec<CatalogEntry>,
    pub current: String,
}

impl ModeChoice {
    pub fn current_entry(&self) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.id == self.current)
    }
}

/// Why the composer can't send, and the link that fixes it.
#[derive(Clone)]
pub struct ComposerBlocker {
    pub text: SharedString,
    pub action_label: Option<SharedString>,
}

#[derive(IntoElement)]
pub struct Composer {
    id: ElementId,
    input: Entity<TextareaState>,
    running: bool,
    /// A message is waiting for the session; sending is blocked.
    queued: bool,
    attachments: Vec<PathBuf>,
    controls: Option<ComposerControls>,
    handlers: ComposerHandlers,
}

impl Composer {
    pub fn new(id: impl Into<ElementId>, input: &Entity<TextareaState>) -> Self {
        Self {
            id: id.into(),
            input: input.clone(),
            running: false,
            queued: false,
            attachments: Vec::new(),
            controls: None,
            handlers: ComposerHandlers::default(),
        }
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    pub fn queued(mut self, queued: bool) -> Self {
        self.queued = queued;
        self
    }

    pub fn attachments(mut self, attachments: Vec<PathBuf>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn controls(mut self, controls: ComposerControls) -> Self {
        self.controls = Some(controls);
        self
    }

    pub fn handlers(mut self, handlers: ComposerHandlers) -> Self {
        self.handlers = handlers;
        self
    }
}

impl RenderOnce for Composer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, input, running, queued, attachments, controls, handlers: h } = self;
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let has_text = !input.read(cx).value().trim().is_empty();
        let blocker = controls.as_ref().and_then(|c| c.blocker.clone());
        let can_send = (has_text || !attachments.is_empty()) && !running && !queued && blocker.is_none();

        let expanded_state = window.use_keyed_state((id.clone(), "expanded"), cx, |_, _| false);
        let expanded = is_expanded(&input, *expanded_state.read(cx), window, cx);
        expanded_state.update(cx, |e, _| *e = expanded);

        let chips = (!attachments.is_empty()).then(|| {
            let remove = h.on_remove_attachment.clone();
            div().flex().flex_wrap().gap_1p5().px_1().children(attachments.iter().enumerate().map(|(ix, p)| {
                let remove = remove.clone();
                attachment_chip(p).pr_0p5().child(
                    div()
                        .id(("remove-attachment", ix))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius::XS)
                        .cursor_pointer()
                        .hover(|s| s.bg(color::hover()).text_color(color::text()))
                        .child(Icon::new(IconName::Close).size(size::ICON_XS))
                        .on_click(move |_, w, cx| remove(ix, w, cx)),
                )
            }))
        });

        let send: AnyElement = if running {
            let stop = h.on_stop.clone();
            div()
                .id("stop")
                .size(px(BUTTON))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::text())
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .child(div().size(px(10.)).rounded(px(2.)).bg(color::bg()))
                .tooltip(tip("Stop"))
                .on_click(move |_, w, cx| stop(w, cx))
                .into_any_element()
        } else {
            let send = h.on_send.clone();
            div()
                .id("send")
                .size(px(BUTTON))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .map(|d| {
                    if can_send {
                        d.bg(color::text()).text_color(color::bg()).cursor_pointer().hover(|s| s.opacity(0.85))
                    } else {
                        d.bg(color::pressed()).text_color(color::text_3())
                    }
                })
                .child(Icon::new(IconName::ArrowUp).size(size::ICON_MD))
                .when(can_send, |d| d.tooltip(tip("Send  ⏎")).on_click(move |_, w, cx| send(w, cx)))
                .when(blocker.is_some(), |d| d.tooltip(tip("Can't send in this mode yet")))
                .into_any_element()
        };

        let (attach_fn, focus_fn) = (h.on_attach.clone(), h.on_focus.clone());
        let attach = icon_button("attach", IconName::Plus, true, false)
            .size(px(BUTTON))
            .rounded_full()
            .tooltip(tip("Attach files or folders"))
            .on_click(move |_, w, cx| {
                focus_fn(w, cx);
                attach_fn(w, cx);
            });
        // XSmall: no vertical editor padding, 4px horizontal.
        let textarea = Textarea::new(&input).appearance(false).xsmall().text_size(text::BODY);

        let body = if expanded {
            // Two zones: text across the full width (its first column lines up with the "+"
            // icon below), the action row under it. The card grows upward.
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().pt(px(space::XS)).px(px(TEXT_INSET)).child(textarea))
                .child(div().flex().items_center().justify_between().child(attach).child(send))
        } else {
            // One row. Line height 20 + 6px top/bottom = the 32px buttons, so the line sits on
            // their center line. No gap after "+": its icon has 8px of air plus the editor's 4px.
            div()
                .flex()
                .items_center()
                .child(attach)
                .child(div().flex_1().min_w_0().py(px((BUTTON - LINE) / 2.)).child(textarea))
                .child(div().flex_none().ml(px(SEND_GAP)).child(send))
        };

        let focus = h.on_focus.clone();
        let card = div()
            .id(id)
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p(px(PAD))
            .rounded(radius::CARD)
            .bg(color::surface())
            .border_1()
            .border_color(if focused { color::border_strong() } else { color::border() })
            .shadow_md()
            .cursor_text()
            // Clicking the card's padding focuses the input, like a real text field.
            .on_click(move |_, w, cx| focus(w, cx))
            .children(chips)
            .child(body);

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            // More than one line: ⌘↓ moves the caret to the end instead of scrolling the
            // transcript (see the key bindings in space-app).
            .when(expanded, |d| d.key_context(MULTILINE_CONTEXT))
            .children(blocker.map(|b| blocker_row(b, h.on_blocker_action.clone())))
            .child(card)
            .children(controls.map(|c| control_row(c, &h, window)))
    }
}

/// "Set up the decision model to use this mode · Open Settings", above the card.
pub fn blocker_row(blocker: ComposerBlocker, on_action: Handler) -> Div {
    // A centered pill in the warning tint, so it reads as "do this first", not as a caption.
    let pill = div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .rounded_full()
        .bg(color::yellow_bg())
        .border_1()
        .border_color(color::yellow_border())
        .text_size(text::SM)
        .text_color(color::text())
        .child(Icon::new(Lucide::ShieldAlert).size(size::ICON).text_color(color::yellow()))
        .child(blocker.text)
        .when_some(blocker.action_label, |d, label| {
            d.child(
                div()
                    .id("blocker-action")
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(color::yellow())
                    .cursor_pointer()
                    .hover(|s| s.text_color(color::text()))
                    .child(label)
                    .child(Icon::new(IconName::ChevronRight).size(size::ICON_SM))
                    .on_click(move |_, w, cx| on_action(w, cx)),
            )
        });
    // Extra space under it on top of the stack's gap, so it stands apart from the card.
    div().flex().justify_center().px(px(space::MD)).mb(px(space::MD)).child(pill)
}

/// The Lucide icon for a catalog icon name.
pub fn mode_icon(icon: Option<&str>) -> Lucide {
    match icon {
        Some("shield-check") => Lucide::ShieldCheck,
        Some("shield-half") => Lucide::ShieldHalf,
        Some("shield-alert") => Lucide::ShieldAlert,
        _ => Lucide::Shield,
    }
}

/// Two-zone layout when the text has more than one visual line *in the single-row layout*.
/// Measuring against the single-row column width in both modes keeps the switch stable (no
/// flip-flop) and decides before layout, so the new text and the new shape land together.
fn is_expanded(input: &Entity<TextareaState>, was_expanded: bool, window: &mut Window, cx: &App) -> bool {
    let state = input.read(cx);
    let value = state.value();
    if value.contains('\n') {
        return true;
    }
    if value.is_empty() {
        return false;
    }
    let editor_width = state.input_bounds().size.width;
    if editor_width <= px(0.) {
        return was_expanded;
    }
    let single_row_editor =
        if was_expanded { editor_width + px(2. * TEXT_INSET) - px(2. * BUTTON + SEND_GAP) } else { editor_width };
    let available = single_row_editor - px(2. * EDITOR_PAD);
    let run = TextRun {
        len: value.len(),
        font: font(text::UI_FONT),
        color: color::text(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(value, text::BODY, &[run], None).width > available
}

fn control_row(c: ComposerControls, h: &ComposerHandlers, window: &Window) -> Div {
    let status = c.status.clone();
    let folder = {
        let (dirs, cwd, empty) = (c.dirs.clone(), c.cwd.clone(), c.thread_empty);
        let (pick, choose) = (h.on_pick_dir.clone(), h.on_choose_folder.clone());
        menu_trigger("project-menu", window)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(color::text_2())
                    .child(Icon::new(IconName::Folder).size(size::ICON))
                    .child(div().max_w(px(220.)).truncate().child(format::dir_name(&c.cwd)))
                    .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3())),
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                folder_menu(menu, &dirs, &cwd, empty, pick.clone(), choose.clone())
            })
    };

    let mut right = div().flex().items_center().gap_0p5();
    if let Some(model) = &status.model {
        let models = status.available_models.clone().unwrap_or_default();
        let locked = c.locked || models.is_empty();
        let trigger = menu_trigger("model-menu", window)
            .disabled(locked)
            .child(div().text_color(color::text_2()).child(format::model_label(model, status.current_model())));
        right = right.child(if locked {
            trigger.into_any_element()
        } else {
            let (current, configure) = (model.clone(), h.on_configure.clone());
            trigger
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    model_menu(menu, &models, &current, configure.clone())
                })
                .into_any_element()
        });
    }
    if let Some(level) = &status.thinking_level {
        let levels = status.thinking_levels.clone().unwrap_or_default();
        let locked = c.locked || levels.is_empty();
        let trigger = menu_trigger("effort-menu", window)
            .disabled(locked)
            .child(div().text_color(color::text_3()).child(format::effort_label(level)));
        right = right.child(if locked {
            trigger.into_any_element()
        } else {
            let (current, configure) = (level.clone(), h.on_configure.clone());
            trigger
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    effort_menu(menu, &levels, &current, configure.clone())
                })
                .into_any_element()
        });
    }
    let tokens = status.context_tokens.unwrap_or(0);
    let window_size = status.context_window.filter(|w| *w > 0);
    right = right.child(
        div()
            .id("context-ring")
            .ml_2()
            .child(ContextRing::new(window_size.map_or(0., |w| tokens as f32 / w as f32)))
            .tooltip(tip(format::context_usage(tokens, window_size))),
    );

    let mode = c.mode.filter(|m| !m.entries.is_empty()).map(|m| {
        let current = m.current_entry().cloned();
        let icon = mode_icon(current.as_ref().and_then(|e| e.icon.as_deref()));
        let alert = icon == Lucide::ShieldAlert;
        let name = current.map_or_else(|| SharedString::from("Permissions"), |e| e.name.into());
        let on_mode = h.on_mode.clone();
        menu_trigger("mode-menu", window)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(color::text_2())
                    .child(Icon::new(icon).size(size::ICON).text_color(if alert { color::yellow() } else { color::text_2() }))
                    .child(name)
                    .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3())),
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                mode_menu(menu, &m.entries, &m.current, on_mode.clone())
            })
    });

    div()
        .flex()
        .items_center()
        .justify_between()
        .pl(px(ROW_INSET_LEFT))
        .pr(px(ROW_INSET_RIGHT))
        .child(div().flex().items_center().gap_1().min_w_0().child(folder).children(mode))
        .child(right)
}

/// Permission mode picker contents: one item per catalog entry, the current one checked.
pub fn mode_menu(menu: PopupMenu, entries: &[CatalogEntry], current: &str, on_mode: StringHandler) -> PopupMenu {
    // The check goes on the right. Each row draws its own shield: the menu's icon slot is fixed
    // at xsmall and would indent the "Permissions" label to the text column.
    let mut menu = menu.min_w(px(220.)).check_side(gpui::component::Side::Right).label("Permissions");
    for e in entries {
        let (id, f) = (e.id.clone(), on_mode.clone());
        let (name, icon) = (SharedString::from(e.name.clone()), mode_icon(e.icon.as_deref()));
        let item = PopupMenuItem::element(move |_, _| {
            div().flex().items_center().gap_2().child(Icon::new(icon).size(size::ICON_MD)).child(name.clone())
        })
            .checked(e.id == current)
            .on_click(move |_, w, cx| f(id.clone(), w, cx));
        menu = menu.item(item);
    }
    menu
}

/// Model picker contents (used by the dropdown and the gallery).
pub fn model_menu(
    menu: PopupMenu,
    models: &[ModelInfo],
    current: &str,
    on_configure: ConfigureHandler,
) -> PopupMenu {
    let mut menu = menu.min_w(px(200.)).max_h(px(420.)).scrollable(true).label("Model");
    for m in models {
        let (provider, id, f) = (m.provider.clone(), m.id.clone(), on_configure.clone());
        menu = menu.item(PopupMenuItem::new(format::model_label(&m.id, Some(m))).checked(m.id == current).on_click(
            move |_, w, cx| {
                f(ConfigureSession { provider: Some(provider.clone()), model: Some(id.clone()), ..Default::default() }, w, cx)
            },
        ));
    }
    menu
}

/// Thinking-effort picker contents.
pub fn effort_menu(
    menu: PopupMenu,
    levels: &[String],
    current: &str,
    on_configure: ConfigureHandler,
) -> PopupMenu {
    let mut menu = menu.min_w(px(160.)).label("Thinking effort");
    for l in levels {
        let (level, f) = (l.clone(), on_configure.clone());
        menu = menu.item(PopupMenuItem::new(format::effort_label(l)).checked(l == current).on_click(move |_, w, cx| {
            f(ConfigureSession { thinking_level: Some(level.clone()), ..Default::default() }, w, cx)
        }));
    }
    menu
}

/// Folder picker: recent folders and "Choose folder…". For a non-empty thread, picking a
/// folder starts a new thread there.
pub fn folder_menu(
    menu: PopupMenu,
    dirs: &[PathBuf],
    cwd: &PathBuf,
    thread_empty: bool,
    on_pick: PathHandler,
    on_choose: Handler,
) -> PopupMenu {
    let mut menu = menu.min_w(px(260.)).label(if thread_empty { "Project folder" } else { "New thread in…" });
    for dir in dirs {
        let (d, f) = (dir.clone(), on_pick.clone());
        menu = menu.item(
            PopupMenuItem::new(format::abbreviate_home(dir))
                .icon(IconName::Folder)
                .checked(thread_empty && dir == cwd)
                .on_click(move |_, w, cx| f(d.clone(), w, cx)),
        );
    }
    menu.separator()
        .item(PopupMenuItem::new("Choose folder…").icon(IconName::FolderOpen).on_click(move |_, w, cx| on_choose(w, cx)))
}

#[cfg(feature = "dev")]
pub mod fixtures {
    use super::*;

    pub fn status(tokens: u64) -> SpaceStatus {
        let m = |id: &str, name: &str| ModelInfo { provider: "claude-cli".into(), id: id.into(), name: Some(name.into()) };
        SpaceStatus {
            session_id: "preview".into(),
            provider: Some("claude-cli".into()),
            model: Some("claude-opus-5-5".into()),
            thinking_level: Some("medium".into()),
            available_models: Some(vec![
                m("claude-opus-5-5", "Claude Opus 5.5"),
                m("claude-sonnet-5-5", "Claude Sonnet 5.5"),
                m("claude-haiku-5-5", "Claude Haiku 5.5"),
                m("claude-fable-5", "Claude Fable 5"),
            ]),
            thinking_levels: Some(["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec()),
            context_tokens: Some(tokens),
            context_window: Some(1_000_000),
        }
    }

    pub fn controls(tokens: u64, locked: bool) -> ComposerControls {
        let cwd = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("projects/space");
        ComposerControls {
            dirs: vec![cwd.clone()],
            cwd,
            thread_empty: false,
            status: status(tokens),
            locked,
            mode: Some(modes("workspace-write", true)),
            blocker: None,
        }
    }

    /// The permissions plugin's three modes; `ready`: the decision model is set up.
    pub fn modes(current: &str, ready: bool) -> ModeChoice {
        let entry = |id: &str, name: &str, icon: &str, default: bool, needs_setup: bool| CatalogEntry {
            id: id.into(),
            name: name.into(),
            icon: Some(icon.into()),
            default,
            ready: ready || !needs_setup,
            ..Default::default()
        };
        ModeChoice {
            entries: vec![
                entry("read-only", "Read Only", "shield-check", false, false),
                entry("workspace-write", "Workspace Write", "shield-half", true, true),
                entry("full-access", "Full access", "shield-alert", false, true),
            ],
            current: current.into(),
        }
    }

    pub fn with_mode(mut c: ComposerControls, current: &str, ready: bool) -> ComposerControls {
        c.mode = Some(modes(current, ready));
        if !ready && current != "read-only" {
            c.blocker = Some(ComposerBlocker {
                text: "Set up the decision model to use this mode.".into(),
                action_label: Some("Open Settings".into()),
            });
        }
        c
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};

    /// A composer over its own persistent input entity, preset to `text`.
    fn example(name: &'static str, text_: &'static str, f: fn(Composer) -> Composer) -> Example {
        Example::new(name, move |window, cx| {
            let input = window.use_keyed_state(SharedString::from(format!("composer-input-{name}")), cx, |window, cx| {
                let mut state = new_input(window, cx);
                state.set_value(text_, window, cx);
                state
            });
            f(Composer::new(SharedString::from(format!("composer-{name}")), &input).controls(fixtures::controls(18_200, false)))
                .into_any_element()
        })
        .width(size::CONTENT_MAX_W)
    }
    let twelve = "1 Collect the failing cases\n2 Reproduce locally\n3 Bisect\n4 Patch\n5 Test\n6 Clippy\n7 Docs\n8 Changelog\n9 Review\n10 Merge\n11 Tag\n12 Release";

    fn menu(name: &'static str, build: fn(PopupMenu) -> PopupMenu) -> Example {
        Example::new(name, move |window, cx| {
            let menu = window.use_keyed_state(SharedString::from(format!("menu-{name}")), cx, |window, cx| {
                PopupMenu::build(window, cx, move |m, _, _| build(m))
            });
            menu.read(cx).clone().into_any_element()
        })
    }

    vec![
        ComponentPreview::new(Area::Composer, "Composer", "Input card and control row.").stacked().examples([
            example("empty", "", |c| c),
            example("one line", "Fix the flaky test in space-core", |c| c),
            example("two lines", "Fix the flaky test in space-core\nthen explain the retry logic", |c| c),
            example(
                "soft wrap",
                "Fix the flaky test in space-core and explain what was wrong with the retry logic in the stub agent, \
                 with a short example",
                |c| c,
            ),
            example("12 lines (scrolls)", twelve, |c| c),
            example("running (stop)", "", |c| c.running(true).controls(fixtures::controls(452_000, true))),
            example("attachments", "Review these", |c| {
                c.attachments(vec![PathBuf::from("Cargo.toml"), PathBuf::from("README.md")])
            }),
            example("agent starting (locked)", "", |c| c.queued(true).controls(fixtures::controls(0, true))),
            example("read only", "Why does the composer flicker when it grows?", |c| {
                c.controls(fixtures::with_mode(fixtures::controls(18_200, false), "read-only", true))
            }),
            example("full access", "", |c| c.controls(fixtures::with_mode(fixtures::controls(18_200, false), "full-access", true))),
            example("mode not set up (sending blocked)", "Clean up the build cache", |c| {
                c.controls(fixtures::with_mode(fixtures::controls(18_200, false), "workspace-write", false))
            }),
            example("no permission modes (no plugin)", "", |c| {
                c.controls(ComposerControls { mode: None, ..fixtures::controls(18_200, false) })
            }),
        ]),
        ComponentPreview::new(Area::Composer, "Menus", "Dropdown contents, rendered open.").examples([
            menu("model", |m| model_menu(m, &fixtures::status(0).available_models.unwrap(), "claude-opus-5-5", Rc::new(|_, _, _| {}))),
            menu("effort", |m| {
                effort_menu(m, &fixtures::status(0).thinking_levels.unwrap(), "medium", Rc::new(|_, _, _| {}))
            }),
            menu("permission mode", |m| {
                let modes = fixtures::modes("workspace-write", true);
                mode_menu(m, &modes.entries, &modes.current, Rc::new(|_, _, _| {}))
            }),
            menu("folder", |m| {
                let c = fixtures::controls(0, false);
                folder_menu(m, &c.dirs, &c.cwd, true, Rc::new(|_, _, _| {}), Rc::new(|_, _| {}))
            }),
        ]),
    ]
}
