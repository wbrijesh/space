//! Activity indicators driven by the [`AnimationClock`]: a spoke spinner (thread view) and a
//! three-dot pulse (sidebar rows).

use std::f32::consts::{PI, TAU};

use gpui::*;

use crate::clock::AnimationClock;
use crate::theme::{color, motion, size};

/// Spoke spinner, rotating once per [`motion::SPINNER_CYCLE`] with ease-in-out.
#[derive(IntoElement)]
pub struct Spinner {
    size: f32,
    color: Hsla,
}

impl Spinner {
    /// `size` in px; use the `size::SPINNER_*` tokens.
    pub fn new(size: f32) -> Self {
        Self { size, color: color::text_3() }
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let phase = AnimationClock::phase_for(motion::SPINNER_CYCLE, window, cx);
        let rotation = motion::ease_in_out(phase) * TAU;
        let Self { size, color } = self;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let center = bounds.center();
                let r_out = size / 2. - motion::SPINNER_STROKE / 2.;
                let r_in = r_out * motion::SPINNER_INNER;
                for i in 0..motion::SPINNER_SPOKES {
                    let a = rotation + i as f32 * TAU / motion::SPINNER_SPOKES as f32 - PI / 2.;
                    let (sin, cos) = a.sin_cos();
                    let mut path = PathBuilder::stroke(px(motion::SPINNER_STROKE));
                    path.move_to(point(center.x + px(r_in * cos), center.y + px(r_in * sin)));
                    path.line_to(point(center.x + px(r_out * cos), center.y + px(r_out * sin)));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                }
            },
        )
        .size(px(size))
        .flex_none()
    }
}

/// Three dots that brighten and lift in sequence ([`motion::DOTS_CYCLE`]).
#[derive(IntoElement)]
pub struct Dots {
    color: Hsla,
}

impl Dots {
    pub fn new() -> Self {
        Self { color: color::text_3() }
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }
}

impl Default for Dots {
    fn default() -> Self {
        Self::new()
    }
}

/// Pulse strength (0..1) of dot `i` at cycle phase `phase`.
pub fn dot_pulse(i: usize, phase: f32) -> f32 {
    let t = (phase - i as f32 * motion::DOT_STAGGER).rem_euclid(1.);
    if t < motion::DOT_PULSE { motion::pulse(t / motion::DOT_PULSE) } else { 0. }
}

impl RenderOnce for Dots {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let phase = AnimationClock::phase_for(motion::DOTS_CYCLE, window, cx);
        div()
            .flex()
            .items_center()
            .gap(px(motion::DOT_GAP))
            .h(px(motion::DOT_SIZE + 2. * motion::DOT_LIFT))
            .flex_none()
            .children((0..3).map(|i| {
                let p = dot_pulse(i, phase);
                div()
                    .size(px(motion::DOT_SIZE))
                    .rounded_full()
                    .bg(self.color)
                    .opacity(motion::DOT_REST_OPACITY + (1. - motion::DOT_REST_OPACITY) * p)
                    .relative()
                    .top(px(-motion::DOT_LIFT * p))
            }))
    }
}

/// Context-usage ring: muted track, accent arc from 12 o'clock clockwise.
#[derive(IntoElement)]
pub struct ContextRing {
    frac: f32,
}

impl ContextRing {
    pub fn new(frac: f32) -> Self {
        Self { frac: frac.clamp(0., 1.) }
    }
}

impl RenderOnce for ContextRing {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        const STROKE: f32 = 2.;
        let ring = size::CONTEXT_RING;
        // Any usage at all gets a visible sliver.
        let sweep = if self.frac > 0. { self.frac.max(0.04) } else { 0. };
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let center = bounds.center();
                let r = (ring - STROKE) / 2.;
                let arc = |from: f32, to: f32| {
                    let steps = ((to - from) * 64.).ceil().max(2.) as usize;
                    let mut path = PathBuilder::stroke(px(STROKE));
                    for i in 0..=steps {
                        let t = from + (to - from) * i as f32 / steps as f32;
                        let a = -PI / 2. + t * TAU;
                        let p = point(center.x + px(r * a.cos()), center.y + px(r * a.sin()));
                        if i == 0 { path.move_to(p) } else { path.line_to(p) }
                    }
                    path.build().ok()
                };
                if let Some(track) = arc(0., 1.) {
                    window.paint_path(track, color::border_strong());
                }
                if sweep > 0.
                    && let Some(progress) = arc(0., sweep)
                {
                    window.paint_path(progress, color::accent());
                }
            },
        )
        .size(px(ring))
        .flex_none()
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    vec![
        ComponentPreview::new(Area::Indicators, "Spinner", "Thread view activity; speed follows the animation clock.")
            .examples([size::SPINNER_SM, size::SPINNER_MD, size::SPINNER_LG].map(|s| {
                Example::new(format!("{s}px"), move |_, _| Spinner::new(s).into_any_element())
            })),
        ComponentPreview::new(Area::Indicators, "Dots", "Sidebar row while a turn is in progress.")
            .example(Example::new("default", |_, _| Dots::new().into_any_element())),
        ComponentPreview::new(Area::Indicators, "Context ring", "Context usage; numbers in the tooltip.").examples(
            [0., 0.01, 0.45, 0.9, 1.].map(|f| {
                Example::new(format!("{:.0}%", f * 100.), move |_, _| ContextRing::new(f).into_any_element())
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring GPUI's `test` macro in with `gpui::*`.
    use super::dot_pulse;
    use crate::theme::motion;

    #[test]
    fn dots_are_staggered() {
        // At the start of the cycle only the first dot is starting; later dots follow.
        assert_eq!(dot_pulse(0, 0.), 0.);
        let peak0 = motion::DOT_PULSE / 2.;
        assert!(dot_pulse(0, peak0) > 0.99);
        assert!(dot_pulse(1, peak0) < dot_pulse(0, peak0));
        assert!(dot_pulse(1, peak0 + motion::DOT_STAGGER) > 0.99);
    }
}
