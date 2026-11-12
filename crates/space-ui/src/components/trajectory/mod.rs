//! The Trajectory view: a thread's model requests, inputs and tool calls as a ledger, with a
//! timing overview above it and an inspector beside it. Presentational: the app passes a
//! [`TrajectoryPage`] (from the trajectory plugin), the selected row's [`TrajectoryDetail`],
//! fold state, the search input, the time filter and callbacks ([`TrajectoryHandlers`]).
//!
//! * [`view_tabs`]: "Chat | Trajectory" under the title bar.
//! * [`TrajectoryView`]: toolbar, overview ([`timeline`]), ledger ([`ledger`]), inspector.

pub mod inspector;
pub mod json_tree;
pub mod ledger;
pub mod timeline;

#[cfg(feature = "dev")]
pub mod fixtures;

use crate::components::tip::TipExt as _;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, Sizable as _};
use gpui::component::input::{Input, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::protocol::trace::{RecordKind, TrajectoryDetail, TrajectoryPage, TrajectoryRow};

pub use inspector::INSPECTOR_W;
pub use ledger::{LedgerItem, LedgerScroll, ledger_items};
pub use timeline::{DurationMode, TimeRange};

use crate::components::images::OpenPicsHandler;
use crate::format;
use crate::theme::{color, radius, size, text};

/// A turn, or a step's tool calls, folded away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FoldKey {
    Turn(u32),
    /// `(turn, step)`: the step's tool rows hidden.
    Step(u32, u32),
}

/// What "fold all" folds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldScope {
    Turns,
    Calls,
}

/// Every fold key of `scope` in `page` (the app folds them all, or clears them when all are
/// folded already).
pub fn all_fold_keys(page: &TrajectoryPage, scope: FoldScope) -> HashSet<FoldKey> {
    page.rows
        .iter()
        .filter_map(|r| match scope {
            FoldScope::Turns => (r.turn > 0).then_some(FoldKey::Turn(r.turn)),
            FoldScope::Calls => match (r.kind, r.step) {
                (RecordKind::Tool, Some(step)) => Some(FoldKey::Step(r.turn, step)),
                _ => None,
            },
        })
        .collect()
}

/// Whether every fold of `scope` is folded (the toolbar button shows "unfold" then).
pub fn all_folded(page: &TrajectoryPage, folds: &HashSet<FoldKey>, scope: FoldScope) -> bool {
    let keys = all_fold_keys(page, scope);
    !keys.is_empty() && keys.is_subset(folds)
}

// ----- formatting -----------------------------------------------------------------------------

/// "15:20:03.423" (local time).
pub fn clock_time(ms: u64) -> String {
    use chrono::{Local, TimeZone as _};
    Local.timestamp_millis_opt(ms as i64).single().map_or_else(String::new, |t| t.format("%H:%M:%S%.3f").to_string())
}

/// "2026-11-07 11:23:02.423" (local time).
pub fn full_time(ms: u64) -> String {
    use chrono::{Local, TimeZone as _};
    Local.timestamp_millis_opt(ms as i64).single().map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
}

/// "129 ms", "1.46 s", "2m 14s".
pub fn duration_label(ms: u64) -> String {
    match ms {
        0..1000 => format!("{ms} ms"),
        1000..60_000 => {
            let s = ms as f64 / 1000.;
            if s < 10. { format!("{s:.2} s") } else { format!("{s:.1} s") }
        }
        _ => format::short_duration(std::time::Duration::from_millis(ms)),
    }
}

// ----- tabs, toolbar, stats -------------------------------------------------------------------

/// Which view of a thread is showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewTab {
    #[default]
    Chat,
    Trajectory,
}

pub type TabHandler = Rc<dyn Fn(ViewTab, &mut Window, &mut App)>;

