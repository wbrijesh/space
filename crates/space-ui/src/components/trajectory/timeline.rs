//! The timing overview above the ledger: one lane each for inputs, model requests and tools.
//!
//! [`layout`] places every row on a line of "units": in [`DurationMode::Sequence`] each row
//! gets one equal slot, in order; in [`DurationMode::Duration`] a unit is a millisecond of real
//! time, with long idle gaps squeezed. The element maps a zoomed window of that line onto its
//! width and turns gestures into a time range (filtering the ledger) or a selected row.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::trace::{RecordKind, RecordStatus, TrajectoryRow};

use crate::theme::{color, radius, text};

/// Gaps with nothing running longer than this are squeezed…
pub const IDLE_MS: f64 = 1000.;
/// …to this many units (ms) wide.
pub const IDLE_UNITS: f64 = 300.;
/// Zooming in stops at a window this narrow (in slots, or this many ms × 50).
pub const MIN_SPAN: f64 = 4.;
/// Hovering this long shows a span's details.
pub const HOVER_DELAY: Duration = Duration::from_millis(500);

const LANE_H: f32 = 12.;
const LANE_GAP: f32 = 2.;
const PAD_Y: f32 = 4.;
const LABEL_W: f32 = 46.;
/// A zero-width span (an input in Duration mode) is drawn this wide.
const MARKER_W: f32 = 3.;

/// How the overview spaces its spans.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DurationMode {
    /// Equal width per row, in order.
    #[default]
    Sequence,
    /// Real time, idle gaps squeezed.
    Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Input,
    Model,
    Tools,
}

impl Lane {
    pub fn of(kind: RecordKind) -> Lane {
        match kind {
            RecordKind::System | RecordKind::User | RecordKind::Context => Lane::Input,
            RecordKind::Assistant => Lane::Model,
            RecordKind::Tool => Lane::Tools,
        }
    }

    fn index(self) -> usize {
        match self {
            Lane::Input => 0,
            Lane::Model => 1,
            Lane::Tools => 2,
        }
    }
}

/// A time range in Unix ms (inclusive): rows active at any moment in it pass the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from_ms: u64,
    pub to_ms: u64,
}

/// One row placed on the line.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub id: SharedString,
    pub kind: RecordKind,
    pub lane: Lane,
    pub x0: f64,
    pub x1: f64,
    /// Model: where the first token arrived (TTFT before, decoding after).
    pub first_token_x: Option<f64>,
    /// Tools: where the guard let the call go (held before).
    pub held_x: Option<f64>,
    pub start_ms: u64,
    /// Absent while running.
    pub end_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub held_ms: u64,
    pub status: RecordStatus,
}

/// A stretch of the line in Duration mode: units `[u0, u1]` stand for times `[t0, t1]` (a
/// squeezed gap maps a long time onto few units).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    u0: f64,
    u1: f64,
    t0: f64,
    t1: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineLayout {
    pub spans: Vec<Span>,
    /// Width of the line, in units.
    pub total: f64,
    pub mode: DurationMode,
    segments: Vec<Segment>,
}

/// When a row ends (running rows: `now`; inputs: when they started).
fn end_of(row: &TrajectoryRow, now_ms: u64) -> Option<u64> {
    let start = row.started_ms?;
    Some(match row.duration_ms {
        Some(d) => start + d,
        None if row.status == RecordStatus::Running => now_ms.max(start),
        None => start,
    })
}

