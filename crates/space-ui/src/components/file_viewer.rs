//! The Files view's tabs and file preview: a strip with the Files page and one tab per open
//! file; a file's header (its path, the viewer choice, wrap, Open in); and the states a
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
use crate::theme::{color, radius, size, text};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type TabHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A tab in the pane's strip: an icon, an optional label, filled when shown, and × (on hover;
/// always on the shown one) when it can close.
pub fn pane_tab(id: ElementId, label: Option<SharedString>, icon: Icon, on: bool, close: Option<Handler>, select: Handler) -> Stateful<Div> {
    let group: SharedString = format!("{id}").into();
    let close_id = ElementId::Name(format!("{id}-close").into());
    div()
        .id(id)
        .group(group.clone())
        .flex_none()
        .max_w(px(180.))
        .h(px(24.))
        .pl_2()
        .when(close.is_none(), |d| d.pr_2())
        .when(close.is_some(), |d| d.pr_1())
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(radius::MD)
        .text_size(text::SM)
        .cursor_pointer()
        .map(|d| if on { d.bg(color::hover()).text_color(color::text()) } else { d.text_color(color::text_3()) })
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .child(icon.size(size::ICON).text_color(color::text_2()))
        .children(label.map(|l| div().min_w_0().truncate().child(l)))
        .when_some(close, |d, close| {
            d.child(
                div()
                    .id(close_id)
                    .size(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius::SM)
                    .text_color(color::text_3())
                    .when(!on, |d| d.invisible().group_hover(group, |s| s.visible()))
                    .hover(|s| s.bg(color::pressed()).text_color(color::text()))
                    .child(Icon::new(Lucide::X).size(size::ICON_SM))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        close(w, cx)
                    }),
            )
        })
        .on_click(move |_, w, cx| select(w, cx))
}

/// The folder's tree: always first in the strip, just its icon.
pub fn tree_tab(on: bool, select: Handler) -> Stateful<Div> {
    pane_tab("files-page-tab".into(), None, Icon::new(Lucide::ListTree), on, None, select).tip("All files")
}

/// An open file's tab: its icon and name.
pub fn file_tab(ix: usize, name: &SharedString, on: bool, select: Handler, close: Handler) -> Stateful<Div> {
    pane_tab(ElementId::NamedInteger("file-tab".into(), ix as u64), Some(name.clone()), Icon::new(file_icon(name)), on, Some(close), select)
}

/// The tree's tab, then the open files' (`active`: the shown file; `None`, the tree).
pub fn file_tabs(names: &[SharedString], active: Option<usize>, on_select: TabHandler, on_close: IndexHandler) -> Stateful<Div> {
    let page = {
        let select = on_select.clone();
        tree_tab(active.is_none(), Rc::new(move |w, cx| select(None, w, cx)))
    };
    div().id("file-tabs").flex_1().min_w_0().flex().items_center().gap_1().overflow_x_scroll().child(page).children(names.iter().enumerate().map(|(ix, name)| {
        let (select, close) = (on_select.clone(), on_close.clone());
        file_tab(ix, name, active == Some(ix), Rc::new(move |w, cx| select(Some(ix), w, cx)), Rc::new(move |w, cx| close(ix, w, cx)))
    }))
}

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
    vec![ComponentPreview::new(Area::Shell, "File viewer", "Tabs, a file's header, and the preview's states.").stacked().examples([
        Example::new("tabs", |_, _| {
            let names: Vec<SharedString> = vec!["files_view.rs".into(), "README.md".into(), "logo.png".into()];
            div().w(px(620.)).h(px(34.)).px_2().flex().items_center().bg(color::bg()).border_1().border_color(color::border())
                .child(file_tabs(&names, Some(0), Rc::new(|_, _, _| {}), Rc::new(|_, _, _| {})))
                .into_any_element()
        }),
        Example::new("header: a changed file", move |_, _| {
            let content = div().flex_1().p_4().text_size(text::SM).text_color(color::text_2()).child("Its changes here.").into_any_element();
            pane(div().size_full().flex().flex_col().child(file_header(h1("crates/space-app/README.md", true))).child(content)).into_any_element()
        }),
        Example::new("can't preview", move |_, _| {
            let body = unpreviewable("Assets.car", "This file isn't text, so it can't be shown here.", "Open in Finder".into(), n2.clone(), None);
            pane(div().size_full().flex().flex_col().child(file_header(h2("crates/space-app/resources/Assets.car", false))).child(body)).into_any_element()
        }),
        Example::new("read failed", move |_, _| {
            let body = unpreviewable("gone.rs", "The file was moved or deleted.", "Open in Zed".into(), n3.clone(), Some(Rc::new(|_, _| {})));
            pane(div().size_full().flex().flex_col().child(body)).into_any_element()
        }),
    ])]
}
