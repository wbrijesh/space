//! Images: a sent message's pictures, the composer's draft thumbnails, and the full-window
//! viewer a picture opens in.

use std::rc::Rc;

use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::ImageRef;

use super::loaders::Spinner;
use crate::theme::{color, radius, size, text};

/// An image to show: a saved file or bytes in memory, and its size in pixels.
#[derive(Clone)]
pub struct Pic {
    pub src: ImageSource,
    pub width: u32,
    pub height: u32,
}

impl Pic {
    pub fn file(image: &ImageRef) -> Self {
        Self { src: image.path.clone().into(), width: image.width, height: image.height }
    }
}

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
pub type StepHandler = Rc<dyn Fn(isize, &mut Window, &mut App)>;

/// Opens the viewer on picture `ix` of a message's pictures.
pub type OpenPicsHandler = Rc<dyn Fn(Vec<Pic>, usize, &mut Window, &mut App)>;

/// A lone picture's longest side in a message.
const SINGLE: f32 = 240.;
/// Each picture's side when a message has several.
const TILE: f32 = 64.;
/// A draft thumbnail's side in the composer.
const THUMB: f32 = 56.;

/// The size a lone picture is shown at: 240 px on its long edge (never scaled up), its shape
/// kept between 1:4 and 4:1 (beyond that it's cropped).
fn single_size(pic: &Pic) -> Size<Pixels> {
    let (w, h) = (pic.width.max(1) as f32, pic.height.max(1) as f32);
    let aspect = (w / h).clamp(0.25, 4.);
    let long = w.max(h).min(SINGLE);
    if aspect >= 1. { size(px(long), px(long / aspect)) } else { size(px(long * aspect), px(long)) }
}

/// A picture filling its box (cropped to it), with a spinner while it loads and a quiet tile if
/// it can't be read.
fn picture(src: ImageSource) -> Img {
    img(src)
        .size_full()
        .object_fit(ObjectFit::Cover)
        .with_loading(|| div().size_full().flex().items_center().justify_center().child(Spinner::new(size::SPINNER_MD)).into_any_element())
        .with_fallback(|| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .text_size(text::XS)
                .text_color(color::text_3())
                .child(Icon::new(IconName::Close).size(size::ICON_SM))
                .child("Image not found")
                .into_any_element()
        })
}

/// A sent message's pictures, right-aligned above its bubble: one shown whole (up to 240 px),
/// several as 64 px tiles. Clicking one opens the viewer.
pub fn message_images(id: impl Into<SharedString>, pics: Vec<Pic>, on_open: Option<OpenPicsHandler>) -> Div {
    let id = id.into();
    let single = pics.len() == 1;
    let tiles = pics.iter().enumerate().map(|(ix, pic)| {
        let frame = if single { single_size(pic) } else { size(px(TILE), px(TILE)) };
        let (all, open) = (pics.clone(), on_open.clone());
        div()
            .id(ElementId::NamedInteger(id.clone(), ix as u64))
            .w(frame.width)
            .h(frame.height)
            .flex_none()
            .overflow_hidden()
            .rounded(if single { radius::XL } else { radius::LG })
            .bg(color::surface_2())
            .border_1()
            .border_color(color::border())
            .child(picture(pic.src.clone()))
            .when_some(open, |d, open| d.cursor_pointer().on_click(move |_, w, cx| open(all.clone(), ix, w, cx)))
    });
    div().flex().flex_wrap().justify_end().gap_1p5().children(tiles)
}

/// A draft image in the composer: its thumbnail (`None` while it's being prepared), with × on
/// hover to remove it.
pub fn draft_thumb(ix: usize, src: Option<ImageSource>, on_remove: IndexHandler) -> Stateful<Div> {
    let tile = div().size_full().flex().items_center().justify_center().map(|d| match src {
        Some(src) => d.child(picture(src)),
        None => d.child(Spinner::new(size::SPINNER_MD)),
    });
    let group: SharedString = format!("draft-image-{ix}").into();
    div()
        .id(("draft-image", ix))
        .group(group.clone())
        .relative()
        .size(px(THUMB))
        .flex_none()
        .rounded(radius::LG)
        .overflow_hidden()
        .bg(color::pressed())
        .border_1()
        .border_color(color::border())
        .child(tile)
        .child(
            div()
                .id(("remove-image", ix))
                .absolute()
                .top(px(3.))
                .right(px(3.))
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgba(0x000000b3))
                .text_color(white())
                .invisible()
                .group_hover(group, |s| s.visible())
                .cursor_pointer()
                .child(Icon::new(IconName::Close).size(size::ICON_XS))
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on_remove(ix, w, cx)
                }),
        )
}

