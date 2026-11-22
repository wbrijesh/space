//! The right pane's tabs. Each thread's pane has one strip of tabs, each one thing: the folder's
//! tree, a file, a terminal, Changes, the side chat, or a new tab. A new tab is the launcher: a
//! search over the folder's files, then everything the pane can open, then recent files. Picking
//! from it turns that tab into what was picked. The tree and Changes are open at most once
//! (asking again goes to their tab), files and side chats once each, terminals and new tabs any
//! number of times. The launcher also lists the thread's side chats, to open or delete. Closing
//! the last tab shows a new tab in its place; closing that hides the pane.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::component::Icon;
use gpui::component::input::{InputEvent, InputState};
use gpui::*;
use space_ui::components::file_tree::file_icon;
use space_ui::components::menu::shortcut_label;
use space_ui::components::right_pane::{LauncherRow, LauncherSection, PaneView, StripTab, launcher};

use crate::app::SpaceApp;

/// One tab in a thread's pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneTab {
    /// A new tab: the launcher.
    Launcher,
    /// The folder's tree.
    Files,
    File(PathBuf),
    /// A terminal, by id (a [`STARTING`] one while its shell starts).
    Terminal(String),
    Changes,
    /// A side chat, by its thread's key.
    SideChat(u64),
}

/// The id of a terminal tab whose shell is still starting.
pub(crate) const STARTING: &str = "starting-";

/// A thread's pane.
#[derive(Default)]
pub(crate) struct Pane {
    pub(crate) open: bool,
    pub(crate) maximized: bool,
    pub(crate) tabs: Vec<PaneTab>,
    /// The tab shown.
    pub(crate) active: usize,
    /// The terminal shown last (⌘J goes back to it).
    pub(crate) last_terminal: Option<String>,
    /// Why a starting terminal (by its tab's id) couldn't start.
    pub(crate) failed: HashMap<String, String>,
}

impl Pane {
    pub(crate) fn active_tab(&self) -> Option<&PaneTab> {
        self.tabs.get(self.active)
    }

    pub(crate) fn position(&self, tab: &PaneTab) -> Option<usize> {
        self.tabs.iter().position(|t| t == tab)
    }

    /// Its terminals' ids, in tab order (starting ones too).
    pub(crate) fn terminals(&self) -> impl Iterator<Item = &str> {
        self.tabs.iter().filter_map(|t| match t {
            PaneTab::Terminal(id) => Some(id.as_str()),
            _ => None,
        })
    }

    pub(crate) fn select(&mut self, ix: usize) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        if let PaneTab::Terminal(id) = &self.tabs[ix] {
            self.last_terminal = Some(id.clone());
        }
    }

    /// Show `tab`: its tab if it's open, else as a new one (see [`Self::add`]).
    pub(crate) fn show(&mut self, tab: PaneTab) {
        match self.position(&tab) {
            Some(ix) => self.select(ix),
            None => self.add(tab),
        }
    }

    /// Add `tab` even if one like it is open: in place of the launcher when that's shown (it
    /// was picked from there), else at the end. It's shown.
    pub(crate) fn add(&mut self, tab: PaneTab) {
        if self.active_tab() == Some(&PaneTab::Launcher) && tab != PaneTab::Launcher {
            self.tabs[self.active] = tab;
            self.select(self.active);
        } else {
            self.tabs.push(tab);
            self.select(self.tabs.len() - 1);
        }
    }

    /// Put `new` in place of `old` (a terminal that started, or restarted); `false` if `old`
    /// isn't open.
    pub(crate) fn replace(&mut self, old: &PaneTab, new: PaneTab) -> bool {
        let Some(ix) = self.position(old) else { return false };
        self.tabs[ix] = new;
        if ix == self.active {
            self.select(ix);
        }
        true
    }

    /// Remove tab `ix`. Closing the shown one shows the one after it (or before, at the end).
    pub(crate) fn remove(&mut self, ix: usize) -> Option<PaneTab> {
        if ix >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(ix);
        if ix < self.active || (ix == self.active && self.active == self.tabs.len() && self.active > 0) {
            self.active -= 1;
        }
        let active = self.active;
        self.select(active);
        Some(tab)
    }
}

