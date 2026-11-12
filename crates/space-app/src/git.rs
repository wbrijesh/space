//! A folder's git branches, for the composer's folder menu: the list (most recently committed
//! first), what HEAD is, how many files are changed, and switching or creating a branch. Each
//! call runs `git -C <dir>` and blocks, so callers run it in the background; failures are git's
//! own message, trimmed to its first lines.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// Last commit (Unix ms).
    pub committed_ms: Option<u64>,
}

/// What's checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// No branch: the short commit.
    Detached(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branches {
    pub head: Head,
    /// Local branches, most recently committed first.
    pub list: Vec<Branch>,
    /// Files with uncommitted changes (untracked included).
    pub changed: usize,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("Couldn't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(message(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// git's error, without its hints, on a few lines. A switch that would overwrite local changes
/// says which files, briefly.
fn message(stderr: &str) -> String {
    if stderr.contains("would be overwritten by checkout") {
        let files: Vec<&str> = stderr.lines().filter(|l| l.starts_with('\t')).map(str::trim).collect();
        let mut list = files.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        if files.len() > 3 {
            list.push_str(&format!(" and {} more", files.len() - 3));
        }
        return if list.is_empty() { "Commit or stash your changes first.".into() } else { format!("Commit or stash your changes first: {list}") };
    }
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("hint:") && *l != "Aborting").collect();
    let text = lines.iter().take(4).map(|l| l.strip_prefix("error: ").or_else(|| l.strip_prefix("fatal: ")).unwrap_or(l)).collect::<Vec<_>>().join("\n");
    if text.is_empty() { "git failed".into() } else { text }
}

/// `None` when `dir` isn't in a git work tree (or git can't run).
pub fn branches(dir: &Path) -> Option<Result<Branches, String>> {
    git(dir, &["rev-parse", "--is-inside-work-tree"]).ok().filter(|s| s.trim() == "true")?;
    Some(read(dir))
}

fn read(dir: &Path) -> Result<Branches, String> {
    let head = match git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(name) => Head::Branch(name.trim().to_string()),
        Err(_) => Head::Detached(git(dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string()),
    };
    let list = git(dir, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)%09%(committerdate:unix)", "refs/heads"])?
        .lines()
        .filter_map(|line| {
            let (name, at) = line.split_once('\t')?;
            Some(Branch { name: name.to_string(), committed_ms: at.trim().parse::<u64>().ok().map(|s| s * 1000) })
        })
        .collect();
    let changed = git(dir, &["status", "--porcelain"])?.lines().count();
    Ok(Branches { head, list, changed })
}

pub fn switch(dir: &Path, name: &str) -> Result<(), String> {
    git(dir, &["switch", name]).map(drop)
}

/// A new branch from what's checked out, switched to.
pub fn create(dir: &Path, name: &str) -> Result<(), String> {
    if !valid_name(dir, name) {
        return Err(format!("“{name}” isn't a valid branch name"));
    }
    git(dir, &["switch", "-c", name]).map(drop)
}

pub fn valid_name(dir: &Path, name: &str) -> bool {
    !name.is_empty() && git(dir, &["check-ref-format", "--branch", name]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap().status.success(), "{args:?}");
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first", "--date=2020-01-01T00:00:00"]);
        run(&["branch", "old"]);
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        run(&["commit", "-q", "-am", "second"]);
        dir
    }

    #[test]
    fn lists_branches_newest_first_with_head_and_changes() {
        let dir = repo();
        let b = branches(dir.path()).unwrap().unwrap();
        assert_eq!(b.head, Head::Branch("main".into()));
        assert_eq!(b.list.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["main", "old"]);
        assert!(b.list[0].committed_ms.is_some());
        assert_eq!(b.changed, 0);
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        assert_eq!(branches(dir.path()).unwrap().unwrap().changed, 1);
    }

    #[test]
    fn switches_creates_and_reports_what_git_refuses() {
        let dir = repo();
        switch(dir.path(), "old").unwrap();
        assert_eq!(branches(dir.path()).unwrap().unwrap().head, Head::Branch("old".into()));
        // A change switching would overwrite: git refuses, and says why.
        std::fs::write(dir.path().join("a.txt"), "local\n").unwrap();
        let err = switch(dir.path(), "main").unwrap_err();
        assert_eq!(err, "Commit or stash your changes first: a.txt");
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();

        create(dir.path(), "feat/x").unwrap();
        assert_eq!(branches(dir.path()).unwrap().unwrap().head, Head::Branch("feat/x".into()));
        assert!(create(dir.path(), "feat/x").unwrap_err().contains("already exists"));
        assert!(create(dir.path(), "bad name").unwrap_err().contains("isn't a valid branch name"));
    }

    #[test]
    fn detached_head_and_folders_outside_git() {
        let dir = repo();
        let run = |args: &[&str]| Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap();
        run(&["switch", "-q", "--detach", "HEAD~1"]);
        assert!(matches!(branches(dir.path()).unwrap().unwrap().head, Head::Detached(sha) if sha.len() >= 7));
        let plain = tempfile::tempdir().unwrap();
        assert!(branches(plain.path()).is_none());
    }
}
