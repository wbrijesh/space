//! Rendering. Everything is drawn from `theme` tokens; stock gpui-component widgets are
//! used for the textarea, popup menus, tooltips and the spinner.

mod composer;
mod sidebar;
mod thread;

use gpui::component::tooltip::Tooltip;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::app::{FocusComposer, GoBack, GoForward, NewThread, SpaceApp, ToggleLogs, ToggleSidebar};
use gpui::assets::IconName as Lucide;
use crate::{format, theme};

pub fn render_root(app: &mut SpaceApp, window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let main = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(theme::bg())
        .child(title_bar_main(app, cx))
        .child(thread::render_body(app, window, cx))
        .when(app.show_logs, |d| d.child(thread::render_logs(app, cx)));

    div()
        .id("space-root")
        .key_context("Space")
        // Focus sink: a click on anything non-focusable lands here and blurs the composer.
        .track_focus(&app.focus)
        .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(None, window, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
        .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
        .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
        .on_action(cx.listener(|this, _: &ToggleLogs, _, cx| this.toggle_logs(cx)))
        .on_action(cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)))
        .size_full()
        .flex()
        .bg(theme::bg())
        .text_color(theme::text())
        .font_family(theme::UI_FONT)
        .text_size(theme::TEXT_MD)
        .when(app.sidebar_open, |d| d.child(sidebar::render(app, window, cx)))
        .child(main)
}

/// Makes `el` a window drag region: drag moves the window, double click zooms/minimizes
/// (per the macOS setting). Controls inside stop mouse-down propagation.
fn drag_region(el: Stateful<Div>, cx: &mut Context<SpaceApp>) -> Stateful<Div> {
    el.on_mouse_down(
        MouseButton::Left,
        cx.listener(|this, e: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            if e.click_count >= 2 {
                this.drag_pending = false;
                window.titlebar_double_click();
            } else {
                this.drag_pending = true;
            }
        }),
    )
    .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.drag_pending = false))
    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, _| {
        if this.drag_pending {
            this.drag_pending = false;
            if e.pressed_button == Some(MouseButton::Left) {
                window.start_window_move();
            }
        }
    }))
}

/// Toggle sidebar, back/forward, new thread. Shown over the sidebar, or at the start of
/// the main title bar while the sidebar is collapsed.
fn nav_buttons(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let can_back = app.back.iter().any(|k| app.thread(*k).is_some());
    let can_fwd = app.forward.iter().any(|k| app.thread(*k).is_some());
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .child(
            icon_button("toggle-sidebar", IconName::PanelLeft, true, false)
                .tooltip(tip_action(if app.sidebar_open { "Hide sidebar" } else { "Show sidebar" }, &ToggleSidebar))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
        )
        .child(
            icon_button("go-back", IconName::ArrowLeft, can_back, false)
                .when(can_back, |b| {
                    b.tooltip(tip_action("Back", &GoBack))
                        .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx)))
                }),
        )
        .child(
            icon_button("go-forward", IconName::ArrowRight, can_fwd, false)
                .when(can_fwd, |b| {
                    b.tooltip(tip_action("Forward", &GoForward))
                        .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx)))
                }),
        )
}

fn new_thread_button(cx: &mut Context<SpaceApp>) -> impl IntoElement {
    icon_button("new-thread", Lucide::SquarePen, true, false)
        .tooltip(tip_action("New thread", &NewThread))
        .on_click(cx.listener(|this, _, window, cx| this.new_thread(None, window, cx)))
}

/// Title bar segment over the sidebar column.
fn title_bar_sidebar(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    drag_region(div().id("titlebar-sidebar"), cx)
        .h(theme::TITLE_BAR_H)
        .flex_none()
        .flex()
        .items_center()
        .pl(theme::TRAFFIC_LIGHTS_W)
        .pr_2()
        .child(nav_buttons(app, cx))
        .child(div().flex_1())
        .child(new_thread_button(cx))
}

/// Title bar over the main column: thread title, folder and path.
fn title_bar_main(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let thread = app.current();
    let title = thread.map(|t| t.title()).unwrap_or_default();
    let path = thread.map(|t| format::abbreviate_home(&t.cwd)).unwrap_or_default();
    drag_region(div().id("titlebar-main"), cx)
        .h(theme::TITLE_BAR_H)
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .border_b_1()
        .border_color(theme::border())
        .when(!app.sidebar_open, |d| {
            d.pl(theme::TRAFFIC_LIGHTS_W).child(nav_buttons(app, cx)).child(new_thread_button(cx)).child(div().w_2())
        })
        .child(Icon::new(IconName::Folder).size(px(15.)).text_color(theme::text_3()))
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .text_size(theme::TEXT_MD)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .font_family(theme::MONO_FONT)
                .text_size(theme::TEXT_XS)
                .text_color(theme::text_3())
                .child(path),
        )
        .child(div().flex_1())
}

// ----- widgets ---------------------------------------------------------------------------

/// 28px square icon button. Mouse-clicks never focus it, so no focus ring sticks.
pub(crate) fn icon_button(id: impl Into<ElementId>, icon: impl Into<Icon>, enabled: bool, active: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .text_color(if enabled { theme::text_2() } else { theme::text_3().opacity(0.5) })
        .when(active, |d| d.bg(theme::hover()).text_color(theme::text()))
        .when(enabled, |d| {
            d.hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                .active(|s| s.bg(theme::pressed()))
        })
        // Keep clicks from reaching drag regions / the focus sink.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(Icon::new(icon).size(px(16.)))
}

pub(crate) fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

pub(crate) fn tip_action(
    text: impl Into<SharedString>,
    action: &dyn Action,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    let action = action.boxed_clone();
    move |window, cx| Tooltip::new(text.clone()).action(action.as_ref(), None).build(window, cx)
}

pub(crate) fn status_dot(color: Hsla) -> Div {
    div().size(px(7.)).flex_none().rounded_full().bg(color)
}
