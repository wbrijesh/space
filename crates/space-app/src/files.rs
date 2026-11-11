//! The Files view: a thread's folder as a tree, path-first like Pierre's Trees.
//!
//! Folders are listed when opened (`ignore`, so `.gitignore` and the like hold, and `.git` is
//! left out), then kept current by a file watcher on the shown thread's folder. A folder whose
//! only child is a folder shows as one row (`src/main`). Git status colors files and marks
//! folders with changes inside. Typing in the filter searches every path under the folder.
//!
//! This module is the model (listing, status, rows) and its I/O; `files_view.rs` connects it to
//! the app and the pane.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use space_ui::components::file_tree::{FileRow, GitMark};

/// A folder or file in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
}

/// The walker for a folder's listings and searches: ignore files apply (from parent folders
/// too), hidden files show, `.git` doesn't.
fn walker(dir: &Path, depth: Option<usize>) -> ignore::WalkBuilder {
    let mut b = ignore::WalkBuilder::new(dir);
    b.hidden(false).max_depth(depth).sort_by_file_name(Ord::cmp).filter_entry(|e| e.file_name() != ".git");
    b
}

/// `dir`'s entries: folders first, then files, each by name (case-insensitive).
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<Entry>> {
    if !dir.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} isn't a folder", dir.display())));
    }
    let mut out: Vec<Entry> = walker(dir, Some(1))
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            dir: e.file_type().is_some_and(|t| t.is_dir()),
            path: e.into_path(),
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// Searching stops after this many paths (a huge folder still answers).
pub const SEARCH_LIMIT: usize = 200_000;

/// Every file under `root` (ignore files apply), relative to it, up to [`SEARCH_LIMIT`].
pub fn all_files(root: &Path) -> Vec<PathBuf> {
    walker(root, None)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .take(SEARCH_LIMIT)
        .collect()
}

/// Whether `rel` matches the filter: each of its words appears in the path (any case).
pub fn matches(rel: &Path, words: &[String]) -> bool {
    let path = rel.to_string_lossy().to_lowercase();
    words.iter().all(|w| path.contains(w.as_str()))
}

pub fn filter_words(filter: &str) -> Vec<String> {
    filter.split_whitespace().map(str::to_lowercase).collect()
}

// ----- previews ----------------------------------------------------------------------------

/// Text larger than this isn't previewed (Open in instead).
pub const MAX_PREVIEW_BYTES: u64 = 4 << 20;

/// What a file can be shown as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Text(String),
    /// An image, and its size in pixels when known.
    Image(Option<(u64, u64)>),
    /// Not text (a NUL in its first 8 KB, or not UTF-8).
    Binary,
    TooLarge(u64),
    Failed(String),
}

pub fn is_image(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tif" | "tiff")
}

/// Read `path` for its preview.
pub fn read_preview(path: &Path) -> Preview {
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Preview::Failed("This isn't a file.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Preview::Failed("The file was moved or deleted.".into()),
        Err(e) => return Preview::Failed(format!("Couldn't read it: {e}")),
    };
    if is_image(path) {
        return Preview::Image(crate::images::file_size(path));
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Preview::TooLarge(meta.len());
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes[..bytes.len().min(8192)].contains(&0) => Preview::Binary,
        Ok(bytes) => String::from_utf8(bytes).map_or(Preview::Binary, Preview::Text),
        Err(e) => Preview::Failed(format!("Couldn't read it: {e}")),
    }
}

// ----- git status --------------------------------------------------------------------------

/// The repository's changes, by absolute path, and the folders that hold any.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitStatus {
    pub files: HashMap<PathBuf, GitMark>,
    pub dirty_dirs: HashSet<PathBuf>,
}

