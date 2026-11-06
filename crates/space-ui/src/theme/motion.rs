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

/// Smooth start and end (cubic). Used for the spinner's rotation.
pub fn ease_in_out(t: f32) -> f32 {
    gpui::ease_in_out(t)
}

/// Smooth 0 → 1 → 0 bump over `t` in 0..1 (sin²). Used for the dot pulse.
pub fn pulse(t: f32) -> f32 {
    (std::f32::consts::PI * t.clamp(0., 1.)).sin().powi(2)
}

/// For the gallery's Tokens page.
pub const ALL: &[(&str, Duration, &str)] = &[
    ("SPINNER_CYCLE", SPINNER_CYCLE, "ease_in_out rotation, 8 spokes, 1.3px, 60–100% radius"),
    ("DOTS_CYCLE", DOTS_CYCLE, "sin² pulse, 16% stagger, 50% pulse window"),
];
