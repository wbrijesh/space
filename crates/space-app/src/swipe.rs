//! Two-finger swipes on the trackpad show and hide one pane (Settings ▸ Trackpad): the right pane
//! by default, or the sidebar. The swipe's direction is the pane's: the right pane comes in with
//! a swipe to the left and goes with one to the right; the sidebar the other way round. Each
//! direction only ever shows or only ever hides, so it works wherever the pointer is.
//!
//! The right pane has three steps, one per swipe: hidden, beside the chat, maximized. Swiping
//! left goes up a step (shows it, then maximizes it); swiping right goes down (restores it beside
//! the chat, then hides it).
//!
//! A swipe is one gesture (fingers down to fingers up) that travels sideways far enough before it
//! travels as far up or down; it acts once. A swipe's scrolling is the swipe's: its events, and
//! the momentum after the fingers lift, are taken before anything scrolls (the transcript doesn't
//! drift while a pane comes and goes), and so is a gesture's sideways start while it may still
//! become one. Once a gesture goes up or down it's a scroll, and everything scrolls as always.
//! With swipes off (Settings ▸ Trackpad), nothing is taken.
//!
//! Content first, as with Safari's back and forward swipe: when a gesture first leans sideways
//! over content that can still scroll that way (a file's long lines, a diff, the pane's tabs:
//! `space_ui::side_scroll`), it's a scroll, to the fingers' lift and its momentum; it never
//! scrolls to the edge and then swipes. At the edge, or over content that doesn't scroll
//! sideways, it may swipe as above. To swipe from an edge, lift and swipe again.

use gpui::*;

use crate::app::SpaceApp;
use crate::settings::SwipePane;

/// Sideways travel, in pixels, that makes a swipe.
const TRIGGER: f32 = 64.;
/// Up-or-down travel past which a gesture is a scroll, not a swipe.
const VERTICAL: f32 = 24.;

/// Which way the fingers moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Swipe {
    Left,
    Right,
}

/// The gesture under way.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Gesture {
    /// Between gestures, or scrolling that never began as one (a mouse, momentum of a scroll).
    #[default]
    None,
    /// Fingers down, not yet a swipe or a scroll: its travel so far, and whether it has leaned
    /// sideways yet (when the content under the pointer had its say).
    Deciding(f32, f32, bool),
    /// It swiped: the rest of it and its momentum are taken, until the next gesture.
    Swiped,
    /// It went up or down, or began by scrolling content sideways: a scroll.
    Scrolling,
}

/// What to do with one scroll event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fed {
    /// Take it: nothing under the pointer scrolls.
    pub(crate) take: bool,
    /// The gesture became a swipe with this event.
    pub(crate) swipe: Option<Swipe>,
}

#[derive(Debug, Default)]
pub(crate) struct SwipeTracker {
    gesture: Gesture,
}

impl SwipeTracker {
    /// One scroll event of a trackpad gesture (its deltas as the platform reports them; momentum
    /// comes as `Moved` after `Ended`). `dx > 0` reads as `Right`. `scrolls(dx)`: whether the
    /// content under the pointer would scroll sideways by `dx`; asked once, when the gesture first
    /// leans sideways.
    pub(crate) fn feed(&mut self, phase: TouchPhase, dx: f32, dy: f32, scrolls: impl FnOnce(f32) -> bool) -> Fed {
        match phase {
            TouchPhase::Started => self.gesture = Gesture::Deciding(0., 0., false),
            TouchPhase::Moved => {}
            // Ended, cancelled: a swipe keeps taking its momentum; anything else is over.
            _ => {
                let take = match self.gesture {
                    Gesture::Swiped => true,
                    Gesture::Deciding(x, y, _) => x.abs() >= y.abs(),
                    _ => false,
                };
                if self.gesture != Gesture::Swiped {
                    self.gesture = Gesture::None;
                }
                return Fed { take, swipe: None };
            }
        }
        let (x, y, leaned) = match self.gesture {
            Gesture::Deciding(x, y, leaned) => (x + dx, y + dy, leaned),
            Gesture::Swiped => return Fed { take: true, swipe: None },
            Gesture::None | Gesture::Scrolling => return Fed::default(),
        };
        if y.abs() > VERTICAL && y.abs() > x.abs() {
            self.gesture = Gesture::Scrolling;
            return Fed::default();
        }
        let sideways = x != 0. && x.abs() >= y.abs();
        // Its first lean sideways, over content that can still scroll that way: content first.
        if sideways && !leaned && scrolls(x) {
            self.gesture = Gesture::Scrolling;
            return Fed::default();
        }
        if x.abs() >= TRIGGER && x.abs() >= 2. * y.abs() {
            self.gesture = Gesture::Swiped;
            return Fed { take: true, swipe: Some(if x > 0. { Swipe::Right } else { Swipe::Left }) };
        }
        self.gesture = Gesture::Deciding(x, y, leaned || sideways);
        // Leaning sideways: may still swipe, so nothing scrolls yet.
        Fed { take: x.abs() >= y.abs(), swipe: None }
    }
}