/// `git status` for the repository `root` is in (`None`: not in one, or git failed).
pub fn git_status(root: &Path) -> Option<GitStatus> {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    let top = run(&["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let out = run(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
    Some(parse_status(&out, &top))
}

/// Parse `git status --porcelain=v2 -z` (paths relative to `top`).
pub fn parse_status(out: &[u8], top: &Path) -> GitStatus {
    let mut files = HashMap::new();
    let mut records = out.split(|b| *b == 0).map(|r| String::from_utf8_lossy(r).into_owned());
    while let Some(rec) = records.next() {
        let (mark, path) = match rec.split_at_checked(2) {
            Some(("? ", path)) => (GitMark::Untracked, path.to_string()),
            Some(("1 ", rest)) => match rest.splitn(8, ' ').collect::<Vec<_>>()[..] {
                [xy, .., path] if rest.splitn(8, ' ').count() == 8 => (mark_for(xy), path.to_string()),
                _ => continue,
            },
            Some(("2 ", rest)) => {
                // Followed by the original path, its own record.
                records.next();
                match rest.splitn(9, ' ').collect::<Vec<_>>()[..] {
                    [_, .., path] if rest.splitn(9, ' ').count() == 9 => (GitMark::Renamed, path.to_string()),
                    _ => continue,
                }
            }
            Some(("u ", rest)) => match rest.splitn(10, ' ').last() {
                Some(path) => (GitMark::Conflict, path.to_string()),
                None => continue,
            },
            _ => continue,
        };
        files.insert(top.join(path), mark);
    }
    let mut dirty_dirs = HashSet::new();
    for path in files.keys() {
        for dir in path.ancestors().skip(1) {
            if !dir.starts_with(top) || !dirty_dirs.insert(dir.to_path_buf()) {
                break;
            }
        }
    }
    GitStatus { files, dirty_dirs }
}

/// An ordinary change's mark from its `XY` (index, worktree).
fn mark_for(xy: &str) -> GitMark {
    if xy.contains('A') {
        GitMark::Added
    } else if xy.contains('D') {
        GitMark::Deleted
    } else {
        GitMark::Modified
    }
}

// ----- the tree ----------------------------------------------------------------------------

/// A thread's tree: what's listed, what's open, what's selected. In memory only.
#[derive(Debug, Default)]
pub struct Tree {
    pub root: PathBuf,
    /// Listings, by folder.
    pub children: HashMap<PathBuf, Vec<Entry>>,
    pub expanded: HashSet<PathBuf>,
    /// Folders whose listing failed, and why.
    pub errors: HashMap<PathBuf, String>,
    pub selected: Option<PathBuf>,
    pub git: GitStatus,
    pub filter: String,
    /// With a filter: every file under the root (relative), once walked.
    pub all: Option<Vec<PathBuf>>,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        Self { root, ..Default::default() }
    }

    /// The folders a row stands for, from the top: one, or a chain whose folders each hold
    /// only the next (shown as one row, `src/main`).
    fn chain(&self, dir: &Entry) -> Vec<Entry> {
        let mut chain = vec![dir.clone()];
        while let Some([only]) = self.children.get(&chain.last().expect("non-empty").path).map(Vec::as_slice) {
            if !only.dir {
                break;
            }
            chain.push(only.clone());
        }
        chain
    }

    /// The rows shown: the open folders' contents, or with a filter, the matching files and
    /// their folders (all open).
    pub fn rows(&self) -> Vec<FileRow> {
        let mut rows = Vec::new();
        if self.filter.trim().is_empty() {
            self.push_dir(&self.root, 0, &mut rows);
        } else {
            self.push_matches(&mut rows);
        }
        rows
    }

    fn row(&self, path: &Path, name: String, depth: usize, dir: bool, expanded: bool) -> FileRow {
        FileRow {
            path: path.to_path_buf(),
            name: name.into(),
            depth,
            dir,
            expanded,
            git: self.git.files.get(path).copied(),
            changes_inside: dir && self.git.dirty_dirs.contains(path),
            selected: self.selected.as_deref() == Some(path),
        }
    }

    fn push_dir(&self, dir: &Path, depth: usize, rows: &mut Vec<FileRow>) {
        let Some(entries) = self.children.get(dir) else { return };
        for e in entries {
            if !e.dir {
                rows.push(self.row(&e.path, e.name.clone(), depth, false, false));
                continue;
            }
            let chain = self.chain(e);
            let last = chain.last().expect("non-empty");
            let open = self.expanded.contains(&last.path);
            let name = chain.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("/");
            rows.push(self.row(&last.path, name, depth, true, open));
            if open {
                self.push_dir(&last.path, depth + 1, rows);
            }
        }
    }

    fn push_matches(&self, rows: &mut Vec<FileRow>) {
        let words = filter_words(&self.filter);
        let Some(all) = &self.all else { return };
        // The matching files, grouped under their folders (folders first, as in the tree).
        #[derive(Default)]
        struct Node {
            dirs: BTreeMap<String, Node>,
            files: Vec<String>,
        }
        let mut top = Node::default();
        for rel in all.iter().filter(|p| matches(p, &words)) {
            let mut node = &mut top;
            let parts: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
            let Some((file, dirs)) = parts.split_last() else { continue };
            for d in dirs {
                node = node.dirs.entry(d.clone()).or_default();
            }
            node.files.push(file.clone());
        }
        fn walk(tree: &Tree, node: &Node, dir: &Path, depth: usize, rows: &mut Vec<FileRow>) {
            let mut dirs: Vec<_> = node.dirs.iter().collect();
            dirs.sort_by_key(|(name, _)| name.to_lowercase());
            for (name, child) in dirs {
                // A folder holding only one folder of matches shows as one row, as in the tree.
                let (mut name, mut child, mut path) = (name.clone(), child, dir.join(name));
                while child.files.is_empty() && child.dirs.len() == 1 {
                    let (n, c) = child.dirs.iter().next().expect("one");
                    name = format!("{name}/{n}");
                    path = path.join(n);
                    child = c;
                }
                rows.push(tree.row(&path, name, depth, true, true));
                walk(tree, child, &path, depth + 1, rows);
            }
            let mut files = node.files.clone();
            files.sort_by_key(|f| f.to_lowercase());
            for f in files {
                let path = dir.join(&f);
                rows.push(tree.row(&path, f, depth, false, false));
            }
        }
        walk(self, &top, &self.root, 0, rows);
    }

    /// The listed folder `changed` is in, to list again after it changed.
    pub fn listed_parent(&self, changed: &Path) -> Option<PathBuf> {
        changed.parent().filter(|d| self.children.contains_key(*d)).map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn tree_on_disk() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["src/main/inner", "docs", "target/debug", "B"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["src/main/inner/app.rs", "src/main/inner/lib.rs", "docs/guide.md", "README.md", "a.txt", ".env", "target/debug/out", "B/x"] {
            fs::write(root.join(f), "").unwrap();
        }
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        // `ignore` reads .gitignore inside a git repository.
        fs::create_dir_all(root.join(".git")).unwrap();
        dir
    }

    fn names(rows: &[FileRow]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}{}", "  ".repeat(r.depth), r.name, if r.dir { "/" } else { "" })).collect()
    }

    fn load(tree: &mut Tree, dir: &Path) {
        tree.children.insert(dir.to_path_buf(), list_dir(dir).unwrap());
    }

    #[test]
    fn listing_hides_ignored_and_git_and_sorts_folders_first() {
        let tmp = tree_on_disk();
        let listed: Vec<_> = list_dir(tmp.path()).unwrap().into_iter().map(|e| (e.name, e.dir)).collect();
        let want = [("B", true), ("docs", true), ("src", true), (".env", false), (".gitignore", false), ("a.txt", false), ("README.md", false)];
        assert_eq!(listed, want.map(|(n, d)| (n.to_string(), d)));
        // Ignore files from the folders above apply to a subfolder's listing too.
        fs::write(tmp.path().join("docs/skip.log"), "").unwrap();
        fs::write(tmp.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        let docs: Vec<_> = list_dir(&tmp.path().join("docs")).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(docs, ["guide.md"]);
        assert!(list_dir(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn single_child_folders_show_as_one_row_once_listed() {
        let tmp = tree_on_disk();
        let root = tmp.path().to_path_buf();
        let mut tree = Tree::new(root.clone());
        load(&mut tree, &root);
        assert_eq!(names(&tree.rows())[..3], ["B/", "docs/", "src/"]);
        // Opening src lists the chain down to the folder with more than one child.
        for d in ["src", "src/main", "src/main/inner"] {
            load(&mut tree, &root.join(d));
        }
        tree.expanded.insert(root.join("src/main/inner"));
        let rows = tree.rows();
        let src = rows.iter().position(|r| r.name.as_ref() == "src/main/inner").expect("one row");
        assert_eq!(rows[src].path, root.join("src/main/inner"));
        assert!(rows[src].expanded);
        assert_eq!(names(&rows[src + 1..src + 3]), ["  app.rs", "  lib.rs"]);
    }

    #[test]
    fn a_filter_shows_matching_files_under_their_folders() {
        let tmp = tree_on_disk();
        let root = tmp.path().to_path_buf();
        let mut tree = Tree::new(root.clone());
        tree.all = Some(all_files(&root));
        assert!(!tree.all.as_ref().unwrap().iter().any(|p| p.starts_with("target")), "ignored files aren't searched");
        tree.filter = "inner RS".into();
        assert_eq!(names(&tree.rows()), ["src/main/inner/", "  app.rs", "  lib.rs"]);
        tree.filter = "guide".into();
        assert_eq!(names(&tree.rows()), ["docs/", "  guide.md"]);
        tree.filter = "nothing-like-this".into();
        assert!(tree.rows().is_empty());
    }

    #[test]
    fn previews_tell_text_from_images_binaries_and_big_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = |n: &str| tmp.path().join(n);
        fs::write(p("a.rs"), "fn main() {}\n").unwrap();
        fs::write(p("b.bin"), [1u8, 0, 2]).unwrap();
        fs::write(p("c.txt"), [0xffu8, 0xfe, 0x41]).unwrap();
        fs::write(p("d.PNG"), [0u8; 4]).unwrap();
        fs::write(p("big.log"), vec![b'x'; MAX_PREVIEW_BYTES as usize + 1]).unwrap();
        assert_eq!(read_preview(&p("a.rs")), Preview::Text("fn main() {}\n".into()));
        assert_eq!(read_preview(&p("b.bin")), Preview::Binary);
        assert_eq!(read_preview(&p("c.txt")), Preview::Binary, "not UTF-8");
        assert_eq!(read_preview(&p("d.PNG")), Preview::Image(None), "not a real image: no size");
        assert_eq!(read_preview(&p("big.log")), Preview::TooLarge(MAX_PREVIEW_BYTES + 1));
        assert!(matches!(read_preview(&p("gone")), Preview::Failed(m) if m.contains("moved or deleted")));
        assert!(matches!(read_preview(tmp.path()), Preview::Failed(_)));
    }

    #[test]
    fn git_status_marks_files_and_their_folders() {
        let top = Path::new("/repo");
        let out = b"1 .M N... 100644 100644 100644 abc abc src/main.rs\0\
                    1 A. N... 000000 100644 100644 000 abc new.rs\0\
                    1 .D N... 100644 100644 000000 abc abc gone.rs\0\
                    2 R. N... 100644 100644 100644 abc abc R100 docs/b.md\0docs/a.md\0\
                    u UU N... 100644 100644 100644 100644 a b c both.rs\0\
                    ? notes/todo.txt\0";
        let s = parse_status(out, top);
        let mark = |p: &str| s.files.get(&top.join(p)).copied();
        assert_eq!(mark("src/main.rs"), Some(GitMark::Modified));
        assert_eq!(mark("new.rs"), Some(GitMark::Added));
        assert_eq!(mark("gone.rs"), Some(GitMark::Deleted));
        assert_eq!(mark("docs/b.md"), Some(GitMark::Renamed));
        assert_eq!(mark("docs/a.md"), None, "a rename's old path isn't listed");
        assert_eq!(mark("both.rs"), Some(GitMark::Conflict));
        assert_eq!(mark("notes/todo.txt"), Some(GitMark::Untracked));
        assert!(s.dirty_dirs.contains(&top.join("src")) && s.dirty_dirs.contains(&top.join("notes")));
        assert!(s.dirty_dirs.contains(top), "the root holds changes");
    }

    #[test]
    fn a_real_repository_reports_an_untracked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git").arg("init").arg("-q").arg(tmp.path()).status().is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        fs::write(tmp.path().join("new.txt"), "x").unwrap();
        let status = git_status(tmp.path()).expect("a repository");
        let top = tmp.path().canonicalize().unwrap();
        assert_eq!(status.files.get(&top.join("new.txt")), Some(&GitMark::Untracked));
        assert!(git_status(&std::env::temp_dir().join("space-surely-not-a-repo-xyz")).is_none());
    }
}
