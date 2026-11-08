//! The Trajectory tab: a thread's trace as the trajectory plugin lays it out. The plugin keeps
//! the data; the app keeps what the tab shows (the loaded rows, the selected row and its detail,
//! folds, the time filter, search matches) and reloads when the plugin says the thread changed.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::component::input::{InputEvent, InputState};
use gpui::*;
use space_core::protocol::trace::{TrajectoryDetail, TrajectoryDetailRequest, TrajectoryPage, TrajectoryRead, TrajectoryStats};
use space_ui::components::images::Pic;
use space_ui::components::trajectory::{
    DurationMode, FoldKey, FoldScope, LedgerScroll, TimeRange, ViewTab, all_fold_keys, all_folded,
};

use crate::app::SpaceApp;

/// Rows per page.
const PAGE: u32 = 200;

/// What the Trajectory tab of the thread it shows holds.
pub(crate) struct TrajectoryState {
    pub thread: String,
    pub page: Rc<TrajectoryPage>,
    pub scroll: LedgerScroll,
    pub selected: Option<SharedString>,
    /// The selected row's detail (`None` while it loads).
    pub detail: Option<Rc<TrajectoryDetail>>,
    pub folds: HashSet<FoldKey>,
    pub mode: DurationMode,
    pub range: Option<TimeRange>,
    /// Ids of the rows matching the search (`None`: no search).
    pub matches: Option<Rc<HashSet<String>>>,
    loading: bool,
    /// It changed while a load was in flight: load again when that one lands.
    again: bool,
}

impl TrajectoryState {
    fn new(thread: String) -> Self {
        Self {
            thread,
            page: Rc::new(TrajectoryPage::default()),
            scroll: LedgerScroll::new(),
            selected: None,
            detail: None,
            folds: HashSet::new(),
            mode: DurationMode::Sequence,
            range: None,
            matches: None,
            loading: false,
            again: false,
        }
    }
}

/// The app's Trajectory state: which threads show the tab, the shown one's data, the search
/// field, and each thread's totals for the stats line.
pub(crate) struct Trajectories {
    pub open: HashSet<u64>,
    pub state: Option<TrajectoryState>,
    pub search: Entity<InputState>,
    pub stats: HashMap<String, TrajectoryStats>,
    _search_sub: Subscription,
}

impl Trajectories {
    pub fn new(window: &mut Window, cx: &mut Context<SpaceApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let sub = cx.subscribe(&search, |this: &mut SpaceApp, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.search_trajectory(cx);
            }
        });
        Self { open: HashSet::new(), state: None, search, stats: HashMap::new(), _search_sub: sub }
    }
}

impl SpaceApp {
    /// The selected thread's tab (Chat unless it shows the Trajectory).
    pub(crate) fn view_tab(&self) -> ViewTab {
        match self.selected {
            Some(key) if self.host.has_trajectory() && self.trajectories.open.contains(&key) => ViewTab::Trajectory,
            _ => ViewTab::Chat,
        }
    }

    pub(crate) fn show_tab(&mut self, tab: ViewTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        match tab {
            ViewTab::Chat => {
                self.trajectories.open.remove(&key);
                self.focus_composer(window, cx);
            }
            ViewTab::Trajectory if self.host.has_trajectory() => {
                self.trajectories.open.insert(key);
                self.ensure_trajectory(cx);
            }
            ViewTab::Trajectory => {}
        }
        cx.notify();
    }

    /// Another thread was selected: load its trajectory if it shows the tab, and its totals for
    /// the stats line.
    pub(crate) fn thread_selected_for_trajectory(&mut self, cx: &mut Context<Self>) {
        if !self.host.has_trajectory() {
            return;
        }
        if self.view_tab() == ViewTab::Trajectory {
            self.ensure_trajectory(cx);
        } else if let Some(id) = self.current().map(|t| t.id.clone()) {
            self.trajectory_changed(id, cx);
        }
    }