/// macOS reports scroll deltas flipped by "natural scrolling" (System Settings ▸ Trackpad), on
/// unless turned off: with it, the deltas follow the fingers.
fn natural_scrolling() -> bool {
    use objc2_foundation::{NSUserDefaults, ns_string};
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = ns_string!("com.apple.swipescrolldirection");
    defaults.objectForKey(key).is_none() || defaults.boolForKey(key)
}

impl SpaceApp {
    /// A trackpad scroll anywhere in the window, before anything under the pointer sees it
    /// (capture phase): take a swipe's scrolling, and show or hide the swipe's pane once.
    pub(crate) fn scrolled(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.settings.settings.trackpad.swipe;
        // Only a trackpad's continuous deltas (a mouse wheel's notches are lines).
        let ScrollDelta::Pixels(delta) = event.delta else { return };
        if pane == SwipePane::Off || event.source != ScrollSource::Pointer {
            return;
        }
        let scrolls = |dx| space_ui::side_scroll::can_scroll(dx, window, cx);
        let fed = self.swipe.feed(event.touch_phase, f32::from(delta.x), f32::from(delta.y), scrolls);
        if fed.take {
            cx.stop_propagation();
        }
        let Some(swipe) = fed.swipe else { return };
        // Which way the fingers went.
        let swipe = match (swipe, natural_scrolling()) {
            (s, true) => s,
            (Swipe::Left, false) => Swipe::Right,
            (Swipe::Right, false) => Swipe::Left,
        };
        match pane {
            SwipePane::RightPane => {
                let pane = self.selected.and_then(|key| self.terminals.pane(key));
                let (open, maximized) = pane.map_or((false, false), |p| (p.open, p.maximized));
                match (swipe, open, maximized) {
                    // Up a step: show it, then maximize it.
                    (Swipe::Left, false, _) => self.toggle_right_pane(window, cx),
                    (Swipe::Left, true, false) => self.toggle_pane_maximized(cx),
                    // Down a step: back beside the chat, then hide it.
                    (Swipe::Right, true, true) => self.toggle_pane_maximized(cx),
                    (Swipe::Right, true, false) => self.toggle_right_pane(window, cx),
                    _ => {}
                }
            }
            SwipePane::Sidebar => {
                if self.sidebar_open != (swipe == Swipe::Right) {
                    self.toggle_sidebar(cx);
                }
            }
            SwipePane::Off => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TouchPhase::{Ended, Moved, Started};

    use super::{Fed, Swipe, SwipeTracker};

    /// Over content that doesn't scroll sideways (the chat, the sidebar, a wrapped file).
    fn flat(_: f32) -> bool {
        false
    }

    /// Over a file's long lines with `start` px to scroll back toward their start and `end` px
    /// toward their end (a positive delta scrolls toward the start).
    fn lines(start: f32, end: f32) -> impl Fn(f32) -> bool + Copy {
        move |dx| if dx > 0. { start > 0. } else { end > 0. }
    }

    /// What one gesture did: its swipes, and which of its events (momentum included) were taken.
    fn run(steps: &[(f32, f32)], scrolls: impl Fn(f32) -> bool + Copy) -> (Vec<Swipe>, Vec<bool>) {
        let mut t = SwipeTracker::default();
        let mut fed = vec![t.feed(Started, 0., 0., scrolls)];
        fed.extend(steps.iter().map(|&(dx, dy)| t.feed(Moved, dx, dy, scrolls)));
        fed.push(t.feed(Ended, 0., 0., scrolls));
        // Momentum, after the fingers lift.
        fed.extend(steps.iter().map(|&(dx, dy)| t.feed(Moved, dx / 2., dy / 2., scrolls)));
        (fed.iter().filter_map(|f| f.swipe).collect(), fed.iter().map(|f| f.take).collect())
    }

    fn gesture(steps: &[(f32, f32)]) -> Vec<Swipe> {
        run(steps, flat).0
    }

    #[test]
    fn a_sideways_gesture_swipes_once() {
        assert_eq!(gesture(&[(20., 1.), (20., -2.), (30., 1.), (40., 0.), (50., 0.)]), [Swipe::Right]);
        assert_eq!(gesture(&[(-30., 0.), (-40., 3.)]), [Swipe::Left]);
    }

    #[test]
    fn short_or_slanted_gestures_dont() {
        assert!(gesture(&[(30., 0.), (20., 0.)]).is_empty(), "too short");
        assert!(gesture(&[(40., 30.), (40., 30.)]).is_empty(), "as much down as across");
    }

    #[test]
    fn a_scroll_stays_a_scroll() {
        // Up and down first: sideways travel later doesn't make it a swipe.
        assert!(gesture(&[(0., 30.), (80., 0.), (80., 0.)]).is_empty());
    }

    #[test]
    fn only_inside_a_gesture() {
        let mut t = SwipeTracker::default();
        // Momentum after the fingers lifted, or a scroll that never began.
        assert_eq!(t.feed(Moved, 200., 0., flat).swipe, None);
        t.feed(Started, 0., 0., flat);
        assert_eq!(t.feed(Moved, 100., 0., flat).swipe, Some(Swipe::Right));
        assert_eq!(t.feed(Moved, 100., 0., flat).swipe, None, "once per gesture");
        t.feed(Ended, 0., 0., flat);
        assert_eq!(t.feed(Moved, -200., 0., flat).swipe, None);
    }

    #[test]
    fn a_swipe_takes_its_scrolling_and_momentum() {
        let mut t = SwipeTracker::default();
        let take = |f: Fed| f.take;
        assert!(take(t.feed(Started, 0., 0., flat)));
        assert!(take(t.feed(Moved, 30., 2., flat)), "leaning sideways: may still swipe");
        assert!(take(t.feed(Moved, 50., 3., flat)), "the swipe");
        assert!(take(t.feed(Moved, 40., -5., flat)), "the rest of it");
        assert!(take(t.feed(Ended, 0., 0., flat)));
        assert!(take(t.feed(Moved, 20., 4., flat)), "momentum");
        // The next gesture goes down: not taken.
        t.feed(Started, 0., 0., flat);
        assert!(!take(t.feed(Moved, 0., 10., flat)));
    }

    #[test]
    fn a_scroll_is_never_taken() {
        let mut t = SwipeTracker::default();
        t.feed(Started, 0., 0., flat);
        assert!(!t.feed(Moved, 2., 12., flat).take);
        assert!(!t.feed(Moved, 1., 20., flat).take, "now a scroll");
        assert!(!t.feed(Moved, 60., 0., flat).take, "sideways later stays a scroll");
        assert!(!t.feed(Ended, 0., 0., flat).take);
        assert!(!t.feed(Moved, 0., 30., flat).take, "its momentum");
    }

    #[test]
    fn a_gesture_begun_mid_scroll_scrolls_to_its_end() {
        // Lines scrolled partway: either way has room. The content scrolls however far the
        // gesture goes, momentum included; nothing after the fingers touch down is taken.
        let far = [(-30., 1.), (-60., 0.), (-90., 2.), (-120., 0.)];
        let (swipes, taken) = run(&far, lines(200., 40.));
        assert!(swipes.is_empty());
        assert_eq!(taken.iter().skip(1).filter(|&&t| t).count(), 0, "{taken:?}");
        let back = [(30., 0.), (90., -1.), (120., 0.)];
        assert!(run(&back, lines(200., 40.)).0.is_empty());
    }

    #[test]
    fn at_an_edge_outward_swipes_and_inward_scrolls() {
        // At the start of the lines: back toward the start swipes, toward the end scrolls.
        assert_eq!(run(&[(30., 1.), (50., 0.)], lines(0., 300.)).0, [Swipe::Right]);
        assert!(run(&[(-30., 1.), (-50., 0.)], lines(0., 300.)).0.is_empty());
        // At their end, the other way round.
        assert_eq!(run(&[(-30., 1.), (-50., 0.)], lines(300., 0.)).0, [Swipe::Left]);
        assert!(run(&[(30., 1.), (50., 0.)], lines(300., 0.)).0.is_empty());
    }

    #[test]
    fn content_is_asked_once_when_the_gesture_leans_sideways() {
        let asked = std::cell::Cell::new(Vec::new());
        let scrolls = |dx: f32| {
            let mut a = asked.take();
            a.push(dx);
            asked.set(a);
            false
        };
        let mut t = SwipeTracker::default();
        t.feed(Started, 0., 0., scrolls);
        t.feed(Moved, 1., 6., scrolls);
        assert!(asked.take().is_empty(), "not while it leans up or down");
        t.feed(Moved, 9., 0., scrolls);
        t.feed(Moved, -20., 0., scrolls);
        t.feed(Moved, -30., 0., scrolls);
        assert_eq!(asked.take(), [10.], "once, the way it first leaned");
    }

    #[test]
    fn over_flat_content_a_swipe_keeps_the_view_still() {
        // The chat: a sideways swipe with a little vertical drift takes every event, so the
        // transcript doesn't drift, exactly as before content had a say.
        let (swipes, taken) = run(&[(20., 3.), (25., -2.), (30., 4.)], flat);
        assert_eq!(swipes, [Swipe::Right]);
        assert!(taken.iter().all(|&t| t), "{taken:?}");
    }
}
