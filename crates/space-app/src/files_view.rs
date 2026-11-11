//! The Files view, wired to the app: a tree per thread (in memory), listed as folders open, a
//! watcher on the shown thread's folder that lists changed folders again and refreshes git
//! status, the filter field, and what clicks, keys and the row menu do.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::component::input::{Editor, EditorState, InputEvent, InputState, RangeDecoration, RangeDecorationStyle, TextDecoration};
use gpui::*;
use notify::Watcher as _;
use space_ui::components::file_tree::{FileRow, GitMark, RowAction, TreeHandlers, TreeKey, file_tree, filter_field, notice};
use space_ui::components::file_viewer::{FileHeader, ViewChoice, file_header, image_preview, loading, unpreviewable};
use space_ui::components::right_pane::PaneView;
use space_ui::components::transcript::agent_message;
use space_ui::theme::color;

use crate::app::SpaceApp;
use crate::diff::{self, Diff, Kind};
use crate::files::{self, Tree};

/// Changes are gathered this long before the tree catches up.
const SETTLE: Duration = Duration::from_millis(200);

/// The Files view's shared parts: the filter field, the list's scroll and focus, the shown
/// file's focus, and the watcher on the shown folder.
pub(crate) struct FilesView {
    pub(crate) filter: Entity<InputState>,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) focus: FocusHandle,
    /// A shown file's (its editor takes it on a click), so ⌘W closes the file.
    pub(crate) file_focus: FocusHandle,
    watch: Option<Watch>,
    _sub: Subscription,
}

struct Watch {
    root: PathBuf,
    _watcher: notify::RecommendedWatcher,
    _task: Task<()>,
}

impl FilesView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SpaceApp>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let sub = cx.subscribe_in(&filter, window, |this, input, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                this.set_files_filter(text, cx);
            }
        });
        Self { filter, scroll: UniformListScrollHandle::new(), focus: cx.focus_handle(), file_focus: cx.focus_handle(), watch: None, _sub: sub }
    }
}

impl SpaceApp {
    /// The shown thread's tree, made (and listed) the first time, and again when the thread's
    /// folder changed.
    fn files_tree(&mut self, key: u64, cx: &mut Context<Self>) -> Option<&mut Tree> {
        let cwd = self.thread(key)?.cwd.clone();
        let root = cwd.canonicalize().unwrap_or(cwd);
        if self.file_trees.get(&key).is_none_or(|t| t.root != root) {
            self.file_trees.insert(key, Tree::new(root.clone()));
            self.list_dir(key, root.clone(), cx);
            self.refresh_git(key, cx);
        }
        self.file_trees.get_mut(&key)
    }

