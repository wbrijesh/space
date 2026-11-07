//! Follow-the-bottom scroll area (transcript, logs): pinned to the bottom while new content
//! streams in, left alone once the user scrolls up, with an optional fade over the bottom edge
//! and a round "scroll to bottom" button.
//!
//! * [`FollowScroll`] is the per-area state (scroll handle, following flag, glide). The owner
//!   keeps it across frames, like a [`ScrollHandle`], and calls [`FollowScroll::jump_to_bottom`]
//!   when a thread opens or a message is sent, or [`FollowScroll::glide_to_bottom`] for ⌘↓.
//!   Content changes need no call: every frame re-pins while following.
//! * [`FollowScrollArea`] renders the scroll container (plus fade and button by props).
//!
//! How following is decided (see [`Follow::frame`]): the area measures its content after
//! layout and before the scroll container's prepaint, so it knows this frame's max offset.
//! Only input moves the offset between frames (content growth below doesn't change an offset
//! measured from the top), so any change since the last frame is the user's scroll. While
//! following, upward user scrolling accumulates; past [`size::FOLLOW_SLOP`] following stops.
//! While not following, a user scroll that ends within the slop of the bottom resumes it.
//! Content growth and resizes never unfollow. While following, the offset is re-pinned
//! through [`ScrollHandle::scroll_to_bottom`], which GPUI applies after layout, so growing
//! text never paints a frame off the bottom.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::widgets::tip_action;
use crate::actions::ScrollToBottom;
use crate::clock::AnimationClock;
use crate::theme::{color, motion, size};

/// Offsets closer than this are equal (layout rounds to hundredths of a pixel).
const EPS: f32 = 0.5;
/// A re-pin forgets the user's small upward nudges once they have stopped scrolling this long.
/// While they keep scrolling, nudges add up even if streaming re-pins in between, so a slow
/// trackpad drag can still leave the bottom.
const GESTURE_IDLE: Duration = Duration::from_millis(250);

pub type FollowHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

// ----- follow logic (no GPUI) -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Request {
    Jump,
    Glide,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Glide {
    from: f32,
    /// Animation time the glide started.
    since: f64,
}

/// What to do with the scroll offset this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scroll {
    Keep,
    /// To the bottom of this frame's layout ([`ScrollHandle::scroll_to_bottom`]).
    Pin,
    /// Scrolled this far from the top.
    To(f32),
}

/// The animation clock for one frame.
#[derive(Debug, Clone, Copy, Default)]
struct Time {
    now: f64,
    pinned: Option<f32>,
    reduce_motion: bool,
}

/// Follow state of one scroll area. Positions are distances scrolled from the top
/// (`-offset.y`); `max` is the furthest the content can scroll.
#[derive(Debug, Clone)]
struct Follow {
    following: bool,
    /// While following: how far the user has scrolled up from the bottom.
    nudge: f32,
    /// Position after the last frame's layout (GPUI clamps it there).
    last_pos: Option<f32>,
    last_max: Option<f32>,
    last_user_scroll: Option<Instant>,
    glide: Option<Glide>,
    request: Option<Request>,
    /// Hold the view this far above the bottom (at most at the top) once the content overflows.
    hold: Option<f32>,
    slop: f32,
}

impl Follow {
    fn new() -> Self {
        Self {
            following: true,
            nudge: 0.,
            last_pos: None,
            last_max: None,
            last_user_scroll: None,
            glide: None,
            request: None,
            hold: None,
            slop: size::FOLLOW_SLOP,
        }
    }

