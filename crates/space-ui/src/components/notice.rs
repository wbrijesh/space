//! A notice in the window's bottom-right corner, over everything (the composer too): what just
//! happened, in a sentence, with Undo when it can be undone. It goes by itself after a moment,
//! unless it lists something to look at; × closes it.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::Icon;
use gpui::*;

use crate::theme::{color, radius, size, text};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The notice's width.
pub const WIDTH: Pixels = px(360.);
/// Its distance from the window's edges.
pub const MARGIN: Pixels = px(12.);

pub struct Notice {
    pub icon: Icon,
    /// The first sentence (what happened).
    pub title: SharedString,
    /// What else to know, if anything.
    pub body: Option<SharedString>,
    /// Things left as they are: a name and why.
    pub kept: Vec<(SharedString, SharedString)>,
    /// A problem, not news: the icon is red.
    pub error: bool,
    /// Undo, with the key that does it too (`⌘Z`).
    pub undo: Option<(SharedString, Handler)>,
    pub on_close: Handler,
}

pub fn notice(n: Notice) -> Stateful<Div> {
    let Notice { icon, title, body, kept, error, undo, on_close } = n;
    let undo = undo.map(|(keys, on_undo)| {
        div()
            .id("notice-undo")
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(24.))
            .px_2()
            .rounded(radius::MD)
            .text_color(color::text())
            .cursor_pointer()
            .hover(|s| s.bg(color::hover()))
            .on_click(move |_, w, cx| on_undo(w, cx))
            .child("Undo")
            .child(div().text_color(color::text_3()).child(keys))
    });
    let close = div()
        .id("notice-close")
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::SM)
        .text_color(color::text_3())
        .cursor_pointer()
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .on_click(move |_, w, cx| on_close(w, cx))
        .child(Icon::new(Lucide::X).size(size::ICON_SM));
    let kept = (!kept.is_empty()).then(|| {
        div()
            .pt_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(color::text_2()).child("Kept as they are:"))
            .children(kept.into_iter().map(|(name, why)| {
                div()
                    .flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().truncate().text_color(color::text()).font_family(text::MONO_FONT).text_size(text::SM).child(name))
                    .child(div().flex_none().text_color(color::text_3()).child(why))
            }))
    });
    div()
        .id("notice")
        .occlude()
        .w(WIDTH)
        .flex()
        .gap_3()
        .p_3()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .shadow_lg()
        .text_size(text::MD)
        .line_height(relative(text::LEADING))
        .cursor_default()
        .child(div().flex_none().pt(px(2.)).text_color(if error { color::red() } else { color::text_2() }).child(icon.size(size::ICON)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().flex().items_start().gap_2().child(div().flex_1().min_w_0().child(title)).child(close))
                .children(body.map(|b| div().text_color(color::text_2()).child(b)))
                .children(kept)
                .children(undo.map(|u| div().pt_1().flex().justify_end().child(u))),
        )
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    fn base(icon: Icon, title: &'static str, body: Option<&'static str>) -> Notice {
        Notice {
            icon,
            title: title.into(),
            body: body.map(Into::into),
            kept: Vec::new(),
            error: false,
            undo: Some(("⌘Z".into(), Rc::new(|_, _| {}))),
            on_close: Rc::new(|_, _| {}),
        }
    }
    vec![ComponentPreview::new(Area::Transcript, "Notice", "Bottom right of the window: what just happened, with Undo.").stacked().examples([
        Example::new("rewound", move |_, _| notice(base(Icon::new(Lucide::RotateCcw), "Rewound.", Some("Claude's file changes after this point were undone."))).into_any_element()),
        Example::new("edited", move |_, _| notice(base(Icon::new(Lucide::Pencil), "Edited.", Some("Claude's file changes after it were undone."))).into_any_element()),
        Example::new("files kept", move |_, _| {
            let mut n = base(Icon::new(Lucide::RotateCcw), "Rewound.", Some("Claude's file changes after this point were undone."));
            n.kept = vec![("src/app.rs".into(), "you changed it since".into()), ("docs/notes.md".into(), "written by a subagent".into())];
            notice(n).into_any_element()
        }),
        Example::new("couldn't", move |_, _| {
            let mut n = base(Icon::new(gpui::component::IconName::TriangleAlert), "Couldn't go back to that message.", Some("Claude's conversation doesn't have it any more."));
            (n.error, n.undo) = (true, None);
            notice(n).into_any_element()
        }),
    ])]
}