    /// List `dir` in the background into thread `key`'s tree. A folder holding only a folder
    /// lists that one too, so the chain shows as one row.
    fn list_dir(&mut self, key: u64, dir: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let target = dir.clone();
            let listed = cx.background_spawn(async move { files::list_dir(&target) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(tree) = this.file_trees.get_mut(&key) else { return };
                if !dir.starts_with(&tree.root) {
                    return;
                }
                match listed {
                    Ok(entries) => {
                        let next = match entries.as_slice() {
                            [only] if only.dir && !tree.children.contains_key(&only.path) => Some(only.path.clone()),
                            _ => None,
                        };
                        tree.errors.remove(&dir);
                        tree.children.insert(dir, entries);
                        if let Some(next) = next {
                            this.list_dir(key, next, cx);
                        }
                    }
                    Err(e) => {
                        tree.children.remove(&dir);
                        tree.errors.insert(dir, e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_git(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(root) = self.file_trees.get(&key).map(|t| t.root.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let status = cx.background_spawn(async move { files::git_status(&root) }).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if let Some(tree) = this.file_trees.get_mut(&key)
                    && tree.git != status
                {
                    tree.git = status;
                    // A commit, a stage or a checkout changes what open files show as changed.
                    let open: Vec<PathBuf> = this.file_tabs.get(&key).map(|t| t.tabs.iter().map(|t| t.path.clone()).collect()).unwrap_or_default();
                    for path in open {
                        this.read_tab(key, path, cx);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Search every file under the folder (once per filter, and again after changes).
    fn walk_all(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(root) = self.file_trees.get(&key).map(|t| t.root.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let all = cx.background_spawn(async move { files::all_files(&root) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(tree) = this.file_trees.get_mut(&key) {
                    tree.all = Some(all);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_files_filter(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(tree) = self.files_tree(key, cx) else { return };
        tree.filter = text;
        let walk = !tree.filter.trim().is_empty() && tree.all.is_none();
        if walk {
            self.walk_all(key, cx);
        }
        self.files.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Watch the shown thread's folder while its Files view shows (one folder at a time).
    fn watch_files(&mut self, root: &Path, cx: &mut Context<Self>) {
        if self.files.watch.as_ref().is_some_and(|w| w.root == root) {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(e) = res {
                let _ = tx.unbounded_send(e.paths);
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => return tracing::warn!("files: can't watch {}: {e}", root.display()),
        };
        if let Err(e) = watcher.watch(root, notify::RecursiveMode::Recursive) {
            return tracing::warn!("files: can't watch {}: {e}", root.display());
        }
        // Changes under ignored folders (build output) don't touch the tree.
        let ignored = {
            let mut b = ignore::gitignore::GitignoreBuilder::new(root);
            b.add(root.join(".gitignore"));
            b.build().ok()
        };
        let watched = root.to_path_buf();
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut paths = first;
                cx.background_executor().timer(SETTLE).await;
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                let git_dir = watched.join(".git");
                let mut git = false;
                paths.retain(|p| {
                    if p.starts_with(&git_dir) {
                        // Staging, commits, checkouts.
                        git |= p.file_name().is_some_and(|n| n == "index" || n == "HEAD");
                        return false;
                    }
                    !ignored.as_ref().is_some_and(|i| i.matched_path_or_any_parents(p, false).is_ignore())
                });
                if paths.is_empty() && !git {
                    continue;
                }
                let root = watched.clone();
                let _ = this.update(cx, |this, cx| this.files_changed(&root, &paths, cx));
            }
        });
        self.files.watch = Some(Watch { root: root.to_path_buf(), _watcher: watcher, _task: task });
    }

    /// Files changed under `root`: list their listed folders again, refresh git status, search
    /// again if a filter is on, and read open files again.
    fn files_changed(&mut self, root: &Path, paths: &[PathBuf], cx: &mut Context<Self>) {
        let keys: Vec<u64> = self.file_trees.iter().filter(|(_, t)| t.root == root).map(|(k, _)| *k).collect();
        for key in keys {
            let Some(tree) = self.file_trees.get_mut(&key) else { continue };
            let mut dirs: Vec<PathBuf> = paths.iter().filter_map(|p| tree.listed_parent(p)).collect();
            dirs.sort();
            dirs.dedup();
            let refilter = !tree.filter.trim().is_empty();
            tree.all = None;
            for dir in dirs {
                self.list_dir(key, dir, cx);
            }
            self.refresh_git(key, cx);
            if refilter {
                self.walk_all(key, cx);
            }
            let open: Vec<PathBuf> = self.file_tabs.get(&key).map(|t| t.tabs.iter().map(|t| t.path.clone()).collect()).unwrap_or_default();
            for path in open.into_iter().filter(|p| paths.contains(p)) {
                self.read_tab(key, path, cx);
            }
        }
    }

    // ----- the view ------------------------------------------------------------------------

    /// The open files' names, and the shown one (`None`: the tree), for the pane's tabs.
    pub(crate) fn file_tab_names(&self, key: u64) -> (Vec<SharedString>, Option<usize>) {
        match self.file_tabs.get(&key) {
            Some(t) => (t.tabs.iter().map(|t| SharedString::from(file_name(&t.path))).collect(), t.active),
            None => (Vec::new(), None),
        }
    }

    /// The Files view's content: the tree's page, or a file's preview. Its key context is
    /// `Files` (⌘W closes the file, or from the tree hides the pane).
    pub(crate) fn files_content(&mut self, key: u64, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.file_tabs.get(&key).and_then(|t| t.active) {
            None => self.files_page(key, cx),
            Some(ix) => self.file_tab_view(key, ix, cx),
        };
        div().key_context("Files").flex_1().min_h_0().flex().flex_col().child(content).into_any_element()
    }

    /// The Files page: the filter, then the tree.
    fn files_page(&mut self, key: u64, cx: &mut Context<Self>) -> AnyElement {
        let field = div().flex_none().p_2().flex().child(filter_field(&self.files.filter));
        div().flex_1().min_h_0().flex().flex_col().child(field).child(self.tree_element(key, cx)).into_any_element()
    }

    /// The tree, or why there's none.
    fn tree_element(&mut self, key: u64, cx: &mut Context<Self>) -> AnyElement {
        // A notice holds the tree's focus too, so ⌘P and the like still reach the app.
        let focus = self.files.focus.clone();
        let notice = move |m: SharedString| div().flex_1().min_h_0().track_focus(&focus).child(notice(m));
        let Some(tree) = self.files_tree(key, cx) else { return notice("No folder.".into()).into_any_element() };
        let root = tree.root.clone();
        let rows = tree.rows();
        let content = if let Some(e) = tree.errors.get(&root) {
            notice(format!("Can't list this folder: {e}").into()).into_any_element()
        } else if !tree.children.contains_key(&root) {
            notice("Listing…".into()).into_any_element()
        } else if !tree.filter.trim().is_empty() && tree.all.is_none() {
            notice("Searching…".into()).into_any_element()
        } else if rows.is_empty() {
            notice((if tree.filter.trim().is_empty() { "This folder is empty." } else { "No files match." }).into()).into_any_element()
        } else {
            let open_label = self.open_in_label().into();
            file_tree(Rc::new(rows), open_label, &self.files.scroll, &self.files.focus, self.tree_handlers(cx)).into_any_element()
        };
        self.watch_files(&root, cx);
        content
    }

    fn open_in_label(&self) -> String {
        format!("Open in {}", self.open_in_app().map_or("Finder", |a| a.known.name))
    }

    /// A file's tab: its header, then its preview.
    fn file_tab_view(&mut self, key: u64, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some((path, content, view, wrap)) = self.file_tabs.get(&key).and_then(|t| {
            let tab = t.tabs.get(ix)?;
            Some((tab.path.clone(), tab.content.clone(), tab.shown_view(), tab.wrap))
        }) else {
            return notice("").into_any_element();
        };
        let root = self.file_trees.get(&key).map(|t| t.root.clone()).unwrap_or_default();
        let shown_path = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
        let name = file_name(&path);
        let open_label: SharedString = self.open_in_label().into();
        let on_open = {
            let (this, path) = (cx.entity().downgrade(), path.clone());
            Rc::new(move |_: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.open_file(&path, cx));
            }) as Rc<dyn Fn(&mut Window, &mut App)>
        };
        let this = cx.entity().downgrade();
        let views = match &content {
            TabContent::Text { markdown, diff, .. } => {
                let mut v: Vec<(&str, TabView)> = Vec::new();
                if *markdown {
                    v.push(("Preview", TabView::Preview));
                }
                if diff.is_some() {
                    v.extend([("Changes", TabView::Changes), ("Full file", TabView::File)]);
                } else if *markdown {
                    v.push(("Source", TabView::File));
                }
                v.into_iter()
                    .map(|(label, choice)| {
                        let this = this.clone();
                        ViewChoice {
                            label: label.into(),
                            on: view == choice,
                            pick: Rc::new(move |_, cx| {
                                let _ = this.update(cx, |this, cx| this.set_tab_view(key, ix, choice, cx));
                            }),
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let changes = match &content {
            TabContent::Text { diff: Some(d), .. } => Some((d.added, d.removed)),
            _ => None,
        };
        let is_text = matches!(content, TabContent::Text { .. });
        let on_wrap = {
            let this = this.clone();
            Rc::new(move |on: bool, w: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.set_tab_wrap(key, ix, on, w, cx));
            }) as Rc<dyn Fn(bool, &mut Window, &mut App)>
        };
        let header = file_header(FileHeader {
            path: shown_path.into(),
            changes,
            views,
            wrap: (is_text && view != TabView::Preview).then_some((wrap, on_wrap)),
            open_label: open_label.clone(),
            on_open: on_open.clone(),
        });
        let code = |editor: &Entity<EditorState>| {
            div().flex_1().min_w_0().min_h_0().child(Editor::new(editor).readonly(true).appearance(false).bordered(false).h_full()).into_any_element()
        };
        let preview = match content {
            TabContent::Loading => loading().into_any_element(),
            TabContent::Image(size) => image_preview(&path, size).into_any_element(),
            TabContent::Text { source, .. } if view == TabView::Preview => div()
                .id("markdown-preview")
                .flex_1()
                .min_w_0()
                .overflow_y_scroll()
                .px_5()
                .py_4()
                .child(agent_message(SharedString::from(format!("file-md-{}", path.display())), &source))
                .into_any_element(),
            TabContent::Text { diff: Some(d), .. } if view == TabView::Changes => code(&d.changes),
            TabContent::Text { diff: Some(d), .. } => code(&d.full),
            TabContent::Text { editor, .. } => code(&editor),
            TabContent::Unpreviewable(why) => unpreviewable(&name, why, open_label, on_open, None).into_any_element(),
            TabContent::Failed(why) => {
                let retry = {
                    let (this, path) = (this.clone(), path.clone());
                    Rc::new(move |_: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| this.read_tab(key, path.clone(), cx));
                    }) as Rc<dyn Fn(&mut Window, &mut App)>
                };
                unpreviewable(&name, why, open_label, on_open, Some(retry)).into_any_element()
            }
        };
        div()
            .id("file-view")
            .track_focus(&self.files.file_focus)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().flex().child(preview))
            .into_any_element()
    }

    // ----- tabs ----------------------------------------------------------------------------

    /// Show `path` in a tab of thread `key`'s Files view: its tab if it's open, else a new one.
    pub(crate) fn show_file(&mut self, key: u64, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.file_tabs.entry(key).or_default();
        let ix = match tabs.tabs.iter().position(|t| t.path == path) {
            Some(ix) => ix,
            None => {
                tabs.tabs.push(FileTab { path: path.to_path_buf(), content: TabContent::Loading, view: None, wrap: false });
                tabs.tabs.len() - 1
            }
        };
        let fresh = matches!(tabs.tabs[ix].content, TabContent::Loading);
        tabs.active = Some(ix);
        self.terminals.panes.entry(key).or_default().view = Some(PaneView::Files);
        if let Some(tree) = self.file_trees.get_mut(&key) {
            tree.selected = Some(path.to_path_buf());
        }
        if fresh {
            self.read_tab(key, path.to_path_buf(), cx);
        }
        window.focus(&self.files.file_focus, cx);
        cx.notify();
    }

    pub(crate) fn select_file_tab(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        self.terminals.panes.entry(key).or_default().view = Some(PaneView::Files);
        let tabs = self.file_tabs.entry(key).or_default();
        tabs.active = ix.filter(|i| *i < tabs.tabs.len());
        if let Some(path) = tabs.active.map(|i| tabs.tabs[i].path.clone())
            && let Some(tree) = self.file_trees.get_mut(&key)
        {
            tree.selected = Some(path);
        }
        let focus = if tabs.active.is_some() { &self.files.file_focus } else { &self.files.focus };
        window.focus(focus, cx);
        cx.notify();
    }

    /// Close an open file; the next one shows (or the tree), and takes focus when the closed one
    /// was shown.
    pub(crate) fn close_file_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let Some(tabs) = self.file_tabs.get_mut(&key) else { return };
        if ix >= tabs.tabs.len() {
            return;
        }
        let was_shown = tabs.active == Some(ix);
        tabs.tabs.remove(ix);
        tabs.active = match tabs.active {
            Some(a) if a == ix => (!tabs.tabs.is_empty()).then(|| ix.min(tabs.tabs.len() - 1)),
            Some(a) if a > ix => Some(a - 1),
            other => other,
        };
        if was_shown {
            let active = tabs.active;
            self.select_file_tab(active, window, cx);
        }
        cx.notify();
    }

    /// ⌘W in the Files view: close the shown file; from the tree, hide the pane.
    pub(crate) fn close_files_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        match self.file_tabs.get(&key).and_then(|t| t.active) {
            Some(ix) => self.close_file_tab(ix, window, cx),
            None => self.hide_terminal(window, cx),
        }
    }

    fn set_tab_view(&mut self, key: u64, ix: usize, view: TabView, cx: &mut Context<Self>) {
        if let Some(tab) = self.file_tabs.get_mut(&key).and_then(|t| t.tabs.get_mut(ix)) {
            tab.view = Some(view);
        }
        cx.notify();
    }

    fn set_tab_wrap(&mut self, key: u64, ix: usize, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.file_tabs.get_mut(&key).and_then(|t| t.tabs.get_mut(ix)) else { return };
        tab.wrap = on;
        if let TabContent::Text { editor, diff, .. } = &tab.content {
            let editors = [Some(editor), diff.as_ref().map(|d| &d.changes), diff.as_ref().map(|d| &d.full)];
            for editor in editors.into_iter().flatten() {
                editor.update(cx, |e, cx| e.set_soft_wrap(on, window, cx));
            }
        }
        cx.notify();
    }

    /// Read the file shown in a tab (again), off the UI thread, and show what it is; a file git
    /// has changes in is also diffed against HEAD.
    fn read_tab(&mut self, key: u64, path: PathBuf, cx: &mut Context<Self>) {
        let handle = self.window_handle;
        let mark = self.file_trees.get(&key).and_then(|t| t.git.files.get(&path).copied());
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let (read, diffs) = cx
                .background_spawn(async move {
                    let read = files::read_preview(&p);
                    let diffs = match (&read, mark) {
                        (files::Preview::Text(new), Some(mark)) => {
                            let old = match mark {
                                GitMark::Added | GitMark::Untracked => String::new(),
                                _ => diff::head_text(&p).unwrap_or_default(),
                            };
                            let changes = diff::diff(&old, new, Some(diff::CONTEXT));
                            (changes.added + changes.removed > 0).then(|| (changes, diff::diff(&old, new, None)))
                        }
                        _ => None,
                    };
                    (read, diffs)
                })
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.show_preview(key, &path, read, diffs, window, cx));
            });
        })
        .detach();
    }

    fn show_preview(&mut self, key: u64, path: &Path, read: files::Preview, diffs: Option<(Diff, Diff)>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.file_tabs.get_mut(&key).and_then(|t| t.tabs.iter_mut().find(|t| t.path == path)) else { return };
        let (language, wrap) = (editor_language(path), tab.wrap);
        tab.content = match read {
            files::Preview::Text(text) => {
                let markdown = is_markdown(path);
                let source = SharedString::from(text.clone());
                let diff = diffs.map(|(changes, full)| {
                    // Read again: the same editors, so their scroll stays, unless the diff changed.
                    if let TabContent::Text { diff: Some(d), .. } = &tab.content
                        && d.rows == (changes.rows.clone(), full.rows.clone())
                    {
                        return d.clone();
                    }
                    DiffEditors {
                        added: changes.added,
                        removed: changes.removed,
                        rows: (changes.rows.clone(), full.rows.clone()),
                        changes: diff_editor(&changes, &language, wrap, window, cx),
                        full: diff_editor(&full, &language, wrap, window, cx),
                    }
                });
                let editor = match &tab.content {
                    // Read again: the same editor, so its scroll and selection stay.
                    TabContent::Text { editor, .. } => {
                        let editor = editor.clone();
                        editor.update(cx, |e, cx| {
                            if e.value() != source {
                                e.set_value(source.clone(), window, cx);
                            }
                        });
                        editor
                    }
                    _ => code_editor(source.clone(), &language, wrap, window, cx),
                };
                TabContent::Text { editor, markdown, source, diff }
            }
            files::Preview::Image(size) => TabContent::Image(size),
            files::Preview::Binary => TabContent::Unpreviewable("This file isn't text, so it can't be shown here.".into()),
            files::Preview::TooLarge(n) => TabContent::Unpreviewable(format!("This file is {} MB, too large to show here.", (n as f64 / 1_048_576.).ceil())),
            files::Preview::Failed(why) => TabContent::Failed(why),
        };
        cx.notify();
    }

    // ----- the tree's clicks, keys and menu ---------------------------------------------------

    fn tree_handlers(&self, cx: &mut Context<Self>) -> TreeHandlers {
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this);
        TreeHandlers {
            on_click: Rc::new(move |ix, w, cx| {
                let _ = t1.update(cx, |this, cx| this.click_file_row(ix, w, cx));
            }),
            on_action: Rc::new(move |ix, action, w, cx| {
                let _ = t2.update(cx, |this, cx| this.file_row_action(ix, action, w, cx));
            }),
            on_key: Rc::new(move |k, w, cx| {
                let _ = t3.update(cx, |this, cx| this.file_tree_key(k, w, cx));
            }),
        }
    }

    fn shown_rows(&self) -> Option<(u64, Vec<FileRow>)> {
        let key = self.selected?;
        Some((key, self.file_trees.get(&key)?.rows()))
    }

    fn select_file_row(&mut self, key: u64, row: &FileRow, ix: usize) {
        if let Some(tree) = self.file_trees.get_mut(&key) {
            tree.selected = Some(row.path.clone());
        }
        self.files.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
    }

    /// Open or close a folder (listing it the first time).
    fn set_folder_open(&mut self, key: u64, dir: &Path, open: bool, cx: &mut Context<Self>) {
        let Some(tree) = self.file_trees.get_mut(&key) else { return };
        if open {
            tree.expanded.insert(dir.to_path_buf());
            if !tree.children.contains_key(dir) {
                self.list_dir(key, dir.to_path_buf(), cx);
            }
        } else {
            tree.expanded.remove(dir);
        }
        cx.notify();
    }

    /// A folder opens or closes; a file shows in a tab.
    fn click_file_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, rows)) = self.shown_rows() else { return };
        let Some(row) = rows.get(ix).cloned() else { return };
        self.select_file_row(key, &row, ix);
        window.focus(&self.files.focus, cx);
        if row.dir {
            // A filter shows every folder open.
            if self.file_trees.get(&key).is_some_and(|t| t.filter.trim().is_empty()) {
                self.set_folder_open(key, &row.path, !row.expanded, cx);
            }
        } else {
            self.show_file(key, &row.path, window, cx);
        }
        cx.notify();
    }

    fn file_tree_key(&mut self, k: TreeKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, rows)) = self.shown_rows() else { return };
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|r| r.selected);
        let filtering = self.file_trees.get(&key).is_some_and(|t| !t.filter.trim().is_empty());
        let go = |this: &mut Self, ix: usize| {
            let ix = ix.min(rows.len() - 1);
            this.select_file_row(key, &rows[ix], ix);
        };
        match (k, at) {
            (TreeKey::Down, None) | (TreeKey::Up, None) => go(self, 0),
            (TreeKey::Down, Some(i)) => go(self, i + 1),
            (TreeKey::Up, Some(i)) => go(self, i.saturating_sub(1)),
            (TreeKey::Right, Some(i)) if rows[i].dir => {
                if !rows[i].expanded && !filtering {
                    self.set_folder_open(key, &rows[i].path, true, cx);
                } else if rows.get(i + 1).is_some_and(|r| r.depth > rows[i].depth) {
                    go(self, i + 1);
                }
            }
            (TreeKey::Left, Some(i)) => {
                if rows[i].dir && rows[i].expanded && !filtering {
                    self.set_folder_open(key, &rows[i].path, false, cx);
                } else if let Some(parent) = rows[..i].iter().rposition(|r| r.depth + 1 == rows[i].depth) {
                    go(self, parent);
                }
            }
            (TreeKey::Enter, Some(i)) => return self.click_file_row(i, window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn file_row_action(&mut self, ix: usize, action: RowAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, rows)) = self.shown_rows() else { return };
        let Some(row) = rows.get(ix) else { return };
        let path = row.path.clone();
        match action {
            RowAction::Open if row.dir => {}
            RowAction::Open => self.show_file(key, &path, window, cx),
            RowAction::OpenIn => self.open_file(&path, cx),
            RowAction::Reveal => {
                cx.background_spawn(async move {
                    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(&path).status();
                })
                .detach();
            }
            RowAction::CopyPath => cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string())),
            RowAction::CopyRelativePath => {
                let root = self.file_trees.get(&key).map(|t| t.root.clone()).unwrap_or_default();
                let rel = path.strip_prefix(&root).unwrap_or(&path);
                cx.write_to_clipboard(ClipboardItem::new_string(rel.display().to_string()));
            }
        }
    }
}

