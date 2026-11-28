//! Which build this is: a release or a development build, and its version.
//!
//! A **release** comes only from `just release` (used by `just install` and `just publish`), whose
//! cargo build sets `SPACE_RELEASE=1`; its version is the release number ("0.1.8"). **Anything
//! else is a development build** (`just build`, `just run`, dev bundles, test copies): its version
//! is the short commit it was built from ("a1b2c3d"), and it knows the checkout it came from and
//! the full commit, to follow that checkout for new commits.
//!
//! A development build learns its commit from `build.txt` beside its executable, written by
//! `scripts/build-info.sh` (through `scripts/dev-bundle.sh`, and copied with SpaceTest), so no
//! crate is rebuilt for a new commit:
//!
//! ```text
//! commit=<full commit>        what it was built from
//! checkout=<repo root>        where
//! edited=true|false           whether the checkout had uncommitted edits
//! built=24 Nov 2026, 00:41    when
//! source=… binary=…           for `scripts/dev-apps.sh status`, to tell whether it's current
//! ```
//!
//! Without the file (a bare `target/debug/Space`, tests) the checkout is the one this crate was
//! compiled in, and the commit is that checkout's HEAD when first asked.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Development,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub mode: Mode,
    /// "0.1.8" for a release; the short commit ("a1b2c3d") for a development build, or
    /// "development" when its commit isn't known.
    pub version: String,
    /// The full commit a development build was built from; `None` for a release (or unknown).
    pub commit: Option<String>,
    /// The checkout a development build was built from (its repo root); `None` for a release.
    pub checkout: Option<PathBuf>,
    /// Whether the checkout had uncommitted edits when it was built.
    pub edited: bool,
    /// When a development build was made ("24 Nov 2026, 00:41"), if known.
    pub built: Option<String>,
}

impl Build {
    pub fn is_release(&self) -> bool {
        self.mode == Mode::Release
    }

    /// The secondary line beside the version: when it was built, and whether from edits
    /// ("24 Nov 2026, 00:41 · edited"); `None` for a release.
    pub fn detail(&self) -> Option<String> {
        let parts: Vec<&str> = self.built.as_deref().into_iter().chain(self.edited.then_some("edited")).collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// This process's build: the build-time mode, then `build.txt` beside the executable.
pub fn current() -> &'static Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    BUILD.get_or_init(|| {
        let file = std::env::current_exe().ok().and_then(|exe| std::fs::read_to_string(exe.with_file_name("build.txt")).ok());
        let mut build = parse(compiled_mode(), file.as_deref(), compiled_checkout());
        if build.mode == Mode::Development && build.commit.is_none() {
            build.commit = build.checkout.as_deref().and_then(head);
            if let Some(commit) = &build.commit {
                build.version = short(commit);
            }
        }
        build
    })
}

/// Release only when the release recipe's build said so; development otherwise.
fn compiled_mode() -> Mode {
    if option_env!("SPACE_RELEASE") == Some("1") { Mode::Release } else { Mode::Development }
}

/// The repo root this crate was compiled in (crates/space-core/../..).
fn compiled_checkout() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(2).unwrap_or(manifest).to_path_buf()
}

fn head(checkout: &Path) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(checkout).args(["rev-parse", "HEAD"]).output().ok()?;
    let commit = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !commit.is_empty()).then_some(commit)
}

fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// A build from its mode, its `build.txt` (if any) and the checkout it was compiled in. A release
/// ignores the file: its version is the crate's.
pub fn parse(mode: Mode, file: Option<&str>, compiled_checkout: PathBuf) -> Build {
    if mode == Mode::Release {
        return Build {
            mode,
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: None,
            checkout: None,
            edited: false,
            built: None,
        };
    }
    let field = |key: &str| {
        file.and_then(|f| f.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(str::trim).map(String::from)))
            .filter(|v| !v.is_empty())
    };
    let commit = field("commit");
    Build {
        mode,
        version: commit.as_deref().map_or_else(|| "development".to_string(), short),
        checkout: Some(field("checkout").map_or(compiled_checkout, PathBuf::from)),
        commit,
        edited: field("edited").as_deref() == Some("true"),
        built: field("built"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "commit=a1b2c3d4e5f60718293a4b5c6d7e8f9012345678\ncheckout=/Users/x/space\nedited=true\n\
                        built=24 Nov 2026, 00:41\nsource=a1b2c3d-123\nbinary=1\n";

    #[test]
    fn a_development_build_is_its_short_commit_from_its_checkout() {
        let b = parse(Mode::Development, Some(FILE), PathBuf::from("/compiled"));
        assert_eq!(b.mode, Mode::Development);
        assert_eq!(b.version, "a1b2c3d");
        assert_eq!(b.commit.as_deref(), Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"));
        assert_eq!(b.checkout.as_deref(), Some(Path::new("/Users/x/space")));
        assert!(b.edited);
        assert_eq!(b.detail().as_deref(), Some("24 Nov 2026, 00:41 · edited"));
    }

    #[test]
    fn without_a_file_it_is_still_development_from_the_compiled_checkout() {
        let b = parse(Mode::Development, None, PathBuf::from("/compiled"));
        assert_eq!(b.mode, Mode::Development);
        assert_eq!(b.version, "development");
        assert_eq!(b.commit, None);
        assert_eq!(b.checkout.as_deref(), Some(Path::new("/compiled")));
        assert_eq!(b.detail(), None);
        let unedited = parse(Mode::Development, Some("commit=0123456789\nedited=false\nbuilt=today\n"), PathBuf::from("/c"));
        assert!(!unedited.edited);
        assert_eq!(unedited.detail().as_deref(), Some("today"));
    }

    #[test]
    fn a_release_is_its_version_whatever_the_file_says() {
        let b = parse(Mode::Release, Some(FILE), PathBuf::from("/compiled"));
        assert!(b.is_release());
        assert_eq!(b.version, env!("CARGO_PKG_VERSION"));
        assert_eq!((b.commit, b.checkout, b.edited, b.built), (None, None, false, None));
    }

    #[test]
    fn this_test_build_is_development_from_this_checkout() {
        assert_eq!(compiled_mode(), Mode::Development);
        assert!(compiled_checkout().join("crates/space-core/Cargo.toml").exists());
    }
}