/// Place `rows` (with a start time) on the line.
pub fn layout(rows: &[TrajectoryRow], mode: DurationMode, now_ms: u64) -> TimelineLayout {
    let mut timed: Vec<(usize, &TrajectoryRow, u64, u64)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| Some((i, r, r.started_ms?, end_of(r, now_ms)?)))
        .collect();
    timed.sort_by_key(|(i, _, start, _)| (*start, *i));
    let span = |r: &TrajectoryRow, start: u64, end: u64, x0: f64, x1: f64, at: &dyn Fn(f64) -> f64| Span {
        id: r.id.clone().into(),
        kind: r.kind,
        lane: Lane::of(r.kind),
        x0,
        x1,
        first_token_x: r.first_token_ms.filter(|_| r.kind == RecordKind::Assistant).map(|t| at(t as f64)),
        held_x: (r.kind == RecordKind::Tool && r.held_ms > 0).then(|| at((start + r.held_ms).min(end) as f64)),
        start_ms: start,
        end_ms: r.duration_ms.map(|d| start + d),
        first_token_ms: r.first_token_ms,
        held_ms: r.held_ms,
        status: r.status,
    };
    match mode {
        DurationMode::Sequence => {
            let spans = timed
                .iter()
                .enumerate()
                .map(|(slot, (_, r, start, end))| {
                    let (x0, x1) = (slot as f64, slot as f64 + 1.);
                    // Inside its slot, times are proportional to the row's own duration.
                    let len = (*end - *start).max(1) as f64;
                    let at = move |t: f64| x0 + ((t - *start as f64) / len).clamp(0., 1.);
                    span(r, *start, *end, x0, x1, &at)
                })
                .collect::<Vec<_>>();
            TimelineLayout { total: spans.len() as f64, spans, mode, segments: Vec::new() }
        }
        DurationMode::Duration => {
            let segments = segments(timed.iter().map(|(_, _, s, e)| (*s as f64, *e as f64)));
            let total = segments.last().map_or(0., |s| s.u1);
            let at = |t: f64| unit_at(&segments, t);
            let spans = timed.iter().map(|(_, r, start, end)| span(r, *start, *end, at(*start as f64), at(*end as f64), &at)).collect();
            TimelineLayout { spans, total, mode, segments }
        }
    }
}

/// The line's segments for intervals `[start, end]` (ms): covered time 1 ms = 1 unit; a gap
/// longer than [`IDLE_MS`] becomes [`IDLE_UNITS`] wide.
fn segments(intervals: impl Iterator<Item = (f64, f64)>) -> Vec<Segment> {
    let mut iv: Vec<(f64, f64)> = intervals.collect();
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Merge into covered stretches.
    let mut covered: Vec<(f64, f64)> = Vec::new();
    for (s, e) in iv {
        match covered.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => covered.push((s, e)),
        }
    }
    let mut out = Vec::new();
    let mut u = 0.;
    let mut prev_end: Option<f64> = None;
    for (s, e) in covered {
        if let Some(p) = prev_end {
            let gap = s - p;
            let w = if gap > IDLE_MS { IDLE_UNITS } else { gap };
            out.push(Segment { u0: u, u1: u + w, t0: p, t1: s });
            u += w;
        }
        out.push(Segment { u0: u, u1: u + (e - s), t0: s, t1: e });
        u += e - s;
        prev_end = Some(e);
    }
    out
}

fn unit_at(segments: &[Segment], t: f64) -> f64 {
    let Some(first) = segments.first() else { return 0. };
    if t <= first.t0 {
        return first.u0;
    }
    for seg in segments {
        if t <= seg.t1 {
            let f = if seg.t1 > seg.t0 { (t - seg.t0) / (seg.t1 - seg.t0) } else { 0. };
            return seg.u0 + f * (seg.u1 - seg.u0);
        }
    }
    segments.last().map_or(0., |s| s.u1)
}

fn time_at(segments: &[Segment], u: f64) -> f64 {
    let Some(first) = segments.first() else { return 0. };
    if u <= first.u0 {
        return first.t0;
    }
    for seg in segments {
        if u <= seg.u1 {
            let f = if seg.u1 > seg.u0 { (u - seg.u0) / (seg.u1 - seg.u0) } else { 0. };
            return seg.t0 + f * (seg.t1 - seg.t0);
        }
    }
    segments.last().map_or(0., |s| s.t1)
}

impl TimelineLayout {
    /// The time range units `[a, b]` stand for. Sequence mode: from the first start to the last
    /// end of the rows whose slots it touches.
    pub fn range_of(&self, a: f64, b: f64) -> Option<TimeRange> {
        let (a, b) = (a.min(b), a.max(b));
        match self.mode {
            DurationMode::Duration => {
                if self.segments.is_empty() {
                    return None;
                }
                Some(TimeRange { from_ms: time_at(&self.segments, a).floor() as u64, to_ms: time_at(&self.segments, b).ceil() as u64 })
            }
            DurationMode::Sequence => {
                let hit: Vec<&Span> = self.spans.iter().filter(|s| s.x1 > a && s.x0 < b.max(a + 1e-9)).collect();
                let from = hit.iter().map(|s| s.start_ms).min()?;
                let to = hit.iter().map(|s| s.end_ms.unwrap_or(s.start_ms)).max()?;
                Some(TimeRange { from_ms: from, to_ms: to })
            }
        }
    }

