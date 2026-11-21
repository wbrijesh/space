//! The right pane's Changes mode: the bar (the branch, where pull requests go, the pull request,
//! one status line and the next step with the others under ▾), its menus, the pull request's
//! editor, and the changed files, each opening to its diff.
//!
//! What the bar offers is decided by the app (`repo_state`); this only draws it.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::input::{Input, InputState, Textarea, TextareaState};
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::composer::BranchChoice;
use super::file_tree::{GitMark, file_icon};
use super::loaders::Spinner;
use super::menu::{choice, dropdown, heading, row, separator, surface};
use super::tip::TipExt as _;
use super::widgets::{icon_button, pill_button};
use crate::theme::{color, radius, size, text};

/// A step the bar can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeAction {
    Commit,
    CommitAndPush,
    CommitAndPr,
    /// Commit on a new branch Claude names, leaving this one as it was.
    CommitToNewBranch,
    Push,
    Pull,
    CreatePr,
    /// Switch to this branch and pull it (after the pull request was merged).
    SwitchTo(SharedString),
    /// Name a branch to be on (not on one now).
    NewBranch,
    EditPr,
}

impl ChangeAction {
    pub fn label(&self) -> SharedString {
        match self {
            ChangeAction::Commit => "Commit".into(),
            ChangeAction::CommitAndPush => "Commit & push".into(),
            ChangeAction::CommitAndPr => "Commit & create PR".into(),
            ChangeAction::CommitToNewBranch => "Commit to new branch".into(),
            ChangeAction::Push => "Push".into(),
            ChangeAction::Pull => "Pull".into(),
            ChangeAction::CreatePr => "Create PR".into(),
            ChangeAction::SwitchTo(b) => format!("Switch to {b}").into(),
            ChangeAction::NewBranch => "New branch…".into(),
            ChangeAction::EditPr => "Edit pull request".into(),
        }
    }

    fn icon(&self) -> Lucide {
        match self {
            ChangeAction::Commit | ChangeAction::CommitToNewBranch => Lucide::GitCommitHorizontal,
            ChangeAction::CommitAndPush | ChangeAction::Push => Lucide::ArrowUp,
            ChangeAction::CommitAndPr | ChangeAction::CreatePr => Lucide::GitPullRequestArrow,
            ChangeAction::Pull => Lucide::ArrowDown,
            ChangeAction::SwitchTo(_) | ChangeAction::NewBranch => Lucide::GitBranch,
            ChangeAction::EditPr => Lucide::Pencil,
        }
    }
}

/// How the status line reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Quiet,
    Warn,
    Good,
}

/// The bar's menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesMenu {
    Branch,
    Base,
    Actions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrChip {
    pub number: u64,
    pub title: SharedString,
    /// "open", "draft", "merged", "closed".
    pub state: SharedString,
}

/// A note under the bar: what just happened, with Undo while it can be taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub text: SharedString,
    pub undo: bool,
}

#[derive(Clone, Default)]
pub struct ChangesBar {
    /// The branch, or the short commit when not on one.
    pub branch: SharedString,
    pub detached: bool,
    /// Where pull requests go (the chip); `None`: not shown.
    pub base: Option<SharedString>,
    pub status: SharedString,
    pub tone: Tone,
    pub primary: Option<ChangeAction>,
    pub menu: Vec<ChangeAction>,
    /// A step is running (the status says which).
    pub running: bool,
    pub pr: Option<PrChip>,
    /// A quiet line under the bar (why pull requests aren't offered).
    pub note: Option<SharedString>,
    pub error: Option<SharedString>,
    pub done: Option<Done>,
    pub open: Option<ChangesMenu>,
    /// The branch menu's contents, while open.
    pub branches: Option<BranchChoice>,
    /// Where a new branch can start besides what's checked out (the default branch).
    pub branch_from: Option<SharedString>,
    /// The base menu: the branches, the chosen one, and whether choosing it goes on to open the
    /// pull request.
    pub bases: Vec<SharedString>,
    pub base_then_create: bool,
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type NameHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
pub type ActionHandler = Rc<dyn Fn(ChangeAction, &mut Window, &mut App)>;
pub type MenuHandler = Rc<dyn Fn(Option<ChangesMenu>, &mut Window, &mut App)>;
/// A new branch: its name, and where it starts (`None`: what's checked out).
pub type NewBranchHandler = Rc<dyn Fn(SharedString, Option<SharedString>, &mut Window, &mut App)>;
pub type OpenHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
pub type RowHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(Clone)]
pub struct ChangesHandlers {
    pub on_action: ActionHandler,
    pub on_menu: MenuHandler,
    pub on_open_pr: Handler,
    pub on_undo: Handler,
    pub on_dismiss: Handler,
    pub on_base: NameHandler,
    pub on_branch: NameHandler,
    pub on_new_branch: NewBranchHandler,
}

