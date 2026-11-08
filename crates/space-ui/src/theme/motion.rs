//! Motion tokens. Animated components read time from [`crate::clock::AnimationClock`] and
//! use these cycles and curves.

use std::time::Duration;

/// Spoke spinner: one rotation per cycle, eased in and out (so it speeds up and slows down).
pub const SPINNER_CYCLE: Duration = Duration::from_millis(1600);
pub const SPINNER_SPOKES: usize = 8;
pub const SPINNER_STROKE: f32 = 1.3;
/// Spokes run from this fraction of the radius to the edge.
pub const SPINNER_INNER: f32 = 0.6;

/// Three-dot pulse: each dot brightens and lifts in turn.
pub const DOTS_CYCLE: Duration = Duration::from_millis(1300);
pub const DOT_SIZE: f32 = 4.;
pub const DOT_GAP: f32 = 3.5;
/// Fraction of the cycle between consecutive dots starting their pulse.
pub const DOT_STAGGER: f32 = 0.16;
/// Fraction of the cycle one dot's pulse lasts.
pub const DOT_PULSE: f32 = 0.5;
/// Opacity of a resting dot; a pulsing dot rises to 1.
pub const DOT_REST_OPACITY: f32 = 0.3;
/// How far a dot lifts at the top of its pulse (px).
pub const DOT_LIFT: f32 = 1.5;

/// Scroll-to-bottom glide (button or ⌘↓), eased out, aimed at the live bottom.
pub const SCROLL_GLIDE: Duration = Duration::from_millis(200);

/// Floating buttons (scroll to bottom) fade in while scaling up from [`POP_SCALE`], and fade out.
pub const POP: Duration = Duration::from_millis(160);
pub const POP_SCALE: f32 = 0.92;

/// Sidebar show/hide: a critically damped spring (no overshoot) with this response, like the
/// spring macOS and SwiftUI use for split views. Most of the travel happens in the first
/// ~250ms; it then settles softly. See [`Spring`].
pub const SIDEBAR_RESPONSE: Duration = Duration::from_millis(350);

/// Fast start, gentle stop (cubic). Used for the toggle knob.
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// Smooth start and end (cubic). Used for the spinner's rotation.
pub fn ease_in_out(t: f32) -> f32 {
    gpui::ease_in_out(t)
}

/// Smooth 0 → 1 → 0 bump over `t` in 0..1 (sin²). Used for the dot pulse.
pub fn pulse(t: f32) -> f32 {
    (std::f32::consts::PI * t.clamp(0., 1.)).sin().powi(2)
}

/// A value (usually 0..1) moving toward a target on a critically damped spring, timed on the
/// animation clock. Retargeting mid-flight keeps the current position and velocity, so a
/// reversal turns smoothly instead of jumping.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    response: Duration,
    target: f32,
    /// Position and velocity when the current motion began, and when it began (clock time);
    /// `None` once settled at `target`.
    start: Option<(f32, f32, f64)>,
}

impl Spring {
    /// Settled at `value`.
    pub fn new(value: f32, response: Duration) -> Self {
        Self { response, target: value, start: None }
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    fn omega(&self) -> f64 {
        std::f64::consts::TAU / self.response.as_secs_f64().max(1e-3)
    }

    /// Position and velocity at clock time `now` (`pinned`: the dev clock's pinned phase, read
    /// as a fraction of the settle time).
    fn state(&self, now: f64, pinned: Option<f32>) -> (f32, f32) {
        let Some((x0, v0, since)) = self.start else { return (self.target, 0.) };
        let w = self.omega();
        let t = match pinned {
            // ωt ≈ 9.2 is within 0.1% of the target.
            Some(p) => p.clamp(0., 1.) as f64 * 9.2 / w,
            None => (now - since).max(0.),
        };
        let c1 = (x0 - self.target) as f64;
        let c2 = v0 as f64 + w * c1;
        let decay = (-w * t).exp();
        ((self.target as f64 + (c1 + c2 * t) * decay) as f32, ((c2 - w * (c1 + c2 * t)) * decay) as f32)
    }

    /// The value at clock time `now`, and whether it's still moving.
    pub fn at(&self, now: f64, pinned: Option<f32>) -> (f32, bool) {
        let (x, v) = self.state(now, pinned);
        let moving = self.start.is_some() && ((x - self.target).abs() > 1e-3 || v.abs() > 1e-2);
        (if moving { x } else { self.target }, moving)
    }

    /// Head for `target` from wherever it is at `now`.
    pub fn set(&mut self, target: f32, now: f64, pinned: Option<f32>) {
        let (x, v) = self.state(now, pinned);
        self.target = target;
        self.start = Some((x, v, now));
    }

    /// Jump to `value` (reduced motion).
    pub fn snap(&mut self, value: f32) {
        self.target = value;
        self.start = None;
    }
}

/// For the gallery's Tokens page.
pub const ALL: &[(&str, Duration, &str)] = &[
    ("SPINNER_CYCLE", SPINNER_CYCLE, "ease_in_out rotation, 8 spokes, 1.3px, 60–100% radius"),
    ("DOTS_CYCLE", DOTS_CYCLE, "sin² pulse, 16% stagger, 50% pulse window"),
    ("SCROLL_GLIDE", SCROLL_GLIDE, "ease_out glide to the bottom (scroll-to-bottom button, ⌘↓)"),
    ("POP", POP, "ease_out fade + scale from 0.92 (scroll-to-bottom button)"),
    ("SIDEBAR_RESPONSE", SIDEBAR_RESPONSE, "critically damped spring: sidebar slides in/out, content resizes"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_settles_without_overshoot_and_reverses_smoothly() {
        let mut s = Spring::new(0., SIDEBAR_RESPONSE);
        s.set(1., 0., None);
        let mut last = 0.;
        for i in 1..=60 {
            let (x, _) = s.at(i as f64 * 0.01, None);
            assert!(x >= last && x <= 1., "monotonic, no overshoot: {x} after {last}");
            last = x;
        }
        assert!(s.at(0.25, None).0 > 0.9, "most of the way by 250ms");
        assert_eq!(s.at(1.0, None), (1., false));

        // Reverse at 100ms: no jump in position.
        let (x, _) = s.at(0.1, None);
        s.set(0., 0.1, None);
        assert!((s.at(0.1, None).0 - x).abs() < 1e-4);
        assert_eq!(s.at(2.0, None), (0., false));
    }
}