    /// The span in `lane` at unit `u` (markers count a little either side).
    pub fn span_at(&self, lane: Lane, u: f64, slop: f64) -> Option<&Span> {
        self.spans.iter().rev().find(|s| s.lane == lane && u >= s.x0 - slop && u <= s.x1 + slop)
    }

    /// Units `[from, to]` of a time range (to draw the selection).
    pub fn units_of(&self, range: TimeRange) -> Option<(f64, f64)> {
        match self.mode {
            DurationMode::Duration => (!self.segments.is_empty())
                .then(|| (unit_at(&self.segments, range.from_ms as f64), unit_at(&self.segments, range.to_ms as f64))),
            DurationMode::Sequence => {
                let hit: Vec<&Span> = self.spans.iter().filter(|s| span_active(s, range)).collect();
                Some((hit.iter().map(|s| s.x0).reduce(f64::min)?, hit.iter().map(|s| s.x1).reduce(f64::max)?))
            }
        }
    }
}

fn span_active(s: &Span, r: TimeRange) -> bool {
    s.start_ms <= r.to_ms && s.end_ms.unwrap_or(s.start_ms) >= r.from_ms
}

/// Whether `row` was active at any moment of `range` (rows without a time never are).
pub fn active_in(row: &TrajectoryRow, range: TimeRange, now_ms: u64) -> bool {
    match (row.started_ms, end_of(row, now_ms)) {
        (Some(start), Some(end)) => start <= range.to_ms && end >= range.from_ms,
        _ => false,
    }
}

/// The visible window `(v0, v1)` after zooming by `factor` (< 1 zooms in) around unit
/// `anchor`, which stays where it is on screen. Clamped to `[0, total]`, at least
/// `min_span` wide.
pub fn zoom(view: (f64, f64), anchor: f64, factor: f64, total: f64, min_span: f64) -> (f64, f64) {
    let (v0, v1) = view;
    let width = ((v1 - v0) * factor).clamp(min_span.min(total), total.max(min_span.min(total)));
    let f = if v1 > v0 { ((anchor - v0) / (v1 - v0)).clamp(0., 1.) } else { 0.5 };
    let mut n0 = anchor - f * width;
    n0 = n0.clamp(0., (total - width).max(0.));
    (n0, n0 + width)
}

/// The window moved by `delta` units, kept inside `[0, total]`.
pub fn pan(view: (f64, f64), delta: f64, total: f64) -> (f64, f64) {
    let width = view.1 - view.0;
    let v0 = (view.0 + delta).clamp(0., (total - width).max(0.));
    (v0, v0 + width)
}

// ----- the element ----------------------------------------------------------------------------

pub type RangeHandler = Rc<dyn Fn(Option<TimeRange>, &mut Window, &mut App)>;
pub type SelectHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

#[derive(Clone, Copy)]
enum DragKind {
    Select,
    Pan,
}

#[derive(Clone, Copy)]
struct Drag {
    kind: DragKind,
    from_x: f32,
    to_x: f32,
    view: (f64, f64),
    moved: bool,
}

/// The overview's own state across frames: the zoomed window, a drag, the hover.
#[derive(Default)]
struct State {
    /// `None`: the whole line.
    view: Option<(f64, f64)>,
    drag: Option<Drag>,
    hover: Option<(Point<Pixels>, Instant)>,
    /// The plot area's bounds at the last paint.
    plot: Bounds<Pixels>,
    /// The line's width when the view was set (a new layout resets a stale view).
    total: f64,
}

