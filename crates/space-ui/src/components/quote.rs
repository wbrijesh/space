//! Quoting the transcript: the floating "Quote" button over a selection, the quote in the
//! composer, and the quote above a sent message.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::theme::{color, radius, size, text};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The floating button's height.
pub const BUTTON_H: Pixels = px(28.);

/// A quote's left bar.
fn bar() -> Div {
    div().flex_none().w(px(2.)).self_stretch().rounded_full().bg(color::border_strong())
}

/// The button that floats over a selection in the transcript. A press on it clears the
/// selection, so the app keeps what it quotes beforehand.
pub fn quote_button() -> Stateful<Div> {
    div()
        .id("quote-selection")
        .occlude()
        .h(BUTTON_H)
        .pl_2()
        .pr_2p5()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::LG)
        .bg(color::surface())
        .border_1()
        .border_color(color::border_strong())
        .shadow_md()
        .text_size(text::SM)
        .font_weight(FontWeight::MEDIUM)
        .text_color(color::text())
        .cursor_pointer()
        .hover(|s| s.bg(color::hover()))
        .child(Icon::new(Lucide::TextQuote).size(size::ICON_MD))
        .child("Quote")
}

/// The quote in the composer card, above the input: a bar, at most two lines, × to remove it.
pub fn composer_quote(quote: &str, on_remove: Handler) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2()
        .px_1()
        .py_1()
        .child(bar())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(text::SM)
                .line_height(relative(1.45))
                .text_color(color::text_2())
                .text_ellipsis()
                .line_clamp(2)
                .child(one_paragraph(quote)),
        )
        .child(
            div()
                .id("remove-quote")
                .flex_none()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius::XS)
                .text_color(color::text_3())
                .cursor_pointer()
                .hover(|s| s.bg(color::hover()).text_color(color::text()))
                .child(Icon::new(IconName::Close).size(size::ICON_MD))
                .on_click(move |_, w, cx| on_remove(w, cx)),
        )
}

/// A sent message's quote, quietly above its bubble: a bar and two lines; a click shows all of
/// it (and folds it again).
#[derive(IntoElement)]
pub struct SentQuote {
    id: SharedString,
    text: SharedString,
}

impl SentQuote {
    pub fn new(id: impl Into<SharedString>, text: impl Into<SharedString>) -> Self {
        Self { id: id.into(), text: text.into() }
    }
}

impl RenderOnce for SentQuote {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open = window.use_keyed_state((ElementId::Name(self.id.clone()), "open"), cx, |_, _| false);
        let shown = *open.read(cx);
        let body = if shown { self.text.to_string() } else { one_paragraph(&self.text) };
        div()
            .id(ElementId::Name(self.id))
            .flex()
            .gap_2()
            .text_size(text::SM)
            .line_height(relative(1.45))
            .text_color(color::text_3())
            .cursor_pointer()
            .hover(|s| s.text_color(color::text_2()))
            .child(bar())
            .child(div().min_w_0().when(!shown, |d| d.text_ellipsis().line_clamp(2)).child(body))
            .on_click(move |_, _, cx| open.update(cx, |o, cx| {
                *o = !*o;
                cx.notify();
            }))
    }
}

/// A quote folded to flowing text (its line breaks as spaces), for the two-line views.
fn one_paragraph(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    const QUOTE: &str = "The guardrail asks when it is less than 80% sure, so an \"allow at 64%\" verdict still \
                         asks you. Raising the folder from / to the project made it confident again.";
    let remove: Handler = Rc::new(|_, _| {});
    vec![ComponentPreview::new(Area::Transcript, "Quote", "Quoting a selection: the button over it, the quote above a sent message.")
        .stacked()
        .examples([
            Example::new("button", move |_, _| div().flex().child(quote_button()).into_any_element()),
            Example::new("in the composer", move |_, _| composer_quote(QUOTE, remove.clone()).into_any_element()).width(px(560.)),
            Example::new("sent (click to open)", |_, _| SentQuote::new("preview-quote", QUOTE).into_any_element()).width(px(420.)),
        ])]
}