// ----- tabs' state -------------------------------------------------------------------------

/// A thread's open files in the Files view (in memory).
#[derive(Default)]
pub(crate) struct FileTabs {
    pub(crate) tabs: Vec<FileTab>,
    /// The shown tab; `None`: the Files page.
    pub(crate) active: Option<usize>,
}

pub(crate) struct FileTab {
    pub(crate) path: PathBuf,
    pub(crate) content: TabContent,
    /// The view picked; `None`: the file's default (see `shown_view`).
    pub(crate) view: Option<TabView>,
    pub(crate) wrap: bool,
}

/// How a text file shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabView {
    /// Markdown, rendered.
    Preview,
    /// A changed file's changes, with a few lines around each.
    Changes,
    /// The whole file (a changed one with its changes in place).
    File,
}

impl FileTab {
    /// The view shown: the one picked if the file has it, else Changes for a changed file,
    /// Preview for Markdown, or the file.
    pub(crate) fn shown_view(&self) -> TabView {
        let TabContent::Text { markdown, diff, .. } = &self.content else { return TabView::File };
        let has = |v: TabView| match v {
            TabView::Preview => *markdown,
            TabView::Changes => diff.is_some(),
            TabView::File => true,
        };
        match self.view.filter(|v| has(*v)) {
            Some(v) => v,
            None if diff.is_some() => TabView::Changes,
            None if *markdown => TabView::Preview,
            None => TabView::File,
        }
    }
}