/// The overview over `rows`. `range` is the current filter (drawn as a selection).
#[allow(clippy::too_many_arguments)]
pub fn timeline(
    id: impl Into<SharedString>,
    rows: &[TrajectoryRow],
    mode: DurationMode,
    range: Option<TimeRange>,
    selected: Option<&str>,
    now_ms: u64,
    on_range: RangeHandler,
    on_select: SelectHandler,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let layout = Rc::new(layout(rows, mode, now_ms));
    let state = window.use_keyed_state(ElementId::Name(format!("{id}-state").into()), cx, |_, _| State::default());
    let (view, hover, drag, plot) = {
        let s = state.read(cx);
        let view = s.view.filter(|_| (s.total - layout.total).abs() < f64::EPSILON || mode == DurationMode::Duration);
        (view.unwrap_or((0., layout.total.max(1.))), s.hover, s.drag, s.plot)
    };
    let height = PAD_Y * 2. + LANE_H * 3. + LANE_GAP * 2.;
    let unit_at_x = move |x: Pixels, plot: Bounds<Pixels>, view: (f64, f64)| -> f64 {
        let w = f32::from(plot.size.width).max(1.);
        view.0 + ((f32::from(x - plot.origin.x) / w) as f64).clamp(0., 1.) * (view.1 - view.0)
    };
    let lane_at_y = move |y: Pixels, plot: Bounds<Pixels>| -> Option<Lane> {
        let dy = f32::from(y - plot.origin.y) - PAD_Y;
        let i = (dy / (LANE_H + LANE_GAP)).floor();
        match i as i32 {
            0 => Some(Lane::Input),
            1 => Some(Lane::Model),
            2 => Some(Lane::Tools),
            _ => None,
        }
    };

    // The hovered span, once the pointer has rested on it.
    let hovered = hover.and_then(|(pos, since)| {
        if drag.is_some() || !plot.contains(&pos) {
            return None;
        }
        if since.elapsed() < HOVER_DELAY {
            window.request_animation_frame();
            return None;
        }
        let lane = lane_at_y(pos.y, plot)?;
        let slop = (view.1 - view.0) * (MARKER_W as f64 / f32::from(plot.size.width).max(1.) as f64);
        layout.span_at(lane, unit_at_x(pos.x, plot, view), slop).cloned().map(|s| (pos, s))
    });

    let paint_layout = layout.clone();
    let selected_id: Option<SharedString> = selected.map(|s| SharedString::from(s.to_string()));
    let range_units = range.and_then(|r| layout.units_of(r));
    let drag_px = drag.filter(|d| matches!(d.kind, DragKind::Select) && d.moved).map(|d| (d.from_x.min(d.to_x), d.from_x.max(d.to_x)));
    let state_paint = state.clone();
    let plot_canvas = canvas(
        move |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            state_paint.update(cx, |s, _| s.plot = bounds);
            paint_spans(&paint_layout, view, bounds, selected_id.as_ref(), range_units, drag_px, window);
        },
    )
    .absolute()
    .top_0()
    .left(px(LABEL_W))
    .right_0()
    .h(px(height));

    let labels = div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(LABEL_W - 6.))
        .pt(px(PAD_Y - 1.))
        .flex()
        .flex_col()
        .items_end()
        .gap(px(LANE_GAP))
        .text_size(px(10.))
        .text_color(color::text_3())
        .children(["Input", "Model", "Tools"].map(|l| div().h(px(LANE_H)).flex().items_center().child(l)));

    let tooltip = hovered.map(|(pos, span)| {
        let local = pos - plot.origin;
        let left = (f32::from(local.x) + LABEL_W + 10.).min(f32::from(plot.size.width) + LABEL_W - 260.).max(0.);
        hover_card(&span, now_ms).absolute().top(px(height - 4.)).left(px(left))
    });

    let (s_down, s_move, s_up, s_up_out, s_rdown, s_rup, s_wheel, s_hover) =
        (state.clone(), state.clone(), state.clone(), state.clone(), state.clone(), state.clone(), state.clone(), state.clone());
    let (l_up, l_rup, l_wheel, l_move) = (layout.clone(), layout.clone(), layout.clone(), layout.clone());
    let (on_range_up, on_range_r, on_range_dbl) = (on_range.clone(), on_range.clone(), on_range);
    let total = layout.total.max(1.);
    let min_span = match mode {
        DurationMode::Sequence => MIN_SPAN,
        DurationMode::Duration => MIN_SPAN * 50.,
    };

    div()
        .id(id)
        .relative()
        .w_full()
        .h(px(height))
        .flex_none()
        .bg(color::trajectory_bg())
        .border_b_1()
        .border_color(color::border())
        .cursor_crosshair()
        .child(labels)
        .child(plot_canvas)
        .children(tooltip)
        .on_hover(move |hovered, window, cx| {
            if !hovered {
                s_hover.update(cx, |s, _| s.hover = None);
                window.refresh();
            }
        })
        .on_mouse_down(MouseButton::Left, move |e, window, cx| {
            if e.click_count >= 2 {
                on_range_dbl(None, window, cx);
                return;
            }
            let x = f32::from(e.position.x);
            s_down.update(cx, |s, _| s.drag = Some(Drag { kind: DragKind::Select, from_x: x, to_x: x, view, moved: false }));
            window.refresh();
        })
        .on_mouse_down(MouseButton::Right, move |e, window, cx| {
            let x = f32::from(e.position.x);
            s_rdown.update(cx, |s, _| s.drag = Some(Drag { kind: DragKind::Pan, from_x: x, to_x: x, view, moved: false }));
            window.refresh();
        })
        .on_mouse_move(move |e, window, cx| {
            let x = f32::from(e.position.x);
            s_move.update(cx, |s, _| {
                if let Some(d) = &mut s.drag {
                    d.to_x = x;
                    d.moved |= (d.to_x - d.from_x).abs() > 3.;
                    if let (DragKind::Pan, true) = (d.kind, d.moved) {
                        let w = f32::from(s.plot.size.width).max(1.) as f64;
                        let delta = -((d.to_x - d.from_x) as f64 / w) * (d.view.1 - d.view.0);
                        // Panning only means something when zoomed in.
                        if d.view.1 - d.view.0 < l_move.total - 1e-9 {
                            s.view = Some(pan(d.view, delta, l_move.total));
                            s.total = l_move.total;
                        }
                    }
                } else {
                    s.hover = Some((e.position, Instant::now()));
                }
            });
            window.refresh();
        })
        .on_mouse_up(MouseButton::Left, move |e, window, cx| {
            let Some(d) = s_up.update(cx, |s, _| s.drag.take()) else { return };
            let plot = s_up.read(cx).plot;
            if d.moved {
                let (a, b) = (unit_at_x(px(d.from_x), plot, d.view), unit_at_x(px(d.to_x), plot, d.view));
                on_range_up(l_up.range_of(a, b), window, cx);
            } else if let Some(lane) = lane_at_y(e.position.y, plot) {
                let slop = (d.view.1 - d.view.0) * (MARKER_W as f64 / f32::from(plot.size.width).max(1.) as f64);
                if let Some(span) = l_up.span_at(lane, unit_at_x(e.position.x, plot, d.view), slop) {
                    on_select(span.id.clone(), window, cx);
                }
            }
            window.refresh();
        })
        .on_mouse_up(MouseButton::Right, move |_, window, cx| {
            let Some(d) = s_rup.update(cx, |s, _| s.drag.take()) else { return };
            // A right-click (no drag) clears the filter.
            if !d.moved {
                let _ = &l_rup;
                on_range_r(None, window, cx);
            }
            window.refresh();
        })
        .on_mouse_up_out(MouseButton::Left, move |_, window, cx| {
            s_up_out.update(cx, |s, _| s.drag = None);
            window.refresh();
        })
        .on_scroll(move |e, window, cx| {
            let dy = f32::from(e.delta.pixel_delta(px(16.)).y) as f64;
            if dy.abs() < f64::EPSILON {
                return;
            }
            let pos = e.position;
            s_wheel.update(cx, |s, _| {
                let anchor = unit_at_x(pos.x, s.plot, view);
                s.view = Some(zoom(view, anchor, (dy * 0.0015).exp(), l_wheel.total.max(1.), min_span.min(total)));
                s.total = l_wheel.total;
            });
            window.refresh();
        })
}

