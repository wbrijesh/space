//! The right pane beside a thread: one strip of tabs (each one thing: the folder's tree, a file,
//! a terminal, Changes, the side chat, or a new tab), then maximize. A new tab is the launcher:
//! a search over the folder's files, then everything the pane can open.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::input::{Input, InputState};
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::actions::{MaximizePane, NewPaneTab};
use crate::theme::{color, radius, size, text};

/// What a tab can show, for its icon and the shortcuts that open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneView {
    Terminal,
    /// The folder's tree.
    Files,
    /// A side chat about the thread: it reads along and can't change anything.
    SideChat,
    /// The folder's uncommitted changes, file by file, and committing, pushing and pull requests.
    Changes,
}

impl PaneView {
    pub fn label(self) -> &'static str {
        match self {
            PaneView::Terminal => "Terminal",
            PaneView::Files => "Files",
            PaneView::SideChat => "Side chat",
            PaneView::Changes => "Changes",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            PaneView::Terminal => Icon::new(Lucide::SquareTerminal),
            PaneView::Files => Icon::new(Lucide::ListTree),
            PaneView::SideChat => Icon::new(Lucide::MessagesSquare),
            PaneView::Changes => Icon::new(Lucide::GitCompare),
        }
    }
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A tab in the strip.
#[derive(Clone)]
pub struct StripTab {
    pub label: SharedString,
    pub icon: Icon,
    /// Faded: its shell ended.
    pub ended: bool,
    /// Something arrived it hasn't shown yet.
    pub unread: bool,
}

impl StripTab {
    pub fn new(label: impl Into<SharedString>, icon: Icon) -> Self {
        Self { label: label.into(), icon, ended: false, unread: false }
    }

    /// The launcher's tab.
    pub fn launcher() -> Self {
        Self::new("New tab", Icon::new(Lucide::LayoutGrid))
    }
}

#[derive(Clone)]
pub struct StripHandlers {
    pub on_select: IndexHandler,
    pub on_close: IndexHandler,
    pub on_new: Handler,
    pub on_maximize: Handler,
}

impl Default for StripHandlers {
    fn default() -> Self {
        Self { on_select: Rc::new(|_, _, _| {}), on_close: Rc::new(|_, _, _| {}), on_new: Rc::new(|_, _| {}), on_maximize: Rc::new(|_, _| {}) }
    }
}

/// The pane's header: its tabs (`active`: the one shown), `+` for a new tab, then maximize (it
/// fills the window beside the sidebar) or restore.
pub fn header(tabs: &[StripTab], active: usize, maximized: bool, h: &StripHandlers) -> Div {
    let new = h.on_new.clone();
    let maximize = h.on_maximize.clone();
    bar()
        .child(
            div()
                .id("pane-tabs")
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .children(tabs.iter().enumerate().map(|(ix, t)| strip_tab(ix, t, ix == active, h)))
                .child(icon_button("pane-tab-new", Lucide::Plus, true, false).tip_action("New tab", &NewPaneTab).on_click(move |_, w, cx| new(w, cx))),
        )
        .child(
            icon_button("pane-maximize", if maximized { Lucide::Minimize2 } else { Lucide::Maximize2 }, true, false)
                .tip_action(if maximized { "Restore" } else { "Maximize" }, &MaximizePane)
                .on_click(move |_, w, cx| maximize(w, cx)),
        )
}

