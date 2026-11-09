//! Composer: the input card (+ · text · send/stop) and the control row below it (project
//! folder, permission mode, model and effort picker, context ring; a click on the ring opens the context
//! and the thread's totals above it). A mode that isn't set up yet blocks
//! sending and says why, with a link to fix it.
//!
//! One visual line: a single row. More (a newline, or text wider than the single-row text
//! column): two zones, text across the full width over a row with + and send.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::component::input::{Textarea, TextareaState};
use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::service::CatalogEntry;
use space_core::protocol::trace::TrajectoryStats;
use space_core::protocol::{ConfigureSession, SpaceStatus};

use super::images::draft_thumb;
use super::loaders::ContextRing;
use super::model_picker::{PaneHandler, PickerPane, model_picker};
use super::trajectory::duration_label;
use super::transcript::attachment_chip;
use super::tip::TipExt as _;
use super::menu::{chevron, choice, heading, popover, row, separator, surface, trigger};
use super::widgets::icon_button;
use crate::actions::{RecallNext, RecallPrevious};
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
/// Key context around the whole composer (↑/↓ recall sent messages from its text field).
pub const COMPOSER_CONTEXT: &str = "Composer";

/// Which way ↑/↓ recall steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStep {
    /// ↑: an older sent message.
    Back,
    /// ↓: a newer one, then the draft.
    Forward,
}

/// Recall a sent message: `true` took the key (the composer's text was replaced); `false` lets
/// it move the caret.
pub type RecallHandler = Rc<dyn Fn(RecallStep, &mut Window, &mut App) -> bool>;

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
pub type PathsHandler = Rc<dyn Fn(Vec<PathBuf>, &mut Window, &mut App)>;
/// Gets a paste first: `true` took it (images, copied files), `false` lets the text go in.
pub type PasteHandler = Rc<dyn Fn(&ClipboardItem, &mut Window, &mut App) -> bool>;

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
    /// One of the blocker's links was clicked (its action string).
    pub on_blocker_action: StringHandler,
    pub on_remove_image: IndexHandler,
    /// Files dropped on the card.
    pub on_drop: PathsHandler,
    pub on_paste: PasteHandler,
    /// Open (`true`) or close the context panel.
    pub on_context: BoolHandler,
    /// Open one of the control row's menus, or close it.
    pub on_menu: MenuHandler,
    /// ↑ on the text's first line, ↓ on its last.
    pub on_recall: RecallHandler,
    /// ⌘⏎ or the white send button while the agent works: after the current step.
    pub on_send_after_step: Handler,
    /// A queued message's row: edit, remove, or send what's queued now.
    pub on_queued: QueuedHandler,
    /// A closed thread was picked to resume (its id).
    pub on_resume: StringHandler,
}

/// A message waiting for the agent, as its row above the composer shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedView {
    pub text: SharedString,
    /// Sent after the current step (else when the turn is done).
    pub after_step: bool,
    /// Images and files sent with it.
    pub extras: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedAction {
    Edit,
    Remove,
    SendNow,
}

pub type QueuedHandler = Rc<dyn Fn(usize, QueuedAction, &mut Window, &mut App)>;

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
            on_blocker_action: Rc::new(|_, _, _| {}),
            on_remove_image: Rc::new(|_, _, _| {}),
            on_drop: Rc::new(|_, _, _| {}),
            on_paste: Rc::new(|_, _, _| false),
            on_context: Rc::new(|_, _, _| {}),
            on_menu: Rc::new(|_, _, _| {}),
            on_recall: Rc::new(|_, _, _| false),
            on_send_after_step: Rc::new(|_, _| {}),
            on_queued: Rc::new(|_, _, _, _| {}),
            on_resume: Rc::new(|_, _, _| {}),
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
    /// The thread's totals (from the trajectory plugin), shown in the context panel.
    pub stats: Option<TrajectoryStats>,
    /// The context panel is open.
    pub context_open: bool,
    /// The control-row menu that's open.
    pub menu: Option<ComposerMenu>,
    /// Closed threads in this folder that can be resumed, last closed first. Offered on a new
    /// thread with nothing typed (empty: no Resume menu).
    pub resumable: Vec<ClosedThread>,
}

