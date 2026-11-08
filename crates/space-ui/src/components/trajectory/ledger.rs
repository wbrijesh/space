//! The ledger: one 30 px row per record, grouped by turn (a label in the gutter) and step (a
//! header with the request's status dot), virtualized.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::trace::{GuardVerdict, RecordKind, RecordStatus, RequestSummary, TrajectoryPage, TrajectoryRow};

use super::{FoldHandler, FoldKey, Handler, IdHandler, duration_label};
use crate::components::loaders::Spinner;
use crate::format;
use crate::theme::{color, radius, size, text};

/// Every ledger item is this tall.
pub const ROW_H: Pixels = px(30.);
const GUTTER_W: f32 = 52.;
const CHIP_W: f32 = 82.;

/// What one line of the ledger shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerItem {
    /// "Load earlier" (the page has older rows).
    LoadEarlier,
    /// A record: its index in the page's rows, whether it opens its turn, and, on the first row
    /// of a step, the step's request (a status dot in the gutter).
    Row { ix: usize, first_of_turn: bool, request: Option<u32> },
    /// A folded turn: one line for all of it.
    Turn { turn: u32, rows: usize, first: usize },
}

/// The ledger's lines for `page`: rows passing `keep` (a step's first model or tool row carries
/// its request's dot), folded turns as one line, folded steps without their tool rows.
pub fn ledger_items(page: &TrajectoryPage, folds: &HashSet<FoldKey>, keep: impl Fn(&TrajectoryRow) -> bool) -> Vec<LedgerItem> {
    let mut out = Vec::new();
    if page.older.is_some() {
        out.push(LedgerItem::LoadEarlier);
    }
    let mut turn_started: HashSet<u32> = HashSet::new();
    let mut headed: HashSet<(u32, u32)> = HashSet::new();
    let mut folded_turns: BTreeMap<u32, usize> = BTreeMap::new();
    for (ix, row) in page.rows.iter().enumerate() {
        if !keep(row) {
            continue;
        }
        if folds.contains(&FoldKey::Turn(row.turn)) {
            match folded_turns.get(&row.turn) {
                Some(&at) => {
                    if let LedgerItem::Turn { rows, .. } = &mut out[at] {
                        *rows += 1;
                    }
                }
                None => {
                    folded_turns.insert(row.turn, out.len());
                    turn_started.insert(row.turn);
                    out.push(LedgerItem::Turn { turn: row.turn, rows: 1, first: ix });
                }
            }
            continue;
        }
        let first_of_turn = turn_started.insert(row.turn);
        let model_or_tool = matches!(row.kind, RecordKind::Assistant | RecordKind::Tool);
        let starts_step = matches!((row.step, model_or_tool), (Some(step), true) if headed.insert((row.turn, step)));
        let request = row.request.filter(|_| starts_step);
        if row.kind == RecordKind::Tool
            && let Some(step) = row.step
            && folds.contains(&FoldKey::Step(row.turn, step))
        {
            continue;
        }
        out.push(LedgerItem::Row { ix, first_of_turn, request });
    }
    out
}

/// The step a row belongs to, for folding (turn: inputs and the system prompt).
pub fn fold_key_of(row: &TrajectoryRow) -> FoldKey {
    match (row.kind, row.step) {
        (RecordKind::Assistant | RecordKind::Tool, Some(step)) => FoldKey::Step(row.turn, step),
        _ => FoldKey::Turn(row.turn),
    }
}

/// The ledger's scroll position, kept by the app across frames: it follows new rows while at
/// the bottom; scrolling up stops that until the bottom is reached again.
#[derive(Clone)]
pub struct LedgerScroll {
    handle: UniformListScrollHandle,
    following: Rc<Cell<bool>>,
}

impl Default for LedgerScroll {
    fn default() -> Self {
        Self { handle: UniformListScrollHandle::new(), following: Rc::new(Cell::new(true)) }
    }
}

impl LedgerScroll {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_following(&self) -> bool {
        self.following.get()
    }

    /// Back to the newest row, following again.
    pub fn jump_to_end(&self) {
        self.following.set(true);
    }

