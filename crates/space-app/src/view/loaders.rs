//! Activity indicators: a calm spoke spinner for the thread view and a three-dot pulse for
//! sidebar rows. Both are GPUI animations (static under the system's reduced-motion setting).

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::*;

/// Spoke spinner: rotates once per cycle with ease-in-out, so it speeds up and slows down.
/// Spokes run from 45% to 100% of the radius; 6 spokes leave wide gaps.
pub const SPINNER_CYCLE: Duration = Duration::from_millis(1600);
pub const SPINNER_SPOKES: usize = 6;
pub const SPINNER_STROKE: f32 = 1.6;

pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    div().size(px(size)).flex_none().with_animation(
        id.into(),
        Animation::new(SPINNER_CYCLE).repeat().with_easing(ease_in_out),
        move |el, delta| {
            el.child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let center = bounds.center();
                        let r_out = size / 2. - SPINNER_STROKE / 2.;
                        let r_in = r_out * 0.45;
                        let rotation = delta * TAU;
                        for i in 0..SPINNER_SPOKES {
                            let a = rotation + i as f32 * TAU / SPINNER_SPOKES as f32 - PI / 2.;
                            let (sin, cos) = a.sin_cos();
                            let mut path = PathBuilder::stroke(px(SPINNER_STROKE));
                            path.move_to(point(center.x + px(r_in * cos), center.y + px(r_in * sin)));
                            path.line_to(point(center.x + px(r_out * cos), center.y + px(r_out * sin)));
                            if let Ok(path) = path.build() {
                                window.paint_path(path, color);
                            }
                        }
                    },
                )
                .size_full(),
            )
        },
    )
}

/// Three-dot pulse: each dot brightens and lifts in turn.
pub const DOTS_CYCLE: Duration = Duration::from_millis(1300);
pub const DOT_SIZE: f32 = 4.;
pub const DOT_GAP: f32 = 3.5;
/// Fraction of the cycle between consecutive dots starting their pulse.
const DOT_STAGGER: f32 = 0.16;
/// Fraction of the cycle one dot's pulse lasts.
const DOT_PULSE: f32 = 0.5;

pub fn dots(id: impl Into<ElementId>, color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(DOT_GAP))
        .h(px(DOT_SIZE + 4.))
        .flex_none()
        .with_animation(id.into(), Animation::new(DOTS_CYCLE).repeat(), move |el, delta| {
            el.children((0..3).map(|i| {
                let t = (delta - i as f32 * DOT_STAGGER).rem_euclid(1.);
                // Smooth bump 0 → 1 → 0 over the pulse window, rest otherwise.
                let pulse = if t < DOT_PULSE { (PI * t / DOT_PULSE).sin().powi(2) } else { 0. };
                div()
                    .size(px(DOT_SIZE))
                    .rounded_full()
                    .bg(color)
                    .opacity(0.3 + 0.7 * pulse)
                    .relative()
                    .top(px(-1.5 * pulse))
            }))
        })
}