    /// Decide this frame's offset. `pos` is the offset as the last frame left it plus any user
    /// input since; `max` is this frame's max offset (after layout). Returns what to do and
    /// whether a glide is animating.
    fn frame(&mut self, pos: f32, max: f32, input_now: Instant, time: Time) -> (Scroll, bool) {
        let max = max.max(0.);
        let max_changed = self.last_max.is_some_and(|m| (m - max).abs() > EPS);
        self.last_max = Some(max);
        if let Some(last) = self.last_pos
            && (pos - last).abs() > EPS
        {
            // Overscroll past either end doesn't count (the wheel isn't clamped until layout).
            let moved = pos.clamp(0., max.max(last)) - last;
            self.user_scrolled(moved, max - pos.clamp(0., max), input_now);
        }
        let mut pos = pos.clamp(0., max);
        let mut scroll = Scroll::Keep;

        // Once the content overflows past the slop (so the view really is scrolled up).
        if let Some(distance) = self.hold
            && max > self.slop
        {
            self.hold = None;
            self.following = false;
            self.nudge = 0.;
            self.glide = None;
            pos = (max - distance).max(0.);
            scroll = Scroll::To(pos);
        }

        match self.request.take() {
            Some(Request::Glide) if !time.reduce_motion && max - pos > EPS => {
                self.glide = Some(Glide { from: pos, since: time.now });
                self.following = false;
            }
            Some(_) => {
                self.glide = None;
                self.follow();
            }
            None => {}
        }

        if let Some(glide) = self.glide {
            let t = match time.pinned {
                Some(p) => p,
                None => ((time.now - glide.since) / motion::SCROLL_GLIDE.as_secs_f64()) as f32,
            }
            .clamp(0., 1.);
            if t >= 1. {
                self.glide = None;
                self.follow();
                return (Scroll::Pin, false);
            }
            // Aimed at this frame's bottom, so content arriving mid-glide is still reached.
            return (Scroll::To(glide.from + (max - glide.from) * motion::ease_out(t)), true);
        }

        if self.following && (self.nudge <= EPS || max_changed) {
            if self.last_user_scroll.is_none_or(|t| input_now.saturating_duration_since(t) >= GESTURE_IDLE) {
                self.nudge = 0.;
            }
            return (Scroll::Pin, false);
        }
        (scroll, false)
    }

    /// The user moved the view by `moved` (positive: down) and it is now `distance` above the
    /// bottom.
    fn user_scrolled(&mut self, moved: f32, distance: f32, now: Instant) {
        self.last_user_scroll = Some(now);
        // The user's scroll wins over a glide.
        self.glide = None;
        if self.following {
            self.nudge = (self.nudge - moved).max(0.);
            if self.nudge > self.slop {
                self.following = false;
            }
        } else if distance <= self.slop {
            self.following = true;
            self.nudge = distance;
        }
    }

    fn follow(&mut self) {
        self.following = true;
        self.nudge = 0.;
    }

    /// Where GPUI left the offset after this frame's layout.
    fn settled(&mut self, pos: f32) {
        self.last_pos = Some(pos);
    }
}

// ----- state -----------------------------------------------------------------------------------

/// The button's fade: shown or hidden, and since when it has been changing (animation time).
#[derive(Debug, Clone, Copy)]
struct Pop {
    visible: bool,
    from: f32,
    since: Option<f64>,
}

impl Pop {
    fn settled(visible: bool) -> Self {
        Self { visible, from: if visible { 1. } else { 0. }, since: None }
    }

    /// Opacity (0..1) at animation time `now`, and whether it is still changing.
    fn at(&self, now: f64, pinned: Option<f32>) -> (f32, bool) {
        let to = if self.visible { 1. } else { 0. };
        let Some(since) = self.since else { return (to, false) };
        let t = match pinned {
            Some(p) => p,
            None => ((now - since) / motion::POP.as_secs_f64()).clamp(0., 1.) as f32,
        };
        (self.from + (to - self.from) * motion::ease_out(t), t < 1.)
    }
}

struct Inner {
    follow: Follow,
    handle: ScrollHandle,
    /// The content's layout this frame (set while laying out, read before prepaint).
    content: Option<LayoutId>,
    pop: Pop,
}

/// Per-area follow state: the scroll handle, whether the area follows its bottom, and a glide
/// in progress. Cheap to clone (shared); keep one per scroll area across frames.
#[derive(Clone)]
pub struct FollowScroll(Rc<RefCell<Inner>>);

impl Default for FollowScroll {
    fn default() -> Self {
        Self::new()
    }
}