/// A closed thread in the Resume menu.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedThread {
    pub id: String,
    pub title: SharedString,
    /// When it was closed: "2 days ago".
    pub closed: SharedString,
}

/// The control row's menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerMenu {
    Folder,
    Mode,
    /// Closed threads to resume.
    Resume,
    /// The model picker, at this pane.
    Model(PickerPane),
}

impl ComposerMenu {
    /// Esc from here: the model picker steps back from a list; everything else closes.
    pub fn back(self) -> Option<ComposerMenu> {
        match self {
            ComposerMenu::Model(p) => p.back().map(ComposerMenu::Model),
            _ => None,
        }
    }
}

pub type MenuHandler = Rc<dyn Fn(Option<ComposerMenu>, &mut Window, &mut App)>;

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

/// Why the composer can't send, and the links that fix it.
#[derive(Clone, Default)]
pub struct ComposerBlocker {
    pub text: SharedString,
    /// `(label, action)`; the first is the main one. The action string goes to
    /// `on_blocker_action`.
    pub actions: Vec<(SharedString, SharedString)>,
    /// "Last tried 2m 14s ago · next try in 2m 46s", when the cause is being retried.
    pub times: Option<SharedString>,
}

#[derive(IntoElement)]
pub struct Composer {
    id: ElementId,
    input: Entity<TextareaState>,
    running: bool,
    /// A message is waiting for the session; sending is blocked.
    queued: bool,
    attachments: Vec<PathBuf>,
    /// Draft images' thumbnails (`None`: still being prepared; sending waits for it).
    images: Vec<Option<ImageSource>>,
    controls: Option<ComposerControls>,
    handlers: ComposerHandlers,
    /// Messages waiting for the agent.
    queued_messages: Vec<QueuedView>,
    /// ⌘ is held and the agent takes messages mid-turn: sending while it works goes in after
    /// the current step (the button turns white).
    after_step: bool,
}

impl Composer {
    pub fn new(id: impl Into<ElementId>, input: &Entity<TextareaState>) -> Self {
        Self {
            id: id.into(),
            input: input.clone(),
            running: false,
            queued: false,
            attachments: Vec::new(),
            images: Vec::new(),
            controls: None,
            handlers: ComposerHandlers::default(),
            queued_messages: Vec::new(),
            after_step: false,
        }
    }

    pub fn queued_messages(mut self, queued: Vec<QueuedView>) -> Self {
        self.queued_messages = queued;
        self
    }