/// What the viewer's keys and buttons do.
pub struct ViewerHandlers {
    pub on_close: Handler,
    /// Go to the previous (-1) or next (+1) picture.
    pub on_step: StepHandler,
}

/// The full-window viewer: picture `ix` on a dark backdrop, fitted to the window. Esc, the
/// backdrop or × closes it; ← / → (and the arrow buttons) move between the message's pictures.
pub fn viewer(pics: &[Pic], ix: usize, focus: &FocusHandle, h: ViewerHandlers) -> Stateful<Div> {
    let Some(pic) = pics.get(ix) else { return div().id("image-viewer") };
    let many = pics.len() > 1;
    let (close, close_key, close_btn) = (h.on_close.clone(), h.on_close.clone(), h.on_close.clone());
    let (step_key, step_prev, step_next) = (h.on_step.clone(), h.on_step.clone(), h.on_step);
    let arrow = |id: &'static str, icon: IconName, f: StepHandler, by: isize| {
        div()
            .id(id)
            .size(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgba(0xffffff1f))
            .hover(|s| s.bg(rgba(0xffffff33)))
            .text_color(white())
            .cursor_pointer()
            .child(Icon::new(icon).size(size::ICON_MD))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                f(by, w, cx)
            })
    };
    div()
        .id("image-viewer")
        .track_focus(focus)
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .p(px(48.))
        .bg(rgba(0x000000e0))
        .occlude()
        .on_key_down(move |e, w, cx| match e.keystroke.key.as_str() {
            "escape" => close_key(w, cx),
            "left" => step_key(-1, w, cx),
            "right" => step_key(1, w, cx),
            _ => {}
        })
        .on_click(move |_, w, cx| close(w, cx))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    // A click on the picture itself doesn't close the viewer.
                    div().id("viewer-picture").max_w_full().max_h_full().on_click(|_, _, cx| cx.stop_propagation()).child(
                        img(pic.src.clone())
                            .max_w_full()
                            .max_h_full()
                            .w(px(pic.width as f32))
                            .h(px(pic.height as f32))
                            .object_fit(ObjectFit::ScaleDown),
                    ),
                ),
        )
        .when(many, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .text_size(text::SM)
                    .text_color(rgba(0xffffffcc))
                    .child(arrow("viewer-prev", IconName::ChevronLeft, step_prev, -1))
                    .child(format!("{} of {}", ix + 1, pics.len()))
                    .child(arrow("viewer-next", IconName::ChevronRight, step_next, 1)),
            )
        })
        .child(
            div()
                .id("viewer-close")
                .absolute()
                .top(px(16.))
                .right(px(16.))
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgba(0xffffff1f))
                .hover(|s| s.bg(rgba(0xffffff33)))
                .text_color(white())
                .cursor_pointer()
                .child(Icon::new(IconName::Close).size(size::ICON_MD))
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    close_btn(w, cx)
                }),
        )
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let open: OpenPicsHandler = Rc::new(|_, _, _, _| {});
    let remove: IndexHandler = Rc::new(|_, _, _| {});
    let (o1, o2, o3) = (open.clone(), open.clone(), open);
    vec![
        ComponentPreview::new(Area::Transcript, "Images", "Pictures sent with a message; click opens the viewer.").stacked().examples([
            Example::new("one (wide)", move |_, _| message_images("p1", vec![sample(1600, 900)], Some(o1.clone())).into_any_element()),
            Example::new("one (tall)", move |_, _| message_images("p2", vec![sample(900, 1600)], Some(o2.clone())).into_any_element()),
            Example::new("several", move |_, _| {
                message_images("p3", (0..6).map(|_| sample(1024, 1024)).collect(), Some(o3.clone())).into_any_element()
            }),
            Example::new("missing file", |_, _| {
                let gone = Pic { src: std::path::PathBuf::from("/nonexistent/space-preview.png").into(), width: 800, height: 600 };
                message_images("p4", vec![gone], None).into_any_element()
            }),
        ]),
        ComponentPreview::new(Area::Composer, "Draft images", "Thumbnails above the input; × on hover removes one.").example(Example::new(
            "ready, preparing",
            move |_, _| {
                div()
                    .flex()
                    .gap_1p5()
                    .child(draft_thumb(0, Some(sample(1024, 1024).src), remove.clone()))
                    .child(draft_thumb(1, None, remove.clone()))
                    .into_any_element()
            },
        )),
    ]
}

/// A preview picture: the app icon, reported at the given size.
#[cfg(feature = "dev")]
pub fn sample(width: u32, height: u32) -> Pic {
    static ICON: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    let image = ICON.get_or_init(|| {
        std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../../space-app/resources/AppIcon.png").to_vec()))
    });
    Pic { src: image.clone().into(), width, height }
}
