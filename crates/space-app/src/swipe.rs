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
    /// Fingers down, not yet a swipe or a scroll: its travel so far.
    Deciding(f32, f32),
    /// It swiped: the rest of it and its momentum are taken, until the next gesture.
    Swiped,
    /// It went up or down: a scroll.
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
    /// comes as `Moved` after `Ended`). `dx > 0` reads as `Right`.
    pub(crate) fn feed(&mut self, phase: TouchPhase, dx: f32, dy: f32) -> Fed {
        match phase {
            TouchPhase::Started => self.gesture = Gesture::Deciding(0., 0.),
            TouchPhase::Moved => {}
            // Ended, cancelled: a swipe keeps taking its momentum; anything else is over.
            _ => {
                let take = match self.gesture {
                    Gesture::Swiped => true,
                    Gesture::Deciding(x, y) => x.abs() >= y.abs(),
                    _ => false,
                };
                if self.gesture != Gesture::Swiped {
                    self.gesture = Gesture::None;
                }
                return Fed { take, swipe: None };
            }
        }
        let (x, y) = match self.gesture {
            Gesture::Deciding(x, y) => (x + dx, y + dy),
            Gesture::Swiped => return Fed { take: true, swipe: None },
            Gesture::None | Gesture::Scrolling => return Fed::default(),
        };
        if y.abs() > VERTICAL && y.abs() > x.abs() {
            self.gesture = Gesture::Scrolling;
            return Fed::default();
        }
        if x.abs() >= TRIGGER && x.abs() >= 2. * y.abs() {
            self.gesture = Gesture::Swiped;
            return Fed { take: true, swipe: Some(if x > 0. { Swipe::Right } else { Swipe::Left }) };
        }
        self.gesture = Gesture::Deciding(x, y);
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
    /// A scroll anywhere in the window: show or hide the swipe's pane once a gesture swipes.
    /// A trackpad scroll anywhere in the window, before anything under the pointer sees it
    /// (capture phase): take a swipe's scrolling, and show or hide the swipe's pane once.
    pub(crate) fn scrolled(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.settings.settings.trackpad.swipe;
        // Only a trackpad's continuous deltas (a mouse wheel's notches are lines).
        let ScrollDelta::Pixels(delta) = event.delta else { return };
        if pane == SwipePane::Off || event.source != ScrollSource::Pointer {
            return;
        }
        let fed = self.swipe.feed(event.touch_phase, f32::from(delta.x), f32::from(delta.y));
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

    fn gesture(steps: &[(f32, f32)]) -> Vec<Swipe> {
        let mut t = SwipeTracker::default();
        let mut out: Vec<Swipe> = t.feed(Started, 0., 0.).swipe.into_iter().collect();
        for &(dx, dy) in steps {
            out.extend(t.feed(Moved, dx, dy).swipe);
        }
        out.extend(t.feed(Ended, 0., 0.).swipe);
        out
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
        assert_eq!(t.feed(Moved, 200., 0.).swipe, None);
        t.feed(Started, 0., 0.);
        assert_eq!(t.feed(Moved, 100., 0.).swipe, Some(Swipe::Right));
        assert_eq!(t.feed(Moved, 100., 0.).swipe, None, "once per gesture");
        t.feed(Ended, 0., 0.);
        assert_eq!(t.feed(Moved, -200., 0.).swipe, None);
    }

    #[test]
    fn a_swipe_takes_its_scrolling_and_momentum() {
        let mut t = SwipeTracker::default();
        let take = |f: Fed| f.take;
        assert!(take(t.feed(Started, 0., 0.)));
        assert!(take(t.feed(Moved, 30., 2.)), "leaning sideways: may still swipe");
        assert!(take(t.feed(Moved, 50., 3.)), "the swipe");
        assert!(take(t.feed(Moved, 40., -5.)), "the rest of it");
        assert!(take(t.feed(Ended, 0., 0.)));
        assert!(take(t.feed(Moved, 20., 4.)), "momentum");
        // The next gesture goes down: not taken.
        t.feed(Started, 0., 0.);
        assert!(!take(t.feed(Moved, 0., 10.)));
    }

    #[test]
    fn a_scroll_is_never_taken() {
        let mut t = SwipeTracker::default();
        t.feed(Started, 0., 0.);
        assert!(!t.feed(Moved, 2., 12.).take);
        assert!(!t.feed(Moved, 1., 20.).take, "now a scroll");
        assert!(!t.feed(Moved, 60., 0.).take, "sideways later stays a scroll");
        assert!(!t.feed(Ended, 0., 0.).take);
        assert!(!t.feed(Moved, 0., 30.).take, "its momentum");
    }
}