/// A tab: its icon and name, a dot for something unseen, and × (on hover, or when shown). A
/// middle click closes it too.
fn strip_tab(ix: usize, t: &StripTab, on: bool, h: &StripHandlers) -> Stateful<Div> {
    let group: SharedString = format!("pane-tab-{ix}").into();
    let (select, close, middle) = (h.on_select.clone(), h.on_close.clone(), h.on_close.clone());
    div()
        .id(("pane-tab", ix))
        .group(group.clone())
        .flex_none()
        .max_w(px(180.))
        .h(px(24.))
        .pl_2()
        .pr_1()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::MD)
        .text_size(text::SM)
        .cursor_pointer()
        .map(|d| if on { d.bg(color::hover()).text_color(color::text()) } else { d.text_color(color::text_3()) })
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .child(t.icon.clone().size(size::ICON).text_color(if t.ended { color::text_3() } else { color::text_2() }))
        .child(div().min_w_0().truncate().when(t.ended, |d| d.text_color(color::text_3())).child(t.label.clone()))
        .when(t.unread && !on, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(color::blue())))
        .child(
            div()
                .id(("pane-tab-close", ix))
                .size(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius::SM)
                .text_color(color::text_3())
                .when(!on, |d| d.invisible().group_hover(group, |s| s.visible()))
                .hover(|s| s.bg(color::pressed()).text_color(color::text()))
                .child(Icon::new(Lucide::X).size(size::ICON_SM))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    close(ix, w, cx)
                }),
        )
        .on_mouse_up(MouseButton::Middle, move |_, w, cx| middle(ix, w, cx))
        .on_click(move |_, w, cx| select(ix, w, cx))
}

/// A row of the launcher.
#[derive(Clone)]
pub struct LauncherRow {
    pub icon: Icon,
    pub label: SharedString,
    /// Dimmed after the label (a file's folder).
    pub detail: Option<SharedString>,
    /// Right-aligned (its key binding).
    pub shortcut: Option<SharedString>,
    pub on_pick: Handler,
    /// A delete button, shown on hover.
    pub on_delete: Option<Handler>,
}

impl LauncherRow {
    pub fn new(icon: Icon, label: impl Into<SharedString>, on_pick: Handler) -> Self {
        Self { icon, label: label.into(), detail: None, shortcut: None, on_pick, on_delete: None }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into()).filter(|d: &SharedString| !d.is_empty());
        self
    }

    pub fn shortcut(mut self, shortcut: Option<String>) -> Self {
        self.shortcut = shortcut.map(Into::into);
        self
    }

    pub fn on_delete(mut self, on_delete: Handler) -> Self {
        self.on_delete = Some(on_delete);
        self
    }
}

/// A run of the launcher's rows, under a title (and a count) or none.
pub struct LauncherSection {
    pub title: Option<SharedString>,
    pub count: Option<usize>,
    pub rows: Vec<LauncherRow>,
}

/// The launcher: the search field, then its sections, or `note` in their place (no match,
/// still searching). `selected` counts rows across the sections; ↑ ↓ and ⏎ are the caller's.
pub fn launcher(search: &Entity<InputState>, sections: Vec<LauncherSection>, note: Option<SharedString>, selected: Option<usize>) -> Stateful<Div> {
    // As tall as a row below it, and sunk into the chat's darker tone so it reads as the place
    // to type.
    let field = div()
        .h(px(28.))
        .px_1()
        .flex()
        .items_center()
        .rounded(radius::MD)
        .bg(color::bg())
        .border_1()
        .border_color(color::border())
        .child(Input::new(search).appearance(false).prefix(Icon::new(IconName::Search).size(size::ICON_SM).text_color(color::text_3())));
    let mut ix = 0;
    let mut column = div().w_full().max_w(px(560.)).mx_auto().px_4().pt_4().pb_6().flex().flex_col().gap_3().child(field);
    match note {
        Some(note) => column = column.child(div().px_2().text_size(text::SM).text_color(color::text_3()).child(note)),
        None => {
            for section in sections.into_iter().filter(|s| !s.rows.is_empty()) {
                let title = section.title.map(|t| {
                    div()
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(text::XS)
                        .text_color(color::text_3())
                        .child(t)
                        .children(section.count.map(|n| div().child(n.to_string())))
                });
                let rows: Vec<_> = section
                    .rows
                    .into_iter()
                    .map(|row| {
                        let el = launcher_row(ix, row, selected == Some(ix));
                        ix += 1;
                        el
                    })
                    .collect();
                column = column.child(div().flex().flex_col().children(title).children(rows));
            }
        }
    }
    div().id("launcher").flex_1().min_h_0().overflow_y_scroll().child(column)
}

