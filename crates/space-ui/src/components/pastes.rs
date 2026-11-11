//! Long pastes: the "Pasted text" chip (in the composer and in a sent message) and the read-only
//! view it opens.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::tip::TipExt as _;
use super::widgets::icon_button;
use crate::format;
use crate::theme::{color, radius, size, text};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
/// Opens a sent message's pasted text.
pub type OpenTextHandler = Rc<dyn Fn(space_core::TextRef, &mut Window, &mut App)>;

/// The text view's widest.
const VIEW_MAX_W: Pixels = px(760.);

/// A pasted text as a chip: a document icon, "Pasted text" and its size. A click opens it
/// (`on_open`); with `on_remove`, × removes it (in the composer).
pub fn paste_chip(id: impl Into<SharedString>, chars: u64, on_open: Option<Handler>, on_remove: Option<Handler>) -> Stateful<Div> {
    let id = id.into();
    div()
        .id(ElementId::NamedInteger(id.clone(), 0))
        .flex()
        .items_center()
        .gap_1p5()
        .h(size::CHIP_H)
        .pl_2()
        .when(on_remove.is_none(), |d| d.pr_2())
        .rounded(radius::SM)
        .bg(color::pressed())
        .text_size(text::XS)
        .text_color(color::text_2())
        .child(Icon::new(Lucide::FileText).size(size::ICON_SM))
        .child("Pasted text")
        .child(div().text_color(color::text_3()).child(format::chars_label(chars)))
        .when_some(on_open, |d, open| {
            d.cursor_pointer().hover(|s| s.bg(color::hover()).text_color(color::text())).on_click(move |_, w, cx| open(w, cx))
        })
        .when_some(on_remove, |d, remove| {
            d.pr_0p5().child(
                div()
                    .id(ElementId::NamedInteger(id, 1))
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius::XS)
                    .cursor_pointer()
                    .hover(|s| s.bg(color::hover()).text_color(color::text()))
                    .child(Icon::new(IconName::Close).size(size::ICON_XS))
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        remove(w, cx)
                    }),
            )
        })
}

/// A pasted text, read-only, in a card over the window: its size, Copy and ×. Esc, the backdrop
/// or × closes it.
pub fn text_view(body: SharedString, chars: u64, focus: &FocusHandle, on_close: Handler) -> Stateful<Div> {
    let (close, close_key, close_btn) = (on_close.clone(), on_close.clone(), on_close);
    let copy = body.clone();
    let header = div()
        .flex_none()
        .h(px(44.))
        .pl_4()
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(color::border())
        .child(Icon::new(Lucide::FileText).size(size::ICON_MD).text_color(color::text_2()))
        .child(div().text_size(text::MD).font_weight(FontWeight::MEDIUM).child("Pasted text"))
        .child(div().flex_1().text_size(text::SM).text_color(color::text_3()).child(format::chars_label(chars)))
        .child(icon_button("paste-view-copy", Lucide::Copy, true, false).tip("Copy").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
        }))
        .child(icon_button("paste-view-close", Lucide::X, true, false).tip("Close  ⎋").on_click(move |_, w, cx| close_btn(w, cx)));
    let card = div()
        .id("paste-view-card")
        .w_full()
        .max_w(VIEW_MAX_W)
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border_strong())
        .shadow_lg()
        .text_color(color::text())
        .cursor_default()
        // A click in the card doesn't close it.
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(header)
        .child(
            div()
                .id("paste-view-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_4()
                .py_3()
                .font_family(text::MONO_FONT)
                .text_size(text::SM)
                .line_height(relative(1.55))
                .text_color(color::text_2())
                .child(body),
        );
    div()
        .id("paste-view")
        .track_focus(focus)
        .absolute()
        .inset_0()
        .flex()
        .justify_center()
        .px(px(48.))
        .py(px(40.))
        .bg(rgba(0x0000008c))
        .occlude()
        .on_key_down(move |e, w, cx| {
            if e.keystroke.key == "escape" {
                close_key(w, cx)
            }
        })
        .on_click(move |_, w, cx| close(w, cx))
        .child(card)
}

/// Sample pasted text for previews.
#[cfg(feature = "dev")]
pub fn sample() -> String {
    let line = "2026-11-11T09:14:02Z ERROR space_core::host: plugin `permissions` exited (status 1) while checking tool call tool-7\n";
    line.repeat(48)
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let noop: Handler = Rc::new(|_, _| {});
    let (open, remove, close) = (noop.clone(), noop.clone(), noop);
    vec![
        ComponentPreview::new(Area::Composer, "Pasted text", "A long paste as a chip; a click opens it read-only.").stacked().examples([
            Example::new("in the composer", move |_, _| {
                div()
                    .flex()
                    .gap_1p5()
                    .child(paste_chip("p1", 5_230, Some(open.clone()), Some(remove.clone())))
                    .child(paste_chip("p2", 48_900, Some(open.clone()), Some(remove.clone())))
                    .into_any_element()
            }),
            Example::new("sent", |_, _| div().flex().child(paste_chip("p3", 5_230, None, None)).into_any_element()),
            Example::new("opened", move |window, cx| {
                let focus = window.use_keyed_state("paste-preview-focus", cx, |_, cx| cx.focus_handle()).read(cx).clone();
                div()
                    .relative()
                    .h(px(420.))
                    .child(text_view(sample().into(), 5_376, &focus, close.clone()))
                    .into_any_element()
            })
            .width(px(720.)),
        ]),
    ]
}
