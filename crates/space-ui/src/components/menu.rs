//! Space's menus: a quiet trigger (a hover fill, no pressed flash) and the menu it opens above
//! itself, with 32px rows, a heading, separators and a check on the right. The composer's
//! folder, permission-mode and model menus are built from these, so they look and behave the
//! same. The app keeps which menu is open; a click outside or Esc closes it.

use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::theme::{color, radius, size, text};

pub const ROW_H: Pixels = px(32.);

/// The trigger: content goes in as children, the chevron last. `open` keeps the hover fill;
/// a disabled trigger is dimmed and inert.
pub fn trigger(id: impl Into<ElementId>, open: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .relative()
        .flex_none()
        .h(px(24.))
        .pl_2()
        .pr_1()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::MD)
        .text_size(text::SM)
        .text_color(color::text_2())
        .map(|d| {
            if enabled {
                d.cursor_pointer().hover(|s| s.bg(color::hover())).when(open, |d| d.bg(color::hover()))
            } else {
                d.opacity(0.5)
            }
        })
}

pub fn chevron() -> Icon {
    Icon::new(IconName::ChevronDown).size(size::ICON_SM).text_color(color::text_3())
}

/// `menu` above its trigger (add as the trigger's child), aligned to the trigger's left or
/// right edge, kept inside the window. A mouse-down outside it calls `close`; the trigger's own
/// click should toggle.
pub fn popover(right: bool, menu: Div, close: impl Fn(&mut Window, &mut App) + 'static) -> Div {
    let anchor = if right { Anchor::BottomRight } else { Anchor::BottomLeft };
    div().absolute().top_0().map(|d| if right { d.right_0() } else { d.left_0() }).child(deferred(
        anchored()
            .anchor(anchor)
            .snap_to_window_with_margin(px(8.))
            .child(div().pb_1p5().child(menu.on_mouse_down_out(move |_, w, cx| close(w, cx)))),
    ))
}

/// The menu's card.
pub fn surface() -> Div {
    div()
        .min_w(px(240.))
        .max_w(px(360.))
        .p_1()
        .flex()
        .flex_col()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border_strong())
        .shadow_lg()
        .text_size(text::MD)
        .text_color(color::text())
        .cursor_default()
}

/// A clickable row (hover fill).
pub fn row(id: impl Into<ElementId>) -> Stateful<Div> {
    row_with(id, |s| s.bg(color::hover()))
}

/// A clickable row with its own hover style.
pub fn row_with(id: impl Into<ElementId>, hover: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(ROW_H)
        .px_2p5()
        .flex()
        .items_center()
        .gap_3()
        .rounded(radius::LG)
        .cursor_pointer()
        .hover(hover)
        // Keep the click from reaching what's under the menu.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A choice: an optional icon, the label, and a check on the right when it's the current one.
pub fn choice(id: impl Into<ElementId>, icon: Option<AnyElement>, label: impl Into<SharedString>, checked: bool) -> Stateful<Div> {
    row(id)
        .children(icon.map(|i| div().flex_none().flex().items_center().text_color(color::text_2()).child(i)))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(div().w(size::ICON).flex_none().when(checked, |d| d.child(Icon::new(IconName::Check).size(size::ICON))))
}

/// A quiet heading over a group of rows.
pub fn heading(label: impl Into<SharedString>) -> Div {
    div().h(px(28.)).px_2p5().flex().items_center().text_size(text::SM).text_color(color::text_3()).child(label.into())
}

pub fn separator() -> Div {
    div().my_1().mx_neg_1().h(px(1.)).bg(color::border())
}
