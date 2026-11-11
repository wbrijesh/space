//! The right pane beside a thread: Terminal or Files. Its header (a switch between the two,
//! then the view's own controls, then hide), the chooser shown before a thread picks a view,
//! and the Files view.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::menu::shortcut_label;
use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::actions::{ShowFiles, ShowTerminal, ToggleRightPane};
use crate::theme::{color, radius, size, text};

/// The pane's views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneView {
    Terminal,
    Files,
}

impl PaneView {
    pub const ALL: [PaneView; 2] = [PaneView::Terminal, PaneView::Files];

    pub fn label(self) -> &'static str {
        match self {
            PaneView::Terminal => "Terminal",
            PaneView::Files => "Files",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            PaneView::Terminal => Icon::new(Lucide::SquareTerminal),
            PaneView::Files => Icon::new(IconName::Folder),
        }
    }

    fn action(self) -> Box<dyn Action> {
        match self {
            PaneView::Terminal => Box::new(ShowTerminal),
            PaneView::Files => Box::new(ShowFiles),
        }
    }
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type ViewHandler = Rc<dyn Fn(PaneView, &mut Window, &mut App)>;

/// A quiet two-option switch: the shown view filled, the other plain.
pub fn view_switch(current: PaneView, on_view: ViewHandler) -> Div {
    div()
        .flex_none()
        .h(px(24.))
        .p(px(2.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded(radius::MD)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .children(PaneView::ALL.map(|v| {
            let on = v == current;
            let pick = on_view.clone();
            div()
                .id(("pane-view", v as usize))
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .rounded(radius::SM)
                .text_size(text::SM)
                .map(|d| if on { d.bg(color::hover()).text_color(color::text()) } else { d.text_color(color::text_3()) })
                .when(!on, |d| d.cursor_pointer().hover(|s| s.text_color(color::text())))
                .child(v.label())
                .on_click(move |_, w, cx| pick(v, w, cx))
        }))
}

/// The pane's header: the switch (once a view is chosen), the view's own controls, and hide.
pub fn header(current: Option<PaneView>, controls: Option<Div>, on_view: ViewHandler, on_hide: Handler) -> Div {
    div()
        .h(size::PANE_BAR_H)
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .border_b_1()
        .border_color(color::border())
        .children(current.map(|v| view_switch(v, on_view)))
        .child(controls.unwrap_or_else(|| div().flex_1()))
        .child(icon_button("pane-hide", Lucide::X, true, false).tip_action("Hide pane", &ToggleRightPane).on_click(move |_, w, cx| on_hide(w, cx)))
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

/// The Files view (filled in later).
pub fn files_placeholder() -> Div {
    div().flex_1().min_h_0().flex().items_center().justify_center().text_size(text::SM).text_color(color::text_3()).child("Files")
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use std::sync::Arc;

    use super::terminal::{StripHandlers, TerminalTab, sample_frame, tab_strip, terminal_grid};
    use crate::preview::{Area, ComponentPreview, Example};

    fn pane(child: impl IntoElement) -> Div {
        div().w(px(560.)).h(px(320.)).flex().flex_col().bg(color::bg()).border_1().border_color(color::border()).child(child)
    }
    let on_view: ViewHandler = Rc::new(|_, _, _| {});
    let hide: Handler = Rc::new(|_, _| {});
    let (v1, v2, v3) = (on_view.clone(), on_view.clone(), on_view);
    let (h1, h2, h3) = (hide.clone(), hide.clone(), hide);
    vec![ComponentPreview::new(Area::Shell, "Right pane", "Beside a thread: Terminal or Files, and the chooser before one is picked.").stacked().examples([
        Example::new("chooser", move |window, _| {
            pane(div().size_full().flex().flex_col().child(header(None, None, v1.clone(), h1.clone())).child(chooser(v1.clone(), window))).into_any_element()
        }),
        Example::new("terminal", move |_, _| {
            let tabs = vec![TerminalTab { title: "zsh".into(), unread: false, ended: false }, TerminalTab { title: "cargo".into(), unread: true, ended: false }];
            let strip = tab_strip(&tabs, 0, false, &StripHandlers::default());
            pane(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header(Some(PaneView::Terminal), Some(strip), v2.clone(), h2.clone()))
                    .child(div().flex_1().child(terminal_grid(Arc::new(sample_frame(70, 12)), false, Rc::new(|_, _, _| {})))),
            )
            .into_any_element()
        }),
        Example::new("files", move |_, _| {
            pane(div().size_full().flex().flex_col().child(header(Some(PaneView::Files), None, v3.clone(), h3.clone())).child(files_placeholder())).into_any_element()
        }),
    ])]
}