impl FollowScroll {
    /// Following, at the bottom.
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Inner {
            follow: Follow::new(),
            handle: ScrollHandle::new(),
            content: None,
            pop: Pop::settled(false),
        })))
    }

    /// Held `distance` above the bottom (or at the top, if the content is shorter) once the
    /// content overflows, with the button already shown. For previews and scenarios.
    pub fn scrolled_up(distance: Pixels) -> Self {
        let this = Self::new();
        this.scroll_up_from_bottom(distance);
        this.0.borrow_mut().pop = Pop::settled(true);
        this
    }

    pub fn scroll_handle(&self) -> ScrollHandle {
        self.0.borrow().handle.clone()
    }

    /// Following the bottom (false while scrolled up, and during a glide).
    pub fn is_following(&self) -> bool {
        self.0.borrow().follow.following
    }

    pub fn is_gliding(&self) -> bool {
        self.0.borrow().follow.glide.is_some()
    }

    /// Show the bottom on the next frame and follow it (thread opened, message sent).
    pub fn jump_to_bottom(&self) {
        let mut inner = self.0.borrow_mut();
        inner.follow.hold = None;
        inner.follow.request = Some(Request::Jump);
    }

    /// Glide to the bottom ([`motion::SCROLL_GLIDE`], on the animation clock), then follow.
    /// A user scroll during the glide cancels it.
    pub fn glide_to_bottom(&self) {
        let mut inner = self.0.borrow_mut();
        inner.follow.hold = None;
        inner.follow.request = Some(Request::Glide);
    }

    /// Stop following and hold the view `distance` above the bottom (at most at the top), as if
    /// the user had scrolled there; applied once the content overflows.
    pub fn scroll_up_from_bottom(&self, distance: Pixels) {
        let mut inner = self.0.borrow_mut();
        inner.follow.request = None;
        inner.follow.hold = Some(f32::from(distance));
    }

    /// Advance the button's fade toward the current state; returns its opacity.
    fn pop(&self, window: &mut Window, cx: &App) -> f32 {
        let clock = AnimationClock::global(cx);
        let (now, pinned) = (clock.now(), clock.pinned());
        let mut inner = self.0.borrow_mut();
        let follow = &inner.follow;
        // A pending hold counts as scrolled up, so a held area shows its button from the start.
        let visible = (!follow.following || follow.hold.is_some()) && follow.glide.is_none();
        if inner.pop.visible != visible {
            let from = inner.pop.at(now, pinned).0;
            inner.pop = Pop { visible, from, since: Some(now) };
        }
        if cx.reduce_motion() {
            inner.pop = Pop::settled(visible);
        }
        let (opacity, moving) = inner.pop.at(now, pinned);
        if moving && clock.is_running() {
            window.request_animation_frame();
        } else if !moving && pinned.is_none() {
            inner.pop.since = None;
        }
        opacity
    }
}

// ----- component -------------------------------------------------------------------------------

/// A vertical scroll area that follows its bottom. Fills its flex parent (`flex_1`); the
/// content should carry its own padding (with [`FollowScrollArea::fade`], bottom padding of
/// [`size::SCROLL_FADE_H`] lets the last line clear the fade).
#[derive(IntoElement)]
pub struct FollowScrollArea {
    id: ElementId,
    state: FollowScroll,
    content: AnyElement,
    fade: bool,
    button: bool,
    force_hover: bool,
    on_follow_change: Option<FollowHandler>,
}

impl FollowScrollArea {
    pub fn new(id: impl Into<ElementId>, state: &FollowScroll, content: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            content: content.into_any_element(),
            fade: false,
            button: false,
            force_hover: false,
            on_follow_change: None,
        }
    }

    /// Fade the content into the background over the bottom edge ([`scroll_fade`]).
    pub fn fade(mut self, on: bool) -> Self {
        self.fade = on;
        self
    }

    /// Show the scroll-to-bottom button while not following.
    pub fn button(mut self, on: bool) -> Self {
        self.button = on;
        self
    }

    /// Gallery: draw the button hovered.
    pub fn force_hover(mut self, on: bool) -> Self {
        self.force_hover = on;
        self
    }

    /// Called (after the frame) when following starts or stops.
    pub fn on_follow_change(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_follow_change = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for FollowScrollArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, state, content, fade, button, force_hover, on_follow_change } = self;
        let scroll = div()
            .id(id)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&state.scroll_handle())
            .child(Measured { state: state.clone(), child: content });
        let opacity = if button { state.pop(window, cx) } else { 0. };
        let button = (opacity > 0.).then(|| {
            let clickable = !state.is_following() && !state.is_gliding();
            scroll_button(state.clone(), opacity, clickable, force_hover)
        });
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(Tracked { state, on_follow_change, child: scroll.into_any_element() })
            .when(fade, |d| d.child(scroll_fade()))
            .children(button)
    }
}