#[derive(Clone)]
pub(crate) enum TabContent {
    Loading,
    /// Text in a read-only editor; Markdown can also show rendered, and a changed file its diff.
    Text { editor: Entity<EditorState>, markdown: bool, source: SharedString, diff: Option<DiffEditors> },
    Image(Option<(u64, u64)>),
    Unpreviewable(String),
    Failed(String),
}

/// A changed file's diff against HEAD, in two read-only editors: the changes only, and the
/// whole file.
#[derive(Clone)]
pub(crate) struct DiffEditors {
    added: usize,
    removed: usize,
    /// What they show, to keep them when a read finds the same diff.
    rows: (Vec<diff::Row>, Vec<diff::Row>),
    changes: Entity<EditorState>,
    full: Entity<EditorState>,
}

/// A read-only code editor: line numbers, syntax colors, search, no fold column.
fn code_editor(text: SharedString, language: &str, wrap: bool, window: &mut Window, cx: &mut Context<SpaceApp>) -> Entity<EditorState> {
    let language = language.to_string();
    cx.new(|cx| {
        let mut e = EditorState::new(window, cx)
            .language(language)
            .line_number(true)
            // No fold column: the line numbers stay narrow.
            .folding(false)
            .soft_wrap(wrap)
            .searchable(true)
            .default_value(text);
        e.set_readonly(true, cx);
        e
    })
}