    /// ⌘⇧T: switch the selected thread between Chat and Trajectory.
    pub(crate) fn toggle_trajectory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = if self.view_tab() == ViewTab::Trajectory { ViewTab::Chat } else { ViewTab::Trajectory };
        self.show_tab(tab, window, cx);
    }

    /// The shown state is the selected thread's, and up to date: the tab only follows changes
    /// while it's shown, so turns sent while Chat was shown are read now.
    pub(crate) fn ensure_trajectory(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.current().map(|t| t.id.clone()) else { return };
        if !self.trajectories.state.as_ref().is_some_and(|s| s.thread == id) {
            self.trajectories.state = Some(TrajectoryState::new(id));
        }
        self.reload_trajectory(cx);
    }

    /// Read the newest page again (keeping as many rows as are loaded).
    pub(crate) fn reload_trajectory(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.trajectories.state.as_mut() else { return };
        if state.loading {
            state.again = true;
            return;
        }
        state.loading = true;
        state.again = false;
        let read = TrajectoryRead {
            thread_id: state.thread.clone(),
            before: None,
            limit: PAGE.max(state.page.rows.len() as u32 + 50),
            query: None,
        };
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let page = host.trajectory_read(read.clone()).await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.trajectories.state.as_mut().filter(|s| s.thread == read.thread_id) else { return };
                state.loading = false;
                if state.again {
                    this.reload_trajectory(cx);
                    return;
                }
                match page {
                    Ok(page) => {
                        this.trajectories.stats.insert(read.thread_id.clone(), page.stats.clone());
                        let follow = state.scroll.is_following() || state.page.rows.is_empty();
                        state.page = Rc::new(page);
                        if follow {
                            state.scroll.jump_to_end();
                        }
                        // The selected row may have changed (it finished, got its output).
                        if let Some(id) = state.selected.clone() {
                            this.load_detail(id.to_string(), cx);
                        }
                    }
                    Err(e) => tracing::debug!("trajectory: {e:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Load earlier": the page before the oldest loaded row.
    pub(crate) fn load_earlier_trajectory(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.trajectories.state.as_ref() else { return };
        let Some(before) = state.page.older.clone() else { return };
        let read = TrajectoryRead { thread_id: state.thread.clone(), before: Some(before), limit: PAGE, query: None };
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let Ok(older) = host.trajectory_read(read.clone()).await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.trajectories.state.as_mut().filter(|s| s.thread == read.thread_id) else { return };
                let mut page = (*state.page).clone();
                let mut rows = older.rows;
                rows.extend(page.rows);
                page.rows = rows;
                let mut requests = older.requests;
                requests.retain(|r| !page.requests.iter().any(|p| p.number == r.number));
                requests.extend(page.requests);
                page.requests = requests;
                page.older = older.older;
                state.page = Rc::new(page);
                cx.notify();
            });
        })
        .detach();
    }

    /// The plugin says a thread's trajectory changed: reload it if it's shown, refresh its totals.
    pub(crate) fn trajectory_changed(&mut self, thread_id: String, cx: &mut Context<Self>) {
        // The tab was opened before the plugin was up (or for another thread).
        if self.view_tab() == ViewTab::Trajectory && self.current().is_some_and(|t| t.id == thread_id) {
            self.ensure_trajectory(cx);
            return;
        }
        if self.current().is_some_and(|t| t.id == thread_id) {
            let host = self.host.clone();
            cx.spawn(async move |this, cx| {
                if let Ok(stats) = host.trajectory_stats(&thread_id).await {
                    let _ = this.update(cx, |this, cx| {
                        this.trajectories.stats.insert(thread_id, stats);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    pub(crate) fn select_trajectory_row(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(state) = self.trajectories.state.as_mut() else { return };
        if state.selected.as_ref() == Some(&id) {
            return;
        }
        state.selected = Some(id.clone());
        state.detail = None;
        self.load_detail(id.to_string(), cx);
        cx.notify();
    }

    fn load_detail(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(state) = self.trajectories.state.as_ref() else { return };
        let request = TrajectoryDetailRequest { thread_id: state.thread.clone(), id: id.clone() };
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let detail = host.trajectory_detail(request).await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.trajectories.state.as_mut() else { return };
                if state.selected.as_deref() != Some(id.as_str()) {
                    return;
                }
                match detail {
                    Ok(d) => state.detail = Some(Rc::new(d)),
                    Err(e) => tracing::debug!("trajectory detail {id}: {e:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn close_trajectory_inspector(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.trajectories.state.as_mut() {
            state.selected = None;
            state.detail = None;
            cx.notify();
        }
    }

    pub(crate) fn toggle_trajectory_fold(&mut self, key: FoldKey, cx: &mut Context<Self>) {
        if let Some(state) = self.trajectories.state.as_mut() {
            if !state.folds.remove(&key) {
                state.folds.insert(key);
            }
            cx.notify();
        }
    }

    /// Fold every turn (or every step's calls), or unfold them when they all are.
    pub(crate) fn fold_all_trajectory(&mut self, scope: FoldScope, cx: &mut Context<Self>) {
        let Some(state) = self.trajectories.state.as_mut() else { return };
        let keys = all_fold_keys(&state.page, scope);
        if all_folded(&state.page, &state.folds, scope) {
            state.folds.retain(|k| !keys.contains(k));
        } else {
            state.folds.extend(keys);
        }
        cx.notify();
    }

    pub(crate) fn set_trajectory_mode(&mut self, mode: DurationMode, cx: &mut Context<Self>) {
        if let Some(state) = self.trajectories.state.as_mut() {
            state.mode = mode;
            cx.notify();
        }
    }

    pub(crate) fn set_trajectory_range(&mut self, range: Option<TimeRange>, cx: &mut Context<Self>) {
        if let Some(state) = self.trajectories.state.as_mut() {
            state.range = range;
            cx.notify();
        }
    }

    /// The search field changed: ask the plugin which rows match (every word, any case).
    fn search_trajectory(&mut self, cx: &mut Context<Self>) {
        let query = self.trajectories.search.read(cx).value().trim().to_string();
        let Some(state) = self.trajectories.state.as_mut() else { return };
        if query.is_empty() {
            state.matches = None;
            cx.notify();
            return;
        }
        let read = TrajectoryRead { thread_id: state.thread.clone(), before: None, limit: 10_000, query: Some(query.clone()) };
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let Ok(page) = host.trajectory_read(read).await else { return };
            let _ = this.update(cx, |this, cx| {
                // A newer search replaced this one.
                if this.trajectories.search.read(cx).value().trim() != query {
                    return;
                }
                if let Some(state) = this.trajectories.state.as_mut() {
                    state.matches = Some(Rc::new(page.rows.into_iter().map(|r| r.id).collect()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// An image in the inspector: open it in the viewer (its size isn't known here; the viewer
    /// fits it to the window).
    pub(crate) fn open_trajectory_images(&mut self, pics: Vec<Pic>, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pics = pics.into_iter().map(|p| Pic { width: p.width.max(4096), height: p.height.max(4096), ..p }).collect();
        self.open_viewer(pics, ix, window, cx);
    }
}
