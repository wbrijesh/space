//! The app's own title bar (the window title bar is transparent): navigation over the
//! sidebar, thread title over the main column. Both are window drag regions.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::IconName;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::ClickHandler;
use crate::actions::{GoBack, GoForward, NewThread, ToggleSidebar, ToggleTerminal};
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
    /// The current thread is a new one (nothing sent yet): the button shows it's where you are.
    pub new_thread_active: bool,
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
            new_thread_active: false,
        }
    }

    /// The sidebar toggle, then back/forward while the sidebar shows. With the sidebar hidden
    /// only the toggle stays (you're working in one thread; ⌘[ / ⌘] still go back and forth):
    /// the arrows fade as it slides away, gone before the title glides over their place.
    fn buttons(&self, shown: f32) -> Div {
        let (toggle, back, fwd) = (self.on_toggle_sidebar.clone(), self.on_back.clone(), self.on_forward.clone());
        let toggle_tip = if self.sidebar_open { "Hide sidebar" } else { "Show sidebar" };
        let arrows = ((shown - 0.35) / 0.45).clamp(0., 1.);
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                icon_button("toggle-sidebar", IconName::PanelLeft, true, false)
                    .tip_action(toggle_tip, &ToggleSidebar)
                    .on_click(move |e, w, cx| toggle(e, w, cx)),
            )
            .when(arrows > 0., |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .opacity(arrows)
                        .child(icon_button("go-back", IconName::ArrowLeft, self.can_back, false).when(self.can_back, |b| {
                            b.tip_action("Back", &GoBack).on_click(move |e, w, cx| back(e, w, cx))
                        }))
                        .child(icon_button("go-forward", IconName::ArrowRight, self.can_forward, false).when(
                            self.can_forward,
                            |b| b.tip_action("Forward", &GoForward).on_click(move |e, w, cx| fwd(e, w, cx)),
                        )),
                )
            })
    }

    /// Sidebar toggle and back/forward, fixed beside the traffic lights whether the sidebar is
    /// shown, hidden or moving (as macOS keeps toolbar items put while a sidebar slides). The
    /// root draws it over both title bars, which leave room for it. `shown`: how far the
    /// sidebar is shown (0 hidden … 1 shown).
    pub fn overlay(&self, shown: f32) -> Div {
        div()
            .absolute()
            .top_0()
            .left(size::TRAFFIC_LIGHTS_W)
            .h(size::TITLE_BAR_H)
            .flex()
            .items_center()
            .child(self.buttons(shown.clamp(0., 1.)))
    }

    fn new_thread(&self) -> Stateful<Div> {
        let f = self.on_new_thread.clone();
        icon_button("new-thread", Lucide::SquarePen, true, self.new_thread_active)
            .tip_action("New thread", &NewThread)
            .on_click(move |e, w, cx| f(e, w, cx))
    }
}

/// With the sidebar hidden, where the main title bar's title starts (window x): after the
/// sidebar toggle, the only button left.
const HIDDEN_TITLE_X: f32 = 92. + 28. + 12.;

/// Title bar segment over the sidebar column (the nav overlay sits on its left).
#[derive(IntoElement)]
pub struct SidebarTitleBar {
    nav: TitleBarNav,
    shown: f32,
}

impl SidebarTitleBar {
    pub fn new(nav: TitleBarNav) -> Self {
        Self { nav, shown: 1. }
    }

    /// How far the sidebar is shown (0 hidden … 1 shown). While it slides, the new-thread
    /// button fades in only once it's clear of the nav overlay it passes under.
    pub fn shown(mut self, shown: f32) -> Self {
        self.shown = shown.clamp(0., 1.);
        self
    }
}

impl RenderOnce for SidebarTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        drag_region(div().id("titlebar-sidebar"), "sidebar", window, cx)
            .h(size::TITLE_BAR_H)
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .pr_2()
            .child(div().opacity(((self.shown - 0.7) / 0.3).clamp(0., 1.)).child(self.nav.new_thread()))
    }
}

