//! Space's menus: a quiet trigger (a hover fill, no pressed flash) and the menu it opens above
//! itself, with 32px rows, a heading, separators and a check on the right. The composer's
//! folder, permission-mode and model menus are built from these, so they look and behave the
//! same. The app keeps which menu is open; a click outside or Esc closes it.
//!
//! [`context_menu`] is the right-click menu, in the same card and rows, with each item's
//! shortcut on the right as macOS menus show it.

use std::rc::Rc;

use gpui::component::kbd::Kbd;
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

/// The menu's card. It occludes what's under it, so hovering or clicking it never reaches
/// the rows or buttons behind (a menu floats over them).
pub fn surface() -> Div {
    div()
        .occlude()
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

/// One item of a [`context_menu`]: its label, what picking it does, and optionally the action
/// whose key binding shows on the right. The action only names the shortcut (read from the
/// key bindings, so it can't drift); picking the item runs `on_pick`.
pub struct ContextItem {
    label: SharedString,
    action: Option<Box<dyn Action>>,
    on_pick: PickHandler,
    /// Shown dimmed, and picking it does nothing.
    disabled: bool,
}

/// What picking a context menu item does.
pub type PickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

impl ContextItem {
    pub fn new(label: impl Into<SharedString>, on_pick: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { label: label.into(), action: None, on_pick: Rc::new(on_pick), disabled: false }
    }

    /// Dim the item and make it inert (it can't apply here).
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Show `action`'s key binding beside the label (nothing when it has none).
    pub fn shortcut(mut self, action: &dyn Action) -> Self {
        self.action = Some(action.boxed_clone());
        self
    }
}

/// Where an open context menu is, the focus it took (for Esc) and the focus it took it from.
struct ContextMenuState {
    at: Option<Point<Pixels>>,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
}

/// Give `el` a right-click menu of `items`: Space's menu card at the pointer, at least
/// [`size::CONTEXT_MENU_W`] wide, compact rows with the shortcut right-aligned and dimmed, a
/// clearly lighter highlight under the pointer. Picking an
/// item, Esc or a click outside closes it. `key` names the menu's state (unique per element).
pub fn context_menu(el: Stateful<Div>, key: impl Into<SharedString>, items: Vec<ContextItem>, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let key: SharedString = format!("context-menu-{}", key.into()).into();
    let state = window.use_keyed_state(key, cx, |_, cx| ContextMenuState { at: None, focus: cx.focus_handle(), previous: None });
    let open_at = state.read(cx).at;
    let opener = state.clone();
    el.on_mouse_down(MouseButton::Right, move |e, window, cx| {
        cx.stop_propagation();
        let previous = window.focused(cx);
        let focus = opener.update(cx, |s, cx| {
            s.at = Some(e.position);
            s.previous = previous;
            cx.notify();
            s.focus.clone()
        });
        window.focus(&focus, cx);
    })
    .when_some(open_at, |d, at| {
        let card = context_menu_card(&items, Some(state), None, window, cx);
        d.child(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(card)).with_priority(1))
    })
}

/// Close the menu and give focus back to where it was.
fn close_context_menu(state: &Entity<ContextMenuState>, window: &mut Window, cx: &mut App) {
    let previous = state.update(cx, |s, cx| {
        s.at = None;
        cx.notify();
        s.previous.take()
    });
    if let Some(previous) = previous {
        window.focus(&previous, cx);
    }
}

/// The open menu's card. `state`: the menu it belongs to (`None` in previews: nothing closes);
/// `highlight`: an item drawn as if the pointer were on it (previews).
fn context_menu_card(
    items: &[ContextItem],
    state: Option<Entity<ContextMenuState>>,
    highlight: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let rows = items.iter().enumerate().map(|(ix, item)| {
        // macOS spaces a shortcut's glyphs apart (⌘ R), which reads easier at a small size.
        let keys = item
            .action
            .as_ref()
            .and_then(|a| window.highest_precedence_binding_for_action(a.as_ref()))
            .and_then(|b| b.keystrokes().first().map(|k| Kbd::format(k.as_keystroke())))
            .map(|k| k.chars().map(String::from).collect::<Vec<_>>().join("\u{200A}"));
        let (pick, state) = (item.on_pick.clone(), state.clone());
        div()
            .id(("context-item", ix))
            .h(size::CONTEXT_ROW_H)
            .px(px(10.))
            .flex()
            .items_center()
            .gap_4()
            .rounded(radius::SM)
            // Keep the click from reaching what's under the menu.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
            .children(keys.map(|k| div().flex_none().text_color(color::text_3()).child(k)))
            .map(|d| {
                if item.disabled {
                    d.text_color(color::text_3()).cursor_default()
                } else {
                    d.cursor_pointer()
                        .hover(|s| s.bg(color::menu_highlight()))
                        .when(highlight == Some(ix), |d| d.bg(color::menu_highlight()))
                        .on_click(move |_, window, cx| {
                            if let Some(state) = &state {
                                close_context_menu(state, window, cx);
                            }
                            pick(window, cx);
                        })
                }
            })
            .into_any_element()
    });
    let card = surface().min_w(size::CONTEXT_MENU_W).p(px(5.)).text_size(text::SM).children(rows);
    match state {
        Some(state) => {
            let focus = state.read(cx).focus.clone();
            let (esc, out) = (state.clone(), state);
            card.key_context("ContextMenu")
                .track_focus(&focus)
                .on_key_down(move |e, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        close_context_menu(&esc, window, cx);
                    }
                })
                .on_mouse_down_out(move |_, window, cx| close_context_menu(&out, window, cx))
        }
        None => card,
    }
}

/// A context menu drawn open, for previews, with item `highlight` under the pointer.
#[cfg(feature = "dev")]
pub fn context_menu_preview(items: &[ContextItem], highlight: Option<usize>, window: &mut Window, cx: &mut App) -> Div {
    context_menu_card(items, None, highlight, window, cx)
}
