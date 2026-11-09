//! `just run` keeps plugins current while the app runs. The app itself is patched live by dx;
//! plugins are other processes, so this watches their sources (and the shared crates they're
//! built from), rebuilds the plugin that changed and restarts it: a service plugin at once, the
//! agent once it's idle (its threads load their sessions again when used).
//!
//! On only when `just run` sets [`ENV`], so scenarios, test copies and `just test` never build.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use gpui::*;
use notify::{RecursiveMode, Watcher as _};
use space_core::protocol::Manifest;

use crate::app::SpaceApp;

/// Set by `just run`: rebuild and restart plugins whose sources change.
pub const ENV: &str = "SPACE_RELOAD_PLUGINS";
/// Changes this close together are one rebuild (an editor saves several files).
const SETTLE: Duration = Duration::from_millis(400);
/// How often a busy agent is checked again before it's restarted.
const AGENT_RETRY: Duration = Duration::from_secs(2);
/// Shared crates every Rust plugin is built from.
const SHARED: &[&str] = &["crates/space-protocol", "crates/space-plugin"];
/// What every TypeScript plugin is built from: the `space-ts` entry and the workspace's packages.
const SHARED_TS: &[&str] = &["plugins/space-ts.ts", "package.json", "bun.lock"];

/// A bundled plugin and how it's built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundled {
    pub id: String,
    pub name: String,
    /// Its folder, relative to the repo (`plugins/titles`).
    pub dir: PathBuf,
    pub build: Build,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Build {
    /// `cargo build -p <crate>`.
    Cargo(String),
    /// TypeScript: every bundled TypeScript plugin is in one Bun executable, `space-ts`
    /// (`plugins/space-ts.ts`), rebuilt with `bun build --compile`.
    Bun,
}

/// The repo this build came from.
fn repo() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    dir.canonicalize().unwrap_or(dir)
}