    /// Bring item `ix` into view (stops following).
    pub fn reveal(&self, ix: usize) {
        self.following.set(false);
        self.handle.scroll_to_item(ix, ScrollStrategy::Center);
    }
}

/// What a ledger line needs to draw itself.
pub(super) struct LedgerData {
    pub page: Rc<TrajectoryPage>,
    pub items: Rc<Vec<LedgerItem>>,
    pub selected: Option<SharedString>,
    pub now_ms: u64,
}

pub(super) struct LedgerHandlers {
    pub on_select: IdHandler,
    pub on_toggle_fold: FoldHandler,
    pub on_load_earlier: Handler,
}

pub(super) fn ledger(data: LedgerData, scroll: &LedgerScroll, h: LedgerHandlers) -> impl IntoElement {
    let count = data.items.len();
    // Follow the tail; reaching the bottom again resumes following.
    if !scroll.following.get() && scroll.handle.is_scrolled_to_end() == Some(true) {
        scroll.following.set(true);
    }
    if scroll.following.get() && count > 0 {
        scroll.handle.scroll_to_item(count - 1, ScrollStrategy::Bottom);
    }
    let selected_turn = data.selected.as_ref().and_then(|id| data.page.rows.iter().find(|r| r.id == id.as_ref()).map(|r| r.turn));
    let following = scroll.following.clone();
    let requests: Rc<BTreeMap<u32, RequestSummary>> = Rc::new(data.page.requests.iter().map(|r| (r.number, r.clone())).collect());
    let (page, items, selected, now_ms) = (data.page, data.items, data.selected, data.now_ms);
    let h = Rc::new(h);
    let list = uniform_list("trajectory-ledger", count, move |range, _, _| {
        range
            .map(|i| match &items[i] {
                LedgerItem::LoadEarlier => load_earlier(h.on_load_earlier.clone()).into_any_element(),
                LedgerItem::Row { ix, first_of_turn, request } => {
                    let row = &page.rows[*ix];
                    let is_selected = selected.as_ref().is_some_and(|s| s.as_ref() == row.id);
                    let dot = request.map(|n| {
                        let req = requests.get(&n);
                        let tools = row.step.map(|step| step_tools(&page.rows, row.turn, step)).unwrap_or_default();
                        let dot_selected = selected.as_ref().is_some_and(|s| s.as_ref() == format!("request:{n}"));
                        step_dot(n, row.step, req, &tools, dot_selected, now_ms)
                    });
                    row_line(row, *first_of_turn, dot, is_selected, selected_turn == Some(row.turn), now_ms, &h).into_any_element()
                }
                LedgerItem::Turn { turn, rows, first } => {
                    let duration = turn_duration(&page.rows, *turn, now_ms);
                    turn_line(*turn, *rows, duration, &page.rows[*first], &h).into_any_element()
                }
            })
            .collect()
    })
    .track_scroll(&scroll.handle)
    .size_full();
    div()
        .id("trajectory-ledger-wrap")
        .flex_1()
        .min_w_0()
        .h_full()
        .on_scroll(move |e, _, _| {
            if f32::from(e.delta.pixel_delta(px(16.)).y) > 0. {
                following.set(false);
            }
        })
        .child(list)
}

/// "bash×3, read" for a step's tool rows.
fn step_tools(rows: &[TrajectoryRow], turn: u32, step: u32) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in rows.iter().filter(|r| r.kind == RecordKind::Tool && r.turn == turn && r.step == Some(step)) {
        let name = r.label.clone().unwrap_or_else(|| "tool".into());
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => counts.push((name, 1)),
        }
    }
    counts
}

fn turn_duration(rows: &[TrajectoryRow], turn: u32, now_ms: u64) -> Option<u64> {
    let of_turn = rows.iter().filter(|r| r.turn == turn);
    let start = of_turn.clone().filter_map(|r| r.started_ms).min()?;
    let end = of_turn
        .filter_map(|r| r.started_ms.map(|s| s + r.duration_ms.unwrap_or(if r.status == RecordStatus::Running { now_ms.saturating_sub(s) } else { 0 })))
        .max()?;
    Some(end.saturating_sub(start))
}

