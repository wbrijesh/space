//! The folder menu's branch page: the selected thread's folder's branches, loaded when the page
//! opens; a field that filters them or names a new one; switching to one, or creating it, in the
//! background. Only before the thread's first message (the menu isn't offered after), and not
//! while another thread works in the same folder (it would change under it).

use std::path::{Path, PathBuf};

use gpui::component::input::{InputEvent, InputState};
use gpui::*;
use space_ui::components::composer::{BranchChoice, BranchRow};
use space_ui::format;

use crate::app::SpaceApp;
use crate::git::{self, Branches, Head};
use crate::threads::Thread;

/// `usage_open` while the folder menu is open (either page).
pub(crate) const FOLDER_MENU: &str = "composer:folder";
/// `usage_open` while it shows the branch page.
pub(crate) const BRANCH_PAGE: &str = "composer:folder:branches";

/// Rows shown before "N more · type to filter".
const SHOWN: usize = 8;

pub(crate) struct BranchMenu {
    dir: PathBuf,
    /// `None` while loading.
    data: Option<Result<Branches, String>>,
    /// A switch or create running, to this branch.
    busy: Option<String>,
    error: Option<String>,
    input: Entity<InputState>,
    _sub: Subscription,
}

impl SpaceApp {
    /// The branch page opened: a fresh field, focused, and the list loaded again.
    pub(crate) fn open_branch_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.selected.and_then(|k| self.thread(k)).map(|t| t.cwd.clone()) else { return };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter or name a new branch"));
        let sub = cx.subscribe_in(&input, window, |this: &mut SpaceApp, _, e: &InputEvent, window, cx| match e {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.branch_enter(window, cx),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.branch_menu = Some(BranchMenu { dir, data: None, busy: None, error: None, input, _sub: sub });
        self.load_branches(cx);
    }

    /// The menu moved: off the branch page, its field (which had focus) is gone, so the
    /// composer takes focus back (Esc and typing would go nowhere).
    pub(crate) fn left_branch_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_menu.is_some() && self.usage_open.as_deref() != Some(BRANCH_PAGE) {
            self.branch_menu = None;
            self.focus_composer(window, cx);
        }
    }

    /// Read the selected thread's branch again (it may have changed outside Space).
    pub(crate) fn refresh_branch(&mut self) {
        if let Some(t) = self.selected.and_then(|k| self.thread_mut(k)) {
            t.branch = format::git_branch(&t.cwd);
        }
    }