impl Default for ChangesHandlers {
    fn default() -> Self {
        Self {
            on_action: Rc::new(|_, _, _| {}),
            on_menu: Rc::new(|_, _, _| {}),
            on_open_pr: Rc::new(|_, _| {}),
            on_undo: Rc::new(|_, _| {}),
            on_dismiss: Rc::new(|_, _| {}),
            on_base: Rc::new(|_, _, _| {}),
            on_branch: Rc::new(|_, _, _| {}),
            on_new_branch: Rc::new(|_, _, _, _| {}),
        }
    }
}

/// The bar: the branch, the base and the pull request; then the status and the steps; then a
/// note, an error or what just happened.
pub fn changes_bar(b: &ChangesBar, h: &ChangesHandlers) -> Div {
    let menu_open = |m: ChangesMenu| b.open == Some(m);
    let toggle = |m: ChangesMenu| {
        let (on_menu, open) = (h.on_menu.clone(), menu_open(m));
        move |_: &ClickEvent, w: &mut Window, cx: &mut App| on_menu(if open { None } else { Some(m) }, w, cx)
    };
    let close = {
        let on_menu = h.on_menu.clone();
        move |w: &mut Window, cx: &mut App| on_menu(None, w, cx)
    };

    let branch = chip("changes-branch", menu_open(ChangesMenu::Branch), !b.running)
        .min_w(px(96.))
        .flex_shrink(0.5)
        .child(Icon::new(Lucide::GitBranch).size(size::ICON_SM).text_color(color::text_3()))
        .child(div().min_w_0().truncate().child(if b.detached { format!("{} (detached)", b.branch).into() } else { b.branch.clone() }))
        .child(Icon::new(IconName::ChevronDown).size(size::ICON_XS).text_color(color::text_3()))
        .tip("Switch or create a branch")
        .when(!b.running, |d| d.on_click(toggle(ChangesMenu::Branch)))
        .when(menu_open(ChangesMenu::Branch), |d| d.child(dropdown(false, branch_menu(b, h), close.clone())));

    let base = b.base.clone().map(|base| {
        let editable = b.pr.is_none() && !b.running;
        chip("changes-base", menu_open(ChangesMenu::Base), editable)
            .child(Icon::new(IconName::ArrowRight).size(size::ICON_XS).text_color(color::text_3()))
            .child(div().min_w_0().truncate().child(base))
            .when(editable, |d| d.child(Icon::new(IconName::ChevronDown).size(size::ICON_XS).text_color(color::text_3())))
            .tip(if editable { "Where pull requests from this branch go" } else { "Where this pull request goes" })
            .when(editable, |d| d.on_click(toggle(ChangesMenu::Base)))
            .when(menu_open(ChangesMenu::Base), |d| d.child(dropdown(false, base_menu(b, h), close.clone())))
    });

    let pr = b.pr.clone().map(|pr| {
        let open = h.on_open_pr.clone();
        chip("changes-pr", false, true)
            .flex_none()
            .child(Icon::new(Lucide::GitPullRequest).size(size::ICON_SM).text_color(pr_color(&pr.state)))
            .child(div().flex_none().child(format!("#{}", pr.number)))
            .child(div().min_w_0().truncate().text_color(color::text_3()).child(pr.state.clone()))
            .child(Icon::new(Lucide::ArrowUpRight).size(size::ICON_XS).text_color(color::text_3()))
            .tip(pr.title.clone())
            .on_click(move |_, w, cx| open(w, cx))
    });

    let status_color = match b.tone {
        Tone::Quiet => color::text_2(),
        Tone::Warn => color::yellow(),
        Tone::Good => color::text_3(),
    };
    let status = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .when(b.running, |d| d.child(Spinner::new(size::SPINNER_SM)))
        .child(div().min_w_0().truncate().text_size(text::SM).text_color(status_color).child(b.status.clone()));

    let steps = (!b.running).then(|| {
        let more = (!b.menu.is_empty()).then(|| {
            let segment = match &b.primary {
                // Joined to the main button.
                Some(_) => div()
                    .id("changes-more")
                    .h(size::ICON_BUTTON)
                    .px_1p5()
                    .flex()
                    .items_center()
                    .rounded_r(radius::LG)
                    .bg(color::accent())
                    .border_l_1()
                    .border_color(color::on_accent().opacity(0.25))
                    .text_color(color::on_accent())
                    .cursor_pointer()
                    .hover(|s| s.bg(color::accent_hover()))
                    .child(Icon::new(IconName::ChevronDown).size(size::ICON_SM)),
                None => icon_button("changes-more", IconName::Ellipsis, true, menu_open(ChangesMenu::Actions)),
            };
            div()
                .relative()
                .child(segment.tip("More").on_click(toggle(ChangesMenu::Actions)))
                .when(menu_open(ChangesMenu::Actions), |d| d.child(dropdown(true, actions_menu(&b.menu, h), close.clone())))
        });
        let primary = b.primary.clone().map(|a| {
            let act = h.on_action.clone();
            let joined = more.is_some();
            pill_button("changes-primary", a.label(), true)
                .flex()
                .gap_1p5()
                .when(joined, |d| d.rounded_r(px(0.)))
                .on_click(move |_, w, cx| act(a.clone(), w, cx))
        });
        div().flex_none().flex().items_center().children(primary).children(more)
    });

    let note = |id: &'static str, t: SharedString, c: Hsla, dismiss: bool, extra: Option<AnyElement>| {
        let on_dismiss = h.on_dismiss.clone();
        div()
            .id(id)
            .px_3()
            .py_1p5()
            .flex()
            .items_start()
            .gap_2()
            .border_b_1()
            .border_color(color::border())
            .text_size(text::SM)
            .line_height(relative(text::LEADING))
            .child(div().flex_1().min_w_0().text_color(c).child(t))
            .children(extra)
            .when(dismiss, |d| {
                d.child(icon_button(SharedString::from(format!("{id}-x")), IconName::Close, true, false).size(px(20.)).on_click(move |_, w, cx| on_dismiss(w, cx)))
            })
    };
    let done = b.done.clone().map(|done| {
        let undo = h.on_undo.clone();
        let undo = done.undo.then(|| {
            div()
                .id("changes-undo")
                .flex_none()
                .text_color(color::accent())
                .cursor_pointer()
                .hover(|s| s.underline())
                .child("Undo")
                .on_click(move |_, w, cx| undo(w, cx))
                .into_any_element()
        });
        note("changes-done", done.text, color::text_2(), true, undo)
    });

    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .h(size::PANE_BAR_H)
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_1().min_w_0().flex().items_center().gap_1().child(branch).children(base))
                .children(pr),
        )
        .child(div().min_h(px(40.)).px_3().pb_2().flex().items_center().gap_2().border_b_1().border_color(color::border()).child(status).children(steps))
        .children(b.error.clone().map(|e| note("changes-error", e, color::red(), true, None)))
        .children(done)
        .children(b.note.clone().map(|n| note("changes-note", n, color::text_3(), false, None)))
}

