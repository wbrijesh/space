//! The right pane's Changes mode, wired to the app: the thread's folder's uncommitted changes
//! file by file (one open at a time, its diff under it), and a bar that offers only what makes
//! sense next (`repo_state`): commit (Claude writes the message), push, pull, open or edit the
//! pull request (Claude writes it; `gh` opens it), switch or create branches mid-conversation.
//!
//! State is per folder, in memory. It's read again when the folder or its `.git` changes (a
//! watcher while the mode shows), after every step, and from the remote (a fetch, and the pull
//! request through `gh`) when the mode first shows, every few minutes while it shows, and after
//! a push. A step runs in the background, one at a time per folder, the status line saying
//! which part is running; a failure stops it there and says why.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::component::input::{EditorState, InputEvent, InputState, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use notify::Watcher as _;
use space_ui::components::changes::{
    ChangeAction, ChangedRow, ChangesBar, Handler, RowHandler, ChangesHandlers, ChangesMenu, Done, PrChip, PrEditorHandlers, PrEditorView, changed_row, changes_bar, changes_empty,
    pr_editor,
};
use space_ui::components::composer::BranchChoice;
use space_ui::components::file_tree::GitMark;
use space_ui::components::file_viewer::loading;
use space_ui::format;
use space_ui::theme::color;

use crate::app::SpaceApp;
use crate::diff::{self, Diff};
use crate::gh::{self, Gh, Pr, PrState};
use crate::git::{self, Head};
use crate::repo::{self, ChangedFile, FileStatus, Repo};
use crate::repo_state::{self, RepoState};

/// The remote is asked again after this long while the mode shows.
const REMOTE_EVERY: Duration = Duration::from_secs(5 * 60);
/// Changes are gathered this long before the state is read again.
const SETTLE: Duration = Duration::from_millis(300);
/// Rows shown in the branch menu before "N more".
const SHOWN_BRANCHES: usize = 8;
/// The key context of the mode (⌘W collapses the open file, then hides the pane).
pub(crate) const CHANGES_CONTEXT: &str = "Changes";

/// One folder's Changes state.
#[derive(Default)]
pub(crate) struct Changes {
    /// `None` until first read; `Err`: not a repository, or git failed.
    repo: Option<Result<Repo, String>>,
    loading: bool,
    load_again: bool,
    /// When the remote was last asked (`None`: not yet); asking now.
    remote_at: Option<Instant>,
    remote_busy: bool,
    fetched: bool,
    gh: Option<Gh>,
    pr: Option<Pr>,
    /// The open file (its path in the repository), and its diff once read.
    expanded: Option<String>,
    diff: Option<FileDiff>,
    /// The running step's label.
    running: Option<String>,
    error: Option<String>,
    /// What just happened, and the commit Undo takes back.
    done: Option<(String, Option<String>)>,
    menu: Option<ChangesMenu>,
    branches: Option<BranchPicker>,
    bases: Vec<String>,
    /// Picking a base goes on to this step.
    after_base: Option<ChangeAction>,
    editor: Option<PrEditor>,
}

struct FileDiff {
    path: String,
    /// `None`: binary or unreadable (`why`).
    view: Result<(Diff, Entity<EditorState>), String>,
}

struct BranchPicker {
    input: Entity<InputState>,
    data: Option<Result<git::Branches, String>>,
    busy: Option<String>,
    error: Option<String>,
    _sub: Subscription,
}

struct PrEditor {
    number: u64,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    base: String,
    base_open: bool,
    saving: bool,
    error: Option<String>,
}

/// The watcher on the shown folder.
pub(crate) struct ChangesWatch {
    root: PathBuf,
    _watcher: notify::RecommendedWatcher,
    _task: Task<()>,
}

/// What a running step carries from one part to the next.
#[derive(Default)]
struct Carry {
    message: Option<String>,
    branch: Option<String>,
    commit: Option<repo::Commit>,
    pr: Option<(String, String)>,
    opened: Option<String>,
}

/// The parts of a step, in order.
#[derive(Debug, Clone)]
enum Part {
    Stage,
    WriteCommit { branch: bool },
    NewBranch,
    Commit,
    Push,
    WritePr { base: String },
    CreatePr { base: String },
    Pull,
    SwitchTo(String),
}

impl Part {
    fn label(&self) -> String {
        match self {
            // Staging is instant: the commit message is what's seen being written.
            Part::Stage | Part::WriteCommit { .. } => "Writing the commit message…".into(),
            Part::NewBranch => "Creating the branch…".into(),
            Part::Commit => "Committing…".into(),
            Part::Push => "Pushing…".into(),
            Part::WritePr { .. } => "Writing the pull request…".into(),
            Part::CreatePr { .. } => "Opening the pull request…".into(),
            Part::Pull => "Pulling…".into(),
            Part::SwitchTo(b) => format!("Switching to {b}…"),
        }
    }
}

const COMMIT_PROMPT: &str = "Write a git commit message for the staged changes below.\n\nMatch how this repository's recent commits are written: the subject's form (an `Area: change` prefix, a plain sentence, conventional-commit types…), its capitalization and length, whether there's a body and how it's wrapped, and any trailer lines they all end with. Without a clear style: an imperative subject under 72 characters, a blank line, then a short body in plain prose saying what changed and why, wrapped at 72 columns (no body for a trivial change).\n\nReply with only the commit message: no preamble, no code fence.";
const BRANCH_LINE: &str = "\n\nThese changes go on a new branch: start your reply with a line `branch: <name>`, a short lowercase, hyphenated name for them (`feat/…`, `fix/…` or the repository's own style), then the commit message.";