/// A diff in a read-only editor: its rows as lines, the old and new numbers in the gutter,
/// added lines green and removed ones red (their changed words brighter), and the unchanged
/// lines left out as a quiet row.
fn diff_editor(d: &Diff, language: &str, wrap: bool, window: &mut Window, cx: &mut Context<SpaceApp>) -> Entity<EditorState> {
    let text: SharedString = d.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n").into();
    let width = d.rows.iter().filter_map(|r| r.old.max(r.new)).max().unwrap_or(1).to_string().len();
    let number = |n: Option<usize>| n.map_or_else(String::new, |n| n.to_string());
    let labels: Vec<SharedString> = d.rows.iter().map(|r| format!("{:>width$} {:>width$}", number(r.old), number(r.new)).into()).collect();
    let (green, red) = (color::green(), color::red());
    let mut backgrounds = HashMap::new();
    let (mut words, mut quiet) = (Vec::new(), Vec::new());
    // Opaque, so it replaces syntax colors (decorations blend over them).
    let muted = color::surface().blend(color::text_3());
    let mut at = 0;
    for (line, r) in d.rows.iter().enumerate() {
        let tint = match r.kind {
            Kind::Added => Some(green),
            Kind::Removed => Some(red),
            Kind::Skipped => {
                backgrounds.insert(line, (color::surface(), color::surface()));
                quiet.push(TextDecoration::new(at..at + r.text.len(), HighlightStyle { color: Some(muted), ..Default::default() }));
                None
            }
            Kind::Same => None,
        };
        if let Some(tint) = tint {
            backgrounds.insert(line, (tint.opacity(0.10), tint.opacity(0.18)));
            words.extend(r.words.iter().map(|w| RangeDecoration::new(at + w.start..at + w.end).with_style(RangeDecorationStyle::Fill).with_color(tint.opacity(0.30))));
        }
        at += r.text.len() + 1;
    }
    let editor = code_editor(text, language, wrap, window, cx);
    editor.update(cx, |e, cx| {
        e.set_line_labels(Some(labels), cx);
        e.set_line_backgrounds(backgrounds, cx);
        if !words.is_empty() {
            e.create_range_decorations_collection(words, cx);
        }
        if !quiet.is_empty() {
            e.create_decorations_collection(quiet, cx);
        }
    });
    editor
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_lowercase().as_str(), "md" | "mdx" | "markdown"))
}

/// The editor's language for a file: its extension (the highlighter knows those), or a name
/// for the well-known files without one.
fn editor_language(path: &Path) -> String {
    let name = file_name(path).to_lowercase();
    match name.as_str() {
        "makefile" | "gnumakefile" => "make".into(),
        "dockerfile" => "dockerfile".into(),
        "justfile" => "make".into(),
        _ => path.extension().and_then(|e| e.to_str()).map_or_else(|| "text".into(), str::to_lowercase),
    }
}