fn pr_color(state: &str) -> Hsla {
    match state {
        "merged" => color::violet(),
        "closed" => color::red(),
        "draft" => color::text_3(),
        _ => color::green(),
    }
}

/// A quiet chip in the bar's first row.
fn chip(id: &'static str, open: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .min_w_0()
        .h(px(24.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(radius::MD)
        .text_size(text::SM)
        .text_color(color::text_2())
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(color::hover())))
        .when(open, |d| d.bg(color::hover()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

fn actions_menu(actions: &[ChangeAction], h: &ChangesHandlers) -> Div {
    surface().min_w(px(220.)).children(actions.iter().enumerate().map(|(ix, a)| {
        let (act, a2, on_menu) = (h.on_action.clone(), a.clone(), h.on_menu.clone());
        row(("changes-step", ix))
            .child(Icon::new(a.icon()).size(size::ICON).text_color(color::text_2()))
            .child(div().flex_1().child(a.label()))
            .on_click(move |_, w, cx| {
                on_menu(None, w, cx);
                act(a2.clone(), w, cx)
            })
    }))
}

fn base_menu(b: &ChangesBar, h: &ChangesHandlers) -> Div {
    let current = b.base.clone();
    surface()
        .min_w(px(240.))
        .child(heading(if b.base_then_create { "Create a pull request into" } else { "Pull requests go into" }))
        .when(b.bases.is_empty(), |d| d.child(div().px_2p5().py_1().text_size(text::SM).text_color(color::text_3()).child("No other branches")))
        .children(b.bases.iter().take(12).enumerate().map(|(ix, name)| {
            let (pick, n) = (h.on_base.clone(), name.clone());
            choice(("changes-base-row", ix), Some(Icon::new(Lucide::GitBranch).size(size::ICON).into_any_element()), name.clone(), current.as_ref() == Some(name))
                .on_click(move |_, w, cx| pick(n.clone(), w, cx))
        }))
}

/// The branch menu: a field that filters (or names a new branch), the branches, and creating
/// the typed name from what's checked out or from the default branch.
fn branch_menu(b: &ChangesBar, h: &ChangesHandlers) -> Div {
    let Some(c) = &b.branches else { return surface().child(heading("Loading branches…")) };
    let enabled = c.blocked.is_none() && c.busy.is_none();
    let note = |t: SharedString, col: Hsla| div().px_2p5().py_1().text_size(text::SM).line_height(relative(text::LEADING)).text_color(col).child(t);
    let rows = c.rows.as_ref().map(|rows| {
        rows.iter()
            .enumerate()
            .map(|(ix, r)| {
                let current = !c.detached && r.name == c.head;
                let busy = c.busy.as_ref() == Some(&r.name);
                let right = if busy {
                    Spinner::new(size::SPINNER_SM).into_any_element()
                } else {
                    div().text_size(text::SM).text_color(color::text_3()).child(r.when.clone()).into_any_element()
                };
                let (name, pick, close) = (r.name.clone(), h.on_branch.clone(), h.on_menu.clone());
                row(("changes-branch-row", ix))
                    .child(Icon::new(Lucide::GitBranch).size(size::ICON).text_color(color::text_2()))
                    .child(div().flex_1().min_w_0().truncate().child(r.name.clone()))
                    .child(div().flex_none().child(right))
                    .child(div().w(size::ICON).flex_none().when(current, |d| d.child(Icon::new(IconName::Check).size(size::ICON))))
                    .when(!enabled && !current, |d| d.opacity(0.5).cursor_default())
                    .on_click(move |_, w, cx| {
                        if current {
                            close(None, w, cx);
                        } else if enabled {
                            pick(name.clone(), w, cx);
                        }
                    })
            })
            .collect::<Vec<_>>()
    });
    let creates = c.create.clone().map(|name| {
        let from_here = if c.detached { "here".to_string() } else { c.head.to_string() };
        let mut options: Vec<(SharedString, Option<SharedString>)> = vec![(format!("from {from_here}").into(), None)];
        if let Some(d) = b.branch_from.clone().filter(|d| c.detached || *d != c.head) {
            options.push((format!("from {d}").into(), Some(d)));
        }
        let matched = c.rows.as_ref().is_some_and(|r| !r.is_empty());
        div().flex().flex_col().when(matched, |d| d.child(separator())).children(options.into_iter().enumerate().map(|(ix, (from_label, from))| {
            let (create, n) = (h.on_new_branch.clone(), name.clone());
            row(("changes-branch-create", ix))
                .child(Icon::new(IconName::Plus).size(size::ICON).text_color(color::text_2()))
                .child(div().flex_1().min_w_0().truncate().child(format!("Create \u{201c}{name}\u{201d}")))
                .child(div().flex_none().text_size(text::SM).text_color(color::text_3()).child(from_label))
                .when(!enabled, |d| d.opacity(0.5).cursor_default())
                .on_click(move |_, w, cx| {
                    if enabled {
                        create(n.clone(), from.clone(), w, cx);
                    }
                })
        }))
    });
    surface()
        .w(px(340.))
        .children(c.input.as_ref().map(|input| {
            div().px_1().py_1().child(
                div().h(px(26.)).px_1().flex().items_center().rounded(radius::MD).bg(color::bg()).border_1().border_color(color::border()).child(
                    Input::new(input).small().appearance(false).prefix(Icon::new(IconName::Search).size(size::ICON_SM).text_color(color::text_3())),
                ),
            )
        }))
        .children(c.blocked.clone().map(|t| note(t, color::text_3())))
        .children(c.error.clone().map(|t| note(t, color::red())))
        .when(c.changed > 0 && enabled, |d| d.child(note(format!("{} uncommitted {} come along", c.changed, if c.changed == 1 { "change" } else { "changes" }).into(), color::text_3())))
        .map(|d| match rows {
            None => d.child(note("Loading branches…".into(), color::text_3())),
            Some(rows) => d.children(rows),
        })
        .when(c.more > 0, |d| d.child(note(format!("{} more", c.more).into(), color::text_3())))
        .children(creates)
}

/// The pull request's editor: its title and description, and where it goes.
pub struct PrEditorView {
    pub number: u64,
    pub title: Entity<InputState>,
    pub body: Entity<TextareaState>,
    pub base: SharedString,
    pub bases: Vec<SharedString>,
    pub base_open: bool,
    pub saving: bool,
    pub error: Option<SharedString>,
}

#[derive(Clone)]
pub struct PrEditorHandlers {
    pub on_save: Handler,
    pub on_cancel: Handler,
    pub on_base_menu: OpenHandler,
    pub on_base: NameHandler,
}

pub fn pr_editor(e: &PrEditorView, h: &PrEditorHandlers) -> Div {
    let label = |t: &'static str| div().text_size(text::SM).text_color(color::text_3()).child(t);
    let field = || div().px_2().rounded(radius::MD).bg(color::bg()).border_1().border_color(color::border());
    let (save, cancel, menu, menu2, pick) = (h.on_save.clone(), h.on_cancel.clone(), h.on_base_menu.clone(), h.on_base_menu.clone(), h.on_base.clone());
    let base_menu = surface().min_w(px(220.)).child(heading("Pull request goes into")).children(e.bases.iter().take(12).enumerate().map(|(ix, name)| {
        let (pick, n) = (pick.clone(), name.clone());
        choice(("pr-base-row", ix), None, name.clone(), *name == e.base).on_click(move |_, w, cx| pick(n.clone(), w, cx))
    }));
    div()
        .flex_none()
        .m_2()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(Lucide::GitPullRequest).size(size::ICON).text_color(color::green()))
                .child(div().flex_1().text_size(text::MD).text_color(color::text()).child(format!("Pull request #{}", e.number))),
        )
        .child(label("Title"))
        .child(field().h(px(30.)).flex().items_center().child(Input::new(&e.title).small().appearance(false)))
        .child(label("Description"))
        .child(field().py_1().h(px(180.)).child(Textarea::new(&e.body).appearance(false).xsmall().text_size(text::SM).h_full()))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(label("Into"))
                .child(
                    chip("pr-base", e.base_open, true)
                        .flex_none()
                        .child(Icon::new(Lucide::GitBranch).size(size::ICON_SM).text_color(color::text_3()))
                        .child(e.base.clone())
                        .child(Icon::new(IconName::ChevronDown).size(size::ICON_XS).text_color(color::text_3()))
                        .on_click({
                            let open = e.base_open;
                            move |_, w, cx| menu(!open, w, cx)
                        })
                        .when(e.base_open, |d| d.child(dropdown(false, base_menu, move |w, cx| menu2(false, w, cx)))),
                )
                .child(div().flex_1().min_w_0())
                .when(e.saving, |d| d.child(Spinner::new(size::SPINNER_SM)))
                .child(pill_button("pr-cancel", "Cancel", false).on_click(move |_, w, cx| cancel(w, cx)))
                .child(pill_button("pr-save", "Save", true).when(e.saving, |d| d.opacity(0.6)).on_click(move |_, w, cx| save(w, cx))),
        )
        .children(e.error.clone().map(|t| div().text_size(text::SM).text_color(color::red()).child(t)))
}