    pub fn after_step(mut self, after_step: bool) -> Self {
        self.after_step = after_step;
        self
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

    pub fn images(mut self, images: Vec<Option<ImageSource>>) -> Self {
        self.images = images;
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
        let Self { id, input, running, queued, attachments, images, controls, handlers: h, queued_messages, after_step } = self;
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let has_text = !input.read(cx).value().trim().is_empty();
        let blocker = controls.as_ref().and_then(|c| c.blocker.clone());
        let preparing = images.iter().any(Option::is_none);
        let has_content = has_text || !attachments.is_empty() || !images.is_empty();
        let can_send = (has_text || !attachments.is_empty() || !images.is_empty())
            && !running
            && !queued
            && !preparing
            && blocker.is_none();

        let expanded_state = window.use_keyed_state((id.clone(), "expanded"), cx, |_, _| false);
        let expanded = is_expanded(&input, *expanded_state.read(cx), window, cx);
        expanded_state.update(cx, |e, _| *e = expanded);

        let thumbs = (!images.is_empty()).then(|| {
            let remove = h.on_remove_image.clone();
            div()
                .flex()
                .flex_wrap()
                .gap_1p5()
                .px_1()
                .children(images.into_iter().enumerate().map(|(ix, src)| draft_thumb(ix, src, remove.clone())))
        });
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

        let queueable = running && has_content && !preparing && blocker.is_none();
        let send: AnyElement = if queueable {
            // Working, and something typed: stop moves aside; the arrow queues it (grey: when the
            // turn is done; white with ⌘: after the current step).
            let (stop, send, steer) = (h.on_stop.clone(), h.on_send.clone(), h.on_send_after_step.clone());
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .id("stop")
                        .size(px(BUTTON))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(color::border_strong())
                        .cursor_pointer()
                        .hover(|s| s.bg(color::hover()))
                        .child(div().size(px(10.)).rounded(px(2.)).bg(color::text_2()))
                        .tip("Stop  ⎋")
                        .on_click(move |_, w, cx| stop(w, cx)),
                )
                .child(
                    div()
                        .id("send-queued")
                        .size(px(BUTTON))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .map(|d| {
                            if after_step {
                                d.bg(color::text()).text_color(color::bg())
                            } else {
                                d.bg(color::border_strong()).text_color(color::text())
                            }
                        })
                        .hover(|s| s.opacity(0.85))
                        .child(Icon::new(IconName::ArrowUp).size(size::ICON_MD))
                        .tip(if after_step { "Send after this step  ⌘⏎" } else { "Send when done  ⏎" })
                        .on_click(move |_, w, cx| if after_step { steer(w, cx) } else { send(w, cx) }),
                )
                .into_any_element()
        } else if running {
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
                .tip("Stop")
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
                .when(can_send, |d| d.tip("Send  ⏎").on_click(move |_, w, cx| send(w, cx)))
                .when(blocker.is_some(), |d| d.tip("Can't send right now"))
                .when(preparing && blocker.is_none(), |d| d.tip("Preparing images…"))
                .into_any_element()
        };

        let (attach_fn, focus_fn) = (h.on_attach.clone(), h.on_focus.clone());
        let attach = icon_button("attach", IconName::Plus, true, false)
            .size(px(BUTTON))
            .rounded_full()
            .tip("Attach images, files or folders")
            .on_click(move |_, w, cx| {
                focus_fn(w, cx);
                attach_fn(w, cx);
            });
        // XSmall: no vertical editor padding, 4px horizontal.
        let paste = h.on_paste.clone();
        let textarea = Textarea::new(&input)
            .appearance(false)
            .xsmall()
            .text_size(text::BODY)
            .on_paste(move |item, w, cx| paste(item, w, cx));

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
            // Files dragged over it: images become draft images, other files are attached.
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(color::accent()))
            .on_drop({
                let drop = h.on_drop.clone();
                move |paths: &ExternalPaths, w, cx| drop(paths.paths().to_vec(), w, cx)
            })
            .children(thumbs)
            .children(chips)
            .child(body);

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            // More than one line: ⌘↓ moves the caret to the end instead of scrolling the
            // transcript (see the key bindings in space-app).
            .key_context(if expanded { "Composer ComposerMultiline" } else { COMPOSER_CONTEXT })
            // ↑ with the caret on the first line, ↓ on the last: step through the sent messages;
            // elsewhere (or with nothing to recall) the key moves the caret.
            .on_action({
                let (input, recall) = (input.clone(), h.on_recall.clone());
                move |_: &RecallPrevious, w, cx| {
                    let on_first_line = input.read(cx).cursor_position().line == 0;
                    if !(on_first_line && recall(RecallStep::Back, w, cx)) {
                        cx.propagate();
                    }
                }
            })
            .on_action({
                let (input, recall) = (input.clone(), h.on_recall.clone());
                move |_: &RecallNext, w, cx| {
                    let state = input.read(cx);
                    let last_line = state.value().matches('\n').count() as u32;
                    if !(state.cursor_position().line == last_line && recall(RecallStep::Forward, w, cx)) {
                        cx.propagate();
                    }
                }
            })
            .children((!queued_messages.is_empty()).then(|| queued_rows(&queued_messages, running, h.on_queued.clone())))
            .children(blocker.map(|b| blocker_row(b, h.on_blocker_action.clone())))
            .child(card)
            .children(controls.map(|c| control_row(c, &h)))
    }
}

