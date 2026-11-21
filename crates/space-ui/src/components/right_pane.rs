//! The right pane beside a thread, in four modes: Files, Terminal, the side chat and Changes. Its header
//! (the modes, then maximize; the title bar's pane button hides it), the bar under it with the
//! mode's own tabs (the side chat's: what it is, and starting it over), and the chooser shown
//! before a thread picks a mode.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::*;

use super::file_viewer::pane_tab;
use super::menu::shortcut_label;
use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::actions::{MaximizePane, ShowChanges, ShowFiles, ShowSideChat, ShowTerminal};
use crate::theme::{color, radius, size, text};

/// The pane's views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneView {
    Terminal,
    Files,
    /// A side chat about the thread: it reads along and can't change anything.
    SideChat,
    /// The folder's uncommitted changes, file by file, and committing, pushing and pull requests.
    Changes,
}

impl PaneView {
    /// In the header's order.
    pub const ALL: [PaneView; 4] = [PaneView::Files, PaneView::Terminal, PaneView::SideChat, PaneView::Changes];

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
            PaneView::Files => Icon::new(IconName::Folder),
            PaneView::SideChat => Icon::new(Lucide::MessagesSquare),
            PaneView::Changes => Icon::new(Lucide::GitCompare),
        }
    }

    fn action(self) -> Box<dyn Action> {
        match self {
            PaneView::Terminal => Box::new(ShowTerminal),
            PaneView::Files => Box::new(ShowFiles),
            PaneView::SideChat => Box::new(ShowSideChat),
            PaneView::Changes => Box::new(ShowChanges),
        }
    }
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type ViewHandler = Rc<dyn Fn(PaneView, &mut Window, &mut App)>;

/// The pane's header: its modes (`view`: the one shown), then maximize (it fills the window
/// beside the sidebar) or restore. A narrow pane (`width`) names only the shown mode; the
/// others are their icons.
pub fn header(view: Option<PaneView>, maximized: bool, width: Pixels, on_view: ViewHandler, on_maximize: Handler) -> Div {
    let compact = width < px(480.);
    let modes = PaneView::ALL.map(|v| {
        let pick = on_view.clone();
        let id = ElementId::Name(format!("pane-mode-{}", v.label()).into());
        let label = (!compact || (view == Some(v) && width >= px(300.))).then(|| v.label().into());
        pane_tab(id, label, v.icon(), view == Some(v), None, Rc::new(move |w, cx| pick(v, w, cx))).flex_none().tip_action(v.label(), v.action().as_ref())
    });
    bar()
        .child(div().flex_1().min_w_0().overflow_hidden().flex().items_center().gap_1().children(modes))
        .child(
            icon_button("pane-maximize", if maximized { Lucide::Minimize2 } else { Lucide::Maximize2 }, true, false)
                .tip_action(if maximized { "Restore" } else { "Maximize" }, &MaximizePane)
                .on_click(move |_, w, cx| on_maximize(w, cx)),
        )
}

/// The bar under the header, with the shown mode's tabs (the open files, or the terminals).
pub fn sub_bar(tabs: impl IntoElement) -> Div {
    bar().child(tabs)
}

/// The side chat's bar: what it is, and ↺ to start it over from the thread as it is now.
pub fn side_chat_bar(on_new: Option<Handler>) -> Div {
    bar()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .px_1()
                .text_size(text::SM)
                .text_color(color::text_3())
                .child("Read-only · follows this thread"),
        )
        .children(on_new.map(|on_new| {
            icon_button("side-chat-new", Lucide::RotateCw, true, false).tip("New side chat").on_click(move |_, w, cx| on_new(w, cx))
        }))
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
        .child(div().text_size(text::MD).text_color(color::text_2()).child("Ask anything about this thread."))
        .child(div().text_size(text::SM).text_color(color::text_3()).child("It reads along as the thread goes on, and can't change anything."))
}

fn bar() -> Div {
    div().h(size::PANE_BAR_H).flex_none().flex().items_center().gap_1().px_2().border_b_1().border_color(color::border())
}

