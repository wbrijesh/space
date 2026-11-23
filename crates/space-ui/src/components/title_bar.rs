//! The app's own title bar (the window title bar is transparent), one row across the window
//! above the activity bar and the panels: the window's buttons, back and forward, the title
//! over the content, then the thread's tools. All of it but the controls drags the window.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::IconName;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::ClickHandler;
use crate::actions::{GoBack, GoForward, ToggleRightPane, ToggleTrajectory};
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

/// Back and forward through the threads you've been to (and from Settings back to the thread).
#[derive(Clone)]
pub struct TitleBarNav {
    pub can_back: bool,
    pub can_forward: bool,
    pub on_back: ClickHandler,
    pub on_forward: ClickHandler,
}

impl TitleBarNav {
    /// No-op handlers (previews).
    pub fn inert(can_back: bool, can_forward: bool) -> Self {
        let noop: ClickHandler = Rc::new(|_, _, _| {});
        Self { can_back, can_forward, on_back: noop.clone(), on_forward: noop }
    }

    fn buttons(&self) -> Div {
        let (back, fwd) = (self.on_back.clone(), self.on_forward.clone());
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                icon_button("go-back", IconName::ArrowLeft, self.can_back, false)
                    .when(self.can_back, |b| b.tip_action("Back", &GoBack).on_click(move |e, w, cx| back(e, w, cx))),
            )
            .child(
                icon_button("go-forward", IconName::ArrowRight, self.can_forward, false)
                    .when(self.can_forward, |b| b.tip_action("Forward", &GoForward).on_click(move |e, w, cx| fwd(e, w, cx))),
            )
    }
}

/// Back and forward's width (two buttons and the gap between them).
const NAV_W: f32 = 2. * 28. + 2.;
/// The title's inset from the content's left edge.
const TITLE_INSET: f32 = 8.;

/// The window's title bar: the traffic lights' room, back and forward, the title over the
/// content (it glides with the sidebar, but never under back and forward), then Open in, the
/// trajectory and the right pane.
#[derive(IntoElement)]
pub struct TitleBar {
    title: SharedString,
    nav: TitleBarNav,
    /// Where the content panel starts (window x): the title lines up just inside it.
    content_x: f32,
    /// The right pane's button: whether the pane is open, and the toggle.
    right_pane: Option<(bool, ClickHandler)>,
    /// Open the thread's folder in an app.
    open_in: Option<super::open_in::OpenIn>,
    /// The trajectory's button: whether the thread shows it in place of the chat, and the toggle.
    trajectory: Option<(bool, ClickHandler)>,
}

impl TitleBar {
    pub fn new(title: impl Into<SharedString>, nav: TitleBarNav, content_x: f32) -> Self {
        Self { title: title.into(), nav, content_x, right_pane: None, open_in: None, trajectory: None }
    }

    /// The "Open in" control at the right, left of the trajectory's button.
    pub fn open_in(mut self, open_in: super::open_in::OpenIn) -> Self {
        self.open_in = Some(open_in);
        self
    }

    /// The trajectory's button, left of the right pane's (filled while it shows).
    pub fn trajectory(mut self, shown: bool, on_toggle: ClickHandler) -> Self {
        self.trajectory = Some((shown, on_toggle));
        self
    }

    /// The right pane's button at the right (filled while the pane is open).
    pub fn right_pane(mut self, open: bool, on_toggle: ClickHandler) -> Self {
        self.right_pane = Some((open, on_toggle));
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let nav_end = f32::from(size::TRAFFIC_LIGHTS_W) + NAV_W;
        let title_x = (self.content_x + TITLE_INSET).max(nav_end + TITLE_INSET);
        drag_region(div().id("titlebar"), "titlebar", window, cx)
            .h(size::TITLE_BAR_H)
            .flex_none()
            .flex()
            .items_center()
            .pl(size::TRAFFIC_LIGHTS_W)
            .pr(size::PANEL_INSET)
            .child(self.nav.buttons())
            .child(div().flex_none().w(px(title_x - nav_end)))
            .child(div().min_w_0().flex_1().truncate().text_size(text::MD).text_color(color::text_2()).child(self.title))
            .when_some(self.open_in, |d, o| d.child(div().mr_1().child(super::open_in::open_in_control(o))))
            .when_some(self.trajectory, |d, (shown, toggle)| {
                d.child(
                    icon_button("toggle-trajectory", Lucide::TextSearch, true, shown)
                        .mr_0p5()
                        .tip_action("Trajectory", &ToggleTrajectory)
                        .on_click(move |e, w, cx| toggle(e, w, cx)),
                )
            })
            .when_some(self.right_pane, |d, (open, toggle)| {
                d.child(
                    icon_button("toggle-right-pane", Lucide::PanelRight, true, open)
                        .tip_action("Right Pane", &ToggleRightPane)
                        .on_click(move |e, w, cx| toggle(e, w, cx)),
                )
            })
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let title = "Fix the failing smoke test in space-core";
    let content_x = f32::from(size::ACTIVITY_W) + f32::from(size::SIDEBAR_W);
    let tools = move |shown: bool| {
        TitleBar::new(title, TitleBarNav::inert(true, false), content_x)
            .open_in(super::open_in::fixture())
            .trajectory(shown, Rc::new(|_, _, _| {}))
            .right_pane(false, Rc::new(|_, _, _| {}))
    };
    vec![
        ComponentPreview::new(Area::Shell, "Title bar", "Across the window; the traffic lights sit in its left inset.")
            .stacked()
            .example(Example::new("with sidebar", move |_, _| tools(false).into_any_element()).width(px(1100.)))
            .example(Example::new("trajectory shown", move |_, _| tools(true).into_any_element()).width(px(1100.)))
            .example(
                Example::new("sidebar hidden", move |_, _| {
                    TitleBar::new(title, TitleBarNav::inert(true, true), f32::from(size::ACTIVITY_W)).into_any_element()
                })
                .width(px(1100.)),
            )
            .example(
                Example::new("open in, menu", move |_, _| {
                    let o = super::open_in::fixture();
                    let menu = super::open_in::open_in_menu(&o.apps, o.on_pick.clone(), o.on_copy_path.clone());
                    div().flex().justify_end().child(menu).into_any_element()
                })
                .width(px(900.)),
            ),
    ]
}
