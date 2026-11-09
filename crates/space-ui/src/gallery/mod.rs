//! In-app component gallery (dev only): every component's named states, rendered through
//! the real component code with fixture data, plus a Tokens page and animation-clock
//! controls. Opened with ⌥⌘G, the Dev menu, or `Space --gallery`.

mod tokens;

use gpui::component::slider::{Slider, SliderEvent, SliderState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::clock::{AnimationClock, SPEEDS, STEP};
use crate::preview::{Area, ComponentPreview, Layout, registry};
use crate::theme::{color, radius, text};

struct GalleryWindow(AnyWindowHandle);
impl Global for GalleryWindow {}

/// Open the gallery window, or bring it to the front.
pub fn open(cx: &mut App) -> Option<AnyWindowHandle> {
    if let Some(handle) = cx.try_global::<GalleryWindow>().map(|g| g.0)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return Some(handle);
    }
    let bounds = Bounds::centered(None, size(px(1240.), px(860.)), cx);
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("Space Gallery".into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(720.), px(480.))),
        ..Default::default()
    };
    let (handle, _) = gpui::open_window(options, cx, |window, cx| cx.new(|cx| Gallery::new(window, cx))).ok()?;
    cx.set_global(GalleryWindow(handle));
    Some(handle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Tokens,
    Component(usize),
}