/// The bundled plugins under `<repo>/plugins` and how each is built.
pub fn bundled(repo: &Path) -> Vec<Bundled> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(repo.join("plugins")).into_iter().flatten().flatten() {
        let dir = entry.path();
        let Ok(manifest) = Manifest::load_from_dir(&dir) else { continue };
        let build = if let Some(name) = cargo_package(&dir.join("Cargo.toml")) {
            Build::Cargo(name)
        } else if dir.join("package.json").is_file() {
            Build::Bun
        } else {
            continue;
        };
        let rel = dir.strip_prefix(repo).map_or(dir.clone(), Path::to_path_buf);
        out.push(Bundled { id: manifest.id.clone(), name: manifest.name.clone(), dir: rel, build });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// `name` of a Cargo.toml's `[package]`.
fn cargo_package(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_package = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(value) = line.strip_prefix("name").map(str::trim).and_then(|l| l.strip_prefix('=')) {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// The plugins a change to `path` (relative to the repo) calls for: its own plugin, every Rust
/// plugin for a shared crate, every TypeScript plugin for the `space-ts` entry or the workspace's
/// packages. Build output, dependencies and VCS files call for none.
pub fn affected(path: &Path, plugins: &[Bundled]) -> Vec<usize> {
    let skipped = path.components().any(|c| {
        matches!(c, Component::Normal(n) if matches!(n.to_str(), Some("target" | "node_modules" | "dist" | ".git")))
    });
    if skipped {
        return Vec::new();
    }
    let all = |bun: bool| plugins.iter().enumerate().filter(|(_, p)| (p.build == Build::Bun) == bun).map(|(i, _)| i).collect();
    if SHARED.iter().any(|s| path.starts_with(s)) {
        return all(false);
    }
    if SHARED_TS.iter().any(|s| path == Path::new(s)) {
        return all(true);
    }
    plugins.iter().position(|p| path.starts_with(&p.dir)).into_iter().collect()
}

/// Start watching (when [`ENV`] is set).
pub fn start(cx: &mut Context<SpaceApp>) {
    if std::env::var_os(ENV).is_none() {
        return;
    }
    let repo = repo();
    let plugins = bundled(&repo);
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<PathBuf>();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, notify::EventKind::Access(_))
        {
            for path in event.paths {
                let _ = tx.unbounded_send(path);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("plugin reload: can't watch files: {e}");
            return;
        }
    };
    for dir in std::iter::once(repo.join("plugins")).chain(SHARED.iter().map(|s| repo.join(s))) {
        if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
            tracing::warn!("plugin reload: can't watch {}: {e}", dir.display());
        }
    }
    // The workspace's package.json and bun.lock (the TypeScript plugins' dependencies).
    if let Err(e) = watcher.watch(&repo, RecursiveMode::NonRecursive) {
        tracing::warn!("plugin reload: can't watch {}: {e}", repo.display());
    }
    eprintln!("space: plugins are rebuilt and restarted when their sources change ({} watched)", plugins.len());
    cx.spawn(async move |this, cx| {
        // Kept alive as long as this task.
        let _watcher = watcher;
        while let Some(first) = rx.next().await {
            // Let a burst of saves settle, then take everything it changed.
            cx.background_executor().timer(SETTLE).await;
            let mut paths = vec![first];
            while let Ok(p) = rx.try_recv() {
                paths.push(p);
            }
            let due: BTreeSet<usize> = paths
                .iter()
                .filter_map(|p| p.strip_prefix(&repo).ok())
                .flat_map(|p| affected(p, &plugins))
                .collect();
            for ix in due {
                let plugin = plugins[ix].clone();
                let (repo, name) = (repo.clone(), plugin.name.clone());
                eprintln!("space: rebuilding {name}…");
                let built = cx.background_spawn(async move { build(&repo, &plugin) }).await;
                let id = plugins[ix].id.clone();
                let Ok(()) = this.update(cx, |this, cx| match built {
                    Ok(()) => {
                        eprintln!("space: {name} rebuilt; restarting it");
                        this.restart_rebuilt(id, cx);
                    }
                    Err(e) => {
                        eprintln!("space: {name} didn't build:\n{e}");
                        this.banner = Some(format!("{name} didn't build: {} (details in the terminal)", first_error(&e)));
                        cx.notify();
                    }
                }) else {
                    return;
                };
            }
        }
    })
    .detach();
}

/// Build one plugin; the error is the build's output.
fn build(repo: &Path, plugin: &Bundled) -> Result<(), String> {
    let mut command = match &plugin.build {
        Build::Cargo(name) => {
            let mut c = std::process::Command::new("cargo");
            c.args(["build", "-p", name]);
            c
        }
        Build::Bun => return build_space_ts(repo),
    };
    run(command.current_dir(repo))
}

/// `bun install`, then compile `space-ts` to a temporary file and move it over the old one: a
/// plugin still running keeps its copy, and nothing starts a half-written file.
fn build_space_ts(repo: &Path) -> Result<(), String> {
    run(std::process::Command::new("bun").arg("install").current_dir(repo))?;
    let out = space_core::process::target_dir().join("space-ts");
    let tmp = out.with_extension("new");
    run(std::process::Command::new("bun")
        .args(["build", "--compile", "plugins/space-ts.ts", "--outfile"])
        .arg(&tmp)
        .current_dir(repo))?;
    std::fs::rename(&tmp, &out).map_err(|e| format!("moving {} into place: {e}", tmp.display()))
}

/// Run a build command; the error is its output.
fn run(command: &mut std::process::Command) -> Result<(), String> {
    let out = command.output().map_err(|e| format!("couldn't run the build: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

/// The first line that says what went wrong.
fn first_error(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("error") || l.contains(": error"))
        .or_else(|| output.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("the build failed");
    space_ui::format::truncate_chars(line, 160)
}

impl SpaceApp {
    /// A plugin was rebuilt: run the new build. The agent waits until nothing is busy.
    fn restart_rebuilt(&mut self, plugin_id: String, cx: &mut Context<Self>) {
        if self.agent_id.as_deref() == Some(plugin_id.as_str()) {
            self.restart_agent_when_idle(cx);
            return;
        }
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = host.restart_service(&plugin_id).await {
                let _ = this.update(cx, |this, cx| {
                    this.banner = Some(format!("Restarting {plugin_id} failed: {e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Stop the agent once it's idle and start the new build; threads load their sessions again.
    fn restart_agent_when_idle(&mut self, cx: &mut Context<Self>) {
        if !self.agent_running() {
            // Asleep or stopped: the next start runs the new build.
            return;
        }
        if self.agent_busy() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(AGENT_RETRY).await;
                let _ = this.update(cx, |this, cx| this.restart_agent_when_idle(cx));
            })
            .detach();
            return;
        }
        self.sleep_agent(cx);
        self.ensure_agent(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Build, Bundled, affected, bundled, first_error};

    fn plugins() -> Vec<Bundled> {
        let p = |id: &str, build: Build| Bundled { id: id.into(), name: id.into(), dir: PathBuf::from(format!("plugins/{id}")), build };
        vec![
            p("pi-agent", Build::Bun),
            p("titles", Build::Cargo("space-titles".into())),
            p("trajectory", Build::Cargo("space-trajectory".into())),
            p("memory", Build::Bun),
        ]
    }

    #[test]
    fn a_change_rebuilds_its_plugin_or_every_rust_plugin_for_a_shared_crate() {
        let ps = plugins();
        let hit = |p: &str| affected(Path::new(p), &ps);
        assert_eq!(hit("plugins/titles/src/main.rs"), [1]);
        assert_eq!(hit("plugins/pi-agent/src/trace.ts"), [0]);
        assert_eq!(hit("crates/space-protocol/src/trace.rs"), [1, 2]);
        assert_eq!(hit("crates/space-plugin/src/lib.rs"), [1, 2]);
        // The space-ts entry and the workspace's packages: every TypeScript plugin.
        assert_eq!(hit("plugins/space-ts.ts"), [0, 3]);
        assert_eq!(hit("bun.lock"), [0, 3]);
        // Build output, dependencies and other crates don't.
        for none in ["plugins/pi-agent/dist/main.js", "plugins/pi-agent/node_modules/x/y.js", "plugins/titles/target/a", "crates/space-ui/src/lib.rs"] {
            assert!(hit(none).is_empty(), "{none}");
        }
    }

    #[test]
    fn finds_the_bundled_plugins_and_how_they_build() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let all = bundled(&repo);
        let pi = all.iter().find(|p| p.id == "pi-agent").expect("pi-agent");
        assert_eq!(pi.build, Build::Bun);
        let titles = all.iter().find(|p| p.id == "titles").expect("titles");
        assert_eq!((titles.build.clone(), titles.dir.clone()), (Build::Cargo("space-titles".into()), PathBuf::from("plugins/titles")));
    }

    #[test]
    fn the_first_error_line_is_the_summary() {
        let out = "   Compiling space-titles v0.1.0\nerror[E0308]: mismatched types\n  --> src/main.rs:3:5\n";
        assert_eq!(first_error(out), "error[E0308]: mismatched types");
        assert_eq!(first_error(""), "the build failed");
    }
}
