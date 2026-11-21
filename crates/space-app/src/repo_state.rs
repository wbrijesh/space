//! What the Changes view's bar offers, from where the repository stands: one status line, the
//! next step as the main button, and the other steps that make sense now in its ▾ menu. Steps
//! that don't apply aren't shown at all. A plain function of [`RepoState`], so every situation
//! is a test below.
//!
//! In order, the first that holds:
//!
//! ```text
//!  git stopped mid-merge/rebase      "Merge in progress · 2 conflicts"   —
//!  a step running                    "Pushing…"                          (spinner)
//!  not on a branch                   "Not on a branch"                   New branch… ▾ Commit
//!  files changed                     "3 files changed"                   Commit ▾ … & push, … & create PR, to new branch…
//!                                    (an open pull request adds ▾ Edit pull request here and below)
//!  behind, not ahead                 "2 new commits on origin"           Pull
//!  ahead and behind                  "… have both moved on"              —
//!  PR open, ahead                    "2 commits not pushed"              Push
//!  PR open                           "Up to date"                        — ▾ Edit pull request
//!  PR merged                         "Merged"                            Switch to main
//!  ahead, PRs possible               "2 commits not pushed"              Create PR ▾ Push
//!  ahead                             "2 commits not pushed"              Push
//!  pushed, ahead of base, no PR      "Pushed · not in a pull request"    Create PR
//!  otherwise                         "Up to date with origin/main"       —
//!  no remote, nothing changed        "Nothing to commit"                 —
//! ```
//!
//! The status never repeats what a chip beside it shows: the branch, the base, the pull request.

use space_ui::components::changes::{ChangeAction, Tone};

use crate::gh::{Gh, PrState};
use crate::repo::Operation;