/// "Chat | Trajectory": text tabs with an accent underline on the selected one.
pub fn view_tabs(selected: ViewTab, on_select: TabHandler) -> Div {
    let tab = |t: ViewTab, label: &'static str| {
        let f = on_select.clone();
        let on = selected == t;
        div()
            .id(label)
            .h_full()
            .px_1()
            .flex()
            .items_center()
            .text_size(text::SM)
            .cursor_pointer()
            .border_b_2()
            .map(|d| if on { d.border_color(color::accent()).text_color(color::text()) } else { d.border_color(transparent_black()).text_color(color::text_2()).hover(|s| s.text_color(color::text())) })
            .child(label)
            .map(|d| match t {
                ViewTab::Chat => d.tip_action(label, &crate::actions::ShowChat),
                ViewTab::Trajectory => d.tip_action(label, &crate::actions::ShowTrajectory),
            })
            .on_click(move |_, w, cx| f(t, w, cx))
    };
    div()
        .flex_none()
        .w_full()
        .h(size::PANE_BAR_H)
        .px_4()
        .flex()
        .items_end()
        .gap_4()
        .border_b_1()
        .border_color(color::border())
        .child(tab(ViewTab::Chat, "Chat"))
        .child(tab(ViewTab::Trajectory, "Trajectory"))
}

