//! A file's tab in the right pane: its header (its path, the viewer choice, wrap, Open in); and the states a
//! preview can be in.

use std::path::Path;
use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::Icon;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::file_tree::file_icon;
use super::tip::TipExt as _;
use super::widgets::{icon_button, pill_button};
use crate::theme::{color, radius, text};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type TabHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// One of a file's views (Preview, Changes, Full file…): its label, shown or not, and picking it.
#[derive(Clone)]
pub struct ViewChoice {
    pub label: SharedString,
    pub on: bool,
    pub pick: Handler,
}

/// What a file's header offers.
#[derive(Clone)]
pub struct FileHeader {
    /// The path shown: relative to the thread's folder, else absolute.
    pub path: SharedString,
    /// Lines added and removed since the last commit (a changed file).
    pub changes: Option<(usize, usize)>,
    /// The views to switch between (none, or two or more).
    pub views: Vec<ViewChoice>,
    /// The text wraps, and the toggle (text viewers only).
    pub wrap: Option<(bool, BoolHandler)>,
    pub open_label: SharedString,
    pub on_open: Handler,
}

/// The path, folders quiet and the name bright, clipped from the left so the name stays.
fn path_label(path: &str) -> Div {
    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) => (format!("{dir}/"), name.to_string()),
        None => (String::new(), path.to_string()),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_end()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .flex()
                .text_size(text::SM)
                .whitespace_nowrap()
                .child(div().text_color(color::text_3()).child(dir))
                .child(div().text_color(color::text()).child(name)),
        )
}

/// A file's header, above its preview.
pub fn file_header(h: FileHeader) -> Div {
    let FileHeader { path, changes, views, wrap, open_label, on_open } = h;
    div()
        .h(px(34.))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .pl_3()
        .pr_2()
        .border_b_1()
        .border_color(color::border())
        // The path fills what's left, starting at the left (`justify_end` keeps the name when
        // it's clipped).
        .child(div().flex_1().min_w_0().flex().child(div().max_w_full().flex().child(path_label(&path))))
        .when_some(changes, |d, (added, removed)| {
            d.child(
                div()
                    .flex_none()
                    .mr_1()
                    .flex()
                    .gap_1()
                    .text_size(text::XS)
                    .font_family(text::MONO_FONT)
                    .child(div().text_color(color::green()).child(format!("+{added}")))
                    .child(div().text_color(color::red()).child(format!("\u{2212}{removed}"))),
            )
        })
        .when(views.len() > 1, |d| {
            d.child(
                div()
                    .flex_none()
                    .h(px(22.))
                    .p(px(2.))
                    .flex()
                    .gap(px(2.))
                    .rounded(radius::MD)
                    .bg(color::surface())
                    .border_1()
                    .border_color(color::border())
                    .children(views.into_iter().enumerate().map(|(ix, v)| segment(ix, v))),
            )
        })
        .when_some(wrap, |d, (on, toggle)| {
            d.child(icon_button("viewer-wrap", Lucide::TextWrap, true, on).tip(if on { "Don't wrap lines" } else { "Wrap lines" }).on_click(move |_, w, cx| toggle(!on, w, cx)))
        })
        .child(icon_button("viewer-open-in", Lucide::ExternalLink, true, false).tip(open_label).on_click(move |_, w, cx| on_open(w, cx)))
}

fn segment(ix: usize, v: ViewChoice) -> Stateful<Div> {
    let ViewChoice { label, on, pick } = v;
    div()
        .id(ElementId::NamedInteger("viewer-view".into(), ix as u64))
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .rounded(radius::SM)
        .text_size(text::XS)
        .map(|d| if on { d.bg(color::hover()).text_color(color::text()) } else { d.text_color(color::text_3()) })
        .when(!on, |d| d.cursor_pointer().hover(|s| s.text_color(color::text())))
        .child(label)
        .on_click(move |_, w, cx| pick(w, cx))
}