/// Title bar over the main column. While the sidebar is hidden the title sits after the
/// sidebar toggle; as the sidebar slides in, it glides to its place beside it.
#[derive(IntoElement)]
pub struct MainTitleBar {
    title: SharedString,
    nav: Option<TitleBarNav>,
    /// How far the sidebar is shown (0 hidden, 1 shown).
    sidebar: f32,
    /// The terminal button: whether the pane is open, and the toggle.
    terminal: Option<(bool, ClickHandler)>,
    /// Open the thread's folder in an app.
    open_in: Option<super::open_in::OpenIn>,
}

impl MainTitleBar {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), nav: None, sidebar: 1., terminal: None, open_in: None }
    }

    /// The "Open in" control at the right, left of the terminal button.
    pub fn open_in(mut self, open_in: super::open_in::OpenIn) -> Self {
        self.open_in = Some(open_in);
        self
    }

    /// A terminal button at the right (filled while the pane is open).
    pub fn terminal(mut self, open: bool, on_toggle: ClickHandler) -> Self {
        self.terminal = Some((open, on_toggle));
        self
    }

    /// The sidebar's shown fraction (0 hidden … 1 shown), which places the title.
    pub fn sidebar(mut self, shown: f32, nav: TitleBarNav) -> Self {
        self.sidebar = shown.clamp(0., 1.);
        self.nav = Some(nav);
        self
    }
}

impl RenderOnce for MainTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Positions in window x, made relative to this bar's left edge (the sidebar's width).
        let edge = f32::from(size::SIDEBAR_W) * self.sidebar;
        let shown_title_x = f32::from(size::SIDEBAR_W) + 16.;
        let title_x = HIDDEN_TITLE_X + (shown_title_x - HIDDEN_TITLE_X) * self.sidebar;
        drag_region(div().id("titlebar-main"), "main", window, cx)
            .relative()
            .overflow_hidden()
            .h(size::TITLE_BAR_H)
            .flex_none()
            .flex()
            .items_center()
            .pl(px(title_x - edge))
            .pr_4()
            .border_b_1()
            .border_color(color::border())
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(text::MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .when_some(self.open_in, |d, o| d.child(div().mr_1().child(super::open_in::open_in_control(o))))
            .when_some(self.terminal, |d, (open, toggle)| {
                d.child(
                    icon_button("toggle-terminal", Lucide::SquareTerminal, true, open)
                        .tip_action("Terminal", &ToggleTerminal)
                        .on_click(move |e, w, cx| toggle(e, w, cx)),
                )
            })
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
                    let nav = TitleBarNav::inert(true, true, false);
                    div()
                        .relative()
                        .flex()
                        .child(
                            div()
                                .w(size::SIDEBAR_W)
                                .bg(color::bg_sidebar())
                                .border_r_1()
                                .border_color(color::border())
                                .child(SidebarTitleBar::new(nav.clone())),
                        )
                        .child(div().flex_1().child(MainTitleBar::new(title).sidebar(1., nav.clone())))
                        .child(nav.overlay(1.))
                        .into_any_element()
                })
                .width(px(900.)),
            )
            .example(
                Example::new("open in", move |_, _| {
                    let nav = TitleBarNav::inert(false, true, true);
                    let bar = MainTitleBar::new(title).sidebar(0., nav.clone()).open_in(super::open_in::fixture()).terminal(false, Rc::new(|_, _, _| {}));
                    div().relative().child(bar).child(nav.overlay(0.)).into_any_element()
                })
                .width(px(900.)),
            )
            .example(
                Example::new("open in, menu", move |_, _| {
                    let o = super::open_in::fixture();
                    let menu = super::open_in::open_in_menu(&o.apps, o.on_pick.clone(), o.on_copy_path.clone());
                    div().flex().justify_end().child(menu).into_any_element()
                })
                .width(px(900.)),
            )
            .example(
                Example::new("sidebar collapsed", move |_, _| {
                    let nav = TitleBarNav::inert(false, true, true);
                    div().relative().child(MainTitleBar::new(title).sidebar(0., nav.clone())).child(nav.overlay(0.)).into_any_element()
                })
                .width(px(900.)),
            ),
    ]
}
