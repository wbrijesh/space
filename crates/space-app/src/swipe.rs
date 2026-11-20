//! Two-finger swipes on the trackpad show and hide one pane (Settings ▸ Trackpad): the right pane
//! by default, or the sidebar. The swipe's direction is the pane's: the right pane comes in with
//! a swipe to the left and goes with one to the right; the sidebar the other way round. Each
//! direction only ever shows or only ever hides, so it works wherever the pointer is.
//!
//! A swipe is one gesture (fingers down to fingers up) that travels sideways far enough before it
//! travels as far up or down; it acts once. Scrolling isn't taken from anything: lists and the
//! transcript scroll as they always do.

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

/// One gesture's travel so far; `None` between gestures, and once a gesture has swiped or turned
/// out to be a scroll.
#[derive(Debug, Default)]
pub(crate) struct SwipeTracker {
    travel: Option<(f32, f32)>,
}

impl SwipeTracker {
    /// One scroll event of a trackpad gesture (its deltas as the platform reports them). Returns
    /// the swipe once, as the gesture becomes one; `dx > 0` reads as `Right`.
    pub(crate) fn feed(&mut self, phase: TouchPhase, dx: f32, dy: f32) -> Option<Swipe> {
        match phase {
            TouchPhase::Started => self.travel = Some((0., 0.)),
            TouchPhase::Moved => {}
            // Ended, cancelled: the gesture is over (momentum after it doesn't count).
            _ => {
                self.travel = None;
                return None;
            }
        }
        let (x, y) = self.travel.as_mut()?;
        *x += dx;
        *y += dy;
        let (x, y) = (*x, *y);
        if y.abs() > VERTICAL && y.abs() > x.abs() {
            self.travel = None;
            return None;
        }
        if x.abs() < TRIGGER || x.abs() < 2. * y.abs() {
            return None;
        }
        self.travel = None;
        Some(if x > 0. { Swipe::Right } else { Swipe::Left })
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
    pub(crate) fn scrolled(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.settings.settings.trackpad.swipe;
        // Only a trackpad's continuous deltas (a mouse wheel's notches are lines).
        let ScrollDelta::Pixels(delta) = event.delta else { return };
        if pane == SwipePane::Off || event.source != ScrollSource::Pointer {
            return;
        }
        let Some(swipe) = self.swipe.feed(event.touch_phase, f32::from(delta.x), f32::from(delta.y)) else { return };
        // Which way the fingers went.
        let swipe = match (swipe, natural_scrolling()) {
            (s, true) => s,
            (Swipe::Left, false) => Swipe::Right,
            (Swipe::Right, false) => Swipe::Left,
        };
        match pane {
            SwipePane::RightPane => {
                let open = self.selected.is_some_and(|key| self.pane_open(key));
                if open != (swipe == Swipe::Left) {
                    self.toggle_right_pane(window, cx);
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

    use super::{Swipe, SwipeTracker};

    fn gesture(steps: &[(f32, f32)]) -> Vec<Swipe> {
        let mut t = SwipeTracker::default();
        let mut out: Vec<Swipe> = t.feed(Started, 0., 0.).into_iter().collect();
        for &(dx, dy) in steps {
            out.extend(t.feed(Moved, dx, dy));
        }
        out.extend(t.feed(Ended, 0., 0.));
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
        assert_eq!(t.feed(Moved, 200., 0.), None);
        assert_eq!(t.feed(Started, 0., 0.), None);
        assert_eq!(t.feed(Moved, 100., 0.), Some(Swipe::Right));
        assert_eq!(t.feed(Moved, 100., 0.), None, "once per gesture");
        assert_eq!(t.feed(Ended, 0., 0.), None);
        assert_eq!(t.feed(Moved, -200., 0.), None);
    }
}
