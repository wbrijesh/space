//! Small building blocks: icon buttons, pill buttons, status dots, menu triggers.
//!
//! Clickable chrome is a plain styled `div` (no focus handle): mouse clicks never focus it,
//! so no focus state can stick after a click.

use gpui::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui::component::{FocusableExt as _, Icon, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::theme::{color, radius, size, text};

/// Square icon button (28px). `active` shows the pressed fill (e.g. a toggled panel).
pub fn icon_button(id: impl Into<ElementId>, icon: impl Into<Icon>, enabled: bool, active: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .size(size::ICON_BUTTON)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::MD)
        .text_color(if enabled { color::text_2() } else { color::text_3().opacity(0.5) })
        .when(active, |d| d.bg(color::hover()).text_color(color::text()))
        .when(enabled, |d| {
            d.hover(|s| s.bg(color::hover()).text_color(color::text())).active(|s| s.bg(color::pressed()))
        })
        // Keep clicks from reaching drag regions / the window's focus sink.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(Icon::new(icon).size(size::ICON_MD))
}

/// 28px rounded text button: filled accent (`primary`) or quiet outline.
pub fn pill_button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_none()
        .h(size::ICON_BUTTON)
        .px_3()
        .flex()
        .items_center()
        .rounded(radius::LG)
        .text_size(text::SM)
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.bg(color::accent()).text_color(color::on_accent()).hover(|s| s.bg(color::accent_hover()))
            } else {
                d.border_1()
                    .border_color(color::border_strong())
                    .text_color(color::text())
                    .hover(|s| s.bg(color::hover()))
            }
        })
        .active(|s| s.opacity(0.85))
        .child(label.into())
}

pub fn status_dot(color: Hsla) -> Div {
    div().size(size::DOT).flex_none().rounded_full().bg(color)
}

/// Quiet button used as a dropdown trigger in the composer's control row: the same look as the
/// model picker (a hover fill, no pressed flash).
pub fn menu_trigger(id: impl Into<ElementId>, window: &Window, cx: &App) -> Button {
    let variant = ButtonCustomVariant::new(cx)
        .color(transparent_black())
        .foreground(color::text_2())
        .hover(color::hover())
        .active(color::hover());
    Button::new(id)
        .custom(variant)
        .small()
        // Menus hand focus back to their trigger on close; only show a ring for keyboard users.
        .focus_ring(window.last_input_was_keyboard())
        .pl_2()
        .pr_1()
        .rounded(radius::MD)
        .text_size(text::SM)
}