/// A changed file in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedRow {
    /// Its folder, relative ("crates/space-ui/src/"), shown quieter before the name.
    pub dir: SharedString,
    pub name: SharedString,
    pub mark: GitMark,
    /// A rename's old path.
    pub from: Option<SharedString>,
    /// Lines added and removed; `None`: binary.
    pub lines: Option<(usize, usize)>,
    pub expanded: bool,
}

pub const CHANGED_ROW_H: Pixels = px(28.);

pub fn changed_row(ix: usize, r: &ChangedRow, on_click: RowHandler) -> Stateful<Div> {
    let counts = match r.lines {
        Some((a, d)) => div()
            .flex_none()
            .flex()
            .gap_1p5()
            .font_family(text::MONO_FONT)
            .text_size(text::XS)
            .when(a > 0, |e| e.child(div().text_color(color::green()).child(format!("+{a}"))))
            .when(d > 0, |e| e.child(div().text_color(color::red()).child(format!("\u{2212}{d}")))),
        None => div().flex_none().text_size(text::XS).text_color(color::text_3()).child("binary"),
    };
    div()
        .id(("changed-row", ix))
        .h(CHANGED_ROW_H)
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(text::SM)
        .cursor_pointer()
        .border_b_1()
        .border_color(color::border())
        .map(|d| if r.expanded { d.bg(color::surface()) } else { d.hover(|s| s.bg(color::hover())) })
        .child(Icon::new(if r.expanded { IconName::ChevronDown } else { IconName::ChevronRight }).size(size::ICON_SM).text_color(color::text_3()))
        .child(Icon::new(file_icon(&r.name)).size(size::ICON).text_color(color::text_3()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(div().flex_none().text_color(r.mark.color()).when(r.mark == GitMark::Deleted, |d| d.line_through()).child(r.name.clone()))
                .child(div().pl_1p5().min_w_0().truncate().text_size(text::XS).text_color(color::text_3()).child(match &r.from {
                    Some(from) => format!("{}  \u{2190} {from}", r.dir).into(),
                    None => r.dir.clone(),
                })),
        )
        .child(counts)
        .child(div().w(px(12.)).flex_none().font_family(text::MONO_FONT).text_size(text::XS).text_color(r.mark.color()).child(r.mark.letter()))
        .on_click(move |_, w, cx| on_click(ix, w, cx))
}

/// One quiet line where there's nothing else to show (not a repository, a diff with no lines).
pub fn changes_empty(text: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .px_6()
        .text_center()
        .text_size(text::SM)
        .text_color(color::text_3())
        .child(text.into())
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};

    fn pane(child: impl IntoElement) -> Div {
        div().w(px(520.)).flex().flex_col().bg(color::bg()).border_1().border_color(color::border()).child(child)
    }
    fn bar(b: ChangesBar) -> AnyElement {
        pane(changes_bar(&b, &ChangesHandlers::default())).into_any_element()
    }
    let base = || ChangesBar { branch: "feat/resize".into(), ..Default::default() };
    let rows = [
        ChangedRow { dir: "crates/space-app/src/".into(), name: "files.rs".into(), mark: GitMark::Modified, from: None, lines: Some((42, 7)), expanded: false },
        ChangedRow { dir: "crates/space-ui/src/".into(), name: "file_tree.rs".into(), mark: GitMark::Modified, from: None, lines: Some((12, 3)), expanded: true },
        ChangedRow { dir: "crates/space-app/src/".into(), name: "resize.rs".into(), mark: GitMark::Untracked, from: None, lines: Some((80, 0)), expanded: false },
        ChangedRow { dir: "assets/".into(), name: "icon.png".into(), mark: GitMark::Added, from: None, lines: None, expanded: false },
        ChangedRow { dir: "".into(), name: "old.rs".into(), mark: GitMark::Deleted, from: None, lines: Some((0, 30)), expanded: false },
    ];
    vec![ComponentPreview::new(Area::Shell, "Changes", "The right pane's Changes mode: a bar that offers only the next step, and the changed files.").stacked().examples([
        Example::new("files changed", move |_, _| bar(ChangesBar { status: "3 files changed".into(), primary: Some(ChangeAction::Commit), menu: vec![ChangeAction::CommitAndPush, ChangeAction::CommitAndPr], ..base() })),
        Example::new("committed, not pushed", move |_, _| {
            bar(ChangesBar {
                base: Some("main".into()),
                status: "2 commits not pushed".into(),
                primary: Some(ChangeAction::CreatePr),
                menu: vec![ChangeAction::Push],
                done: Some(Done { text: "Committed a1b2c3d · Resizable file tree".into(), undo: true }),
                ..base()
            })
        }),
        Example::new("running", move |_, _| bar(ChangesBar { status: "Writing the commit message…".into(), running: true, ..base() })),
        Example::new("pull request open", move |_, _| {
            bar(ChangesBar {
                base: Some("main".into()),
                status: "PR #123 open · up to date".into(),
                tone: Tone::Good,
                menu: vec![ChangeAction::EditPr],
                pr: Some(PrChip { number: 123, title: "Resizable file tree".into(), state: "open".into() }),
                ..base()
            })
        }),
        Example::new("on main, no gh", move |_, _| {
            bar(ChangesBar {
                branch: "main".into(),
                status: "1 file changed on main".into(),
                primary: Some(ChangeAction::Commit),
                menu: vec![ChangeAction::CommitAndPush, ChangeAction::CommitToNewBranch],
                note: Some("Install the GitHub CLI (brew install gh) for pull requests".into()),
                ..Default::default()
            })
        }),
        Example::new("mid-merge", move |_, _| bar(ChangesBar { status: "Merge in progress · 2 conflicts".into(), tone: Tone::Warn, error: Some("Couldn't push: rejected".into()), ..base() })),
        Example::new("changed files", move |_, _| {
            let click: RowHandler = Rc::new(|_, _, _| {});
            pane(div().flex().flex_col().children(rows.iter().enumerate().map(|(ix, r)| changed_row(ix, r, click.clone())))).into_any_element()
        }),
        Example::new("nothing to show", move |_, _| pane(div().h(px(200.)).flex().child(changes_empty("Not a git repository"))).into_any_element()),
    ])]
}