/// The record kind's label and color.
pub fn kind_style(kind: RecordKind) -> (&'static str, Hsla) {
    match kind {
        RecordKind::System => ("SYSTEM", color::text_2()),
        RecordKind::User => ("USER", color::blue()),
        RecordKind::Context => ("CONTEXT", color::green()),
        RecordKind::Assistant => ("ASSISTANT", color::violet()),
        RecordKind::Tool => ("TOOL", color::yellow()),
    }
}

/// The kind chip: small caps on a tinted fill.
pub fn kind_chip(kind: RecordKind) -> Div {
    let (label, c) = kind_style(kind);
    div()
        .flex_none()
        .h(px(18.))
        .px_1p5()
        .flex()
        .items_center()
        .rounded(radius::XS)
        .bg(if kind == RecordKind::System { color::pressed() } else { c.opacity(0.14) })
        .text_color(c)
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
}

/// The request status dot's color.
pub fn status_color(status: RecordStatus, retries: u32) -> Hsla {
    match status {
        RecordStatus::Failed => color::red(),
        RecordStatus::Cancelled => color::text_3(),
        RecordStatus::Running => color::blue(),
        RecordStatus::Done if retries > 0 => color::yellow(),
        RecordStatus::Done => color::green(),
    }
}

/// A step's request in the gutter: its status dot, the step in a tooltip; a click selects it.
pub struct StepDot {
    number: u32,
    color: Hsla,
    tip: String,
    selected: bool,
}

fn step_dot(number: u32, step: Option<u32>, req: Option<&RequestSummary>, tools: &[(String, usize)], selected: bool, now_ms: u64) -> StepDot {
    let mut parts: Vec<String> = step.map(|s| format!("Step {s}")).into_iter().collect();
    parts.push(format!("Request #{number}"));
    if let Some(r) = req {
        let end = r.ended_ms.unwrap_or(if r.status == RecordStatus::Running { now_ms } else { r.started_ms });
        parts.push(duration_label(end.saturating_sub(r.started_ms)));
        if r.retries > 0 {
            parts.push(format!("{} retr{}", r.retries, if r.retries == 1 { "y" } else { "ies" }));
        }
    }
    if !tools.is_empty() {
        parts.push(tools.iter().map(|(n, c)| if *c > 1 { format!("{n}×{c}") } else { n.clone() }).collect::<Vec<_>>().join(", "));
    }
    let color = req.map_or(color::text_3(), |r| status_color(r.status, r.retries));
    StepDot { number, color, tip: parts.join(" · "), selected }
}

/// The gutter: the turn's label at the top of its first line, the step's request dot, the active
/// turn's rail, the selection rail.
fn gutter(turn: u32, first_of_turn: bool, dot: Option<StepDot>, active_turn: bool, selected: bool, on_select: IdHandler) -> Div {
    div()
        .relative()
        .w(px(GUTTER_W))
        .h_full()
        .flex_none()
        .when(selected, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(color::accent())))
        .when(active_turn, |d| d.child(div().absolute().right(px(4.)).top_0().bottom_0().w(px(2.)).bg(color::border_strong())))
        .when(first_of_turn && turn > 0, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(6.))
                    .text_size(px(9.))
                    .line_height(px(11.))
                    .font_family(text::MONO_FONT)
                    .text_color(color::text_3())
                    .child(format!("Turn {turn}")),
            )
        })
        .children(dot.map(|dot| {
            let id: SharedString = format!("request:{}", dot.number).into();
            div()
                .id(ElementId::Name(format!("ledger-dot-{}", dot.number).into()))
                .absolute()
                .left(px(GUTTER_W - 18.))
                .top((ROW_H - px(8.)) / 2.)
                .size(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(dot.selected, |d| d.border_1().border_color(color::text()))
                .child(div().size(px(6.)).rounded_full().bg(dot.color))
                .tooltip(super::super::widgets::tip(dot.tip))
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on_select(id.clone(), w, cx);
                })
        }))
}