/// Messages waiting for the agent, above the card: when each goes (a small arrow, white for
/// after this step, grey for when done), what it says, and edit / remove. If the turn isn't
/// running (it failed), they can be sent now.
pub fn queued_rows(queued: &[QueuedView], running: bool, on_queued: QueuedHandler) -> Div {
    let row = |ix: usize, q: &QueuedView| {
        let (edit, remove) = (on_queued.clone(), on_queued.clone());
        let extras = match q.extras {
            0 => String::new(),
            1 => " · 1 attachment".into(),
            n => format!(" · {n} attachments"),
        };
        div()
            .id(("queued", ix))
            .h(px(32.))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_2()
            .rounded(radius::XL)
            .bg(color::surface())
            .border_1()
            .border_color(color::border())
            .text_size(text::SM)
            .child(
                div()
                    .size(px(18.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| if q.after_step { d.bg(color::text()).text_color(color::bg()) } else { d.bg(color::border_strong()).text_color(color::text()) })
                    .child(Icon::new(IconName::ArrowUp).size(size::ICON_XS)),
            )
            .child(div().flex_none().text_color(color::text_3()).child(if q.after_step { "After this step" } else { "When done" }))
            .child(div().flex_1().min_w_0().truncate().text_color(color::text_2()).child(format!("{}{extras}", format::one_line(&q.text))))
            .child(icon_button(("queued-edit", ix), Lucide::Pencil, true, false).tip("Edit").on_click(move |_, w, cx| edit(ix, QueuedAction::Edit, w, cx)))
            .child(icon_button(("queued-remove", ix), Lucide::X, true, false).tip("Remove").on_click(move |_, w, cx| remove(ix, QueuedAction::Remove, w, cx)))
    };
    let send_now = on_queued.clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(queued.iter().enumerate().map(|(ix, q)| row(ix, q)))
        .when(!running, |d| {
            d.child(
                div().flex().justify_end().child(
                    super::widgets::pill_button("queued-send-now", if queued.len() == 1 { "Send it now" } else { "Send them now" }, false)
                        .on_click(move |_, w, cx| send_now(0, QueuedAction::SendNow, w, cx)),
                ),
            )
        })
}

/// Why sending is blocked, above the card: a warning box shaped like the transcript's error card,
/// with its links on the right and, while retrying, the retry times under the text.
pub fn blocker_row(blocker: ComposerBlocker, on_action: StringHandler) -> Div {
    let links = blocker.actions.into_iter().enumerate().map(|(i, (label, action))| {
        let f = on_action.clone();
        let main = i == 0;
        div()
            .id(("blocker-action", i))
            .flex()
            .items_center()
            .gap_0p5()
            .when(main, |d| d.font_weight(gpui::FontWeight::MEDIUM).text_color(color::yellow()))
            .when(!main, |d| d.text_color(color::text_2()))
            .cursor_pointer()
            .hover(|s| s.text_color(color::text()))
            .child(label)
            .when(main, |d| d.child(Icon::new(IconName::ChevronRight).size(size::ICON_SM)))
            .on_click(move |_, w, cx| f(action.to_string(), w, cx))
    });
    let body = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(blocker.text)
        .children(blocker.times.map(|t| div().text_color(color::text_2()).child(t)));
    // The error card's shape (transcript::error_card) in the warning tint, as wide as the thread.
    // Extra space under it on top of the stack's gap, so it stands apart from the composer.
    div()
        .w_full()
        .flex()
        .items_start()
        .gap_2p5()
        .p_3()
        .mb(px(space::MD))
        .rounded(radius::XL)
        .bg(color::yellow_bg())
        .border_1()
        .border_color(color::yellow_border())
        .text_size(text::SM)
        .line_height(relative(text::LEADING))
        .text_color(color::text())
        .child(Icon::new(Lucide::ShieldAlert).size(size::ICON_MD).text_color(color::yellow()).mt(px(1.)))
        .child(body)
        .child(div().flex().flex_none().items_center().gap_3().children(links))
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

fn control_row(c: ComposerControls, h: &ComposerHandlers) -> Div {
    let status = c.status.clone();
    let menu_open = c.menu;
    let on_menu = h.on_menu.clone();
    let toggle_menu = move |m: ComposerMenu| {
        let on_menu = on_menu.clone();
        move |_: &ClickEvent, w: &mut Window, cx: &mut App| on_menu((menu_open != Some(m)).then_some(m), w, cx)
    };
    let close_menu: Handler = {
        let on_menu = h.on_menu.clone();
        Rc::new(move |w, cx| on_menu(None, w, cx))
    };
    let folder = {
        let is_open = menu_open == Some(ComposerMenu::Folder);
        trigger("project-menu", is_open, true)
            .on_click(toggle_menu(ComposerMenu::Folder))
            .child(Icon::new(IconName::Folder).size(size::ICON))
            .child(div().max_w(px(220.)).truncate().child(format::dir_name(&c.cwd)))
            .child(chevron())
            .when(is_open, |d| {
                let menu = folder_menu(&c.dirs, &c.cwd, c.thread_empty, h.on_pick_dir.clone(), h.on_choose_folder.clone(), close_menu.clone());
                let close = close_menu.clone();
                d.child(popover(false, menu, move |w, cx| close(w, cx)))
            })
    };

    let mut right = div().flex().items_center().gap_0p5();
    let picker_locked = c.locked || status.available_models.as_ref().is_none_or(|m| m.is_empty());
    let picker_open = match menu_open {
        Some(ComposerMenu::Model(pane)) => Some(pane),
        _ => None,
    };
    let on_pane: PaneHandler = {
        let on_menu = h.on_menu.clone();
        Rc::new(move |pane, w, cx| on_menu(pane.map(ComposerMenu::Model), w, cx))
    };
    if let Some(picker) = model_picker(&status, picker_locked, picker_open, on_pane, h.on_configure.clone()) {
        right = right.child(picker);
    }
    let tokens = status.context_tokens.unwrap_or(0);
    let window_size = status.context_window.filter(|w| *w > 0);
    let open = c.context_open;
    let (toggle, close) = (h.on_context.clone(), h.on_context.clone());
    let stats = c.stats.clone();
    right = right.child(
        div()
            .id("context-ring")
            .relative()
            .ml_1()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::MD)
            .cursor_pointer()
            .when(open, |d| d.bg(color::hover()))
            .hover(|s| s.bg(color::hover()))
            .child(ContextRing::new(window_size.map_or(0., |w| tokens as f32 / w as f32)))
            .when(!open, |d| d.tip(format::context_usage(tokens, window_size)))
            .on_click(move |_, w, cx| toggle(!open, w, cx))
            .when(open, |d| {
                // Above the ring, kept inside the window.
                d.child(div().absolute().top_0().right_0().child(deferred(
                    anchored().anchor(Anchor::BottomRight).snap_to_window_with_margin(px(8.)).child(
                        div()
                            .pb_1()
                            .child(context_panel(tokens, window_size, stats.as_ref()).on_mouse_down_out(move |_, w, cx| close(false, w, cx))),
                    ),
                )))
            }),
    );

    let mode = c.mode.filter(|m| !m.entries.is_empty()).map(|m| {
        let current = m.current_entry().cloned();
        let icon = mode_icon(current.as_ref().and_then(|e| e.icon.as_deref()));
        let alert = icon == Lucide::ShieldAlert;
        let name = current.map_or_else(|| SharedString::from("Permissions"), |e| e.name.into());
        let is_open = menu_open == Some(ComposerMenu::Mode);
        trigger("mode-menu", is_open, true)
            .on_click(toggle_menu(ComposerMenu::Mode))
            .child(Icon::new(icon).size(size::ICON).text_color(if alert { color::yellow() } else { color::text_2() }))
            .child(name)
            .child(chevron())
            .when(is_open, |d| {
                let menu = mode_menu(&m.entries, &m.current, h.on_mode.clone(), close_menu.clone());
                let close = close_menu.clone();
                d.child(popover(false, menu, move |w, cx| close(w, cx)))
            })
    });

    let resume = (!c.resumable.is_empty()).then(|| {
        let is_open = menu_open == Some(ComposerMenu::Resume);
        trigger("resume-menu", is_open, true)
            .on_click(toggle_menu(ComposerMenu::Resume))
            .child(Icon::new(Lucide::Play).size(size::ICON))
            .child("Resume")
            .child(chevron())
            .when(!is_open, |d| d.tip("Resume a closed thread"))
            .when(is_open, |d| {
                let menu = resume_menu(&c.resumable, h.on_resume.clone(), close_menu.clone());
                let close = close_menu.clone();
                d.child(popover(false, menu, move |w, cx| close(w, cx)))
            })
    });

    div()
        .flex()
        .items_center()
        .justify_between()
        .pl(px(ROW_INSET_LEFT))
        .pr(px(ROW_INSET_RIGHT))
        .child(div().flex().items_center().gap_1().min_w_0().child(folder).children(mode).children(resume))
        .child(right)
}

/// Closed threads of this folder, last closed first: the title and when it was closed.
pub fn resume_menu(threads: &[ClosedThread], on_resume: StringHandler, close: Handler) -> Div {
    surface().min_w(px(300.)).child(heading("Closed threads")).child(
        div().id("resume-list").flex().flex_col().max_h(px(320.)).overflow_y_scroll().children(threads.iter().enumerate().map(|(ix, t)| {
            let (id, f, close) = (t.id.clone(), on_resume.clone(), close.clone());
            row(("resume", ix))
                .child(div().flex_1().min_w_0().truncate().child(t.title.clone()))
                .child(div().flex_none().text_size(text::SM).text_color(color::text_3()).child(t.closed.clone()))
                .on_click(move |_, w, cx| {
                    close(w, cx);
                    f(id.clone(), w, cx)
                })
        })),
    )
}

/// The context window's use and, with a trajectory plugin, the thread's totals.
pub fn context_panel(tokens: u64, window: Option<u64>, stats: Option<&TrajectoryStats>) -> Div {
    let tok = |n: u64| format!("{} tok", format::thousands(n));
    let row = |label: &str, value: String| {
        div()
            .flex()
            .justify_between()
            .gap_6()
            .child(div().text_color(color::text_3()).child(label.to_string()))
            .child(div().text_color(color::text()).child(value))
    };
    let header = |icon: Lucide, title: &str, value: String| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_6()
            .font_weight(FontWeight::MEDIUM)
            .child(div().flex().items_center().gap_2().child(Icon::new(icon).size(size::ICON)).child(title.to_string()))
            .child(value)
    };
    let frac = window.map_or(0., |w| (tokens as f32 / w as f32).clamp(0., 1.));
    let context = match window {
        Some(w) => {
            let pct = tokens as f64 / w as f64 * 100.;
            let pct = if tokens > 0 && pct < 1. { "<1%".to_string() } else { format!("{pct:.0}%") };
            format!("{} / {} · {pct}", format::compact_tokens(tokens), format::compact_tokens(w))
        }
        None => "Not reported yet".to_string(),
    };
    let mut panel = div()
        .w(px(300.))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .shadow_lg()
        .text_size(text::SM)
        .cursor_default()
        .child(header(Lucide::Gauge, "Context", context))
        .child(
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(color::border_strong())
                .child(div().h_full().w(relative(if frac > 0. { frac.max(0.02) } else { 0. })).rounded_full().bg(color::accent())),
        );
    if let Some(stats) = stats.filter(|s| s.steps > 0) {
        let u = &stats.usage;
        let mut rows = vec![row("Turns", stats.turns.to_string()), row("Steps", stats.steps.to_string())];
        if let Some(tps) = stats.tokens_per_second() {
            rows.push(row("Speed", format!("{tps:.0} tok/s")));
        }
        if stats.generation_ms > 0 {
            rows.push(row("Generating", format!("{} for {}", duration_label(stats.generation_ms), tok(stats.generated_tokens))));
        }
        if let Some(hit) = u.cache_hit_percent() {
            rows.push(row("Cache hit", format!("{hit}%")));
        }
        rows.push(row("Uncached input", tok(u.input)));
        if let Some(r) = u.cache_read {
            rows.push(row("Cached input", tok(r)));
        }
        if let Some(w) = u.cache_write {
            rows.push(row("Cache write", tok(w)));
        }
        rows.push(row("Output", tok(u.output)));
        panel = panel
            .child(
                header(Lucide::Database, "Thread usage", tok(u.total()))
                    .mt_1()
                    .pt_3()
                    .border_t_1()
                    .border_color(color::border()),
            )
            .children(rows);
    }
    panel
}