/// The round button, centered over the area just above its bottom edge. Its row has no
/// hitbox, so the wheel still scrolls the content around it; the mouse-down stops at the
/// button, so the composer keeps its focus.
fn scroll_button(state: FollowScroll, opacity: f32, clickable: bool, force_hover: bool) -> Div {
    let scale = motion::POP_SCALE + (1. - motion::POP_SCALE) * opacity;
    let diameter = size::ROUND_BUTTON * scale;
    let circle = div()
        .id("scroll-to-bottom")
        .size(px(diameter))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .shadow_md()
        .opacity(opacity)
        .text_color(if force_hover { color::text() } else { color::text_2() })
        .child(Icon::new(IconName::ArrowDown).size(size::ICON_MD * scale))
        .when(clickable, |d| {
            d.cursor_pointer()
                .hover(|s| s.text_color(color::text()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .tooltip(tip_action("Scroll to bottom", &ScrollToBottom))
                .on_click(move |_, window, _| {
                    state.glide_to_bottom();
                    window.refresh();
                })
        });
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(size::SCROLL_BUTTON_INSET)
        .h(px(size::ROUND_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .child(circle)
}

/// Fade over the bottom edge of a scroll area so content dims into the background before it
/// meets the composer. Place inside a `relative()` parent, after the scroll container. Has no
/// hitbox, so wheel and clicks pass through to the content below.
pub fn scroll_fade() -> Div {
    let bg = color::bg();
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(size::SCROLL_FADE_H)
        .bg(linear_gradient(180., linear_color_stop(bg.opacity(0.), 0.), linear_color_stop(bg, 1.)))
}

/// Wraps the scroll container: decides the offset after layout, before the container's
/// prepaint applies it, and records where GPUI left it afterwards.
struct Tracked {
    state: FollowScroll,
    on_follow_change: Option<FollowHandler>,
    child: AnyElement,
}

impl IntoElement for Tracked {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Tracked {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let content = self.state.0.borrow_mut().content.take();
        if let Some(content) = content {
            let content_h = window.layout_bounds(content).size.height;
            let clock = AnimationClock::global(cx);
            let time = Time { now: clock.now(), pinned: clock.pinned(), reduce_motion: cx.reduce_motion() };
            let running = clock.is_running();
            let mut inner = self.state.0.borrow_mut();
            let offset = inner.handle.offset();
            let was = (inner.follow.following, inner.follow.glide.is_some());
            let max = f32::from(content_h - bounds.size.height).max(0.);
            let (scroll, animating) = inner.follow.frame(-f32::from(offset.y), max, Instant::now(), time);
            match scroll {
                Scroll::Keep => {}
                Scroll::Pin => inner.handle.scroll_to_bottom(),
                Scroll::To(y) => inner.handle.set_offset(point(offset.x, px(-y))),
            }
            let now = (inner.follow.following, inner.follow.glide.is_some());
            let held = inner.follow.hold.is_some();
            drop(inner);
            // Another frame for the glide, a pending hold, and the button's fade.
            if (animating && running) || held || now != was {
                window.request_animation_frame();
            }
            if now.0 != was.0
                && let Some(f) = self.on_follow_change.clone()
            {
                let following = now.0;
                window.defer(cx, move |window, cx| f(following, window, cx));
            }
        }
        self.child.prepaint(window, cx);
        let inner = &mut *self.state.0.borrow_mut();
        inner.follow.settled(-f32::from(inner.handle.offset().y));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// Wraps the content: records its layout id, so [`Tracked`] can read its height after layout.
struct Measured {
    state: FollowScroll,
    child: AnyElement,
}

impl IntoElement for Measured {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Measured {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.request_layout(window, cx);
        self.state.0.borrow_mut().content = Some(id);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    use crate::theme::text;

    const HEIGHT: Pixels = px(300.);
    const LINES: usize = 40;

    /// A transcript-like area over numbered paragraphs, with its own persistent state.
    fn example(name: &'static str, make: fn() -> FollowScroll, hover: bool) -> Example {
        Example::new(name, move |window, cx| {
            let state = window.use_keyed_state(SharedString::from(format!("follow-scroll-{name}")), cx, |_, _| make());
            let state = state.read(cx).clone();
            let content = div().flex().flex_col().gap_3().px_6().pt_4().pb(size::SCROLL_FADE_H).children(
                (1..=LINES).map(|i| {
                    div()
                        .text_size(text::BODY)
                        .line_height(relative(text::LEADING))
                        .child(format!("{i}. The reducer folds each session event into the transcript as it arrives."))
                }),
            );
            div()
                .h(HEIGHT)
                .flex()
                .flex_col()
                .bg(color::bg())
                .child(FollowScrollArea::new(SharedString::from(format!("p-follow-{name}")), &state, content)
                    .fade(true)
                    .button(true)
                    .force_hover(hover))
                .into_any_element()
        })
        .width(size::CONTENT_MAX_W)
    }

    fn mid_glide() -> FollowScroll {
        let state = FollowScroll::scrolled_up(px(900.));
        state.glide_to_bottom();
        state
    }

    vec![ComponentPreview::new(
        Area::Transcript,
        "Follow scroll",
        "Follows the bottom while you're there; scroll up and it stays put, with a button (⌘↓) to glide back.",
    )
    .stacked()
    .examples([
        example("at the bottom", FollowScroll::new, false),
        example("scrolled up", || FollowScroll::scrolled_up(px(400.)), false),
        example("button hover", || FollowScroll::scrolled_up(px(400.)), true),
        // Pin the clock (Pin phase) to hold it mid-way; live, it glides once and follows.
        example("mid-glide (pin the clock)", mid_glide, false),
    ])]
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob exports its own `test` attribute.
    use std::time::{Duration, Instant};

    use super::{EPS, Follow, GESTURE_IDLE, Request, Scroll, Time, size};

    const SLOP: f32 = size::FOLLOW_SLOP;

    struct Area {
        follow: Follow,
        /// Offset (distance from the top) as GPUI would hold it.
        pos: f32,
        max: f32,
        clock: f64,
        input: Instant,
    }

    impl Area {
        fn new(max: f32) -> Self {
            let t0 = Instant::now();
            let mut area = Self { follow: Follow::new(), pos: 0., max, clock: 0., input: t0 };
            area.frame();
            area
        }

        /// One frame: decide, apply like GPUI (pin or set, then clamp), record.
        fn frame(&mut self) -> Scroll {
            let time = Time { now: self.clock, ..Default::default() };
            let (scroll, _) = self.follow.frame(self.pos, self.max, self.input, time);
            match scroll {
                Scroll::Keep => {}
                Scroll::Pin => self.pos = self.max,
                Scroll::To(y) => self.pos = y,
            }
            self.pos = self.pos.clamp(0., self.max);
            self.follow.settled(self.pos);
            scroll
        }

        /// The user scrolls by `dy` (negative: up) with the wheel, then a frame.
        fn wheel(&mut self, dy: f32) {
            self.input += Duration::from_millis(16);
            self.pos += dy;
            self.frame();
        }

        /// Content grows (or shrinks) by `dy`, then a frame.
        fn grow(&mut self, dy: f32) {
            self.max += dy;
            self.frame();
        }

        fn idle(&mut self) {
            self.input += GESTURE_IDLE;
        }

        fn at_bottom(&self) -> bool {
            (self.max - self.pos).abs() < EPS
        }
    }

    #[test]
    fn starts_following_and_streaming_stays_pinned() {
        let mut a = Area::new(500.);
        assert!(a.follow.following && a.at_bottom());
        for _ in 0..20 {
            a.grow(37.);
            assert!(a.follow.following && a.at_bottom());
        }
    }

    #[test]
    fn scrolling_up_past_the_slop_stops_following() {
        let mut a = Area::new(500.);
        a.wheel(-(SLOP - 4.));
        assert!(a.follow.following, "within the slop");
        a.wheel(-10.);
        assert!(!a.follow.following, "past the slop");
        let pos = a.pos;
        a.grow(200.);
        assert_eq!(a.pos, pos, "new content arrives below without moving the view");
        assert!(!a.follow.following);
    }

    #[test]
    fn slow_drag_up_while_streaming_still_escapes() {
        let mut a = Area::new(500.);
        for _ in 0..6 {
            a.wheel(-5.);
            a.grow(30.); // re-pins in between
        }
        assert!(!a.follow.following, "nudges add up during a gesture");
    }

    #[test]
    fn content_growth_never_unfollows() {
        let mut a = Area::new(100.);
        a.grow(5000.);
        assert!(a.follow.following && a.at_bottom());
        // A small nudge, then growth in the same frame: still following.
        a.input += Duration::from_millis(16);
        a.pos -= 5.;
        a.max += 400.;
        a.frame();
        assert!(a.follow.following && a.at_bottom());
        // Shrinking (a tool collapses) doesn't unfollow either.
        a.grow(-300.);
        assert!(a.follow.following && a.at_bottom());
    }

    #[test]
    fn old_nudges_are_forgotten_after_the_gesture() {
        let mut a = Area::new(500.);
        a.wheel(-15.);
        a.idle();
        a.grow(10.);
        assert_eq!(a.follow.nudge, 0.);
        a.wheel(-15.);
        assert!(a.follow.following, "15px from the bottom, not 30");
    }

    #[test]
    fn scrolling_back_to_the_bottom_resumes() {
        let mut a = Area::new(1000.);
        a.wheel(-300.);
        assert!(!a.follow.following);
        a.wheel(200.);
        assert!(!a.follow.following, "still 100px up");
        a.wheel(90.);
        assert!(a.follow.following, "within the slop of the bottom");
        a.grow(50.);
        assert!(a.at_bottom());
    }

    #[test]
    fn overscroll_and_short_content_dont_unfollow() {
        let mut a = Area::new(0.);
        for _ in 0..10 {
            a.wheel(-20.);
        }
        assert!(a.follow.following, "nothing to scroll");
        let mut a = Area::new(400.);
        a.wheel(80.); // past the bottom
        assert!(a.follow.following && a.at_bottom());
    }

    #[test]
    fn jump_resumes_following() {
        let mut a = Area::new(800.);
        a.wheel(-400.);
        a.follow.request = Some(Request::Jump);
        a.frame();
        assert!(a.follow.following && a.at_bottom());
    }

    #[test]
    fn glide_eases_to_the_live_bottom_then_follows() {
        let mut a = Area::new(1000.);
        a.wheel(-600.);
        a.follow.request = Some(Request::Glide);
        a.frame();
        assert!(a.follow.glide.is_some() && !a.follow.following);
        let start = a.pos;
        a.clock += 0.1;
        a.grow(100.); // content arrives mid-glide
        assert!(a.pos > start && a.pos < a.max);
        a.clock += 0.11;
        a.frame();
        assert!(a.follow.following && a.at_bottom() && a.follow.glide.is_none());
    }

    #[test]
    fn user_scroll_cancels_a_glide() {
        let mut a = Area::new(1000.);
        a.wheel(-600.);
        a.follow.request = Some(Request::Glide);
        a.frame();
        a.clock += 0.05;
        a.frame();
        a.wheel(-50.);
        assert!(a.follow.glide.is_none() && !a.follow.following);
        let pos = a.pos;
        a.clock += 1.;
        a.frame();
        assert_eq!(a.pos, pos, "the user's scroll is respected");
    }

    #[test]
    fn pinned_clock_holds_a_glide_mid_way() {
        let mut a = Area::new(1000.);
        a.wheel(-600.);
        a.follow.request = Some(Request::Glide);
        let time = Time { now: 0., pinned: Some(0.5), reduce_motion: false };
        let (scroll, animating) = a.follow.frame(a.pos, a.max, a.input, time);
        let Scroll::To(y) = scroll else { panic!("{scroll:?}") };
        assert!(animating && y > 400. && y < 1000.);
    }

    #[test]
    fn hold_waits_for_overflow() {
        let mut a = Area::new(0.);
        a.follow.hold = Some(300.);
        a.frame();
        assert!(a.follow.following && a.follow.hold.is_some(), "nothing to scroll yet");
        a.grow(100.);
        assert!(!a.follow.following && a.pos == 0., "at the top: shorter than the distance");
        a.grow(100.);
        assert_eq!(a.pos, 0., "stays put as content arrives");

        let mut a = Area::new(1000.);
        a.follow.hold = Some(300.);
        a.frame();
        assert!(!a.follow.following && (a.max - a.pos - 300.).abs() < EPS);
    }
}