/// What the launcher can open (its rows, for clicks and ⏎ alike).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    Changes,
    Files,
    Terminal,
    NewSideChat,
    SideChat(u64),
    File(PathBuf),
    /// Bring back the side chat just deleted.
    Undo,
}

/// The launcher's search, shared by every new tab (one shows at a time).
pub(crate) struct Launcher {
    pub(crate) search: Entity<InputState>,
    /// The highlighted row (↑ ↓, ⏎ opens it).
    pub(crate) selected: usize,
    _sub: Subscription,
}

/// Files listed for a search, at most.
const MATCHES_MAX: usize = 50;

impl Launcher {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SpaceApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search files"));
        let sub = cx.subscribe_in(&search, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.launcher.selected = 0;
                let text = input.read(cx).value().to_string();
                this.launcher_search(text, cx);
            }
        });
        Self { search, selected: 0, _sub: sub }
    }
}

impl SpaceApp {
    /// Show `tab` in thread `key`'s pane, opening the pane, and focus it.
    pub(crate) fn show_pane_tab(&mut self, key: u64, tab: PaneTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.terminals.panes.entry(key).or_default();
        pane.open = true;
        pane.show(tab);
        self.pane_tab_shown(key, window, cx);
    }

    /// The pane opened, or its shown tab changed: focus what it shows.
    pub(crate) fn pane_tab_shown(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_slide(cx);
        let tab = self.terminals.pane(key).and_then(|p| p.active_tab()).cloned();
        match tab {
            Some(PaneTab::Launcher) => {
                self.launcher.selected = 0;
                let search = self.launcher.search.clone();
                search.update(cx, |s, cx| {
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
            }
            Some(PaneTab::Files) => window.focus(&self.files.focus, cx),
            Some(PaneTab::File(path)) => {
                if let Some(tree) = self.file_trees.get_mut(&key) {
                    tree.selected = Some(path);
                }
                window.focus(&self.files.file_focus, cx);
            }
            Some(PaneTab::Terminal(_)) => self.focus_terminal(key, window, cx),
            Some(PaneTab::Changes) => window.focus(&self.changes_focus, cx),
            Some(PaneTab::SideChat(side)) => self.focus_side_chat(side, window, cx),
            None => {}
        }
        cx.notify();
    }

    /// A tab was clicked.
    pub(crate) fn select_pane_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(pane) = self.terminals.panes.get_mut(&key) else { return };
        pane.select(ix);
        self.pane_tab_shown(key, window, cx);
    }

    /// ⌃Tab / ⌃⇧Tab.
    pub(crate) fn step_pane_tab(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(pane) = self.terminals.panes.get(&key).filter(|p| p.open && !p.tabs.is_empty()) else { return };
        let ix = (pane.active as isize + by).rem_euclid(pane.tabs.len() as isize) as usize;
        self.select_pane_tab(ix, window, cx);
    }

    /// `+` or ⌘T: a new tab (the launcher; the one open, if there is one).
    pub(crate) fn new_pane_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        let Some(key) = self.selected else { return };
        self.show_pane_tab(key, PaneTab::Launcher, window, cx);
    }