impl SpaceApp {
    /// The selected thread's folder, while its pane shows Changes.
    fn changes_dir(&self) -> Option<PathBuf> {
        self.selected.and_then(|k| self.thread(k)).map(|t| t.cwd.clone())
    }

    // ----- reading ------------------------------------------------------------------------

    /// Read the folder's state again (once at a time; a change meanwhile reads once more after).
    pub(crate) fn load_changes(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let state = self.changes.entry(dir.clone()).or_default();
        if state.loading {
            state.load_again = true;
            return;
        }
        state.loading = true;
        let handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let at = dir.clone();
            let read = cx.background_spawn(async move { repo::read(&at) }).await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(state) = this.changes.get_mut(&dir) else { return };
                    state.loading = false;
                    let read = read.unwrap_or_else(|| Err("Not a git repository".into()));
                    let root = read.as_ref().ok().map(|r| r.root.clone());
                    if state.repo.as_ref() != Some(&read) {
                        state.repo = Some(read);
                        // The open file left the list (committed, reverted): close it.
                        let files = state.repo.as_ref().and_then(|r| r.as_ref().ok()).map(|r| r.files.clone()).unwrap_or_default();
                        if state.expanded.as_ref().is_some_and(|p| !files.iter().any(|f| &f.path == p)) {
                            state.expanded = None;
                            state.diff = None;
                        }
                        cx.notify();
                    }
                    let again = std::mem::take(&mut state.load_again);
                    if let Some(path) = state.expanded.clone() {
                        this.read_changed_file(dir.clone(), path, window, cx);
                    }
                    if let Some(root) = root {
                        this.watch_changes(&dir, &root, cx);
                    }
                    if again {
                        this.load_changes(dir.clone(), cx);
                    }
                });
            });
        })
        .detach();
    }

    /// Ask the remote: fetch (what `behind` counts), and the branch's pull request.
    fn refresh_remote(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        if state.remote_busy {
            return;
        }
        let (root, remote) = (repo.root.clone(), repo.remote.clone());
        state.remote_busy = true;
        state.remote_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let (fetch_root, gh_root) = (root.clone(), root.clone());
            let fetch_remote = remote.clone();
            let fetch = cx.background_spawn(async move { fetch_remote.map(|r| repo::fetch(&fetch_root, &r)) });
            let status = cx.background_spawn(async move { if remote.is_some() { Some(gh::status(&gh_root)) } else { None } });
            let (fetched, status) = futures::join!(fetch, status);
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.changes.get_mut(&dir) else { return };
                state.remote_busy = false;
                if let Some(Err(e)) = &fetched {
                    tracing::info!("changes: fetch in {} failed: {e}", root.display());
                }
                state.fetched = matches!(fetched, Some(Ok(())));
                if let Some((gh, pr)) = status {
                    state.gh = Some(gh);
                    state.pr = pr;
                }
                this.load_changes(dir.clone(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Watch the shown folder's repository: its files and its `.git` (commits, checkouts, the
    /// index), whoever changes them (Claude, a terminal, another app).
    fn watch_changes(&mut self, dir: &Path, root: &Path, cx: &mut Context<Self>) {
        if self.changes_watch.as_ref().is_some_and(|w| w.root == root) {
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
            Err(e) => return tracing::warn!("changes: can't watch {}: {e}", root.display()),
        };
        if let Err(e) = watcher.watch(root, notify::RecursiveMode::Recursive) {
            return tracing::warn!("changes: can't watch {}: {e}", root.display());
        }
        let ignored = {
            let mut b = ignore::gitignore::GitignoreBuilder::new(root);
            b.add(root.join(".gitignore"));
            b.build().ok()
        };
        let (git_dir, dir) = (root.join(".git"), dir.to_path_buf());
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut paths = first;
                cx.background_executor().timer(SETTLE).await;
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                let relevant = paths.iter().any(|p| match p.strip_prefix(&git_dir) {
                    Ok(inside) => {
                        let first = inside.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
                        matches!(first.as_str(), "index" | "HEAD" | "ORIG_HEAD" | "FETCH_HEAD" | "MERGE_HEAD" | "refs" | "packed-refs" | "rebase-merge" | "rebase-apply")
                    }
                    Err(_) => !ignored.as_ref().is_some_and(|i| i.matched_path_or_any_parents(p, false).is_ignore()),
                });
                if relevant {
                    let dir = dir.clone();
                    let _ = this.update(cx, |this, cx| this.load_changes(dir, cx));
                }
            }
        });
        self.changes_watch = Some(ChangesWatch { root: root.to_path_buf(), _watcher: watcher, _task: task });
    }

    /// Read the open file's diff: HEAD against what's on disk.
    fn read_changed_file(&mut self, dir: PathBuf, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.changes.get(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        let Some(file) = repo.files.iter().find(|f| f.path == path).cloned() else { return };
        let root = repo.root.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let read = cx.background_spawn(async move { file_diff(&root, &file) }).await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(state) = this.changes.get_mut(&dir).filter(|s| s.expanded.as_deref() == Some(path.as_str())) else { return };
                    let view = match read {
                        Ok(d) => {
                            // The same diff: keep its editor (and its scroll).
                            if let Some(FileDiff { view: Ok((old, editor)), .. }) = state.diff.as_ref().filter(|f| f.path == path)
                                && old.rows == d.rows
                            {
                                Ok((old.clone(), editor.clone()))
                            } else {
                                let editor = crate::files_view::diff_editor(&d, &crate::files_view::editor_language(Path::new(&path)), false, window, cx);
                                Ok((d, editor))
                            }
                        }
                        Err(e) => Err(e),
                    };
                    state.diff = Some(FileDiff { path, view });
                    cx.notify();
                });
            });
        })
        .detach();
    }

    // ----- the list -----------------------------------------------------------------------

    fn toggle_changed_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        window.focus(&self.changes_focus, cx);
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        let Some(path) = repo.files.get(ix).map(|f| f.path.clone()) else { return };
        if state.expanded.as_deref() == Some(path.as_str()) {
            state.expanded = None;
            state.diff = None;
        } else {
            state.expanded = Some(path.clone());
            self.read_changed_file(dir, path, window, cx);
        }
        cx.notify();
    }

    /// ⌘W in Changes: close the open file; with none open, hide the pane.
    pub(crate) fn close_changes_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.changes_dir().and_then(|d| self.changes.get_mut(&d)).filter(|s| s.expanded.is_some() || s.editor.is_some());
        match open {
            Some(state) => {
                if state.editor.take().is_none() {
                    state.expanded = None;
                    state.diff = None;
                }
                cx.notify();
            }
            None => self.hide_terminal(window, cx),
        }
    }

    // ----- menus --------------------------------------------------------------------------

    fn set_changes_menu(&mut self, menu: Option<ChangesMenu>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(state) = self.changes.get_mut(&dir) else { return };
        if menu != Some(ChangesMenu::Base) {
            state.after_base = None;
        }
        state.menu = menu;
        match menu {
            Some(ChangesMenu::Branch) => self.open_branch_picker(dir, window, cx),
            Some(ChangesMenu::Base) => self.load_bases(dir, cx),
            _ => {
                state.branches = None;
                window.focus(&self.changes_focus, cx);
            }
        }
        cx.notify();
    }

    fn open_branch_picker(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter or name a new branch"));
        let sub = cx.subscribe_in(&input, window, |this: &mut SpaceApp, _, e: &InputEvent, window, cx| match e {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.branch_picker_enter(window, cx),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        if let Some(state) = self.changes.get_mut(&dir) {
            state.branches = Some(BranchPicker { input, data: None, busy: None, error: None, _sub: sub });
        }
        cx.spawn(async move |this, cx| {
            let at = dir.clone();
            let data = cx.background_spawn(async move { git::branches(&at) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(p) = this.changes.get_mut(&dir).and_then(|s| s.branches.as_mut()) {
                    p.data = Some(data.unwrap_or_else(|| Err("Not a git repository".into())));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⏎ in the branch field: the first matching branch, else create the typed name here.
    fn branch_picker_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(choice) = self.branch_picker_choice(&dir, cx) else { return };
        if let Some(first) = choice.rows.as_ref().and_then(|r| r.first()) {
            if choice.detached || first.name != choice.head {
                self.switch_changes_branch(first.name.to_string(), None, false, window, cx);
            } else {
                self.set_changes_menu(None, window, cx);
            }
        } else if let Some(name) = choice.create {
            self.switch_changes_branch(name.to_string(), None, true, window, cx);
        }
    }

    fn branch_picker_choice(&self, dir: &Path, cx: &App) -> Option<BranchChoice> {
        let state = self.changes.get(dir)?;
        let picker = state.branches.as_ref()?;
        let mut choice = BranchChoice { input: Some(picker.input.clone()), busy: picker.busy.clone().map(Into::into), ..Default::default() };
        choice.blocked = self.switch_blocked(dir).map(Into::into);
        match &picker.data {
            None => {}
            Some(Err(e)) => choice.error = Some(e.clone().into()),
            Some(Ok(data)) => {
                let query = picker.input.read(cx).value().trim().to_string();
                let rows = crate::branches::filter(data, &query, crate::app::now_ms());
                choice.more = rows.len().saturating_sub(SHOWN_BRANCHES);
                choice.create = (!query.is_empty() && !data.list.iter().any(|b| b.name == query)).then(|| query.clone().into());
                choice.rows = Some(rows.into_iter().take(SHOWN_BRANCHES).collect());
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
        if let Some(e) = &picker.error {
            choice.error = Some(e.clone().into());
        }
        Some(choice)
    }

    /// Why the folder can't switch branches now: a thread working in it would have its files
    /// change under it.
    fn switch_blocked(&self, dir: &Path) -> Option<String> {
        let same = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        self.threads.iter().chain(self.side_chats.values().map(|s| &s.thread)).any(|t| t.state.running && same(&t.cwd, dir)).then(|| "A thread is working in this folder.".to_string())
    }

    /// Switch the folder to branch `name`, or create it (from `from`, else what's checked out).
    fn switch_changes_branch(&mut self, name: String, from: Option<String>, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        if self.switch_blocked(&dir).is_some() {
            return;
        }
        let Some(picker) = self.changes.get_mut(&dir).and_then(|s| s.branches.as_mut()).filter(|p| p.busy.is_none()) else { return };
        picker.busy = Some(name.clone());
        picker.error = None;
        cx.notify();
        let before = format::git_branch(&dir);
        cx.spawn_in(window, async move |this, cx| {
            let (at, branch) = (dir.clone(), name.clone());
            let result = cx
                .background_spawn(async move {
                    let root = repo::root(&at).unwrap_or(at);
                    if create { repo::create_branch(&root, &branch, from.as_deref()) } else { git::switch(&root, &branch) }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        if let Some(state) = this.changes.get_mut(&dir) {
                            state.branches = None;
                            state.menu = None;
                            state.pr = None;
                        }
                        this.branch_switched(&dir, before.as_deref(), cx);
                        window.focus(&this.changes_focus, cx);
                        this.refresh_remote(dir.clone(), cx);
                    }
                    Err(e) => {
                        if let Some(p) = this.changes.get_mut(&dir).and_then(|s| s.branches.as_mut()) {
                            p.busy = None;
                            p.error = Some(e);
                        }
                    }
                }
                this.load_changes(dir.clone(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The folder is on another branch now: threads in it show it, and those mid-conversation
    /// are told with their next message (they'd assume the old branch).
    fn branch_switched(&mut self, dir: &Path, before: Option<&str>, cx: &mut Context<Self>) {
        let now = format::git_branch(dir);
        let same = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        let note = match (before, now.as_deref()) {
            (Some(b), Some(n)) if b != n => Some(format!(
                "<branch-changed from=\"{b}\" to=\"{n}\">The user switched this folder from branch `{b}` to `{n}` outside this conversation; uncommitted changes came along. Work on `{n}` from now on.</branch-changed>"
            )),
            _ => None,
        };
        let keys: Vec<u64> = self.threads.iter().filter(|t| same(&t.cwd, dir)).map(|t| t.key).collect();
        for key in keys {
            if let Some(t) = self.thread_mut(key) {
                t.branch = format::git_branch(&t.cwd);
                if !t.is_empty() && note.is_some() {
                    t.branch_note = note.clone();
                }
            }
        }
        cx.notify();
    }

    fn load_bases(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(Some(Ok(repo))) = self.changes.get(&dir).map(|s| &s.repo) else { return };
        let (root, remote, current) = (repo.root.clone(), repo.remote.clone(), repo.branch().map(str::to_string));
        cx.spawn(async move |this, cx| {
            let bases = cx.background_spawn(async move { repo::bases(&root, remote.as_deref(), current.as_deref()) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.changes.get_mut(&dir) {
                    state.bases = bases;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A base picked: remembered for the repository; then the step that asked, if any.
    fn pick_base(&mut self, base: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &mut state.repo else { return };
        repo.base = Some(base.clone());
        repo.base_saved = true;
        let root = repo.root.clone();
        let then = state.after_base.take();
        state.menu = None;
        cx.background_spawn(async move {
            if let Err(e) = repo::save_base(&root, &base) {
                tracing::warn!("changes: couldn't save the base: {e}");
            }
        })
        .detach();
        if let Some(action) = then {
            self.changes_action(action, window, cx);
        } else {
            self.load_changes(dir, cx);
        }
        cx.notify();
    }

    // ----- steps --------------------------------------------------------------------------

    pub(crate) fn changes_action(&mut self, action: ChangeAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        if state.running.is_some() {
            return;
        }
        state.menu = None;
        let base = repo.base.clone();
        // The first pull request in a repository asks where it goes (then it's the default).
        let wants_base = matches!(action, ChangeAction::CreatePr | ChangeAction::CommitAndPr);
        if wants_base && (!repo.base_saved || base.is_none()) {
            state.after_base = Some(action);
            state.menu = Some(ChangesMenu::Base);
            self.load_bases(dir, cx);
            cx.notify();
            return;
        }
        let base = base.unwrap_or_default();
        let parts = match action {
            ChangeAction::Commit => vec![Part::Stage, Part::WriteCommit { branch: false }, Part::Commit],
            ChangeAction::CommitAndPush => vec![Part::Stage, Part::WriteCommit { branch: false }, Part::Commit, Part::Push],
            ChangeAction::CommitAndPr => {
                vec![Part::Stage, Part::WriteCommit { branch: false }, Part::Commit, Part::Push, Part::WritePr { base: base.clone() }, Part::CreatePr { base }]
            }
            ChangeAction::CommitToNewBranch => vec![Part::Stage, Part::WriteCommit { branch: true }, Part::NewBranch, Part::Commit],
            ChangeAction::Push => vec![Part::Push],
            ChangeAction::Pull => vec![Part::Pull],
            ChangeAction::CreatePr => vec![Part::Push, Part::WritePr { base: base.clone() }, Part::CreatePr { base }],
            ChangeAction::SwitchTo(b) => vec![Part::SwitchTo(b.to_string())],
            ChangeAction::NewBranch => return self.set_changes_menu(Some(ChangesMenu::Branch), window, cx),
            ChangeAction::EditPr => return self.open_pr_editor(window, cx),
        };
        if parts.iter().any(|p| matches!(p, Part::NewBranch | Part::SwitchTo(_))) && self.switch_blocked(&dir).is_some() {
            let why = self.switch_blocked(&dir);
            if let Some(state) = self.changes.get_mut(&dir) {
                state.error = why;
            }
            cx.notify();
            return;
        }
        self.run_parts(dir, parts, cx);
    }

    /// The session Claude writes with: the selected thread's, else any running one.
    fn writer_session(&self) -> Option<String> {
        let selected = self.selected.and_then(|k| self.thread(k)).and_then(|t| t.live_session()).map(|s| s.id().to_string());
        selected.or_else(|| self.threads.iter().find_map(|t| t.live_session()).map(|s| s.id().to_string()))
    }

    fn run_parts(&mut self, dir: PathBuf, parts: Vec<Part>, cx: &mut Context<Self>) {
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        let root = repo.root.clone();
        state.running = parts.first().map(Part::label);
        state.error = None;
        state.done = None;
        let (host, session) = (self.host.clone(), self.writer_session());
        let before = format::git_branch(&dir);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut carry = Carry::default();
            let mut failed = None;
            let mut remote_changed = false;
            for part in parts {
                let label = part.label();
                let _ = this.update(cx, |this, cx| {
                    if let Some(s) = this.changes.get_mut(&dir) {
                        s.running = Some(label);
                        cx.notify();
                    }
                });
                remote_changed |= matches!(part, Part::Push | Part::CreatePr { .. } | Part::Pull | Part::SwitchTo(_));
                let result = run_part(&part, &root, &mut carry, &host, session.as_deref(), cx).await;
                if let Err(e) = result {
                    failed = Some(e);
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                let Some(s) = this.changes.get_mut(&dir) else { return };
                s.running = None;
                s.error = failed;
                s.done = match (&carry.opened, &carry.commit) {
                    // The pull request's chip shows it.
                    (Some(_), _) => None,
                    (None, Some(c)) => Some((format!("Committed {} · {}", c.sha, c.subject), Some(c.sha.clone()))),
                    _ => None,
                };
                this.branch_switched(&dir, before.as_deref(), cx);
                if remote_changed {
                    this.refresh_remote(dir.clone(), cx);
                } else {
                    this.load_changes(dir.clone(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn undo_commit(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some((_, Some(sha))) = state.done.take() else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        let root = repo.root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { repo::uncommit(&root, &sha) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(s) = this.changes.get_mut(&dir) {
                    s.error = result.err();
                    s.done = s.error.is_none().then(|| ("Commit undone".to_string(), None));
                }
                this.load_changes(dir, cx);
                cx.notify();
            });
        })
        .detach();
    }

    // ----- the pull request's editor --------------------------------------------------------

    fn open_pr_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(pr) = self.changes.get(&dir).and_then(|s| s.pr.clone()) else { return };
        let title = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("Title");
            s.set_value(pr.title.clone(), window, cx);
            s
        });
        let body = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).placeholder("Description");
            s.set_value(pr.body.clone(), window, cx);
            s
        });
        title.update(cx, |s, cx| s.focus(window, cx));
        if let Some(state) = self.changes.get_mut(&dir) {
            state.editor = Some(PrEditor { number: pr.number, title, body, base: pr.base_ref_name.clone(), base_open: false, saving: false, error: None });
        }
        self.load_bases(dir, cx);
        cx.notify();
    }

    fn save_pr(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.changes_dir() else { return };
        let Some(state) = self.changes.get_mut(&dir) else { return };
        let Some(Ok(repo)) = &state.repo else { return };
        let root = repo.root.clone();
        let Some(editor) = state.editor.as_mut().filter(|e| !e.saving) else { return };
        let (number, title, body, base) = (editor.number, editor.title.read(cx).value().trim().to_string(), editor.body.read(cx).value().to_string(), editor.base.clone());
        if title.is_empty() {
            editor.error = Some("A pull request needs a title.".into());
            cx.notify();
            return;
        }
        editor.saving = true;
        editor.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { gh::edit(&root, number, &title, &body, &base) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(s) = this.changes.get_mut(&dir) else { return };
                match result {
                    Ok(()) => {
                        s.editor = None;
                        s.done = Some((format!("Pull request #{number} saved"), None));
                        this.refresh_remote(dir, cx);
                    }
                    Err(e) => {
                        if let Some(ed) = s.editor.as_mut() {
                            ed.saving = false;
                            ed.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The selected thread's Changes, for dev scripts.
    #[cfg(feature = "dev")]
    pub(crate) fn changes_summary(&self, cx: &App) -> String {
        let Some(dir) = self.changes_dir() else { return "no thread".into() };
        let Some(state) = self.changes.get(&dir) else { return "not shown yet".into() };
        let repo = match &state.repo {
            None => return "loading".into(),
            Some(Err(e)) => return format!("error {e:?}"),
            Some(Ok(r)) => r,
        };
        let b = self.changes_bar_data(&dir, repo, cx);
        let files: Vec<String> = repo.files.iter().map(|f| format!("{:?} {} {:?}", f.status, f.path, f.lines)).collect();
        format!(
            "branch={} base={:?} status={:?} primary={:?} menu={:?} open={:?} pr={:?} files={files:?} expanded={:?} diff={} error={:?} done={:?} note={:?} gh={:?} fetched={} told={:?}",
            b.branch,
            b.base,
            b.status,
            b.primary.map(|a| a.label()),
            b.menu.iter().map(|a| a.label()).collect::<Vec<_>>(),
            b.open,
            b.pr.map(|p| format!("#{} {} {}", p.number, p.state, p.title)),
            state.expanded,
            state.diff.as_ref().map_or("none".into(), |d| match &d.view {
                Ok((d, _)) => format!("+{} -{} rows={}", d.added, d.removed, d.rows.len()),
                Err(e) => e.clone(),
            }),
            b.error,
            b.done.map(|d| (d.text, d.undo)),
            b.note,
            state.gh,
            state.fetched,
            self.threads.iter().filter_map(|t| t.branch_note.as_deref()).collect::<Vec<_>>(),
        )
    }

    // ----- the view -----------------------------------------------------------------------

    /// The Changes mode of thread `key`'s pane.
    pub(crate) fn changes_content(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(dir) = self.thread(key).map(|t| t.cwd.clone()) else { return div().into_any_element() };
        let state = self.changes.entry(dir.clone()).or_default();
        let first = state.repo.is_none() && !state.loading;
        let remote_due = matches!(state.repo, Some(Ok(_))) && !state.remote_busy && state.remote_at.is_none_or(|at| at.elapsed() > REMOTE_EVERY);
        if first {
            self.load_changes(dir.clone(), cx);
        }
        if remote_due {
            self.refresh_remote(dir.clone(), cx);
        }
        let body = match self.changes.get(&dir).and_then(|s| s.repo.clone()) {
            None => loading().into_any_element(),
            // "Not a git repository", or why it couldn't be read.
            Some(Err(e)) => changes_empty(e).into_any_element(),
            Some(Ok(repo)) => self.changes_page(&dir, &repo, window, cx),
        };
        div()
            .key_context(CHANGES_CONTEXT)
            .track_focus(&self.changes_focus)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && !e.keystroke.modifiers.modified() {
                    let open = this.changes_dir().and_then(|d| this.changes.get(&d)).is_some_and(|s| s.menu.is_some());
                    if open {
                        this.set_changes_menu(None, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(body)
            .into_any_element()
    }

    fn changes_page(&mut self, dir: &Path, repo: &Repo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let bar = self.changes_bar_data(dir, repo, cx);
        let handlers = self.changes_handlers(cx);
        let editor = self.pr_editor_view(dir, cx);
        let state = self.changes.get(dir).expect("made above");
        let on_click: RowHandler = {
            let this = cx.entity().downgrade();
            Rc::new(move |ix, w, cx| {
                let _ = this.update(cx, |this, cx| this.toggle_changed_file(ix, w, cx));
            })
        };
        let rows: Vec<ChangedRow> = repo.files.iter().map(|f| changed_row_data(f, state.expanded.as_deref() == Some(f.path.as_str()))).collect();
        let open = rows.iter().position(|r| r.expanded);
        // Nothing changed: the bar's status says so, and the list is empty.
        let list = if repo.files.is_empty() {
            div().flex_1().into_any_element()
        } else {
            let row = |ix: usize, r: &ChangedRow| changed_row(ix, r, on_click.clone()).into_any_element();
            match open {
                None => div()
                    .id("changes-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows.iter().enumerate().map(|(ix, r)| row(ix, r)))
                    .into_any_element(),
                Some(at) => {
                    let diff = match state.diff.as_ref().filter(|d| Some(&d.path) == state.expanded.as_ref()) {
                        None => loading().into_any_element(),
                        Some(FileDiff { view: Ok((d, editor)), .. }) if d.added + d.removed > 0 => {
                            div().size_full().child(gpui::component::input::Editor::new(editor).appearance(false).size_full()).into_any_element()
                        }
                        Some(FileDiff { view: Ok(_), .. }) => changes_empty("Mode or name change only").into_any_element(),
                        Some(FileDiff { view: Err(why), .. }) => changes_empty(why.clone()).into_any_element(),
                    };
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when(at > 0, |d| {
                            d.child(div().id("changes-before").flex_shrink(1.).min_h(px(0.)).overflow_y_scroll().flex().flex_col().children(rows[..at].iter().enumerate().map(|(ix, r)| row(ix, r))))
                        })
                        .child(row(at, &rows[at]))
                        .child(div().flex_1().min_h(px(160.)).flex().flex_col().border_b_1().border_color(color::border()).child(diff))
                        .when(at + 1 < rows.len(), |d| {
                            d.child(
                                div()
                                    .id("changes-after")
                                    .flex_shrink(1.)
                                    .min_h(px(0.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .children(rows[at + 1..].iter().enumerate().map(|(i, r)| row(at + 1 + i, r))),
                            )
                        })
                        .into_any_element()
                }
            }
        };
        let _ = window;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(changes_bar(&bar, &handlers))
            .children(editor)
            .child(list)
            .into_any_element()
    }

    fn changes_bar_data(&self, dir: &Path, repo: &Repo, cx: &App) -> ChangesBar {
        let state = self.changes.get(dir);
        let pr = state.and_then(|s| s.pr.as_ref());
        let input = RepoState {
            branch: repo.branch().map(str::to_string),
            changed: repo.files.len(),
            conflicts: repo.conflicts(),
            operation: repo.operation,
            remote: repo.remote.clone(),
            upstream: repo.upstream.clone(),
            ahead: repo.ahead,
            behind: state.filter(|s| s.fetched).map(|_| repo.behind),
            on_base: repo.on_base(),
            base: repo.base.clone(),
            ahead_of_base: repo.ahead_of_base,
            gh: state.and_then(|s| s.gh.clone()),
            pr: pr.map(|p| (p.number, p.state)),
            running: state.and_then(|s| s.running.clone()),
        };
        let b = repo_state::bar(&input);
        let (branch, detached) = match &repo.head {
            Head::Branch(n) => (n.clone(), false),
            Head::Detached(sha) => (sha.clone(), true),
        };
        let undo_ok = |sha: &str| repo.last.as_ref().is_some_and(|c| c.sha == sha) && (repo.upstream.is_none() || repo.ahead > 0);
        ChangesBar {
            branch: branch.into(),
            detached,
            base: b.show_base.then(|| pr.filter(|p| p.state == PrState::Open).map(|p| p.base_ref_name.clone()).or_else(|| repo.base.clone())).flatten().map(Into::into),
            status: b.status.into(),
            tone: b.tone,
            primary: b.primary,
            menu: b.menu,
            running: input.running.is_some(),
            pr: pr.filter(|p| p.state != PrState::Closed || input.ahead == 0).map(|p| PrChip {
                number: p.number,
                title: p.title.clone().into(),
                state: match (p.state, p.is_draft) {
                    (PrState::Open, true) => "draft",
                    (PrState::Open, false) => "open",
                    (PrState::Merged, _) => "merged",
                    (PrState::Closed, _) => "closed",
                }
                .into(),
            }),
            note: b.note.map(Into::into),
            error: state.and_then(|s| s.error.clone()).map(Into::into),
            done: state.and_then(|s| s.done.clone()).map(|(text, sha)| Done { text: text.into(), undo: sha.as_deref().is_some_and(undo_ok) }),
            open: state.and_then(|s| s.menu),
            branches: self.branch_picker_choice(dir, cx),
            branch_from: repo.default_branch.clone().map(Into::into),
            bases: state.map(|s| s.bases.iter().cloned().map(Into::into).collect()).unwrap_or_default(),
            base_then_create: state.is_some_and(|s| s.after_base.is_some()),
        }
    }

    fn changes_handlers(&self, cx: &mut Context<Self>) -> ChangesHandlers {
        let this = cx.entity().downgrade();
        let h = |f: fn(&mut SpaceApp, &mut Window, &mut Context<SpaceApp>)| -> Handler {
            let this = this.clone();
            Rc::new(move |w, cx| {
                let _ = this.update(cx, |app, cx| f(app, w, cx));
            })
        };
        let (t1, t2, t3, t4, t5) = (this.clone(), this.clone(), this.clone(), this.clone(), this.clone());
        ChangesHandlers {
            on_action: Rc::new(move |a, w, cx| {
                let _ = t1.update(cx, |app, cx| app.changes_action(a, w, cx));
            }),
            on_menu: Rc::new(move |m, w, cx| {
                let _ = t2.update(cx, |app, cx| app.set_changes_menu(m, w, cx));
            }),
            on_open_pr: h(|app, _, _| {
                if let Some(url) = app.changes_dir().and_then(|d| app.changes.get(&d)).and_then(|s| s.pr.as_ref()).map(|p| p.url.clone()) {
                    crate::app::open_url(url);
                }
            }),
            on_undo: h(|app, _, cx| app.undo_commit(cx)),
            on_dismiss: h(|app, _, cx| {
                if let Some(s) = app.changes_dir().and_then(|d| app.changes.get_mut(&d)) {
                    s.error = None;
                    s.done = None;
                    cx.notify();
                }
            }),
            on_base: Rc::new(move |name, w, cx| {
                let _ = t3.update(cx, |app, cx| app.pick_base(name.to_string(), w, cx));
            }),
            on_branch: Rc::new(move |name, w, cx| {
                let _ = t4.update(cx, |app, cx| app.switch_changes_branch(name.to_string(), None, false, w, cx));
            }),
            on_new_branch: Rc::new(move |name, from, w, cx| {
                let _ = t5.update(cx, |app, cx| app.switch_changes_branch(name.to_string(), from.map(|f| f.to_string()), true, w, cx));
            }),
        }
    }

    fn pr_editor_view(&self, dir: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.changes.get(dir)?;
        let e = state.editor.as_ref()?;
        let view = PrEditorView {
            number: e.number,
            title: e.title.clone(),
            body: e.body.clone(),
            base: e.base.clone().into(),
            bases: state.bases.iter().cloned().map(Into::into).collect(),
            base_open: e.base_open,
            saving: e.saving,
            error: e.error.clone().map(Into::into),
        };
        let this = cx.entity().downgrade();
        let (t1, t2, t3, t4) = (this.clone(), this.clone(), this.clone(), this);
        let handlers = PrEditorHandlers {
            on_save: Rc::new(move |_, cx| {
                let _ = t1.update(cx, |app, cx| app.save_pr(cx));
            }),
            on_cancel: Rc::new(move |w, cx| {
                let _ = t2.update(cx, |app, cx| {
                    if let Some(s) = app.changes_dir().and_then(|d| app.changes.get_mut(&d)) {
                        s.editor = None;
                    }
                    w.focus(&app.changes_focus, cx);
                    cx.notify();
                });
            }),
            on_base_menu: Rc::new(move |open, _, cx| {
                let _ = t3.update(cx, |app, cx| {
                    if let Some(e) = app.changes_dir().and_then(|d| app.changes.get_mut(&d)).and_then(|s| s.editor.as_mut()) {
                        e.base_open = open;
                        cx.notify();
                    }
                });
            }),
            on_base: Rc::new(move |name, _, cx| {
                let _ = t4.update(cx, |app, cx| {
                    if let Some(e) = app.changes_dir().and_then(|d| app.changes.get_mut(&d)).and_then(|s| s.editor.as_mut()) {
                        e.base = name.to_string();
                        e.base_open = false;
                        cx.notify();
                    }
                });
            }),
        };
        Some(pr_editor(&view, &handlers).into_any_element())
    }
}

/// One part of a step, in the background (Claude's writing through the host).
async fn run_part(part: &Part, root: &Path, carry: &mut Carry, host: &space_core::Host, session: Option<&str>, cx: &mut AsyncApp) -> Result<(), String> {
    let root = root.to_path_buf();
    let bg = |f: Box<dyn FnOnce() -> Result<(), String> + Send>| cx.background_spawn(async move { f() });
    match part {
        Part::Stage => {
            let r = root.clone();
            bg(Box::new(move || repo::stage_all(&r))).await
        }
        Part::WriteCommit { branch } => {
            let r = root.clone();
            let brief = cx.background_spawn(async move { repo::commit_brief(&r) }).await?;
            let session = session.ok_or("Start this thread's agent first: Claude writes the commit message.")?;
            let prompt = format!("{COMMIT_PROMPT}{}\n\n{brief}", if *branch { BRANCH_LINE } else { "" });
            let reply = host.complete(session, &prompt).await.map_err(|e| format!("Claude couldn't write the commit message: {e}"))?;
            let (name, message) = repo::parse_message(&reply, *branch);
            if message.is_empty() {
                return Err("Claude's commit message was empty.".into());
            }
            carry.message = Some(message);
            carry.branch = name;
            Ok(())
        }
        Part::NewBranch => {
            let subject = carry.message.as_deref().and_then(|m| m.lines().next()).unwrap_or_default();
            let name = carry.branch.clone().unwrap_or_else(|| repo::branch_from_subject(subject));
            let r = root.clone();
            let name = cx.background_spawn(async move { repo::unused_branch_name(&r, &name) }).await;
            let r = root.clone();
            bg(Box::new(move || repo::create_branch(&r, &name, None))).await
        }
        Part::Commit => {
            let message = carry.message.clone().ok_or("No commit message.")?;
            let r = root.clone();
            let commit = cx.background_spawn(async move { repo::commit(&r, &message) }).await?;
            carry.commit = Some(commit);
            Ok(())
        }
        Part::Push => {
            let r = root.clone();
            bg(Box::new(move || {
                let repo = repo::read(&r).ok_or("Not a git repository")??;
                let remote = repo.remote.clone().ok_or("This repository has no remote to push to.")?;
                if repo.upstream.is_some() && repo.ahead == 0 {
                    return Ok(());
                }
                repo::push(&repo.root, &remote, repo.upstream.is_some())
            }))
            .await
        }
        Part::WritePr { base } => {
            let (r, b) = (root.clone(), base.clone());
            let brief = cx
                .background_spawn(async move {
                    let repo = repo::read(&r).ok_or("Not a git repository")??;
                    repo::pr_brief(&repo.root, repo.remote.as_deref(), &b)
                })
                .await?;
            let session = session.ok_or("Start this thread's agent first: Claude writes the pull request.")?;
            let prompt = format!(
                "Write a GitHub pull request for this branch, going into `{base}`.\n\nReply with the title on the first line (plain text, no `#`, under 72 characters), a blank line, then the description in GitHub Markdown: what changed and why, briefly, and a short list of the notable changes when there are several. If there's a pull request template, fill it in instead. Reply with only the pull request: no preamble, no code fence.\n\n{brief}"
            );
            let reply = host.complete(session, &prompt).await.map_err(|e| format!("Claude couldn't write the pull request: {e}"))?;
            let (title, body) = gh::parse_pr(&reply);
            if title.is_empty() {
                return Err("Claude's pull request had no title.".into());
            }
            carry.pr = Some((title, body));
            Ok(())
        }
        Part::CreatePr { base } => {
            let (title, body) = carry.pr.clone().ok_or("No pull request text.")?;
            let (r, b) = (root.clone(), base.clone());
            let url = cx
                .background_spawn(async move {
                    let repo = repo::read(&r).ok_or("Not a git repository")??;
                    let branch = repo.branch().ok_or("Not on a branch")?.to_string();
                    gh::create(&repo.root, &branch, &b, &title, &body)
                })
                .await?;
            carry.opened = Some(url);
            Ok(())
        }
        Part::Pull => {
            let r = root.clone();
            bg(Box::new(move || repo::pull(&r))).await
        }
        Part::SwitchTo(b) => {
            let (r, b) = (root.clone(), b.clone());
            bg(Box::new(move || repo::switch_and_pull(&r, &b))).await
        }
    }
}

/// A changed file's diff: HEAD (its old path, for a rename) against the file on disk.
fn file_diff(root: &Path, f: &ChangedFile) -> Result<Diff, String> {
    let old = match f.status {
        FileStatus::Added | FileStatus::Untracked => String::new(),
        _ => repo::head_text(root, f.from.as_deref().unwrap_or(&f.path)).unwrap_or_default(),
    };
    let new = if f.status == FileStatus::Deleted {
        String::new()
    } else {
        match crate::files::read_preview(&root.join(&f.path)) {
            crate::files::Preview::Text(t) => t,
            crate::files::Preview::Image(_) | crate::files::Preview::Binary => return Err("Binary file".into()),
            crate::files::Preview::TooLarge(_) => return Err("Too large to show".into()),
            crate::files::Preview::Failed(why) => return Err(why),
        }
    };
    if old.contains('\0') {
        return Err("Binary file".into());
    }
    Ok(diff::diff(&old, &new, Some(diff::CONTEXT)))
}

fn changed_row_data(f: &ChangedFile, expanded: bool) -> ChangedRow {
    let (dir, name) = match f.path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), f.path.clone()),
    };
    let mark = match f.status {
        FileStatus::Modified => GitMark::Modified,
        FileStatus::Added => GitMark::Added,
        FileStatus::Deleted => GitMark::Deleted,
        FileStatus::Renamed => GitMark::Renamed,
        FileStatus::Untracked => GitMark::Untracked,
        FileStatus::Conflicted => GitMark::Conflict,
    };
    ChangedRow { dir: dir.into(), name: name.into(), mark, from: f.from.clone().map(Into::into), lines: f.lines, expanded }
}

/// Folders' Changes state, by thread folder.
pub(crate) type ChangesByFolder = HashMap<PathBuf, Changes>;