    fn load_branches(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.branch_menu.as_ref().map(|m| m.dir.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let read = dir.clone();
            let data = cx.background_spawn(async move { git::branches(&read) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = this.branch_menu.as_mut().filter(|m| m.dir == dir) {
                    menu.data = Some(data.unwrap_or_else(|| Err("Not a git repository".into())));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⏎ in the field: the first matching branch, else create the typed name.
    fn branch_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.selected.and_then(|k| self.thread(k)) else { return };
        let Some(choice) = self.branch_choice(thread, cx) else { return };
        if let Some(first) = choice.rows.as_ref().and_then(|r| r.first()) {
            let name = first.name.to_string();
            if !choice.detached && name == choice.head.as_ref() {
                self.show_usage(None, cx);
                self.focus_composer(window, cx);
            } else {
                self.switch_branch(name, false, window, cx);
            }
        } else if let Some(name) = choice.create {
            self.switch_branch(name.to_string(), true, window, cx);
        }
    }

    /// Switch the selected thread's folder to `name` (`create`: a new branch from what's checked
    /// out). Closes the menu when it worked; else says why on the page.
    pub(crate) fn switch_branch(&mut self, name: String, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(thread) = self.thread(key) else { return };
        if !thread.is_empty() || self.branch_blocked(thread).is_some() {
            return;
        }
        let dir = thread.cwd.clone();
        let Some(menu) = self.branch_menu.as_mut().filter(|m| m.dir == dir && m.busy.is_none()) else { return };
        menu.busy = Some(name.clone());
        menu.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (at, branch) = (dir.clone(), name.clone());
            let result = cx.background_spawn(async move { if create { git::create(&at, &branch) } else { git::switch(&at, &branch) } }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(t) = this.thread_mut(key) {
                    t.branch = format::git_branch(&t.cwd);
                }
                let Some(menu) = this.branch_menu.as_mut().filter(|m| m.dir == dir) else { return };
                menu.busy = None;
                match result {
                    Ok(()) => {
                        this.branch_menu = None;
                        if this.usage_open.as_deref().is_some_and(|s| s.starts_with(FOLDER_MENU)) {
                            this.show_usage(None, cx);
                        }
                        this.focus_composer(window, cx);
                    }
                    Err(e) => {
                        menu.error = Some(e);
                        this.load_branches(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Why the thread's folder can't switch branches now.
    fn branch_blocked(&self, thread: &Thread) -> Option<String> {
        let same = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        self.threads
            .iter()
            .any(|t| t.key != thread.key && t.state.running && same(&t.cwd, &thread.cwd))
            .then(|| "Another thread is working in this folder.".to_string())
    }

    /// What the composer shows for the thread's branch: the pill, and the branch page while
    /// it's open. `None` outside git.
    pub(crate) fn branch_choice(&self, thread: &Thread, cx: &App) -> Option<BranchChoice> {
        let head = thread.branch.clone()?;
        let mut choice = BranchChoice { head: head.into(), ..Default::default() };
        let Some(menu) = self.branch_menu.as_ref().filter(|m| m.dir == thread.cwd) else { return Some(choice) };
        choice.input = Some(menu.input.clone());
        choice.busy = menu.busy.clone().map(Into::into);
        choice.blocked = self.branch_blocked(thread).map(Into::into);
        match &menu.data {
            None => {}
            Some(Err(e)) => choice.error = Some(e.clone().into()),
            Some(Ok(data)) => {
                let query = menu.input.read(cx).value().trim().to_string();
                let rows = filter(data, &query, crate::app::now_ms());
                choice.more = rows.len().saturating_sub(SHOWN);
                choice.create = (!query.is_empty() && !data.list.iter().any(|b| b.name == query)).then(|| query.clone().into());
                choice.rows = Some(rows.into_iter().take(SHOWN).collect());
                choice.changed = data.changed;
                match &data.head {
                    Head::Branch(name) => choice.head = name.clone().into(),
                    Head::Detached(sha) => {
                        choice.head = sha.clone().into();
                        choice.detached = true;
                    }
                }
            }
        }
        if let Some(e) = &menu.error {
            choice.error = Some(e.clone().into());
        }
        Some(choice)
    }
}

/// The branches matching `query` (any part, any case), most recent first.
fn filter(data: &Branches, query: &str, now_ms: u64) -> Vec<BranchRow> {
    let q = query.to_lowercase();
    data.list
        .iter()
        .filter(|b| b.name.to_lowercase().contains(&q))
        .map(|b| BranchRow { name: b.name.clone().into(), when: b.committed_ms.map(|at| format::time_ago(at, now_ms)).unwrap_or_default().into() })
        .collect()
}

#[cfg(test)]
mod tests {
    // Explicit: `super::*` brings gpui's `test` macro along.
    use super::filter;
    use crate::git::{Branch, Branches, Head};

    #[test]
    fn filters_by_any_part_keeping_the_order() {
        let b = |name: &str| Branch { name: name.into(), committed_ms: Some(0) };
        let data = Branches { head: Head::Branch("main".into()), list: vec![b("main"), b("feat/Files"), b("fix/files-tree")], changed: 0 };
        let names = |q| filter(&data, q, 0).into_iter().map(|r| r.name.to_string()).collect::<Vec<_>>();
        assert_eq!(names("files"), ["feat/Files", "fix/files-tree"]);
        assert_eq!(names(""), ["main", "feat/Files", "fix/files-tree"]);
        assert!(names("zzz").is_empty());
    }
}