pub struct Gallery {
    previews: Vec<ComponentPreview>,
    page: Page,
    phase: Entity<SliderState>,
    /// Alternate canvas: raised surface instead of the window background.
    raised: bool,
    _subscriptions: Vec<Subscription>,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = AnimationClock::global(cx).pinned().unwrap_or(0.25);
        let phase = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.01).default_value(initial));
        let sub = cx.subscribe_in(&phase, window, |_, _, e: &SliderEvent, _, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = e;
            let p = v.start();
            AnimationClock::update(cx, |c| c.pin(Some(p)));
        });
        let mut previews = registry();
        // `SPACE_GALLERY_ONLY=<text>|<text>…`: keep only examples whose name contains one of
        // them (for screenshots of examples further down a long page).
        if let Ok(only) = std::env::var("SPACE_GALLERY_ONLY") {
            for p in &mut previews {
                p.examples.retain(|e| only.split('|').any(|o| e.name.contains(o)));
            }
        }
        let page = std::env::var("SPACE_GALLERY_PAGE")
            .ok()
            .and_then(|want| {
                if want.eq_ignore_ascii_case("tokens") {
                    return Some(Page::Tokens);
                }
                previews.iter().position(|p| p.name.eq_ignore_ascii_case(&want)).map(Page::Component)
            })
            .unwrap_or(Page::Tokens);
        let mut this = Self { previews, page: Page::Tokens, phase, raised: false, _subscriptions: vec![sub] };
        this.show(page, cx);
        this
    }

    fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        // Some states (soft-wrapped composer) are decided from the previous frame's layout:
        // render once more after the first layout.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(60)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: ElementId, label: String, selected: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded(radius::MD)
                .text_size(text::MD)
                .cursor_pointer()
                .map(|d| {
                    if selected {
                        d.bg(color::pressed()).text_color(color::text())
                    } else {
                        d.text_color(color::text_2()).hover(|s| s.bg(color::surface()).text_color(color::text()))
                    }
                })
                .child(label)
        };
        let heading = |label: &'static str| {
            div().px_2p5().pt_4().pb_1().text_size(text::XS).font_weight(FontWeight::SEMIBOLD).text_color(color::text_3()).child(label)
        };
        let mut col = div().flex().flex_col().gap_0p5().px_2().pb_4();
        col = col.child(heading("TOKENS")).child(
            item("page-tokens".into(), "Design tokens".into(), self.page == Page::Tokens)
                .on_click(cx.listener(|this, _, _, cx| this.show(Page::Tokens, cx))),
        );
        for area in Area::ALL {
            col = col.child(heading(match area {
                Area::Shell => "SHELL",
                Area::Sidebar => "SIDEBAR",
                Area::Composer => "COMPOSER",
                Area::Transcript => "TRANSCRIPT",
                Area::Trajectory => "TRAJECTORY",
                Area::Settings => "SETTINGS",
                Area::Indicators => "INDICATORS",
            }));
            for (ix, p) in self.previews.iter().enumerate().filter(|(_, p)| p.area == area) {
                col = col.child(
                    item(("page", ix).into(), p.name.to_string(), self.page == Page::Component(ix))
                        .on_click(cx.listener(move |this, _, _, cx| this.show(Page::Component(ix), cx))),
                );
            }
        }
        div()
            .id("gallery-nav")
            .w(px(220.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .bg(color::bg_sidebar())
            .border_r_1()
            .border_color(color::border())
            .child(col)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let clock = AnimationClock::global(cx).clone();
        let seg = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .h(px(26.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded(radius::SM)
                .text_size(text::SM)
                .cursor_pointer()
                .map(|d| {
                    if on {
                        d.bg(color::pressed()).text_color(color::text())
                    } else {
                        d.text_color(color::text_2()).hover(|s| s.bg(color::hover()))
                    }
                })
                .child(label)
        };
        let group = || div().flex().items_center().gap_0p5().p_0p5().rounded(radius::LG).border_1().border_color(color::border());
        let label = |s: &'static str| div().text_size(text::XS).text_color(color::text_3()).child(s);

        let mut speeds = group();
        for (i, s) in SPEEDS.iter().enumerate() {
            let s = *s;
            let id = ["speed-0", "speed-1", "speed-2"][i];
            speeds = speeds.child(
                seg(id, format!("{s}×"), (clock.speed() - s).abs() < 1e-9)
                    .on_click(cx.listener(move |_, _, _, cx| AnimationClock::update(cx, |c| c.set_speed(s)))),
            );
        }
        let pinned = clock.pinned().is_some();
        let phase = self.phase.clone();
        div()
            .flex_none()
            .min_h(px(48.))
            .px_4()
            .py_2()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_4()
            .gap_y_2()
            .border_b_1()
            .border_color(color::border())
            .child(label("Animation"))
            .child(
                group()
                    .child(
                        seg("clock-live", "Live".into(), !clock.is_paused() && !pinned)
                            .on_click(cx.listener(|_, _, _, cx| AnimationClock::update(cx, |c| {
                                c.pin(None);
                                c.resume();
                            }))),
                    )
                    .child(
                        seg("clock-pause", "Pause".into(), clock.is_paused() && !pinned)
                            .on_click(cx.listener(|_, _, _, cx| AnimationClock::update(cx, |c| {
                                c.pin(None);
                                c.pause();
                            }))),
                    )
                    .child(
                        seg("clock-step", "Step".into(), false)
                            .on_click(cx.listener(|_, _, _, cx| AnimationClock::update(cx, |c| {
                                c.pin(None);
                                c.pause();
                                c.step(STEP);
                            }))),
                    ),
            )
            .child(speeds)
            .child(
                group().child(seg("clock-pin", "Pin phase".into(), pinned).on_click(cx.listener(move |_, _, _, cx| {
                    let value = phase.read(cx).value().start();
                    AnimationClock::update(cx, |c| c.pin(if c.pinned().is_some() { None } else { Some(value) }))
                }))),
            )
            .child(div().w(px(160.)).flex_none().child(Slider::new(&self.phase)))
            .child(
                div()
                    .w(px(44.))
                    .font_family(text::MONO_FONT)
                    .text_size(text::XS)
                    .text_color(color::text_3())
                    .child(clock.pinned().map_or("—".to_string(), |p| format!("{p:.2}"))),
            )
            .child(label("Canvas"))
            .child(
                group()
                    .child(seg("bg-window", "Window".into(), !self.raised).on_click(cx.listener(|this, _, _, cx| {
                        this.raised = false;
                        cx.notify();
                    })))
                    .child(seg("bg-raised", "Raised".into(), self.raised).on_click(cx.listener(|this, _, _, cx| {
                        this.raised = true;
                        cx.notify();
                    }))),
            )
    }

    fn component_page(&self, preview: &ComponentPreview, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let canvas_bg = if self.raised { color::surface() } else { color::bg() };
        let mut examples = div().flex().gap_6();
        examples = match preview.layout {
            Layout::Grid => examples.flex_wrap().items_start(),
            Layout::Stack => examples.flex_col(),
        };
        for ex in &preview.examples {
            let frame = div()
                .p_4()
                .rounded(radius::XL)
                .bg(canvas_bg)
                .border_1()
                .border_color(color::border())
                // Preferred width, shrinking to fit narrow windows.
                .when_some(ex.width, |d, w| d.w_full().max_w(w + px(34.)))
                .child((ex.render)(window, cx));
            examples = examples.child(
                div()
                    .when(ex.width.is_some(), |d| d.w_full())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_family(text::MONO_FONT).text_size(text::XS).text_color(color::text_3()).child(ex.name.clone()))
                    .child(frame),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(preview.name))
                    .child(div().text_size(text::MD).text_color(color::text_2()).child(preview.description)),
            )
            .child(examples)
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.page {
            Page::Tokens => tokens::page(),
            Page::Component(ix) => {
                let preview = self.previews[ix].clone();
                self.component_page(&preview, window, cx)
            }
        };
        div()
            .size_full()
            .flex()
            .bg(color::bg())
            .text_color(color::text())
            .font_family(text::UI_FONT)
            .text_size(text::MD)
            .child(self.nav(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.toolbar(cx))
                    .child(div().id("gallery-body").flex_1().min_h_0().overflow_y_scroll().p_8().child(body)),
            )
            .child(crate::components::tip::layer(window, cx))
    }
}