fn launcher_row(ix: usize, row: LauncherRow, on: bool) -> Stateful<Div> {
    let group: SharedString = format!("launcher-row-{ix}").into();
    let pick = row.on_pick.clone();
    div()
        .id(("launcher-row", ix))
        .group(group.clone())
        .h(px(28.))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(radius::MD)
        .text_size(text::SM)
        .text_color(color::text())
        .cursor_pointer()
        .when(on, |d| d.bg(color::hover()))
        .hover(|s| s.bg(color::hover()))
        .child(row.icon.size(size::ICON).text_color(color::text_2()))
        .child(div().flex_none().max_w(px(320.)).truncate().child(row.label))
        .child(div().flex_1().min_w_0().truncate().text_color(color::text_3()).children(row.detail))
        .children(row.shortcut.map(|k| div().flex_none().text_size(text::XS).text_color(color::text_3()).child(k)))
        .when_some(row.on_delete, |d, delete| {
            d.child(
                icon_button(("launcher-row-delete", ix), Lucide::Trash, true, false)
                    .size(px(20.))
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .tip("Delete")
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        delete(w, cx)
                    }),
            )
        })
        .on_click(move |_, w, cx| pick(w, cx))
}

/// The side chat before anything's asked.
pub fn side_chat_empty() -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .px_6()
        .text_center()
        .child(div().text_size(text::MD).text_color(color::text_2()).child("Read-only side chat"))
        .child(div().text_size(text::SM).text_color(color::text_3()).child("Ask about this thread without interrupting it. It can read files, not change them."))
}

fn bar() -> Div {
    div().h(size::PANE_BAR_H).flex_none().flex().items_center().gap_1().px_2().border_b_1().border_color(color::border())
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use std::sync::Arc;

    use super::file_tree::file_icon;
    use super::terminal::{sample_frame, terminal_grid};
    use crate::preview::{Area, ComponentPreview, Example};

    fn pane(child: impl IntoElement) -> Div {
        div().w(px(560.)).h(px(360.)).flex().flex_col().bg(color::bg()).border_1().border_color(color::border()).child(child)
    }
    fn tabs() -> Vec<StripTab> {
        vec![
            StripTab::new("Changes", PaneView::Changes.icon()),
            StripTab::new("fit.rs", Icon::new(file_icon("fit.rs"))),
            StripTab { unread: true, ..StripTab::new("cargo", PaneView::Terminal.icon()) },
            StripTab::launcher(),
        ]
    }
    fn noop() -> Handler {
        Rc::new(|_, _| {})
    }
    vec![ComponentPreview::new(Area::Shell, "Right pane", "One strip of tabs beside a thread; a new tab lists what it can open.").stacked().examples([
        Example::new("launcher", move |window, cx| {
            let search = window.use_keyed_state("launcher-preview-search", cx, |w, cx| InputState::new(w, cx).placeholder("Search files"));
            let row = |v: PaneView, keys: &str| LauncherRow::new(v.icon(), v.label(), noop()).shortcut(Some(keys.into()));
            let sections = vec![
                LauncherSection {
                    title: None,
                    count: None,
                    rows: vec![row(PaneView::Changes, "⌃⇧G"), row(PaneView::Files, "⌘P"), row(PaneView::Terminal, "⌘J"), row(PaneView::SideChat, "⇧⌘L")],
                },
                LauncherSection {
                    title: Some("Recent".into()),
                    count: None,
                    rows: vec![LauncherRow::new(Icon::new(file_icon("fit.rs")), "fit.rs", noop()).detail("crates/space-app/src")],
                },
            ];
            pane(div().size_full().flex().flex_col().child(header(&tabs(), 3, false, &StripHandlers::default())).child(launcher(&search, sections, None, Some(0))))
                .into_any_element()
        }),
        Example::new("a terminal tab", move |_, _| {
            pane(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header(&tabs(), 2, false, &StripHandlers::default()))
                    .child(div().flex_1().child(terminal_grid(Arc::new(sample_frame(70, 12)), false, Rc::new(|_, _, _| {})))),
            )
            .into_any_element()
        }),
        Example::new("side chat: nothing asked yet", move |_, _| {
            let tabs = vec![StripTab::new("Side chat", PaneView::SideChat.icon())];
            pane(div().size_full().flex().flex_col().child(header(&tabs, 0, false, &StripHandlers::default())).child(side_chat_empty())).into_any_element()
        }),
    ])]
}