fn tool_button(id: &'static str, icon: Lucide, label: &'static str, on: bool, f: impl Fn(&mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(22.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(radius::MD)
        .text_size(text::XS)
        .cursor_pointer()
        .map(|d| if on { d.bg(color::pressed()).text_color(color::text()) } else { d.text_color(color::text_2()) })
        .hover(|s| s.bg(color::hover()).text_color(color::text()))
        .child(Icon::new(icon).size(size::ICON_SM))
        .child(label)
        .on_click(move |_, w, cx| f(w, cx))
}

// ----- the view -------------------------------------------------------------------------------

pub type IdHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
pub type FoldHandler = Rc<dyn Fn(FoldKey, &mut Window, &mut App)>;
pub type FoldAllHandler = Rc<dyn Fn(FoldScope, &mut Window, &mut App)>;
pub type ModeHandler = Rc<dyn Fn(DurationMode, &mut Window, &mut App)>;

/// What the view's controls do. Every one defaults to a no-op.
#[derive(Clone)]
pub struct TrajectoryHandlers {
    /// A row (its id) or a request (`request:<n>`) was picked.
    pub on_select: IdHandler,
    pub on_close_inspector: Handler,
    pub on_toggle_fold: FoldHandler,
    /// Fold or unfold every turn / every step's calls (see [`all_fold_keys`]).
    pub on_fold_all: FoldAllHandler,
    pub on_duration_mode: ModeHandler,
    /// The overview's time filter changed (`None`: cleared).
    pub on_range: timeline::RangeHandler,
    pub on_load_earlier: Handler,
    pub on_copy: IdHandler,
    pub on_open_image: OpenPicsHandler,
    /// "Continue in Pi": copy this command.
    pub on_continue: IdHandler,
}

impl Default for TrajectoryHandlers {
    fn default() -> Self {
        let noop: Handler = Rc::new(|_, _| {});
        Self {
            on_select: Rc::new(|_, _, _| {}),
            on_continue: Rc::new(|_, _, _| {}),
            on_close_inspector: noop.clone(),
            on_toggle_fold: Rc::new(|_, _, _| {}),
            on_fold_all: Rc::new(|_, _, _| {}),
            on_duration_mode: Rc::new(|_, _, _| {}),
            on_range: Rc::new(|_, _, _| {}),
            on_load_earlier: noop,
            on_copy: Rc::new(|_, _, _| {}),
            on_open_image: Rc::new(|_, _, _, _| {}),
        }
    }
}

/// The whole Trajectory tab.
#[derive(IntoElement)]
pub struct TrajectoryView {
    page: Rc<TrajectoryPage>,
    selected: Option<SharedString>,
    detail: Option<Rc<TrajectoryDetail>>,
    folds: HashSet<FoldKey>,
    search: Option<Entity<InputState>>,
    /// Rows the search matched (`None`: no search).
    matches: Option<Rc<HashSet<String>>>,
    mode: DurationMode,
    range: Option<TimeRange>,
    scroll: LedgerScroll,
    now_ms: u64,
    handlers: TrajectoryHandlers,
    /// Continue the session in another tool: its name, the command, and whether it was just copied.
    continue_in: Option<(SharedString, SharedString, bool)>,
}

impl TrajectoryView {
    pub fn new(page: Rc<TrajectoryPage>, scroll: &LedgerScroll) -> Self {
        Self {
            page,
            selected: None,
            detail: None,
            folds: HashSet::new(),
            search: None,
            matches: None,
            mode: DurationMode::default(),
            range: None,
            scroll: scroll.clone(),
            now_ms: 0,
            handlers: TrajectoryHandlers::default(),
            continue_in: None,
        }
    }

    /// The selected row id or `request:<n>`, and its detail once loaded (`None`: loading).
    pub fn selected(mut self, id: Option<SharedString>, detail: Option<Rc<TrajectoryDetail>>) -> Self {
        self.selected = id;
        self.detail = detail;
        self
    }

    pub fn folds(mut self, folds: HashSet<FoldKey>) -> Self {
        self.folds = folds;
        self
    }

    /// The search field (the app owns the input; it runs the search and passes the matching
    /// row ids).
    pub fn search(mut self, input: &Entity<InputState>, matches: Option<Rc<HashSet<String>>>) -> Self {
        self.search = Some(input.clone());
        self.matches = matches;
        self
    }

    pub fn mode(mut self, mode: DurationMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn range(mut self, range: Option<TimeRange>) -> Self {
        self.range = range;
        self
    }

    /// Now (Unix ms), for running rows.
    pub fn now(mut self, now_ms: u64) -> Self {
        self.now_ms = now_ms;
        self
    }

    /// The "Continue in <app>" button left of the search (`copied`: it was just clicked).
    pub fn continue_in(mut self, app: impl Into<SharedString>, command: impl Into<SharedString>, copied: bool) -> Self {
        self.continue_in = Some((app.into(), command.into(), copied));
        self
    }

    pub fn handlers(mut self, handlers: TrajectoryHandlers) -> Self {
        self.handlers = handlers;
        self
    }
}

impl RenderOnce for TrajectoryView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { page, selected, detail, folds, search, matches, mode, range, scroll, now_ms, handlers: h, continue_in } = self;
        let keep = |r: &TrajectoryRow| {
            matches.as_ref().is_none_or(|m| m.contains(&r.id)) && range.is_none_or(|rg| timeline::active_in(r, rg, now_ms))
        };
        let items = Rc::new(ledger_items(&page, &folds, keep));

        // Toolbar.
        let (fm, fa, fc) = (h.on_duration_mode.clone(), h.on_fold_all.clone(), h.on_fold_all.clone());
        let turns_folded = all_folded(&page, &folds, FoldScope::Turns);
        let calls_folded = all_folded(&page, &folds, FoldScope::Calls);
        let toolbar = div()
            .flex_none()
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(color::border())
            .child(tool_button("tj-duration", Lucide::Clock, "Duration", mode == DurationMode::Duration, move |w, cx| {
                fm(if mode == DurationMode::Duration { DurationMode::Sequence } else { DurationMode::Duration }, w, cx)
            }))
            .child(tool_button("tj-turns", if turns_folded { Lucide::SquarePlus } else { Lucide::SquareMinus }, "Turns", turns_folded, move |w, cx| fa(FoldScope::Turns, w, cx)))
            .child(tool_button("tj-calls", if calls_folded { Lucide::SquarePlus } else { Lucide::SquareMinus }, "Calls", calls_folded, move |w, cx| fc(FoldScope::Calls, w, cx)))
            .when(range.is_some(), |d| {
                let clear = h.on_range.clone();
                d.child(
                    div()
                        .id("tj-range")
                        .ml_2()
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(radius::SM)
                        .bg(color::accent().opacity(0.15))
                        .text_size(text::XS)
                        .text_color(color::accent_hover())
                        .cursor_pointer()
                        .child("Time filter ×")
                        .tip("Clear (right-click or Esc on the overview)")
                        .on_click(move |_, w, cx| clear(None, w, cx)),
                )
            })
            .child(div().flex_1())
            .children(continue_in.map(|(app, command, copied)| {
                let (copy, text) = (h.on_continue.clone(), command.clone());
                let label: SharedString = if copied { "Copied".into() } else { format!("Continue in {app}").into() };
                div()
                    .id("tj-continue")
                    .mr_1()
                    .h(px(22.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(radius::MD)
                    .text_size(text::XS)
                    .text_color(color::text_2())
                    .cursor_pointer()
                    .hover(|s| s.bg(color::hover()).text_color(color::text()))
                    .child(Icon::new(if copied { Lucide::Check } else { Lucide::SquareTerminal }).size(size::ICON_SM))
                    .child(label)
                    .tip(format!("Copy a command that continues this thread in {app}, from a copy of it:\n{command}"))
                    .on_click(move |_, w, cx| copy(text.clone(), w, cx))
            }))
            .children(search.map(|input| div().w(px(240.)).child(Input::new(&input).small().prefix(Icon::new(Lucide::Search).size(size::ICON_SM).text_color(color::text_3())))));

        let overview = timeline::timeline(
            "trajectory-timeline",
            &page.rows,
            mode,
            range,
            selected.as_deref(),
            now_ms,
            h.on_range.clone(),
            h.on_select.clone(),
            window,
            cx,
        );

        let ledger_el = ledger::ledger(
            ledger::LedgerData { page: page.clone(), items: items.clone(), selected: selected.clone(), now_ms },
            &scroll,
            ledger::LedgerHandlers { on_select: h.on_select.clone(), on_toggle_fold: h.on_toggle_fold.clone(), on_load_earlier: h.on_load_earlier.clone() },
        );
        let empty = page.rows.is_empty();

        let inspector_el = selected.as_ref().map(|id| {
            let row = page.rows.iter().find(|r| r.id == id.as_ref());
            let request = id.strip_prefix("request:").and_then(|n| n.parse::<u32>().ok()).and_then(|n| page.requests.iter().find(|r| r.number == n));
            inspector::inspector(
                inspector::InspectorData { selected: id.clone(), row, request, detail: detail.as_deref(), now_ms },
                inspector::InspectorHandlers {
                    on_close: h.on_close_inspector.clone(),
                    on_copy: h.on_copy.clone(),
                    on_select: h.on_select.clone(),
                    on_open_image: h.on_open_image.clone(),
                },
                window,
                cx,
            )
        });
        let close_range = h.on_range.clone();

        div()
            .id("trajectory-view")
            .size_full()
            .flex()
            .flex_col()
            .bg(color::trajectory_bg())
            .on_key_down(move |e, w, cx| {
                if e.keystroke.key == "escape" && range.is_some() {
                    close_range(None, w, cx);
                }
            })
            .child(toolbar)
            .child(overview)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .map(|d| {
                        if empty {
                            d.child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(text::SM)
                                    .text_color(color::text_3())
                                    .child("Nothing recorded yet. Send a message to see what the agent does."),
                            )
                        } else {
                            d.child(ledger_el)
                        }
                    })
                    .children(inspector_el),
            )
    }
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    fixtures::previews()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use space_core::protocol::trace::{RecordKind, TrajectoryPage, TrajectoryRow};

    use super::{FoldKey, FoldScope, all_fold_keys, all_folded, duration_label};

    #[test]
    fn durations_read_naturally() {
        assert_eq!([duration_label(129), duration_label(1460), duration_label(12_340), duration_label(134_000)], ["129 ms", "1.46 s", "12.3 s", "2m 14s"]);
    }

    #[test]
    fn fold_all_keys() {
        let row = |turn, kind, step| TrajectoryRow { turn, kind, step, ..Default::default() };
        let page = TrajectoryPage {
            rows: vec![row(0, RecordKind::System, None), row(1, RecordKind::User, None), row(1, RecordKind::Tool, Some(1)), row(2, RecordKind::Tool, Some(2))],
            ..Default::default()
        };
        assert_eq!(all_fold_keys(&page, FoldScope::Turns), HashSet::from([FoldKey::Turn(1), FoldKey::Turn(2)]));
        assert_eq!(all_fold_keys(&page, FoldScope::Calls), HashSet::from([FoldKey::Step(1, 1), FoldKey::Step(2, 2)]));
        assert!(all_folded(&page, &HashSet::from([FoldKey::Turn(1), FoldKey::Turn(2)]), FoldScope::Turns));
        assert!(!all_folded(&page, &HashSet::from([FoldKey::Turn(1)]), FoldScope::Turns));
    }
}