/// Permission mode picker contents: one item per catalog entry, the current one checked.
pub fn mode_menu(entries: &[CatalogEntry], current: &str, on_mode: StringHandler, close: Handler) -> Div {
    surface().child(heading("Permissions")).children(entries.iter().map(|e| {
        let (id, f, close) = (e.id.clone(), on_mode.clone(), close.clone());
        let icon = Icon::new(mode_icon(e.icon.as_deref())).size(size::ICON_MD).into_any_element();
        choice(ElementId::Name(format!("mode-{}", e.id).into()), Some(icon), e.name.clone(), e.id == current).on_click(
            move |_, w, cx| {
                close(w, cx);
                f(id.clone(), w, cx)
            },
        )
    }))
}

/// Folder picker: recent folders and "Choose folder…". For a non-empty thread, picking a
/// folder starts a new thread there.
pub fn folder_menu(
    dirs: &[PathBuf],
    cwd: &PathBuf,
    thread_empty: bool,
    on_pick: PathHandler,
    on_choose: Handler,
    close: Handler,
) -> Div {
    let folder = |i: IconName| Some(Icon::new(i).size(size::ICON).into_any_element());
    let choose_close = close.clone();
    surface()
        .min_w(px(260.))
        .child(heading(if thread_empty { "Project folder" } else { "New thread in…" }))
        .children(dirs.iter().enumerate().map(|(ix, dir)| {
            let (d, f, close) = (dir.clone(), on_pick.clone(), close.clone());
            choice(("folder", ix), folder(IconName::Folder), format::abbreviate_home(dir), thread_empty && dir == cwd).on_click(
                move |_, w, cx| {
                    close(w, cx);
                    f(d.clone(), w, cx)
                },
            )
        }))
        .child(separator())
        .child(choice("choose-folder", folder(IconName::FolderOpen), "Choose folder…", false).on_click(move |_, w, cx| {
            choose_close(w, cx);
            on_choose(w, cx)
        }))
}