/// Instead of a preview: why there's none, with Open in (and Retry for a failed read).
pub fn unpreviewable(name: &str, message: impl Into<SharedString>, open_label: SharedString, on_open: Handler, on_retry: Option<Handler>) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .p_6()
        .child(Icon::new(file_icon(name)).size(px(28.)).text_color(color::text_3()))
        .child(div().text_size(text::SM).text_color(color::text_2()).text_center().child(message.into()))
        .child(
            div()
                .flex()
                .gap_2()
                .child(pill_button("unpreviewable-open", open_label, false).on_click(move |_, w, cx| on_open(w, cx)))
                .when_some(on_retry, |d, retry| d.child(pill_button("unpreviewable-retry", "Retry", false).on_click(move |_, w, cx| retry(w, cx)))),
        )
}

/// An image, scaled down to fit the pane (never enlarged), centred. `size`: its pixels, which
/// give it its box (as the image viewer does); without it, it fills the pane.
pub fn image_preview(path: &Path, size: Option<(u64, u64)>) -> Div {
    let pic = img(path.to_path_buf())
        .max_w_full()
        .max_h_full()
        .object_fit(ObjectFit::ScaleDown)
        .map(|i| match size {
            Some((w, h)) => i.w(px(w as f32)).h(px(h as f32)),
            None => i.size_full(),
        })
        .with_fallback(|| div().p_4().text_size(text::SM).text_color(color::text_3()).child("This image can't be shown.").into_any_element());
    div().flex_1().min_w_0().min_h_0().p_3().flex().items_center().justify_center().overflow_hidden().child(pic)
}

/// While a file is read.
pub fn loading() -> Div {
    div().flex_1().flex().items_center().justify_center().text_size(text::SM).text_color(color::text_3()).child("Reading…")
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let noop: Handler = Rc::new(|_, _| {});
    let toggle: BoolHandler = Rc::new(|_, _, _| {});
    let (n1, n2, n3) = (noop.clone(), noop.clone(), noop);
    let b2 = toggle;
    let pick: Handler = Rc::new(|_, _| {});
    let header = move |path: &'static str, changed: bool| FileHeader {
        path: path.into(),
        changes: changed.then_some((12, 3)),
        views: if changed {
            vec![
                ViewChoice { label: "Preview".into(), on: false, pick: pick.clone() },
                ViewChoice { label: "Changes".into(), on: true, pick: pick.clone() },
                ViewChoice { label: "Full file".into(), on: false, pick: pick.clone() },
            ]
        } else {
            Vec::new()
        },
        wrap: Some((false, b2.clone())),
        open_label: "Open in Zed".into(),
        on_open: n1.clone(),
    };
    let pane = |child: Div| div().w(px(620.)).h(px(260.)).flex().flex_col().bg(color::bg()).border_1().border_color(color::border()).child(child);
    let (h1, h2) = (header.clone(), header);
    vec![ComponentPreview::new(Area::Shell, "File viewer", "A file's header, and the preview's states.").stacked().examples([
        Example::new("header: a changed file", move |_, _| {
            let content = div().flex_1().p_4().text_size(text::SM).text_color(color::text_2()).child("Its changes here.").into_any_element();
            pane(div().size_full().flex().flex_col().child(file_header(h1("crates/space-app/README.md", true))).child(content)).into_any_element()
        }),
        Example::new("can't preview", move |_, _| {
            let body = unpreviewable("Assets.car", "Binary file", "Open in Finder".into(), n2.clone(), None);
            pane(div().size_full().flex().flex_col().child(file_header(h2("crates/space-app/resources/Assets.car", false))).child(body)).into_any_element()
        }),
        Example::new("read failed", move |_, _| {
            let body = unpreviewable("gone.rs", "The file was moved or deleted.", "Open in Zed".into(), n3.clone(), Some(Rc::new(|_, _| {})));
            pane(div().size_full().flex().flex_col().child(body)).into_any_element()
        }),
    ])]
}
