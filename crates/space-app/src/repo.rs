//! A folder's git state for the Changes view, and the git steps it runs: the changed files with
//! their line counts, the branch and how it stands against its remote, a merge or rebase in
//! progress; then committing everything, pushing, pulling (fast-forward only), branching, and
//! what Claude reads to write a commit message or a pull request.
//!
//! Every call runs `git -C <root>` and blocks, so callers run it in the background. Calls that
//! go over the network never prompt (a credential that isn't stored fails) and give up after a
//! while. Failures are git's own message, trimmed (`git::message`).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::git::{Head, message};

/// Local calls; network calls get longer (`FETCH`, `PUSH`).
const LOCAL: Duration = Duration::from_secs(20);
pub const FETCH: Duration = Duration::from_secs(30);
pub const PUSH: Duration = Duration::from_secs(120);
/// What Claude reads of a diff, at most (the rest is left out, said so).
const MAX_DIFF: usize = 40_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Relative to the repository's root.
    pub path: String,
    /// A rename's old path.
    pub from: Option<String>,
    pub status: FileStatus,
    /// Lines added and removed; `None` for a binary file.
    pub lines: Option<(usize, usize)>,
}

/// A merge, rebase, cherry-pick or revert git stopped in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Operation::Merge => "Merge",
            Operation::Rebase => "Rebase",
            Operation::CherryPick => "Cherry-pick",
            Operation::Revert => "Revert",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
}

/// What git says about a folder, read without the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The work tree's top folder.
    pub root: PathBuf,
    pub head: Head,
    pub files: Vec<ChangedFile>,
    pub operation: Option<Operation>,
    /// The remote pushes go to: the upstream's, else `origin`, else the first; `None`: no remote.
    pub remote: Option<String>,
    /// The branch's upstream (`origin/feat`), if it has one.
    pub upstream: Option<String>,
    /// Commits not on the upstream (with none: not on any remote branch).
    pub ahead: usize,
    /// Commits on the upstream not here (as of the last fetch).
    pub behind: usize,
    /// The remote's default branch (`main`), if known.
    pub default_branch: Option<String>,
    /// The branch pull requests go into: the one picked last in this repository, else the
    /// default branch.
    pub base: Option<String>,
    /// A base was picked before (so a pull request doesn't ask).
    pub base_saved: bool,
    /// Commits on this branch not on the base.
    pub ahead_of_base: usize,
    /// HEAD, when there's a commit.
    pub last: Option<Commit>,
}

impl Repo {
    pub fn branch(&self) -> Option<&str> {
        match &self.head {
            Head::Branch(b) => Some(b),
            Head::Detached(_) => None,
        }
    }

    pub fn conflicts(&self) -> usize {
        self.files.iter().filter(|f| f.status == FileStatus::Conflicted).count()
    }

    /// On the default branch (or on the base: a pull request from it into itself makes no sense).
    pub fn on_base(&self) -> bool {
        let b = self.branch();
        b.is_some() && (b == self.default_branch.as_deref() || b == self.base.as_deref())
    }
}

/// `git` with the network made safe: no prompts, a usual `PATH` (a GUI app gets a minimal one,
/// and credential helpers such as `gh` live in Homebrew's folder).
pub(crate) fn command(program: &str, dir: &Path) -> Command {
    let mut c = Command::new(program);
    c.current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").env("PATH", tool_path()).stdin(Stdio::null());
    c
}

/// `PATH` with the usual tool folders added.
pub(crate) fn tool_path() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from);
    let mut all: Vec<PathBuf> = std::env::split_paths(&path).collect();
    for e in extra {
        if !all.contains(&e) {
            all.push(e);
        }
    }
    std::env::join_paths(all).unwrap_or(path)
}

