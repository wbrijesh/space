//! Plugin discovery.
//!
//! Search order (a project plugin replaces a user plugin with the same `id`; neither may
//! take the `id` of a bundled plugin):
//! 1. bundled: `<repo>/plugins/*` (or every dir listed in `SPACE_PLUGIN_DIRS`)
//! 2. user: `~/.space/plugins/*`
//! 3. project: `<project>/.space/plugins/*`

use std::path::{Path, PathBuf};

use space_protocol::{MANIFEST_FILE, Manifest, ManifestError};

/// Env var: `:`-separated list of directories whose subdirectories are plugins. Replaces
/// the bundled-plugin lookup when set.
pub const PLUGIN_DIRS_ENV: &str = "SPACE_PLUGIN_DIRS";

/// Where a plugin was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    Bundled,
    User,
    Project,
}

/// A plugin directory with a valid manifest.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: Manifest,
    /// Absolute plugin directory.
    pub dir: PathBuf,
    pub source: PluginSource,
}

impl DiscoveredPlugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
}

/// Which root directories to scan.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryOptions {
    pub bundled_roots: Vec<PathBuf>,
    pub user_root: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
}

impl DiscoveryOptions {
    /// The standard locations for a project directory (if any).
    pub fn standard(project_dir: Option<&Path>) -> Self {
        Self {
            bundled_roots: bundled_plugin_roots(),
            user_root: dirs::home_dir().map(|h| h.join(".space").join("plugins")),
            project_root: project_dir.map(|p| p.join(".space").join("plugins")),
        }
    }
}

/// Result of a discovery pass: valid plugins plus per-directory errors.
#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub plugins: Vec<DiscoveredPlugin>,
    pub errors: Vec<ManifestError>,
}

/// Scan the configured roots.
pub fn discover(opts: &DiscoveryOptions) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let roots = opts
        .bundled_roots
        .iter()
        .map(|r| (r.as_path(), PluginSource::Bundled))
        .chain(opts.user_root.as_deref().map(|r| (r, PluginSource::User)))
        .chain(opts.project_root.as_deref().map(|r| (r, PluginSource::Project)));

    for (root, source) in roots {
        for dir in plugin_dirs(root) {
            match Manifest::load_from_dir(&dir) {
                Ok(manifest) => {
                    let plugin = DiscoveredPlugin { manifest, dir, source };
                    // Later roots override earlier ones with the same id, keeping position,
                    // except bundled plugins: a user or project plugin can't stand in for one
                    // (and inherit its settings, secrets and trust).
                    if let Some(existing) = report.plugins.iter_mut().find(|p| p.id() == plugin.id()) {
                        if existing.source == PluginSource::Bundled && source != PluginSource::Bundled {
                            report.errors.push(ManifestError::Invalid {
                                path: plugin.dir.join(MANIFEST_FILE),
                                message: format!("uses the id of a bundled plugin (\"{}\")", plugin.id()),
                            });
                            continue;
                        }
                        *existing = plugin;
                    } else {
                        report.plugins.push(plugin);
                    }
                }
                Err(e) => report.errors.push(e),
            }
        }
    }
    report
}

/// Subdirectories of `root` that contain a manifest, sorted by name.
fn plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(MANIFEST_FILE).is_file())
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect();
    dirs.sort();
    dirs
}

/// Directories to scan for bundled plugins.
///
/// `SPACE_PLUGIN_DIRS` wins if set. An installed app (see [`installed_app_contents`]) has its
/// plugins in `Contents/Resources/plugins`. Otherwise look for a repo checkout (a directory with
/// `Cargo.toml` and `plugins/`) above the executable, then fall back to the repo this crate
/// was compiled from (development builds).
pub fn bundled_plugin_roots() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os(PLUGIN_DIRS_ENV) {
        return std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect();
    }
    if let Some(contents) = std::env::current_exe().ok().and_then(|exe| installed_app_contents(&exe)) {
        return vec![contents.join("Resources/plugins")];
    }
    if let Some(root) = std::env::current_exe().ok().and_then(|exe| find_repo_root(&exe)) {
        return vec![root.join("plugins")];
    }
    if let Some(root) = find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x")) {
        return vec![root.join("plugins")];
    }
    Vec::new()
}

