//! Shell pieces: sidebar column, banners, empty-state heading.

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::widgets::{icon_button, pill_button};
use crate::ClickHandler;
use crate::theme::{color, radius, size, text};

/// Sidebar column: title bar segment and the scrolling thread list.
pub fn sidebar_column(title_bar: impl IntoElement, rows: Vec<AnyElement>) -> Div {
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
                .child(div().flex().flex_col().gap_0p5().px_2().pt_1().pb_4().children(rows)),
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
/// a thread to Full access): a yellow shield, the message, Cancel and the confirming action.
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

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let noop: ClickHandler = std::rc::Rc::new(|_, _, _| {});
    vec![
        ComponentPreview::new(Area::Shell, "Banners", "Problems and notices above the composer.").stacked().examples([
            Example::new("agent problem", move |_, _| {
                problem_card("The agent exited: exited with code 3: model request failed", Some(noop.clone()))
                    .into_any_element()
            })
            .width(size::CONTENT_MAX_W),
            Example::new("notice", |_, _| {
                banner("Agent plugin `foo` not found; using `pi-agent`.", |_, _, _| {}).into_any_element()
            })
            .width(size::CONTENT_MAX_W),
            Example::new("confirm a mode (Full access)", |_, _| {
                confirm_card(
                    "Full access lets the agent change files anywhere on this Mac. A decision model still checks every \
                     operation first.",
                    "Use Full access",
                    |_, _, _| {},
                    |_, _, _| {},
                )
                .into_any_element()
            })
            .width(size::CONTENT_MAX_W),
        ]),
        ComponentPreview::new(Area::Shell, "Empty state", "New thread: heading over the centered composer.")
            .example(Example::new("heading", |_, _| empty_heading().into_any_element()).width(size::CONTENT_MAX_W)),
    ]
}