/// Run `c`, giving up (and killing it) after `timeout`. Its stdout, or its stderr as the error.
pub(crate) fn run(mut c: Command, timeout: Duration) -> Result<String, String> {
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Couldn't run {}: {e}", c.get_program().to_string_lossy()))?;
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let out = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut s);
        }
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} took too long, and was stopped", c.get_program().to_string_lossy()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        Err(message(&String::from_utf8_lossy(&err)))
    }
}

fn git_for(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut c = command("git", dir);
    c.args(args);
    run(c, timeout)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_for(dir, args, LOCAL)
}

/// The work tree `dir` is in; `None` outside git.
pub fn root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty())
}

/// Everything the Changes view shows about the repository `root` is in, without the network.
pub fn read(dir: &Path) -> Option<Result<Repo, String>> {
    let root = root(dir)?;
    Some(read_at(root))
}

fn read_at(root: PathBuf) -> Result<Repo, String> {
    let dir = root.as_path();
    let born = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
    let head = match git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(name) => Head::Branch(name.trim().to_string()),
        Err(_) => Head::Detached(git(dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string()),
    };
    let files = changed_files(dir, born)?;
    let operation = operation(dir);
    let remotes: Vec<String> = git(dir, &["remote"])?.lines().map(str::to_string).collect();
    let upstream = git(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let remote = upstream
        .as_deref()
        .and_then(|u| remotes.iter().find(|r| u.starts_with(&format!("{r}/"))))
        .or_else(|| remotes.iter().find(|r| *r == "origin"))
        .or(remotes.first())
        .cloned();
    let count = |args: &[&str]| git(dir, args).ok().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0);
    let (ahead, behind) = match (&upstream, born) {
        (Some(_), true) => {
            let lr = git(dir, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"]).unwrap_or_default();
            let mut n = lr.split_whitespace().filter_map(|s| s.parse::<usize>().ok());
            let behind = n.next().unwrap_or(0);
            (n.next().unwrap_or(0), behind)
        }
        (None, true) if remote.is_some() => (count(&["rev-list", "--count", "HEAD", "--not", "--remotes"]), 0),
        _ => (0, 0),
    };
    let default_branch = remote.as_deref().and_then(|r| default_branch(dir, r));
    let saved = git(dir, &["config", "--get", "space.prbase"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let base_saved = saved.is_some();
    let base = saved.or_else(|| default_branch.clone());
    let ahead_of_base = match (&base, born) {
        (Some(b), true) => count(&["rev-list", "--count", &format!("{}..HEAD", base_ref(dir, remote.as_deref(), b))]),
        _ => 0,
    };
    let last = born
        .then(|| git(dir, &["log", "-1", "--format=%h%x09%s"]).ok())
        .flatten()
        .and_then(|s| s.trim().split_once('\t').map(|(sha, subject)| Commit { sha: sha.into(), subject: subject.into() }));
    Ok(Repo { root, head, files, operation, remote, upstream, ahead, behind, default_branch, base, base_saved, ahead_of_base, last })
}

/// The remote's default branch: what its `HEAD` points at, else `main` or `master` if there.
fn default_branch(dir: &Path, remote: &str) -> Option<String> {
    if let Ok(s) = git(dir, &["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{remote}/HEAD")]) {
        return s.trim().strip_prefix(&format!("{remote}/")).map(str::to_string);
    }
    ["main", "master"].into_iter().find(|b| git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{remote}/{b}")]).is_ok()).map(str::to_string)
}

/// The ref to compare against for base `name`: the remote's copy when there is one.
fn base_ref(dir: &Path, remote: Option<&str>, name: &str) -> String {
    match remote {
        Some(r) if git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{r}/{name}")]).is_ok() => format!("{r}/{name}"),
        _ => name.to_string(),
    }
}

fn operation(dir: &Path) -> Option<Operation> {
    let git_dir = git(dir, &["rev-parse", "--absolute-git-dir"]).ok()?;
    let at = Path::new(git_dir.trim());
    if at.join("rebase-merge").exists() || at.join("rebase-apply").exists() {
        Some(Operation::Rebase)
    } else if at.join("MERGE_HEAD").exists() {
        Some(Operation::Merge)
    } else if at.join("CHERRY_PICK_HEAD").exists() {
        Some(Operation::CherryPick)
    } else if at.join("REVERT_HEAD").exists() {
        Some(Operation::Revert)
    } else {
        None
    }
}

/// Uncommitted changes, untracked files included, sorted by path, with their line counts.
fn changed_files(dir: &Path, born: bool) -> Result<Vec<ChangedFile>, String> {
    let status = git(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut files = parse_status(&status);
    let counts = if born { parse_numstat(&git(dir, &["diff", "HEAD", "--numstat", "-z"]).unwrap_or_default()) } else { Vec::new() };
    for f in &mut files {
        f.lines = match counts.iter().find(|(p, _)| *p == f.path) {
            Some((_, lines)) => *lines,
            // Untracked, or not in HEAD yet: every line is new.
            None => count_lines(&dir.join(&f.path)).map(|n| if f.status == FileStatus::Deleted { (0, n) } else { (n, 0) }),
        };
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// `git status --porcelain=v1 -z`: `XY path\0`, and a rename's old path after it.
pub(crate) fn parse_status(s: &str) -> Vec<ChangedFile> {
    let mut parts = s.split('\0').filter(|p| !p.is_empty());
    let mut files = Vec::new();
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
        let status = match (x, y) {
            (b'?', b'?') => FileStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflicted,
            (b'R' | b'C', _) => FileStatus::Renamed,
            (b'D', _) | (_, b'D') => FileStatus::Deleted,
            (b'A', _) => FileStatus::Added,
            _ => FileStatus::Modified,
        };
        let from = (status == FileStatus::Renamed).then(|| parts.next().map(str::to_string)).flatten();
        files.push(ChangedFile { path: path.to_string(), from, status, lines: Some((0, 0)) });
    }
    files
}

/// `git diff --numstat -z`: `added\tremoved\tpath\0`, or for a rename `added\tremoved\t\0old\0new\0`;
/// `-` counts for a binary file.
pub(crate) fn parse_numstat(s: &str) -> Vec<(String, Option<(usize, usize)>)> {
    let mut parts = s.split('\0');
    let mut out = Vec::new();
    while let Some(entry) = parts.next() {
        let mut f = entry.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
        let lines = a.parse().ok().zip(r.parse().ok());
        let path = if path.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        out.push((path, lines));
    }
    out
}

/// A text file's lines; `None` for a binary (or unreadable, or huge) one.
fn count_lines(path: &Path) -> Option<usize> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count();
    Some(if bytes.last().is_some_and(|b| *b != b'\n') { n + 1 } else { n })
}

/// A file as committed in HEAD, by its path in the repository; `None` when HEAD doesn't have it.
pub fn head_text(root: &Path, path: &str) -> Option<String> {
    git(root, &["show", &format!("HEAD:{path}")]).ok()
}

// ----- the network ---------------------------------------------------------------------------

/// Fetch from the remote (what `behind` counts, and the base's latest).
pub fn fetch(root: &Path, remote: &str) -> Result<(), String> {
    git_for(root, &["fetch", "--quiet", "--prune", remote], FETCH).map(drop)
}

/// Push the branch; one without an upstream gets one on `remote`.
pub fn push(root: &Path, remote: &str, has_upstream: bool) -> Result<(), String> {
    if has_upstream { git_for(root, &["push", "--quiet"], PUSH) } else { git_for(root, &["push", "--quiet", "-u", remote, "HEAD"], PUSH) }.map(drop)
}

/// Bring in the upstream's new commits, only when that needs no merge or rebase.
pub fn pull(root: &Path) -> Result<(), String> {
    git_for(root, &["pull", "--ff-only", "--quiet"], PUSH).map(drop).map_err(|e| {
        if e.contains("Not possible to fast-forward") || e.contains("diverging") {
            "The branch and its remote have both moved on: merge or rebase in the terminal.".to_string()
        } else {
            e
        }
    })
}

// ----- committing ----------------------------------------------------------------------------

/// Stage every change (untracked files too), for the commit that follows.
pub fn stage_all(root: &Path) -> Result<(), String> {
    git(root, &["add", "-A"]).map(drop)
}

/// Commit what's staged with `message`.
pub fn commit(root: &Path, message: &str) -> Result<Commit, String> {
    git(root, &["commit", "--quiet", "-m", message])?;
    let s = git(root, &["log", "-1", "--format=%h%x09%s"])?;
    let (sha, subject) = s.trim().split_once('\t').unwrap_or((s.trim(), ""));
    Ok(Commit { sha: sha.into(), subject: subject.into() })
}

/// Take back commit `sha` if it's still HEAD and not pushed: its changes stay, uncommitted.
pub fn uncommit(root: &Path, sha: &str) -> Result<(), String> {
    let head = git(root, &["rev-parse", "--short", "HEAD"])?;
    if !sha.starts_with(head.trim()) && !head.trim().starts_with(sha) {
        return Err("That commit isn't the latest any more.".into());
    }
    if git(root, &["branch", "-r", "--contains", "HEAD"]).is_ok_and(|s| !s.trim().is_empty()) {
        return Err("That commit was pushed already.".into());
    }
    match git(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"]) {
        Ok(_) => git(root, &["reset", "--soft", "--quiet", "HEAD~1"]).map(drop),
        // The first commit: back to no commits, its files staged.
        Err(_) => git(root, &["update-ref", "-d", "HEAD"]).map(drop),
    }
}

/// What Claude reads to write the commit message: the staged changes, and how this repository's
/// recent commits are written.
pub fn commit_brief(root: &Path) -> Result<String, String> {
    let stat = git(root, &["diff", "--cached", "--stat"])?;
    if stat.trim().is_empty() {
        return Err("Nothing to commit.".into());
    }
    let diff = git(root, &["diff", "--cached", "--no-color"]).unwrap_or_default();
    let log = git(root, &["log", "-8", "--format=%B%x1e"]).unwrap_or_default();
    let examples: Vec<&str> = log.split('\x1e').map(str::trim).filter(|m| !m.is_empty()).collect();
    Ok(format!(
        "<recent-commits>\n{}\n</recent-commits>\n\n<staged-changes>\n{}\n{}\n</staged-changes>",
        if examples.is_empty() { "(none yet)".to_string() } else { examples.join("\n---\n") },
        stat.trim_end(),
        clip(&diff, MAX_DIFF),
    ))
}

/// What Claude reads to write a pull request into `base`: the branch's commits and changes.
pub fn pr_brief(root: &Path, remote: Option<&str>, base: &str) -> Result<String, String> {
    let from = base_ref(root, remote, base);
    let log = git(root, &["log", "--reverse", "--format=%B%x1e", &format!("{from}..HEAD")])?;
    let commits: Vec<&str> = log.split('\x1e').map(str::trim).filter(|m| !m.is_empty()).collect();
    if commits.is_empty() {
        return Err(format!("This branch has no commits that aren't on {base}."));
    }
    let range = format!("{from}...HEAD");
    let stat = git(root, &["diff", "--stat", &range]).unwrap_or_default();
    let diff = git(root, &["diff", "--no-color", &range]).unwrap_or_default();
    let template = pr_template(root).map(|t| format!("\n\n<pull-request-template>\n{t}\n</pull-request-template>")).unwrap_or_default();
    Ok(format!(
        "<commits>\n{}\n</commits>\n\n<changes>\n{}\n{}\n</changes>{template}",
        commits.join("\n---\n"),
        stat.trim_end(),
        clip(&diff, MAX_DIFF / 2),
    ))
}

fn pr_template(root: &Path) -> Option<String> {
    let names = [".github/pull_request_template.md", ".github/PULL_REQUEST_TEMPLATE.md", "pull_request_template.md", "docs/pull_request_template.md"];
    names.iter().find_map(|n| std::fs::read_to_string(root.join(n)).ok()).map(|t| clip(&t, 4000))
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more characters left out)", &s[..end], s.len() - end)
}

// ----- branches ------------------------------------------------------------------------------

/// A branch named `name` from `from` (`None`: what's checked out), switched to. Uncommitted
/// changes come along.
pub fn create_branch(root: &Path, name: &str, from: Option<&str>) -> Result<(), String> {
    if !crate::git::valid_name(root, name) {
        return Err(format!("“{name}” isn't a valid branch name"));
    }
    let mut args = vec!["switch", "--quiet", "-c", name];
    args.extend(from);
    git(root, &args).map(drop)
}

/// `name`, or `name-2`, `name-3`… whichever isn't a branch yet.
pub fn unused_branch_name(root: &Path, name: &str) -> String {
    let taken = |n: &str| git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{n}")]).is_ok();
    if !taken(name) {
        return name.to_string();
    }
    (2..).map(|i| format!("{name}-{i}")).find(|n| !taken(n)).unwrap_or_else(|| name.to_string())
}

/// A branch name from a commit subject (when Claude didn't give one): its words, lowercase,
/// hyphenated, without an `Area:` prefix's colon.
pub fn branch_from_subject(subject: &str) -> String {
    let words: Vec<String> = subject
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(str::to_lowercase)
        .collect();
    if words.is_empty() { "changes".into() } else { words.join("-") }
}

/// Switch to `name` and bring it up to date with its remote (fast-forward only; a pull that
/// can't is left for later, the switch stands).
pub fn switch_and_pull(root: &Path, name: &str) -> Result<(), String> {
    git(root, &["switch", "--quiet", name])?;
    let has_upstream = git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
    if has_upstream {
        pull(root)?;
    }
    Ok(())
}

/// Branches a pull request can go into, most recently committed first: the remote's (without
/// `HEAD` and the current branch), else the local ones.
pub fn bases(root: &Path, remote: Option<&str>, current: Option<&str>) -> Vec<String> {
    let list = |pattern: &str, strip: &str| -> Vec<String> {
        git(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", pattern])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix(strip).map(str::to_string))
            .filter(|b| b != "HEAD" && !b.is_empty() && Some(b.as_str()) != current)
            .collect()
    };
    let remote_list = remote.map(|r| list(&format!("refs/remotes/{r}"), &format!("{r}/"))).unwrap_or_default();
    if remote_list.is_empty() { list("refs/heads", "") } else { remote_list }
}

/// Remember `base` as where this repository's pull requests go (git's own config, so it stays
/// with the repository).
pub fn save_base(root: &Path, base: &str) -> Result<(), String> {
    git(root, &["config", "--local", "space.prbase", base]).map(drop)
}

/// Claude's reply as a commit message: no fences or quotes around it, no trailing blank lines.
/// With `branch`, a first line `branch: <name>` is the new branch's name.
pub fn parse_message(reply: &str, branch: bool) -> (Option<String>, String) {
    let mut text = reply.trim();
    if let Some(inner) = text.strip_prefix("```") {
        let inner = inner.split_once('\n').map_or(inner, |(_, rest)| rest);
        text = inner.strip_suffix("```").unwrap_or(inner).trim();
    }
    let mut name = None;
    if branch
        && let Some((first, rest)) = text.split_once('\n')
        && let Some(n) = first.trim().strip_prefix("branch:").or_else(|| first.trim().strip_prefix("Branch:"))
    {
        let n: String = n.trim().trim_matches('`').chars().map(|c| if c.is_whitespace() { '-' } else { c }).collect();
        name = (!n.is_empty()).then_some(n);
        text = rest.trim();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut message = lines.join("\n").trim().to_string();
    if message.len() > 1 && message.starts_with('"') && message.ends_with('"') {
        message = message[1..message.len() - 1].to_string();
    }
    (name, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        sh(d, &["init", "-q", "-b", "main"]);
        sh(d, &["config", "user.email", "t@example.com"]);
        sh(d, &["config", "user.name", "T"]);
        std::fs::write(d.join("a.txt"), "one\ntwo\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "first"]);
        dir
    }

    /// A clone of a bare remote made from `repo()`, on `main`, its upstream set.
    fn cloned() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let src = repo();
        let remote = tempfile::tempdir().unwrap();
        let bare = remote.path().join("r.git");
        sh(src.path(), &["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);
        let work = tempfile::tempdir().unwrap();
        let w = work.path().join("w");
        sh(work.path(), &["clone", "-q", bare.to_str().unwrap(), "w"]);
        sh(&w, &["config", "user.email", "t@example.com"]);
        sh(&w, &["config", "user.name", "T"]);
        drop(src);
        (remote, work, w)
    }

    #[test]
    fn status_and_numstat_parse() {
        let files = parse_status(" M src/a.rs\0?? new.txt\0R  b.rs\0old b.rs\0UU c.rs\0 D gone.rs\0A  added.rs\0");
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.status, f.from.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("src/a.rs", FileStatus::Modified, None),
                ("new.txt", FileStatus::Untracked, None),
                ("b.rs", FileStatus::Renamed, Some("old b.rs")),
                ("c.rs", FileStatus::Conflicted, None),
                ("gone.rs", FileStatus::Deleted, None),
                ("added.rs", FileStatus::Added, None),
            ]
        );
        let n = parse_numstat(concat!("3\t1\tsrc/a.rs\0-\t-\timg.png\0", "2\t0\t\0old.rs\0new.rs\0"));
        assert_eq!(n[0], ("src/a.rs".into(), Some((3, 1))));
        assert_eq!(n[1], ("img.png".into(), None));
        assert_eq!(n[2].0, "new.rs");
    }

    #[test]
    fn reads_changes_with_line_counts() {
        let dir = repo();
        let d = dir.path();
        std::fs::write(d.join("a.txt"), "one\n2\nthree\n").unwrap();
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/new.txt"), "x\ny").unwrap();
        let r = read(&d.join("sub")).unwrap().unwrap();
        assert_eq!(r.root.canonicalize().unwrap(), d.canonicalize().unwrap());
        assert_eq!(r.head, Head::Branch("main".into()));
        let files: Vec<_> = r.files.iter().map(|f| (f.path.as_str(), f.status, f.lines)).collect();
        assert_eq!(files, [("a.txt", FileStatus::Modified, Some((2, 1))), ("sub/new.txt", FileStatus::Untracked, Some((2, 0)))]);
        assert_eq!((r.remote.as_deref(), r.ahead, r.operation), (None, 0, None));
        assert_eq!(r.last.as_ref().map(|c| c.subject.as_str()), Some("first"));
        assert!(read(tempfile::tempdir().unwrap().path()).is_none());
    }

    #[test]
    fn commits_everything_and_takes_it_back() {
        let dir = repo();
        let d = dir.path();
        std::fs::write(d.join("b.txt"), "b\n").unwrap();
        stage_all(d).unwrap();
        let brief = commit_brief(d).unwrap();
        assert!(brief.contains("b.txt") && brief.contains("first"), "{brief}");
        let c = commit(d, "Add b\n\nBecause.").unwrap();
        assert_eq!(c.subject, "Add b");
        assert!(read(d).unwrap().unwrap().files.is_empty());
        uncommit(d, &c.sha).unwrap();
        let r = read(d).unwrap().unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.last.unwrap().subject, "first");
        stage_all(d).unwrap();
        commit(d, "again").unwrap();
        assert!(uncommit(d, &c.sha).is_err(), "not HEAD any more");
        assert_eq!(commit_brief(d).unwrap_err(), "Nothing to commit.");
    }

    #[test]
    fn tracks_the_remote_pushes_pulls_and_bases() {
        let (_remote, _work, w) = cloned();
        let r = read(&w).unwrap().unwrap();
        assert_eq!((r.remote.as_deref(), r.upstream.as_deref(), r.default_branch.as_deref()), (Some("origin"), Some("origin/main"), Some("main")));
        assert!(r.on_base());
        // A branch with a commit: ahead of the remote and of its base, no upstream yet.
        create_branch(&w, "feat/x", None).unwrap();
        std::fs::write(w.join("c.txt"), "c\n").unwrap();
        stage_all(&w).unwrap();
        commit(&w, "Add c").unwrap();
        let r = read(&w).unwrap().unwrap();
        assert_eq!((r.upstream.as_deref(), r.ahead, r.ahead_of_base, r.on_base()), (None, 1, 1, false));
        assert_eq!(r.base.as_deref(), Some("main"));
        assert!(!r.base_saved);
        let brief = pr_brief(&w, r.remote.as_deref(), "main").unwrap();
        assert!(brief.contains("Add c"), "{brief}");
        push(&w, "origin", false).unwrap();
        let r = read(&w).unwrap().unwrap();
        assert_eq!((r.upstream.as_deref(), r.ahead), (Some("origin/feat/x"), 0));
        assert_eq!(bases(&w, Some("origin"), Some("feat/x")), ["main"]);
        save_base(&w, "main").unwrap();
        assert!(read(&w).unwrap().unwrap().base_saved);
        assert_eq!(unused_branch_name(&w, "feat/x"), "feat/x-2");
        assert_eq!(unused_branch_name(&w, "feat/y"), "feat/y");
        switch_and_pull(&w, "main").unwrap();
        assert_eq!(read(&w).unwrap().unwrap().head, Head::Branch("main".into()));
        fetch(&w, "origin").unwrap();
        pull(&w).unwrap();
    }

    #[test]
    fn a_merge_in_progress_and_its_conflicts() {
        let dir = repo();
        let d = dir.path();
        sh(d, &["switch", "-q", "-c", "other"]);
        std::fs::write(d.join("a.txt"), "other\n").unwrap();
        sh(d, &["commit", "-q", "-am", "other"]);
        sh(d, &["switch", "-q", "main"]);
        std::fs::write(d.join("a.txt"), "mine\n").unwrap();
        sh(d, &["commit", "-q", "-am", "mine"]);
        let _ = Command::new("git").arg("-C").arg(d).args(["merge", "-q", "other"]).output();
        let r = read(d).unwrap().unwrap();
        assert_eq!((r.operation, r.conflicts()), (Some(Operation::Merge), 1));
    }

    #[test]
    fn names_a_branch_from_a_subject() {
        assert_eq!(branch_from_subject("Sidebar: ⌘ numbers show after 0.3 s of holding"), "sidebar-numbers-show-after-0-3");
        assert_eq!(branch_from_subject("!!!"), "changes");
    }

    #[test]
    fn reads_claudes_messages() {
        assert_eq!(parse_message("```\nFix it\n\nBody\n```", false), (None, "Fix it\n\nBody".into()));
        assert_eq!(parse_message("\"Fix it\"", false), (None, "Fix it".into()));
        assert_eq!(parse_message("branch: fix/the thing\nFix it", true), (Some("fix/the-thing".into()), "Fix it".into()));
        assert_eq!(parse_message("Fix it", true), (None, "Fix it".into()));
    }

    #[test]
    fn slow_commands_are_stopped() {
        let mut c = Command::new("sleep");
        c.arg("5");
        let start = Instant::now();
        assert!(run(c, Duration::from_millis(200)).unwrap_err().contains("took too long"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