/// The `Contents` folder of the app `exe` runs from, when it's an installed app: inside a
/// `….app/Contents/MacOS/` and not under a `target/` folder (the dev builds dx and the release
/// step put there are still development: their plugins come from the repo). An installed app
/// carries its plugins: binaries in `Contents/Helpers`, manifests in `Contents/Resources/plugins`.
pub fn installed_app_contents(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_app = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension().is_some_and(|e| e == "app");
    let in_target = exe.ancestors().any(|d| d.file_name().is_some_and(|n| n == "target"));
    (is_app && !in_target).then(|| contents.to_path_buf())
}

/// Walk up from `start` to find a directory containing both `Cargo.toml` and `plugins/`.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .skip(1)
        .find(|d| d.join("Cargo.toml").is_file() && d.join("plugins").is_dir())
        .map(|d| d.canonicalize().unwrap_or_else(|_| d.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn an_installed_app_is_one_outside_target() {
        let app = |p: &str| installed_app_contents(Path::new(p));
        assert_eq!(app("/Applications/Space.app/Contents/MacOS/Space"), Some(PathBuf::from("/Applications/Space.app/Contents")));
        assert_eq!(app("/opt/homebrew/Caskroom/space/0.1.0/Space.app/Contents/MacOS/Space"), Some(PathBuf::from("/opt/homebrew/Caskroom/space/0.1.0/Space.app/Contents")));
        // Builds under target/ are development builds (the repo's plugins).
        assert_eq!(app("/Users/me/space/target/release/Space.app/Contents/MacOS/Space"), None);
        assert_eq!(app("/Users/me/space/target/dx/space-app/debug/macos/Space.app/Contents/MacOS/Space"), None);
        // A bare binary isn't an app.
        assert_eq!(app("/Users/me/space/target/debug/Space"), None);
        assert_eq!(app("/usr/local/bin/Space"), None);
    }

    fn write_plugin(root: &Path, dir: &str, id: &str, name: &str) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join(MANIFEST_FILE),
            format!("id='{id}'\nname='{name}'\nversion='1'\nprovides=['agent']\n[run]\ncommand='node'\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovery_order_and_override() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let user = tmp.path().join("user");
        let project = tmp.path().join("project");
        write_plugin(&bundled, "a", "alpha", "Bundled Alpha");
        write_plugin(&bundled, "b", "beta", "Bundled Beta");
        write_plugin(&user, "a2", "alpha", "User Alpha");
        write_plugin(&user, "d", "delta", "User Delta");
        write_plugin(&project, "d2", "delta", "Project Delta");
        write_plugin(&project, "g", "gamma", "Project Gamma");
        // Invalid manifest is reported, not fatal.
        fs::create_dir_all(project.join("bad")).unwrap();
        fs::write(project.join("bad").join(MANIFEST_FILE), "id = 3").unwrap();
        // Directory without manifest is ignored.
        fs::create_dir_all(bundled.join("empty")).unwrap();

        let report = discover(&DiscoveryOptions {
            bundled_roots: vec![bundled],
            user_root: Some(user),
            project_root: Some(project.clone()),
        });
        let names: Vec<_> = report.plugins.iter().map(|p| p.manifest.name.as_str()).collect();
        // A user plugin can't take a bundled plugin's id; a project plugin may replace a user one.
        assert_eq!(names, ["Bundled Alpha", "Bundled Beta", "Project Delta", "Project Gamma"]);
        assert_eq!(report.plugins[0].source, PluginSource::Bundled);
        assert_eq!(report.errors.len(), 2);
        assert!(report.errors.iter().any(|e| e.to_string().contains("uses the id of a bundled plugin (\"alpha\")")));
        assert!(report.errors.iter().any(|e| e.to_string().contains("bad")));
    }

    #[test]
    fn missing_roots_are_fine() {
        let report = discover(&DiscoveryOptions {
            bundled_roots: vec!["/nonexistent/space".into()],
            user_root: None,
            project_root: None,
        });
        assert!(report.plugins.is_empty() && report.errors.is_empty());
    }

    #[test]
    fn finds_this_repo() {
        let root = find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x")).unwrap();
        assert!(root.join("crates").is_dir());
    }
}
