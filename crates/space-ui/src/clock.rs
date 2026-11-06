//! App-wide animation clock. Every animated component reads its phase from here instead of
//! wall-clock time, so development tools can pause, slow down, step or pin all motion at
//! once (and a pinned or paused frame stays exactly put).
//!
//! In normal use the clock is live at 1×, and components request animation frames only
//! while it is running. With the system's reduced-motion setting, animations render a
//! still frame.

use std::time::{Duration, Instant};

use gpui::{App, Global, Window};

#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClock {
    /// Animation time accumulated before `origin`, in seconds.
    base: f64,
    /// Wall-clock moment `base` refers to; `None` while paused.
    origin: Option<Instant>,
    speed: f64,
    /// Overrides every animation's phase (0..1) while set.
    pinned: Option<f32>,
}

impl Default for AnimationClock {
    fn default() -> Self {
        Self { base: 0., origin: Some(Instant::now()), speed: 1., pinned: None }
    }
}

impl Global for AnimationClock {}

/// Speeds offered by the dev controls.
pub const SPEEDS: [f64; 3] = [0.1, 0.25, 1.];
/// One step while paused: a 60 Hz frame.
pub const STEP: Duration = Duration::from_micros(16_667);

impl AnimationClock {
    /// The app's clock (installed by [`crate::init`]).
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Change the clock and repaint every window.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Self)) {
        f(cx.global_mut::<Self>());
        cx.refresh_windows();
    }

    /// Phase (0..1) of an animation with period `cycle` at the current animation time, and
    /// request the next frame when the clock is running. Components call this in `render`.
    pub fn phase_for(cycle: Duration, window: &mut Window, cx: &App) -> f32 {
        if cx.reduce_motion() {
            return 0.;
        }
        let clock = Self::global(cx);
        if clock.is_running() {
            window.request_animation_frame();
        }
        clock.phase(cycle)
    }

    pub fn now(&self) -> f64 {
        self.now_at(Instant::now())
    }

    fn now_at(&self, now: Instant) -> f64 {
        match self.origin {
            Some(origin) => self.base + now.saturating_duration_since(origin).as_secs_f64() * self.speed,
            None => self.base,
        }
    }

    pub fn phase(&self, cycle: Duration) -> f32 {
        self.phase_at(cycle, Instant::now())
    }

    fn phase_at(&self, cycle: Duration, now: Instant) -> f32 {
        if let Some(p) = self.pinned {
            return p.clamp(0., 1.);
        }
        let cycle = cycle.as_secs_f64().max(1e-6);
        (self.now_at(now) / cycle).rem_euclid(1.) as f32
    }

    /// Time is advancing and nothing is pinned, so animations need new frames.
    pub fn is_running(&self) -> bool {
        self.origin.is_some() && self.pinned.is_none()
    }

    pub fn is_paused(&self) -> bool {
        self.origin.is_none()
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn pinned(&self) -> Option<f32> {
        self.pinned
    }

    pub fn pause(&mut self) {
        self.pause_at(Instant::now());
    }

    fn pause_at(&mut self, now: Instant) {
        if self.origin.is_some() {
            self.base = self.now_at(now);
            self.origin = None;
        }
    }

    pub fn resume(&mut self) {
        if self.origin.is_none() {
            self.origin = Some(Instant::now());
        }
    }

    pub fn toggle_pause(&mut self) {
        if self.is_paused() { self.resume() } else { self.pause() }
    }

    /// Advance a paused clock by `dt` of animation time (no-op while running).
    pub fn step(&mut self, dt: Duration) {
        if self.origin.is_none() {
            self.base += dt.as_secs_f64();
        }
    }

    /// Change the speed without a jump in animation time.
    pub fn set_speed(&mut self, speed: f64) {
        self.set_speed_at(speed, Instant::now());
    }

    fn set_speed_at(&mut self, speed: f64, now: Instant) {
        if self.origin.is_some() {
            self.base = self.now_at(now);
            self.origin = Some(now);
        }
        self.speed = speed.max(0.);
    }

    /// Pin every animation to `phase` (0..1), or unpin with `None`.
    pub fn pin(&mut self, phase: Option<f32>) {
        self.pinned = phase.map(|p| p.clamp(0., 1.));
    }

    /// Apply a `SPACE_CLOCK`-style spec: `live`, `paused`, `pin:<0..1>` or `speed:<x>`.
    pub fn apply_spec(&mut self, spec: &str) -> Result<(), String> {
        let spec = spec.trim();
        match spec.split_once(':') {
            None if spec == "live" => self.reset(),
            None if spec == "paused" => self.pause(),
            Some(("pin", v)) => self.pin(Some(v.parse::<f32>().map_err(|e| format!("bad phase {v:?}: {e}"))?)),
            Some(("speed", v)) => self.set_speed(v.parse::<f64>().map_err(|e| format!("bad speed {v:?}: {e}"))?),
            _ => return Err(format!("unknown clock spec {spec:?} (live | paused | pin:<0..1> | speed:<x>)")),
        }
        Ok(())
    }

    /// Back to live at 1×.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CYCLE: Duration = Duration::from_secs(2);

    fn clock_at(t0: Instant) -> AnimationClock {
        AnimationClock { base: 0., origin: Some(t0), speed: 1., pinned: None }
    }

    #[test]
    fn live_phase_wraps() {
        let t0 = Instant::now();
        let c = clock_at(t0);
        assert!((c.phase_at(CYCLE, t0 + Duration::from_millis(500)) - 0.25).abs() < 1e-4);
        assert!((c.phase_at(CYCLE, t0 + Duration::from_millis(2500)) - 0.25).abs() < 1e-4);
        assert!(c.is_running());
    }

    #[test]
    fn pause_freezes_and_step_advances() {
        let t0 = Instant::now();
        let mut c = clock_at(t0);
        c.pause_at(t0 + Duration::from_secs(1));
        assert!(c.is_paused() && !c.is_running());
        let later = t0 + Duration::from_secs(30);
        assert!((c.phase_at(CYCLE, later) - 0.5).abs() < 1e-4);
        c.step(Duration::from_millis(500));
        assert!((c.phase_at(CYCLE, later) - 0.75).abs() < 1e-4);
    }

    #[test]
    fn speed_change_keeps_time_continuous() {
        let t0 = Instant::now();
        let mut c = clock_at(t0);
        let t1 = t0 + Duration::from_secs(1);
        c.set_speed_at(0.25, t1);
        assert!((c.now_at(t1) - 1.0).abs() < 1e-6);
        assert!((c.now_at(t1 + Duration::from_secs(4)) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn specs_parse() {
        let mut c = AnimationClock::default();
        c.apply_spec("pin:0.35").unwrap();
        assert_eq!(c.pinned(), Some(0.35));
        c.apply_spec("speed:0.25").unwrap();
        assert_eq!(c.speed(), 0.25);
        c.apply_spec("paused").unwrap();
        assert!(c.is_paused());
        c.apply_spec("live").unwrap();
        assert!(c.is_running() && c.speed() == 1.);
        assert!(c.apply_spec("pin:x").is_err());
        assert!(c.apply_spec("warp").is_err());
    }

    #[test]
    fn pin_overrides_phase() {
        let t0 = Instant::now();
        let mut c = clock_at(t0);
        c.pin(Some(0.4));
        assert!(!c.is_running());
        assert_eq!(c.phase_at(CYCLE, t0 + Duration::from_secs(7)), 0.4);
        assert_eq!(c.phase_at(Duration::from_millis(300), t0), 0.4);
        c.pin(Some(3.));
        assert_eq!(c.pinned(), Some(1.));
        c.pin(None);
        assert!(c.is_running());
    }
}
