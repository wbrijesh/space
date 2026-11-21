//! Shell pieces: sidebar column, banners, empty-state heading.

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::widgets::{icon_button, pill_button};
use crate::ClickHandler;
use crate::theme::{color, radius, size, text};

/// Sidebar column: title bar segment and the scrolling thread list. The rows are the list's
/// direct children, so `scroll` can tell which are in view.
pub fn sidebar_column(title_bar: impl IntoElement, rows: Vec<AnyElement>, scroll: &ScrollHandle) -> Div {
    div()
        .flex()
        .flex_col()
        .w(size::SIDEBAR_W)
        .flex_none()
        .h_full()
        .bg(color::bg_sidebar())
        .border_r_1()
        .border_color(color::border())
        .child(title_bar)
        .child(
            div()
                .id("thread-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .flex()
                .flex_col()
                .gap_0p5()
                .px_2()
                .pt_1()
                .pb_4()
                .children(rows),
        )
}

/// Quiet info banner with a dismiss button.
pub fn banner(message: impl Into<SharedString>, on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .text_size(text::SM)
        .text_color(color::text_2())
        .child(Icon::new(IconName::Info).size(size::ICON).text_color(color::text_3()))
        .child(div().flex_1().min_w_0().child(message.into()))
        .child(icon_button("dismiss-banner", IconName::Close, true, false).on_click(on_dismiss))
}

/// Red card for an agent problem, with an optional Restart action.
pub fn problem_card(message: impl Into<SharedString>, on_restart: Option<ClickHandler>) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2p5()
        .p_3()
        .rounded(radius::XL)
        .bg(color::red_bg())
        .border_1()
        .border_color(color::red_border())
        .child(Icon::new(IconName::CircleAlert).size(size::ICON_MD).text_color(color::red()).mt(px(1.)))
        .child(div().flex_1().min_w_0().text_size(text::SM).line_clamp(4).child(message.into()))
        .when_some(on_restart, |d, restart| {
            d.child(pill_button("problem-restart", "Restart", false).on_click(move |e, w, cx| restart(e, w, cx)))
        })
}

/// A question above the composer that needs an answer before something happens (e.g. switching
/// a thread to a mode that asks first): a yellow shield, the message, Cancel and the confirming action.
pub fn confirm_card(
    message: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .p_3()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::yellow_border())
        .child(Icon::new(Lucide::ShieldAlert).size(size::ICON_MD).text_color(color::yellow()))
        .child(div().flex_1().min_w_0().text_size(text::SM).child(message.into()))
        .child(pill_button("confirm-cancel", "Cancel", false).on_click(on_cancel))
        .child(pill_button("confirm-ok", confirm_label, true).on_click(on_confirm))
}

/// Heading above the centered composer of an empty thread.
pub fn empty_heading() -> Div {
    div()
        .flex()
        .justify_center()
        .text_size(text::TITLE)
        .font_weight(FontWeight::SEMIBOLD)
        .child("What should we work on?")
}

/// Over the chat when a sidebar or pane opened by hand leaves it narrower than
/// [`size::CHAT_MIN_W`]: what's wrong, and a button to hide each one that's open.
pub fn too_narrow(on_hide_sidebar: Option<ClickHandler>, on_hide_pane: Option<ClickHandler>) -> Div {
    let button = |id: &'static str, label: &'static str, handler: ClickHandler| {
        pill_button(id, label, false).on_click(move |e, w, cx| handler(e, w, cx))
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .px_6()
        .text_center()
        // Opaque: the squeezed chat under it is unreadable anyway, and showing through it would
        // make this hard to read. Clicks stay on the overlay; scrolls pass, so a two-finger swipe
        // still shows and hides a pane.
        .bg(color::bg())
        .block_mouse_except_scroll()
        .child(div().text_size(text::MD).font_weight(FontWeight::MEDIUM).text_color(color::text()).child("Not enough room for the chat"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_2()
                .children(on_hide_sidebar.map(|h| button("narrow-hide-sidebar", "Hide sidebar", h)))
                .children(on_hide_pane.map(|h| button("narrow-hide-pane", "Hide right pane", h))),
        )
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let noop: ClickHandler = std::rc::Rc::new(|_, _, _| {});
    vec![
        ComponentPreview::new(Area::Shell, "Banners", "Problems and notices above the composer.").stacked().examples([
            Example::new("agent problem", {
                let noop = noop.clone();
                move |_, _| {
                    problem_card("The agent exited: exited with code 3: model request failed", Some(noop.clone()))
                        .into_any_element()
                }
            })
            .width(size::CONTENT_MAX_W),
            Example::new("notice", |_, _| {
                banner("Agent plugin `foo` not found; using `claude-agent`.", |_, _, _| {}).into_any_element()
            })
            .width(size::CONTENT_MAX_W),
            Example::new("confirm a mode", |_, _| {
                confirm_card(
                    "Trusted lets the agent change files anywhere on this Mac without asking.",
                    "Use Trusted",
                    |_, _, _| {},
                    |_, _, _| {},
                )
                .into_any_element()
            })
            .width(size::CONTENT_MAX_W),
        ]),
        ComponentPreview::new(Area::Shell, "Too narrow", "Over the chat when the sidebar or pane leaves it under 600 px.").example(
            Example::new("both open", {
                let noop = noop.clone();
                move |_, _| div().relative().w(px(420.)).h(px(260.)).child(too_narrow(Some(noop.clone()), Some(noop.clone()))).into_any_element()
            }),
        ),
        ComponentPreview::new(Area::Shell, "Empty state", "New thread: heading over the centered composer.")
            .example(Example::new("heading", |_, _| empty_heading().into_any_element()).width(size::CONTENT_MAX_W)),
    ]
}