/// Where the repository stands, as the bar needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoState {
    /// `None`: not on a branch (detached).
    pub branch: Option<String>,
    pub changed: usize,
    pub conflicts: usize,
    pub operation: Option<Operation>,
    /// The remote's name; `None`: no remote (nothing to push or pull).
    pub remote: Option<String>,
    /// What the branch tracks ("origin/main"); `None`: nothing yet.
    pub upstream: Option<String>,
    pub ahead: usize,
    /// `None` until a fetch says.
    pub behind: Option<usize>,
    /// On the default branch (or the base).
    pub on_base: bool,
    pub base: Option<String>,
    pub ahead_of_base: usize,
    /// `None` until known.
    pub gh: Option<Gh>,
    pub pr: Option<(u64, PrState)>,
    /// The running step's label.
    pub running: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bar {
    pub status: String,
    pub tone: Tone,
    pub primary: Option<ChangeAction>,
    pub menu: Vec<ChangeAction>,
    /// Show where pull requests go (the base chip).
    pub show_base: bool,
    /// Why pull requests aren't offered, when that's worth saying.
    pub note: Option<&'static str>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn bar(s: &RepoState) -> Bar {
    let remote = s.remote.as_deref();
    let gh_ready = s.gh == Some(Gh::Ready);
    // A pull request can come from this branch (once its state is known).
    let pr_branch = remote.is_some() && s.branch.is_some() && !s.on_base && s.base.is_some();
    let pr_open = matches!(s.pr, Some((_, PrState::Open)));
    let can_open_pr = pr_branch && gh_ready && !pr_open && !matches!(s.pr, Some((_, PrState::Merged)));
    let note = (pr_branch && s.changed + s.ahead + s.ahead_of_base > 0).then(|| s.gh.as_ref().and_then(Gh::why_not)).flatten();
    let mut b = Bar { show_base: pr_branch && gh_ready, note, ..Default::default() };

    if let Some(op) = s.operation {
        b.status = if s.conflicts > 0 { format!("{} in progress · {}", op.label(), plural(s.conflicts, "conflict", "conflicts")) } else { format!("{} in progress", op.label()) };
        b.tone = Tone::Warn;
        b.show_base = false;
        return b;
    }
    if let Some(label) = &s.running {
        b.status = label.clone();
        return b;
    }
    if s.branch.is_none() {
        b.status = "Not on a branch".into();
        b.tone = Tone::Warn;
        b.primary = Some(ChangeAction::NewBranch);
        if s.changed > 0 {
            b.menu.push(ChangeAction::Commit);
        }
        return b;
    }
    if s.changed > 0 {
        b.status = plural(s.changed, "file changed", "files changed");
        b.primary = Some(ChangeAction::Commit);
        if remote.is_some() {
            b.menu.push(ChangeAction::CommitAndPush);
        }
        if can_open_pr {
            b.menu.push(ChangeAction::CommitAndPr);
        }
        if s.on_base {
            b.menu.push(ChangeAction::CommitToNewBranch);
        }
        if pr_open {
            b.menu.push(ChangeAction::EditPr);
        }
        return b;
    }
    let Some(remote) = remote else {
        b.status = "Nothing to commit".into();
        return b;
    };
    let behind = s.behind.unwrap_or(0);
    if behind > 0 && s.ahead == 0 {
        b.status = format!("{} on {remote}", plural(behind, "new commit", "new commits"));
        b.primary = Some(ChangeAction::Pull);
        return b;
    }
    if behind > 0 {
        b.status = format!("This branch and {remote} have both moved on: merge or rebase in the terminal");
        b.tone = Tone::Warn;
        return b;
    }
    if let Some((_, state)) = s.pr {
        match state {
            PrState::Open if s.ahead > 0 => {
                b.status = format!("{} not pushed", plural(s.ahead, "commit", "commits"));
                b.primary = Some(ChangeAction::Push);
                b.menu.push(ChangeAction::EditPr);
                return b;
            }
            PrState::Open => {
                b.status = "Up to date".into();
                b.tone = Tone::Good;
                b.menu.push(ChangeAction::EditPr);
                return b;
            }
            PrState::Merged => {
                b.status = "Merged".into();
                b.tone = Tone::Good;
                b.primary = s.base.clone().map(|base| ChangeAction::SwitchTo(base.into()));
                b.show_base = false;
                return b;
            }
            // Closed without merging: as if there were none.
            PrState::Closed => {}
        }
    }
    if s.ahead > 0 {
        b.status = format!("{} not pushed", plural(s.ahead, "commit", "commits"));
        if can_open_pr {
            b.primary = Some(ChangeAction::CreatePr);
            b.menu.push(ChangeAction::Push);
        } else {
            b.primary = Some(ChangeAction::Push);
        }
        return b;
    }
    if can_open_pr && s.upstream.is_some() && s.ahead_of_base > 0 {
        b.status = "Pushed · not in a pull request".into();
        b.primary = Some(ChangeAction::CreatePr);
        return b;
    }
    b.status = match &s.upstream {
        Some(u) => format!("Up to date with {u}"),
        None => "Up to date".into(),
    };
    b.tone = Tone::Good;
    b.show_base = b.show_base && pr_open;
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChangeAction::*;

    /// A feature branch with a remote and gh signed in, clean and pushed.
    fn feature() -> RepoState {
        RepoState {
            branch: Some("feat/x".into()),
            remote: Some("origin".into()),
            upstream: Some("origin/feat/x".into()),
            behind: Some(0),
            base: Some("main".into()),
            gh: Some(Gh::Ready),
            ..Default::default()
        }
    }

    fn main_branch() -> RepoState {
        RepoState { branch: Some("main".into()), upstream: Some("origin/main".into()), on_base: true, ..feature() }
    }

    fn check(s: RepoState, status: &str, primary: Option<ChangeAction>, menu: &[ChangeAction]) -> Bar {
        let b = bar(&s);
        assert_eq!((b.status.as_str(), b.primary.clone(), b.menu.as_slice()), (status, primary, menu), "{s:?}");
        b
    }

    #[test]
    fn git_stopped_mid_merge_offers_nothing() {
        let b = check(RepoState { operation: Some(Operation::Merge), conflicts: 2, changed: 3, ..feature() }, "Merge in progress · 2 conflicts", None, &[]);
        assert_eq!(b.tone, Tone::Warn);
        check(RepoState { operation: Some(Operation::Rebase), ..feature() }, "Rebase in progress", None, &[]);
    }

    #[test]
    fn a_running_step_is_all_it_shows() {
        check(RepoState { running: Some("Pushing…".into()), changed: 1, ..feature() }, "Pushing…", None, &[]);
    }

    #[test]
    fn not_on_a_branch() {
        check(RepoState { branch: None, ..feature() }, "Not on a branch", Some(NewBranch), &[]);
        check(RepoState { branch: None, changed: 1, ..feature() }, "Not on a branch", Some(NewBranch), &[Commit]);
    }

    #[test]
    fn changed_files_commit() {
        check(RepoState { changed: 3, ..feature() }, "3 files changed", Some(Commit), &[CommitAndPush, CommitAndPr]);
        check(RepoState { changed: 1, ..main_branch() }, "1 file changed", Some(Commit), &[CommitAndPush, CommitToNewBranch]);
        // A pull request open already: no second one.
        check(RepoState { changed: 1, pr: Some((4, PrState::Open)), ..feature() }, "1 file changed", Some(Commit), &[CommitAndPush, EditPr]);
        // No remote: committing is all there is.
        check(RepoState { changed: 2, remote: None, upstream: None, ..feature() }, "2 files changed", Some(Commit), &[]);
        check(RepoState { changed: 2, remote: None, upstream: None, ..main_branch() }, "2 files changed", Some(Commit), &[CommitToNewBranch]);
    }

    #[test]
    fn behind_pulls_and_diverged_says_so() {
        check(RepoState { behind: Some(2), ..feature() }, "2 new commits on origin", Some(Pull), &[]);
        let b = check(
            RepoState { behind: Some(1), ahead: 1, ..feature() },
            "This branch and origin have both moved on: merge or rebase in the terminal",
            None,
            &[],
        );
        assert_eq!(b.tone, Tone::Warn);
        // Not fetched yet: behind isn't assumed.
        check(RepoState { behind: None, ahead: 1, ..main_branch() }, "1 commit not pushed", Some(Push), &[]);
    }

    #[test]
    fn with_a_pull_request() {
        check(RepoState { ahead: 2, pr: Some((12, PrState::Open)), ..feature() }, "2 commits not pushed", Some(Push), &[EditPr]);
        let b = check(RepoState { pr: Some((12, PrState::Open)), ahead_of_base: 3, ..feature() }, "Up to date", None, &[EditPr]);
        assert!(b.show_base);
        let b = check(RepoState { pr: Some((12, PrState::Merged)), ..feature() }, "Merged", Some(SwitchTo("main".into())), &[]);
        assert!(!b.show_base);
        // Closed unmerged: a new one can be opened.
        check(RepoState { ahead: 1, pr: Some((12, PrState::Closed)), ..feature() }, "1 commit not pushed", Some(CreatePr), &[Push]);
    }

    #[test]
    fn ahead_creates_a_pull_request_or_pushes() {
        let b = check(RepoState { ahead: 2, upstream: None, ..feature() }, "2 commits not pushed", Some(CreatePr), &[Push]);
        assert!(b.show_base);
        let b = check(RepoState { ahead: 2, ..main_branch() }, "2 commits not pushed", Some(Push), &[]);
        assert!(!b.show_base);
        // No gh: push, and say why there are no pull requests.
        let b = check(RepoState { ahead: 1, gh: Some(Gh::Missing), ..feature() }, "1 commit not pushed", Some(Push), &[]);
        assert!(b.note.is_some_and(|n| n.contains("brew install gh")));
        // Not known yet: no pull request steps (nor a note) until it is.
        let b = check(RepoState { ahead: 1, gh: None, ..feature() }, "1 commit not pushed", Some(Push), &[]);
        assert!(b.note.is_none());
    }

    #[test]
    fn pushed_but_not_in_a_pull_request() {
        check(RepoState { ahead_of_base: 2, ..feature() }, "Pushed · not in a pull request", Some(CreatePr), &[]);
    }

    #[test]
    fn nothing_to_do() {
        let b = check(feature(), "Up to date with origin/feat/x", None, &[]);
        assert_eq!(b.tone, Tone::Good);
        assert!(!b.show_base);
        check(main_branch(), "Up to date with origin/main", None, &[]);
        check(RepoState { remote: None, upstream: None, ..feature() }, "Nothing to commit", None, &[]);
    }
}