#[cfg(feature = "dev")]
pub mod fixtures {
    use super::*;
    use space_core::protocol::ModelInfo;

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
            stats: None,
            context_open: false,
            menu: None,
            resumable: Vec::new(),
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
            c.blocker = Some(not_set_up());
        }
        c
    }

    fn action(label: &str, action: &str) -> (SharedString, SharedString) {
        (SharedString::from(label.to_string()), SharedString::from(action.to_string()))
    }

    const TIMES: &str = "Last tried 2m 14s ago · next try in 2m 46s";

    /// The blockers the permissions plugin shows, by state (as the app formats them).
    pub fn not_set_up() -> ComposerBlocker {
        ComposerBlocker {
            text: "Workspace Write needs an OpenAI API key.".into(),
            actions: vec![action("Open Settings", "open-settings:permissions")],
            times: None,
        }
    }

    pub fn failing(text: &str, fix: Option<(&str, &str)>) -> ComposerBlocker {
        let mut actions: Vec<_> = fix.into_iter().map(|(l, a)| action(l, a)).collect();
        actions.push(action("Retry now", "retry:permissions"));
        ComposerBlocker { text: text.to_string().into(), actions, times: Some(TIMES.into()) }
    }

    pub fn key_rejected() -> ComposerBlocker {
        failing("OpenAI rejected your API key, so actions are paused.", Some(("Update key", "open-settings:permissions")))
    }

    pub fn unreachable() -> ComposerBlocker {
        failing("Can't reach OpenAI (no answer within 3s), so actions are paused.", None)
    }

    pub fn no_access() -> ComposerBlocker {
        failing(
            "This OpenAI account doesn't have the Decisions API (Decision API is not enabled for this user.), so actions \
             are paused.",
            Some(("Open Settings", "open-settings:permissions")),
        )
    }

    pub fn no_credit() -> ComposerBlocker {
        failing("OpenAI says the account is out of credit, so actions are paused.", Some(("Open billing", "open-url:https://platform.openai.com")))
    }

    pub fn incompatible() -> ComposerBlocker {
        failing("OpenAI's decision API changed (Unknown parameter); Space needs an update. Actions are paused.", None)
    }

    /// A plugin from your home or project folder waits for your approval.
    pub fn needs_approval() -> ComposerBlocker {
        ComposerBlocker {
            text: "Alerts needs your approval to run.".into(),
            actions: vec![action("Review in Settings", "open-settings:alerts")],
            times: None,
        }
    }

    pub fn with_blocker(mut c: ComposerControls, blocker: ComposerBlocker) -> ComposerControls {
        c.mode = Some(modes("workspace-write", false));
        c.blocker = Some(blocker);
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

    fn menu(name: &'static str, build: fn() -> Div) -> Example {
        Example::new(name, move |_, _| div().flex().child(build()).into_any_element())
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
            example("key rejected (paused, retrying)", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::key_rejected()))
            }),
            example("can't reach OpenAI", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::unreachable()))
            }),
            example("no Decisions access", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::no_access()))
            }),
            example("out of credit", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::no_credit()))
            }),
            example("API changed", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::incompatible()))
            }),
            example("a plugin needs approval", "", |c| {
                c.controls(fixtures::with_blocker(fixtures::controls(18_200, false), fixtures::needs_approval()))
            }),
            example("no permission modes (no plugin)", "", |c| {
                c.controls(ComposerControls { mode: None, ..fixtures::controls(18_200, false) })
            }),
        ]),
        ComponentPreview::new(Area::Composer, "Menus", "Dropdown contents, rendered open.").examples([
            menu("permission mode", || {
                let modes = fixtures::modes("workspace-write", true);
                mode_menu(&modes.entries, &modes.current, Rc::new(|_, _, _| {}), Rc::new(|_, _| {}))
            }),
            menu("folder", || {
                let c = fixtures::controls(0, false);
                folder_menu(&c.dirs, &c.cwd, true, Rc::new(|_, _, _| {}), Rc::new(|_, _| {}), Rc::new(|_, _| {}))
            }),
            menu("resume", || {
                let t = |id: &str, title: &str, closed: &str| ClosedThread { id: id.into(), title: title.to_string().into(), closed: closed.to_string().into() };
                let threads = [
                    t("a", "Fix the failing smoke test in space-core", "2 hours ago"),
                    t("b", "Terminal plugin with alacritty", "yesterday"),
                    t("c", "Greeting exchange", "3 weeks ago"),
                ];
                resume_menu(&threads, Rc::new(|_, _, _| {}), Rc::new(|_, _| {}))
            }),
        ]),
    ]
}
