//! The app's own title bar (the window title bar is transparent): navigation over the
//! sidebar, thread title over the main column. Both are window drag regions.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::IconName;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::widgets::{icon_button, tip_action};
use crate::ClickHandler;
use crate::actions::{GoBack, GoForward, NewThread, ToggleSidebar};
use crate::theme::{color, size, text};

/// Makes `el` a window drag region: dragging moves the window, a double click zooms or
/// minimizes (per the macOS setting). Controls inside stop mouse-down propagation.
pub fn drag_region(el: Stateful<Div>, key: &'static str, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let pending = window.use_keyed_state(SharedString::from(format!("drag-{key}")), cx, |_, _| false);
    let (down, up, moved) = (pending.clone(), pending.clone(), pending);
    el.on_mouse_down(MouseButton::Left, move |e, window, cx| {
        cx.stop_propagation();
        let double = e.click_count >= 2;
        down.update(cx, |p, _| *p = !double);
        if double {
            window.titlebar_double_click();
        }
    })
    .on_mouse_up(MouseButton::Left, move |_, _, cx| up.update(cx, |p, _| *p = false))
    .on_mouse_move(move |e, window, cx| {
        if *moved.read(cx) {
            moved.update(cx, |p, _| *p = false);
            if e.pressed_button == Some(MouseButton::Left) {
                window.start_window_move();
            }
        }
    })
}

/// Sidebar toggle, back/forward and new thread.
#[derive(Clone)]
pub struct TitleBarNav {
    pub sidebar_open: bool,
    pub can_back: bool,
    pub can_forward: bool,
    pub on_toggle_sidebar: ClickHandler,
    pub on_back: ClickHandler,
    pub on_forward: ClickHandler,
    pub on_new_thread: ClickHandler,
}

impl TitleBarNav {
    /// No-op handlers (previews).
    pub fn inert(sidebar_open: bool, can_back: bool, can_forward: bool) -> Self {
        let noop: ClickHandler = Rc::new(|_, _, _| {});
        Self {
            sidebar_open,
            can_back,
            can_forward,
            on_toggle_sidebar: noop.clone(),
            on_back: noop.clone(),
            on_forward: noop.clone(),
            on_new_thread: noop,
        }
    }

    fn buttons(&self) -> Div {
        let (toggle, back, fwd) = (self.on_toggle_sidebar.clone(), self.on_back.clone(), self.on_forward.clone());
        let toggle_tip = if self.sidebar_open { "Hide sidebar" } else { "Show sidebar" };
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                icon_button("toggle-sidebar", IconName::PanelLeft, true, false)
                    .tooltip(tip_action(toggle_tip, &ToggleSidebar))
                    .on_click(move |e, w, cx| toggle(e, w, cx)),
            )
            .child(icon_button("go-back", IconName::ArrowLeft, self.can_back, false).when(self.can_back, |b| {
                b.tooltip(tip_action("Back", &GoBack)).on_click(move |e, w, cx| back(e, w, cx))
            }))
            .child(icon_button("go-forward", IconName::ArrowRight, self.can_forward, false).when(
                self.can_forward,
                |b| b.tooltip(tip_action("Forward", &GoForward)).on_click(move |e, w, cx| fwd(e, w, cx)),
            ))
    }

    fn new_thread(&self) -> Stateful<Div> {
        let f = self.on_new_thread.clone();
        icon_button("new-thread", Lucide::SquarePen, true, false)
            .tooltip(tip_action("New thread", &NewThread))
            .on_click(move |e, w, cx| f(e, w, cx))
    }
}

/// Title bar segment over the sidebar column.
#[derive(IntoElement)]
pub struct SidebarTitleBar {
    nav: TitleBarNav,
}

impl SidebarTitleBar {
    pub fn new(nav: TitleBarNav) -> Self {
        Self { nav }
    }
}

impl RenderOnce for SidebarTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        drag_region(div().id("titlebar-sidebar"), "sidebar", window, cx)
            .h(size::TITLE_BAR_H)
            .flex_none()
            .flex()
            .items_center()
            .pl(size::TRAFFIC_LIGHTS_W)
            .pr_2()
            .child(self.nav.buttons())
            .child(div().flex_1())
            .child(self.nav.new_thread())
    }
}

/// Title bar over the main column. With the sidebar collapsed it also carries the nav.
#[derive(IntoElement)]
pub struct MainTitleBar {
    title: SharedString,
    nav: Option<TitleBarNav>,
}

impl MainTitleBar {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), nav: None }
    }

    /// Show the navigation here (sidebar collapsed).
    pub fn nav(mut self, nav: Option<TitleBarNav>) -> Self {
        self.nav = nav;
        self
    }
}

impl RenderOnce for MainTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        drag_region(div().id("titlebar-main"), "main", window, cx)
            .h(size::TITLE_BAR_H)
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(color::border())
            .when_some(self.nav, |d, nav| {
                d.pl(size::TRAFFIC_LIGHTS_W).child(nav.buttons()).child(nav.new_thread()).child(div().w_2())
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(text::MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let title = "Fix the failing smoke test in space-core";
    vec![
        ComponentPreview::new(Area::Shell, "Title bar", "Window title bar; the window's traffic lights sit in the left inset.")
            .stacked()
            .example(
                Example::new("with sidebar", move |_, _| {
                    div()
                        .flex()
                        .child(
                            div()
                                .w(size::SIDEBAR_W)
                                .bg(color::bg_sidebar())
                                .border_r_1()
                                .border_color(color::border())
                                .child(SidebarTitleBar::new(TitleBarNav::inert(true, true, false))),
                        )
                        .child(div().flex_1().child(MainTitleBar::new(title)))
                        .into_any_element()
                })
                .width(px(900.)),
            )
            .example(
                Example::new("sidebar collapsed", move |_, _| {
                    MainTitleBar::new(title).nav(Some(TitleBarNav::inert(false, true, true))).into_any_element()
                })
                .width(px(900.)),
            ),
    ]
}