fn line_base(id: ElementId, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(ROW_H)
        .w_full()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(color::border().opacity(0.35))
        .text_size(text::SM)
        .when(selected, |d| d.bg(color::pressed()))
        .when(!selected, |d| d.hover(|s| s.bg(color::surface())))
        .cursor_pointer()
}

fn row_line(row: &TrajectoryRow, first_of_turn: bool, dot: Option<StepDot>, selected: bool, active_turn: bool, now_ms: u64, h: &Rc<LedgerHandlers>) -> Stateful<Div> {
    let id: SharedString = row.id.clone().into();
    let (on_select, on_fold) = (h.on_select.clone(), h.on_toggle_fold.clone());
    let fold = fold_key_of(row);
    // A tool's preview already starts with its name ("bash · cargo test → ok").
    let preview = row.preview.clone();
    let meta = row_meta(row, now_ms);
    line_base(ElementId::Name(format!("ledger-{}", row.id).into()), selected)
        // A line between turns.
        .when(first_of_turn && row.turn > 1, |d| d.border_t_1().border_color(color::border()))
        .child(gutter(row.turn, first_of_turn, dot, active_turn, selected, h.on_select.clone()))
        .child(div().w(px(CHIP_W)).flex_none().flex().justify_end().pr_3().child(kind_chip(row.kind)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(if row.kind == RecordKind::Context { color::text_2() } else { color::text() })
                .when(row.kind == RecordKind::Context, |d| {
                    d.children(row.label.clone().map(|l| div().flex_none().mr_2().text_color(color::text_3()).child(l)))
                })
                .flex()
                .child(div().min_w_0().overflow_hidden().text_ellipsis().child(format::one_line(&preview))),
        )
        .child(meta)
        .on_click(move |e, w, cx| {
            if e.click_count() >= 2 {
                on_fold(fold, w, cx);
            } else {
                on_select(id.clone(), w, cx);
            }
        })
}

/// Right side of a row: spinner, images, guard, tokens, duration.
fn row_meta(row: &TrajectoryRow, now_ms: u64) -> Div {
    let dim = |s: String| div().text_color(color::text_3()).child(s);
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .text_size(text::XS)
        .font_family(text::MONO_FONT)
        .when(row.images > 0 || row.files > 0, |d| {
            let mut parts = Vec::new();
            if row.images > 0 {
                parts.push(format!("{} image{}", row.images, if row.images == 1 { "" } else { "s" }));
            }
            if row.files > 0 {
                parts.push(format!("{} file{}", row.files, if row.files == 1 { "" } else { "s" }));
            }
            d.child(dim(parts.join(" · ")))
        })
        .when_some(row.guard, |d, g| {
            let (label, c) = match g {
                GuardVerdict::Allow => ("✓ guard", color::green()),
                GuardVerdict::Deny => ("✕ denied", color::red()),
                GuardVerdict::Held => ("‖ held", color::yellow()),
            };
            d.child(div().text_color(c).child(label))
        })
        .when(row.held_ms > 0, |d| d.child(dim(format!("held {}", duration_label(row.held_ms)))))
        .when_some(row.tokens, |d, t| d.child(dim(format!("{} tok", format::compact_tokens(t)))))
        .map(|d| match (row.status, row.duration_ms, row.started_ms) {
            (RecordStatus::Running, _, start) => d
                .child(Spinner::new(size::SPINNER_SM))
                .children(start.map(|s| dim(duration_label(now_ms.saturating_sub(s))))),
            (RecordStatus::Failed, _, _) => d.child(div().text_color(color::red()).child("failed")),
            (RecordStatus::Cancelled, _, _) => d.child(dim("stopped".into())),
            (_, Some(ms), _) if row.kind != RecordKind::User && row.kind != RecordKind::Context => d.child(dim(duration_label(ms))),
            _ => d,
        })
}

#[allow(clippy::too_many_arguments)]
fn turn_line(turn: u32, rows: usize, duration: Option<u64>, first: &TrajectoryRow, h: &Rc<LedgerHandlers>) -> Stateful<Div> {
    let on_fold = h.on_toggle_fold.clone();
    let mut parts = vec![format!("{rows} record{}", if rows == 1 { "" } else { "s" })];
    if let Some(d) = duration {
        parts.push(duration_label(d));
    }
    line_base(ElementId::Name(format!("ledger-turn-{turn}").into()), false)
        .child(gutter(turn, true, None, false, false, h.on_select.clone()))
        .child(div().w(px(CHIP_W)).flex_none().flex().justify_end().pr_3().text_color(color::text_3()).child("▸"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(color::text_2())
                .child(format!("{} · {}", format::one_line(&first.preview), parts.join(" · "))),
        )
        .on_click(move |_, w, cx| on_fold(FoldKey::Turn(turn), w, cx))
}

fn load_earlier(on_load: Handler) -> Stateful<Div> {
    div()
        .id("ledger-load-earlier")
        .h(ROW_H)
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(text::SM)
        .text_color(color::text_2())
        .cursor_pointer()
        .hover(|s| s.bg(color::surface()).text_color(color::text()))
        .border_b_1()
        .border_color(color::border().opacity(0.5))
        .child("Load earlier")
        .on_click(move |_, w, cx| on_load(w, cx))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use space_core::protocol::trace::{RecordKind, TrajectoryPage, TrajectoryRow};

    use super::{FoldKey, LedgerItem, fold_key_of, ledger_items};

    fn row(id: &str, kind: RecordKind, turn: u32, step: Option<u32>) -> TrajectoryRow {
        TrajectoryRow { id: id.into(), kind, turn, step, request: step, ..Default::default() }
    }

    fn page() -> TrajectoryPage {
        TrajectoryPage {
            rows: vec![
                row("s", RecordKind::System, 0, None),
                row("u1", RecordKind::User, 1, None),
                row("c1", RecordKind::Context, 1, None),
                row("a1", RecordKind::Assistant, 1, Some(1)),
                row("t1", RecordKind::Tool, 1, Some(1)),
                row("t2", RecordKind::Tool, 1, Some(1)),
                row("a2", RecordKind::Assistant, 1, Some(2)),
                row("u2", RecordKind::User, 2, None),
                row("a3", RecordKind::Assistant, 2, Some(3)),
            ],
            ..Default::default()
        }
    }

    fn ids(page: &TrajectoryPage, items: &[LedgerItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                LedgerItem::LoadEarlier => "earlier".into(),
                LedgerItem::Row { ix, first_of_turn, request } => format!(
                    "{}{}{}",
                    page.rows[*ix].id,
                    if *first_of_turn { "*" } else { "" },
                    request.map(|n| format!("#{n}")).unwrap_or_default()
                ),
                LedgerItem::Turn { turn, rows, .. } => format!("turn{turn}({rows})"),
            })
            .collect()
    }

    #[test]
    fn a_steps_first_model_row_carries_its_request() {
        let p = page();
        let items = ledger_items(&p, &HashSet::new(), |_| true);
        assert_eq!(ids(&p, &items), ["s*", "u1*", "c1", "a1#1", "t1", "t2", "a2#2", "u2*", "a3#3"]);
    }

    #[test]
    fn folds_and_filters() {
        let mut p = page();
        p.older = Some("cursor".into());
        let folds = HashSet::from([FoldKey::Step(1, 1), FoldKey::Turn(2)]);
        let items = ledger_items(&p, &folds, |_| true);
        assert_eq!(ids(&p, &items), ["earlier", "s*", "u1*", "c1", "a1#1", "a2#2", "turn2(2)"]);
        // A filter: the first passing row of a step carries its request.
        let items = ledger_items(&p, &HashSet::new(), |r| r.id.starts_with('t'));
        assert_eq!(ids(&p, &items), ["earlier", "t1*#1", "t2"]);
        assert_eq!(fold_key_of(&p.rows[4]), FoldKey::Step(1, 1));
        assert_eq!(fold_key_of(&p.rows[1]), FoldKey::Turn(1));
    }
}