/// Before a thread picks a view: two large rows, centred, each with its icon and shortcut.
pub fn chooser(on_view: ViewHandler, window: &Window) -> Div {
    div().flex_1().min_h_0().flex().items_center().justify_center().p_6().child(
        div().w_full().max_w(px(280.)).flex().flex_col().gap_2().children(PaneView::ALL.map(|v| {
            let pick = on_view.clone();
            let keys = shortcut_label(v.action().as_ref(), window);
            div()
                .id(("pane-choose", v as usize))
                .h(px(52.))
                .px_4()
                .flex()
                .items_center()
                .gap_3()
                .rounded(radius::XL)
                .bg(color::surface())
                .border_1()
                .border_color(color::border())
                .cursor_pointer()
                .hover(|s| s.bg(color::hover()).border_color(color::border_strong()))
                .child(v.icon().size(size::ICON_MD).text_color(color::text_2()))
                .child(div().flex_1().text_size(text::MD).text_color(color::text()).child(v.label()))
                .children(keys.map(|k| div().text_size(text::SM).text_color(color::text_3()).child(k)))
                .on_click(move |_, w, cx| pick(v, w, cx))
        })),
    )
}


#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use std::sync::Arc;

    use gpui::prelude::FluentBuilder as _;

    use super::file_viewer::file_tabs;
    use super::terminal::{StripHandlers, TerminalTab, sample_frame, tab_strip, terminal_grid};
    use crate::preview::{Area, ComponentPreview, Example};

    fn pane(child: impl IntoElement) -> Div {
        div().w(px(560.)).h(px(320.)).flex().flex_col().bg(color::bg()).border_1().border_color(color::border()).child(child)
    }
    fn header_for(view: Option<PaneView>) -> Div {
        header(view, false, px(560.), Rc::new(|_, _, _| {}), Rc::new(|_, _| {}))
    }
    fn placeholder(label: &'static str) -> Div {
        div().flex_1().min_h_0().flex().items_center().justify_center().text_size(text::SM).text_color(color::text_3()).child(label)
    }
    let names: Vec<SharedString> = vec!["main.rs".into(), "README.md".into()];
    let files = move |tree: bool| {
        let tabs = file_tabs(&names, Some(0), tree, Rc::new(|_, _| {}), Rc::new(|_, _, _| {}), Rc::new(|_, _, _| {}));
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(tree, |d| d.child(div().w(size::PANE_TREE_W).flex_none().border_r_1().border_color(color::border()).flex().child(placeholder("Folder tree"))))
            .child(placeholder("main.rs"));
        pane(div().size_full().flex().flex_col().child(header_for(Some(PaneView::Files))).child(sub_bar(tabs)).child(body)).into_any_element()
    };
    let (f1, f2) = (files.clone(), files);
    vec![ComponentPreview::new(Area::Shell, "Right pane", "Beside a thread: Files, Terminal or the side chat, each with its own bar, and the chooser before one is picked.").stacked().examples([
        Example::new("chooser", move |window, _| {
            pane(div().size_full().flex().flex_col().child(header_for(None)).child(chooser(Rc::new(|_, _, _| {}), window))).into_any_element()
        }),
        Example::new("files: the tree, no file open", move |_, _| {
            pane(div().size_full().flex().flex_col().child(header_for(Some(PaneView::Files))).child(placeholder("Folder tree"))).into_any_element()
        }),
        Example::new("files: open files beside the tree", move |_, _| f1(true)),
        Example::new("files: tree hidden", move |_, _| f2(false)),
        Example::new("terminal", move |_, _| {
            let tabs = vec![TerminalTab { title: "zsh".into(), unread: false, ended: false }, TerminalTab { title: "cargo".into(), unread: true, ended: false }];
            let strip = tab_strip(&tabs, Some(0), &StripHandlers::default());
            pane(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header_for(Some(PaneView::Terminal)))
                    .child(sub_bar(strip))
                    .child(div().flex_1().child(terminal_grid(Arc::new(sample_frame(70, 12)), false, Rc::new(|_, _, _| {})))),
            )
            .into_any_element()
        }),
        Example::new("side chat: nothing asked yet", move |_, _| {
            pane(div().size_full().flex().flex_col().child(header_for(Some(PaneView::SideChat))).child(side_chat_bar(None)).child(side_chat_empty()))
                .into_any_element()
        }),
        Example::new("side chat", move |_, _| {
            pane(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header_for(Some(PaneView::SideChat)))
                    .child(side_chat_bar(Some(Rc::new(|_, _| {}))))
                    .child(placeholder("The side chat's transcript"))
                    .child(div().h(px(64.)).m_2().rounded(radius::XL).border_1().border_color(color::border()).bg(color::surface())),
            )
            .into_any_element()
        }),
    ])]
}