    /// ⌘W in the pane: close the shown tab (in Changes, its open file first).
    pub(crate) fn close_pane_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let changes = self.terminals.pane(key).and_then(|p| p.active_tab()) == Some(&PaneTab::Changes);
        if changes && self.close_changes_item(cx) {
            return;
        }
        self.close_current_pane_tab(window, cx);
    }

    fn close_current_pane_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        if let Some(ix) = self.terminals.pane(key).filter(|p| p.open && !p.tabs.is_empty()).map(|p| p.active) {
            self.close_pane_tab(ix, window, cx);
        }
    }

    /// Close tab `ix` of the current thread's pane. A terminal's shell closes with it (asked
    /// first when something runs in it); a side chat is kept, to open again from the launcher.
    pub(crate) fn close_pane_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(tab) = self.terminals.pane(key).and_then(|p| p.tabs.get(ix)).cloned() else { return };
        if let PaneTab::Terminal(id) = &tab
            && let Some(session) = self.terminals.session(id).filter(|s| s.busy())
        {
            let title = session.state.lock().unwrap().title.clone();
            let answer = window.prompt(
                PromptLevel::Warning,
                &format!("Close the terminal running {title}?"),
                Some("Closing it stops what's running in it."),
                &["Close", "Cancel"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    let _ = this.update_in(cx, |this, window, cx| this.remove_pane_tab(key, &tab, true, window, cx));
                }
            })
            .detach();
            return;
        }
        self.remove_pane_tab(key, &tab, true, window, cx);
    }

    /// Take `tab` out of thread `key`'s pane (closing a terminal's shell, forgetting a file). The
    /// last one gives way to a new tab; the last new tab hides the pane. With `refocus`, focus
    /// goes to the tab shown next, or the composer.
    pub(crate) fn remove_pane_tab(&mut self, key: u64, tab: &PaneTab, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        match tab {
            PaneTab::Terminal(id) => self.close_shell(id, cx),
            PaneTab::File(path) => self.forget_file(key, path),
            _ => {}
        }
        let Some(pane) = self.terminals.panes.get_mut(&key) else { return };
        let Some(ix) = pane.position(tab) else { return };
        let was_shown = ix == pane.active;
        pane.remove(ix);
        if let PaneTab::Terminal(id) = tab {
            pane.failed.remove(id);
        }
        // The last tab closed: a new tab takes its place, and the pane stays; closing that one too
        // hides the pane.
        if pane.tabs.is_empty() && *tab != PaneTab::Launcher {
            pane.add(PaneTab::Launcher);
        }
        let empty = pane.tabs.is_empty();
        if empty {
            pane.open = false;
            pane.maximized = false;
            self.terminal_slide(cx);
        }
        if refocus && self.selected == Some(key) {
            if empty {
                self.focus_composer(window, cx);
            } else if was_shown {
                self.pane_tab_shown(key, window, cx);
            }
        }
        cx.notify();
    }

    /// A launcher row was picked: its tab takes the launcher's place.
    pub(crate) fn pick_launcher(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        match pick {
            Pick::Changes => self.show_pane_tab(key, PaneTab::Changes, window, cx),
            Pick::Files => self.show_pane_tab(key, PaneTab::Files, window, cx),
            Pick::NewSideChat => {
                self.new_side_chat(window, cx);
            }
            Pick::SideChat(side) => self.open_side_chat(side, window, cx),
            Pick::Terminal => self.new_terminal(window, cx),
            Pick::File(path) => self.show_file(key, &path, window, cx),
            Pick::Undo => self.undo_delete_side_chat(window, cx),
        }
    }

    /// The search changed: search every file under the folder (once walked).
    fn launcher_search(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let walk = !text.trim().is_empty() && self.files_tree_for(key, cx).is_some_and(|t| t.all.is_none());
        if walk {
            self.walk_all(key, cx);
        }
        cx.notify();
    }

    /// The launcher's rows, in order, with what each opens; or a note in their place.
    #[allow(clippy::type_complexity)]
    fn launcher_rows(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) -> (Vec<(Option<&'static str>, Vec<(Pick, LauncherRow)>)>, Option<SharedString>) {
        let text = self.launcher.search.read(cx).value().to_string();
        let words = crate::files::filter_words(&text);
        let root = self.files_tree_for(key, cx).map(|t| t.root.clone());
        let row = |pick: Pick, row: LauncherRow| (pick.clone(), row);
        let file_row = |path: &Path, root: Option<&Path>| {
            let rel = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path);
            let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = rel.parent().map(|d| d.display().to_string()).unwrap_or_default();
            LauncherRow::new(Icon::new(file_icon(&name)), name, Rc::new(|_, _| {})).detail(dir)
        };
        if !words.is_empty() {
            let Some(tree) = self.file_trees.get(&key) else { return (Vec::new(), Some("No folder".into())) };
            let Some(all) = &tree.all else { return (Vec::new(), Some("Searching…".into())) };
            let root = tree.root.clone();
            let rows: Vec<_> = all
                .iter()
                .filter(|p| crate::files::matches(p, &words))
                .take(MATCHES_MAX)
                .map(|rel| {
                    let path = root.join(rel);
                    let r = file_row(&path, Some(&root));
                    row(Pick::File(path), r)
                })
                .collect();
            if rows.is_empty() {
                return (Vec::new(), Some("No files match".into()));
            }
            return (vec![(None, rows)], None);
        }
        let item = |v: PaneView, action: &dyn Action, pick: Pick| {
            row(pick, LauncherRow::new(v.icon(), v.label(), Rc::new(|_, _| {})).shortcut(shortcut_label(action, window)))
        };
        let mut open = vec![
            item(PaneView::Changes, &space_ui::actions::ShowChanges, Pick::Changes),
            item(PaneView::Files, &space_ui::actions::ShowFiles, Pick::Files),
        ];
        open.push(item(PaneView::Terminal, &space_ui::actions::ShowTerminal, Pick::Terminal));
        let new_side = LauncherRow::new(PaneView::SideChat.icon(), "New side chat", Rc::new(|_, _| {})).shortcut(shortcut_label(&space_ui::actions::ShowSideChat, window));
        open.push(row(Pick::NewSideChat, new_side));
        let mut sides: Vec<_> = self
            .side_chats_of(key, window, cx)
            .into_iter()
            .filter_map(|k| self.side_chats.get(&k).filter(|s| !s.thread.is_empty()).map(|s| (k, s.title())))
            .map(|(k, title)| row(Pick::SideChat(k), LauncherRow::new(PaneView::SideChat.icon(), title, Rc::new(|_, _| {}))))
            .collect();
        if self.deleted_side.as_ref().is_some_and(|d| d.side.thread.side_of == Some(key)) {
            let undo = LauncherRow::new(Icon::new(gpui::assets::IconName::RotateCw), "Side chat deleted", Rc::new(|_, _| {})).detail("Undo");
            sides.insert(0, row(Pick::Undo, undo));
        }
        let recent: Vec<_> = self
            .recent_files
            .get(&key)
            .map(|r| r.iter().map(|p| row(Pick::File(p.clone()), file_row(p, root.as_deref()))).collect())
            .unwrap_or_default();
        (vec![(None, open), (Some("Side chats"), sides), (Some("Recent"), recent)], None)
    }

    /// The launcher in thread `key`'s pane. ↑ ↓ move through its rows and ⏎ opens one.
    pub(crate) fn launcher_content(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (groups, note) = self.launcher_rows(key, window, cx);
        let count: usize = groups.iter().map(|(_, rows)| rows.len()).sum();
        self.launcher.selected = self.launcher.selected.min(count.saturating_sub(1));
        let this = cx.entity().downgrade();
        let sections = groups
            .into_iter()
            .map(|(title, rows)| LauncherSection {
                count: (title == Some("Side chats")).then(|| rows.iter().filter(|(p, _)| matches!(p, Pick::SideChat(_))).count()).filter(|n| *n > 0),
                title: title.map(Into::into),
                rows: rows
                    .into_iter()
                    .map(|(pick, mut row)| {
                        if let Pick::SideChat(side) = pick {
                            let this = this.clone();
                            row = row.on_delete(Rc::new(move |w, cx| {
                                let _ = this.update(cx, |this, cx| this.delete_side_chat(side, w, cx));
                            }));
                        }
                        let this = this.clone();
                        row.on_pick = Rc::new(move |w, cx| {
                            let _ = this.update(cx, |this, cx| this.pick_launcher(pick.clone(), w, cx));
                        });
                        row
                    })
                    .collect(),
            })
            .collect();
        let selected = (count > 0).then_some(self.launcher.selected);
        div()
            .key_context("Launcher")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.modifiers.modified() {
                    return;
                }
                match e.keystroke.key.as_str() {
                    "down" => this.launcher.selected = (this.launcher.selected + 1).min(count.saturating_sub(1)),
                    "up" => this.launcher.selected = this.launcher.selected.saturating_sub(1),
                    "enter" => {
                        let Some(key) = this.selected else { return };
                        let (groups, _) = this.launcher_rows(key, window, cx);
                        let pick = groups.into_iter().flat_map(|(_, rows)| rows).nth(this.launcher.selected).map(|(p, _)| p);
                        if let Some(pick) = pick {
                            this.pick_launcher(pick, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(launcher(&self.launcher.search, sections, note, selected))
            .into_any_element()
    }

    /// The strip's tabs for thread `key`'s pane.
    pub(crate) fn strip_tabs(&self, key: u64) -> Vec<StripTab> {
        let Some(pane) = self.terminals.pane(key) else { return Vec::new() };
        pane.tabs
            .iter()
            .map(|tab| match tab {
                PaneTab::Launcher => StripTab::launcher(),
                PaneTab::Files => StripTab::new(PaneView::Files.label(), PaneView::Files.icon()),
                PaneTab::File(path) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    StripTab::new(name.clone(), Icon::new(file_icon(&name)))
                }
                PaneTab::Terminal(id) => match self.terminals.session(id) {
                    Some(s) => {
                        let t = s.tab();
                        StripTab { unread: t.unread, ended: t.ended, ..StripTab::new(t.title, PaneView::Terminal.icon()) }
                    }
                    None => StripTab::new(PaneView::Terminal.label(), PaneView::Terminal.icon()),
                },
                PaneTab::Changes => StripTab::new(PaneView::Changes.label(), PaneView::Changes.icon()),
                PaneTab::SideChat(side) => {
                    let title = self.side_chats.get(side).map_or_else(|| PaneView::SideChat.label().to_string(), |s| s.title());
                    StripTab::new(title, PaneView::SideChat.icon())
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Pane, PaneTab};

    fn pane(tabs: &[PaneTab], active: usize) -> Pane {
        Pane { tabs: tabs.to_vec(), active, ..Default::default() }
    }
    fn file(name: &str) -> PaneTab {
        PaneTab::File(PathBuf::from(name))
    }

    #[test]
    fn picking_from_the_launcher_takes_its_place() {
        let mut p = pane(&[PaneTab::Changes, PaneTab::Launcher], 1);
        p.add(file("a.rs"));
        assert_eq!(p.tabs, [PaneTab::Changes, file("a.rs")]);
        assert_eq!(p.active, 1);
    }

    #[test]
    fn what_is_open_already_is_switched_to() {
        let mut p = pane(&[PaneTab::Changes, file("a.rs"), PaneTab::Launcher], 2);
        p.show(PaneTab::Changes);
        assert_eq!(p.active, 0);
        assert_eq!(p.tabs.len(), 3);
        p.show(PaneTab::Files);
        assert_eq!(p.tabs, [PaneTab::Changes, file("a.rs"), PaneTab::Launcher, PaneTab::Files]);
        assert_eq!(p.active, 3);
    }

    #[test]
    fn a_new_tab_goes_to_the_end_unless_the_launcher_is_shown() {
        let mut p = pane(&[PaneTab::Changes], 0);
        p.show(PaneTab::Launcher);
        assert_eq!(p.tabs, [PaneTab::Changes, PaneTab::Launcher]);
        p.add(PaneTab::Terminal("t1".into()));
        p.add(PaneTab::Terminal("t2".into()));
        assert_eq!(p.tabs, [PaneTab::Changes, PaneTab::Terminal("t1".into()), PaneTab::Terminal("t2".into())]);
        assert_eq!(p.last_terminal.as_deref(), Some("t2"));
    }

    #[test]
    fn closing_the_shown_tab_shows_the_next_one() {
        let mut p = pane(&[PaneTab::Changes, file("a.rs"), file("b.rs")], 1);
        p.remove(1);
        assert_eq!(p.active_tab(), Some(&file("b.rs")));
        p.remove(1);
        assert_eq!(p.active_tab(), Some(&PaneTab::Changes));
        let mut p = pane(&[PaneTab::Changes, file("a.rs"), file("b.rs")], 2);
        p.remove(0);
        assert_eq!(p.active_tab(), Some(&file("b.rs")));
        p.remove(0);
        p.remove(0);
        assert!(p.tabs.is_empty());
        assert_eq!(p.active, 0);
    }

    #[test]
    fn a_started_terminal_replaces_its_placeholder() {
        let mut p = pane(&[PaneTab::Terminal("starting-1".into())], 0);
        assert!(p.replace(&PaneTab::Terminal("starting-1".into()), PaneTab::Terminal("t1".into())));
        assert_eq!(p.last_terminal.as_deref(), Some("t1"));
        assert!(!p.replace(&PaneTab::Terminal("gone".into()), PaneTab::Changes));
    }
}