fn lane_color(span: &Span) -> Hsla {
    match span.kind {
        RecordKind::System => color::text_3(),
        RecordKind::User => color::blue(),
        RecordKind::Context => color::green(),
        RecordKind::Assistant => color::violet(),
        RecordKind::Tool => color::yellow(),
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_spans(
    layout: &TimelineLayout,
    view: (f64, f64),
    bounds: Bounds<Pixels>,
    selected: Option<&SharedString>,
    range_units: Option<(f64, f64)>,
    drag_px: Option<(f32, f32)>,
    window: &mut Window,
) {
    let w = f32::from(bounds.size.width).max(1.);
    let (v0, v1) = view;
    let to_x = |u: f64| bounds.origin.x + px(((u - v0) / (v1 - v0).max(1e-9)) as f32 * w);
    // The selected range, behind the spans.
    if let Some((a, b)) = range_units {
        let (x0, x1) = (to_x(a.min(b)), to_x(a.max(b)));
        window.paint_quad(fill(Bounds::from_corners(point(x0, bounds.origin.y), point(x1.max(x0 + px(1.)), bounds.bottom())), color::accent().opacity(0.12)));
    }
    for span in &layout.spans {
        if span.x1 < v0 || span.x0 > v1 {
            continue;
        }
        let lane = span.lane.index() as f32;
        let y = bounds.origin.y + px(PAD_Y + lane * (LANE_H + LANE_GAP) + 3.);
        let h = px(LANE_H - 6.);
        let (mut x0, mut x1) = (to_x(span.x0), to_x(span.x1));
        // Equal slots get a hairline gap; zero-width spans a marker.
        if layout.mode == DurationMode::Sequence {
            x1 = (x1 - px(2.)).max(x0 + px(1.));
        }
        if x1 - x0 < px(MARKER_W) {
            x0 -= px(MARKER_W / 2.);
            x1 = x0 + px(MARKER_W);
        }
        let base = lane_color(span);
        let running = span.status == RecordStatus::Running;
        let alpha = if running { 0.6 } else { 0.9 };
        let rect = Bounds::from_corners(point(x0, y), point(x1, y + h));
        window.paint_quad(fill(rect, base.opacity(alpha)).corner_radii(px(2.)));
        // Model: time to first token in a lighter shade. Tools: the guard's hold dimmer.
        let split = span.first_token_x.or(span.held_x);
        if let Some(s) = split {
            let sx = to_x(s).clamp(x0, x1);
            if sx > x0 {
                window.paint_quad(fill(Bounds::from_corners(point(x0, y), point(sx, y + h)), color::bg().opacity(0.45)).corner_radii(px(2.)));
            }
        }
        if selected.is_some_and(|id| *id == span.id) {
            window.paint_quad(
                quad(rect.dilate(px(1.5)), px(3.), transparent_black(), px(1.), color::text(), BorderStyle::default()),
            );
        }
    }
    if let Some((a, b)) = drag_px {
        let rect = Bounds::from_corners(point(px(a), bounds.origin.y), point(px(b), bounds.bottom()));
        window.paint_quad(quad(rect, px(0.), color::accent().opacity(0.15), px(1.), color::accent(), BorderStyle::default()));
    }
}

/// Start → end, total, and TTFT · decoding (model) or held · running (tools).
fn hover_card(span: &Span, now_ms: u64) -> Div {
    let clock = |ms: u64| super::clock_time(ms);
    let dur = |ms: u64| super::duration_label(ms);
    let end = span.end_ms;
    let total = end.unwrap_or(now_ms).saturating_sub(span.start_ms);
    let mut lines = vec![match end {
        Some(e) => format!("{} → {}", clock(span.start_ms), clock(e)),
        None => format!("{} → running", clock(span.start_ms)),
    }];
    if span.lane != Lane::Input {
        lines.push(format!("Total {}", dur(total)));
    }
    if let Some(ft) = span.first_token_ms {
        let ttft = ft.saturating_sub(span.start_ms);
        let decode = end.map(|e| e.saturating_sub(ft));
        lines.push(match decode {
            Some(d) => format!("TTFT {} · decoding {}", dur(ttft), dur(d)),
            None => format!("TTFT {}", dur(ttft)),
        });
    }
    if span.held_ms > 0 {
        lines.push(format!("Held by the guard {}", dur(span.held_ms)));
    }
    div()
        .min_w(px(180.))
        .flex()
        .flex_col()
        .gap_0p5()
        .px_2p5()
        .py_1p5()
        .rounded(radius::LG)
        .bg(color::surface())
        .border_1()
        .border_color(color::border_strong())
        .shadow_md()
        .text_size(text::XS)
        .text_color(color::text_2())
        .font_family(text::MONO_FONT)
        .children(lines.into_iter().enumerate().map(|(i, l)| div().when(i == 0, |d| d.text_color(color::text())).child(l)))
}

#[cfg(test)]
mod tests {
    use space_core::protocol::trace::{RecordKind, RecordStatus, TrajectoryRow};

    use super::{DurationMode, IDLE_UNITS, TimeRange, active_in, layout, pan, zoom};

    fn row(id: &str, kind: RecordKind, start: u64, dur: Option<u64>) -> TrajectoryRow {
        TrajectoryRow { id: id.into(), kind, started_ms: Some(start), duration_ms: dur, ..Default::default() }
    }

    #[test]
    fn sequence_gives_each_row_one_slot_in_start_order() {
        let rows = [row("b", RecordKind::Assistant, 2000, Some(1000)), row("a", RecordKind::User, 1000, None), row("x", RecordKind::Tool, 0, None)];
        let mut no_time = row("n", RecordKind::Tool, 0, None);
        no_time.started_ms = None;
        let l = layout(&[rows[0].clone(), rows[1].clone(), no_time], DurationMode::Sequence, 0);
        assert_eq!(l.total, 2.);
        assert_eq!(l.spans.iter().map(|s| (s.id.as_ref(), s.x0, s.x1)).collect::<Vec<_>>(), [("a", 0., 1.), ("b", 1., 2.)]);
        // TTFT splits the slot by the row's own timing.
        let mut a = row("m", RecordKind::Assistant, 0, Some(400));
        a.first_token_ms = Some(100);
        let l = layout(&[a], DurationMode::Sequence, 0);
        assert_eq!(l.spans[0].first_token_x, Some(0.25));
    }

    #[test]
    fn duration_is_real_time_with_long_gaps_squeezed() {
        let rows = [
            row("u", RecordKind::User, 10_000, None),
            row("m", RecordKind::Assistant, 10_000, Some(2_000)),
            // 500 ms gap: kept.
            row("t", RecordKind::Tool, 12_500, Some(500)),
            // 60 s gap: squeezed to IDLE_UNITS.
            row("m2", RecordKind::Assistant, 73_000, Some(1_000)),
        ];
        let l = layout(&rows, DurationMode::Duration, 0);
        let x = |id: &str| l.spans.iter().find(|s| s.id.as_ref() == id).map(|s| (s.x0, s.x1)).unwrap();
        assert_eq!(x("u"), (0., 0.));
        assert_eq!(x("m"), (0., 2_000.));
        assert_eq!(x("t"), (2_500., 3_000.));
        assert_eq!(x("m2"), (3_000. + IDLE_UNITS, 3_000. + IDLE_UNITS + 1_000.));
        assert_eq!(l.total, 4_000. + IDLE_UNITS);
        // Units map back to times, through the squeezed gap too.
        let r = l.range_of(2_600., 3_000. + IDLE_UNITS + 500.).unwrap();
        assert_eq!((r.from_ms, r.to_ms), (12_600, 73_500));
    }

    #[test]
    fn range_filter_keeps_rows_active_anywhere_in_it() {
        let r = TimeRange { from_ms: 100, to_ms: 200 };
        assert!(active_in(&row("a", RecordKind::Tool, 50, Some(60)), r, 0));
        assert!(active_in(&row("b", RecordKind::Tool, 200, Some(10)), r, 0));
        assert!(active_in(&row("c", RecordKind::User, 150, None), r, 0));
        assert!(!active_in(&row("d", RecordKind::Tool, 0, Some(99)), r, 0));
        assert!(!active_in(&row("e", RecordKind::Tool, 201, Some(5)), r, 0));
        // Running rows last until now.
        let mut running = row("f", RecordKind::Assistant, 0, None);
        running.status = RecordStatus::Running;
        assert!(active_in(&running, r, 150));
        assert!(!active_in(&running, r, 90));
        // Sequence mode: a range is the touched rows' times.
        let l = layout(&[row("a", RecordKind::User, 100, None), row("b", RecordKind::Assistant, 120, Some(80))], DurationMode::Sequence, 0);
        assert_eq!(l.range_of(1.2, 1.8), Some(TimeRange { from_ms: 120, to_ms: 200 }));
        assert_eq!(l.units_of(TimeRange { from_ms: 150, to_ms: 160 }), Some((1., 2.)));
    }

    #[test]
    fn zoom_keeps_the_anchor_in_place_and_stays_inside() {
        let v = zoom((0., 100.), 25., 0.5, 100., 4.);
        assert_eq!(v, (12.5, 62.5));
        // The anchor is at the same fraction of the window.
        assert!(((25. - v.0) / (v.1 - v.0) - 0.25).abs() < 1e-9);
        // Never narrower than the minimum, never outside.
        let tight = zoom((10., 20.), 15., 0.01, 100., 4.);
        assert!((tight.1 - tight.0 - 4.).abs() < 1e-9 && tight.0 >= 0.);
        assert_eq!(zoom((0., 100.), 50., 3., 100., 4.), (0., 100.));
        assert_eq!(zoom((80., 100.), 99., 1.5, 100., 4.), (70., 100.));
        assert_eq!(pan((10., 30.), 100., 100.), (80., 100.));
        assert_eq!(pan((10., 30.), -50., 100.), (0., 20.));
    }
}
