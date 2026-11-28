//! Updates: Space building Space. A development build follows the branch its checkout is on; each
//! new commit there is built in a worktree of its own (never the checkout you work in) and
//! staged as a whole app in `<space dir>/builds/<version>/Space.app`. A release build follows
//! GitHub Releases. Either way, once a build is ready its parts are compared with the running
//! build's (`components.json`) to say what switching costs ([`UpdateKind`]), and it waits for you:
//!
//! - **App**: the app relaunches from the new build.
//! - **Server**: the keeper runs the new server, which takes over the agents; the app relaunches.
//! - **Agent**: the agents stop (when idle, or now), then as Server; a new keeper means
//!   everything stops and the new app starts it all again.
//!
//! A new app that doesn't come up goes back to the one before; a new server that keeps failing,
//! the keeper replaces with the one before. The running build and the one before it are kept.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use space_core::build_info::{self, Build};
use space_core::remote::{Components, Update, UpdateKind, UpdateStage};
use space_keeper::{KeeperClient, ServerCommand};

use crate::Server;

/// How often the checkout is looked at for a new commit.
const WATCH_EVERY: Duration = Duration::from_secs(3);
/// How long a relaunched app has to come up before the one before it comes back.
const APP_GRACE: Duration = Duration::from_secs(25);

/// A build ready to switch to.
#[derive(Debug, Clone)]
pub struct Staged {
    pub version: String,
    /// Its `Space.app`.
    pub app: PathBuf,
    pub components: Option<Components>,
    pub kind: UpdateKind,
}

pub struct Updater {
    server: Arc<Server>,
    keeper: KeeperClient,
    space_dir: PathBuf,
    /// The server's own arguments, for the next server.
    server_args: Vec<String>,
    running: Build,
    running_components: Option<Components>,
    staged: Mutex<Option<Staged>>,
    applying: Mutex<bool>,
}

impl Updater {
    pub fn new(server: Arc<Server>, keeper: KeeperClient, space_dir: PathBuf, server_args: Vec<String>) -> Arc<Updater> {
        Arc::new(Updater {
            server,
            keeper,
            running_components: running_components(),
            running: build_info::current().clone(),
            space_dir,
            server_args,
            staged: Mutex::new(None),
            applying: Mutex::new(false),
        })
    }

    fn builds(&self) -> PathBuf {
        self.space_dir.join("builds")
    }

    fn set(&self, update: Option<Update>) {
        self.server.set_update(update);
    }

    /// Follow the checkout's branch (development builds) for new commits, building each.
    pub async fn watch(self: Arc<Self>) {
        let Some(checkout) = self.running.checkout.clone().filter(|_| !self.running.is_release()) else { return };
        let mut tried: Option<String> = self.running.commit.clone();
        loop {
            tokio::time::sleep(WATCH_EVERY).await;
            if *self.applying.lock().unwrap() {
                continue;
            }
            let head = match git(&checkout, &["rev-parse", "HEAD"]).await {
                Ok(head) => head,
                Err(e) => {
                    tracing::debug!("reading the checkout's HEAD: {e:#}");
                    if !checkout.exists() {
                        self.set(Some(Update { version: String::new(), stage: UpdateStage::Failed, kind: None, waiting_for_idle: false, error: Some("Can't check for updates: the checkout isn't there".into()) }));
                    }
                    continue;
                }
            };
            if tried.as_deref() == Some(head.as_str()) {
                continue;
            }
            tried = Some(head.clone());
            let version = head[..7.min(head.len())].to_string();
            if self.running.commit.as_deref() == Some(head.as_str()) {
                // Back on the running commit (a branch switched back): nothing to update.
                *self.staged.lock().unwrap() = None;
                self.set(None);
                continue;
            }
            self.set(Some(Update { version: version.clone(), stage: UpdateStage::Getting, kind: None, waiting_for_idle: false, error: None }));
            match self.build(&checkout, &head, &version).await {
                Ok(staged) => self.ready(staged),
                Err(e) => {
                    tracing::warn!("building {version}: {e:#}");
                    self.set(Some(Update { version, stage: UpdateStage::Failed, kind: None, waiting_for_idle: false, error: Some(format!("{e:#}")) }));
                }
            }
        }
    }

    fn ready(&self, staged: Staged) {
        tracing::info!("Space {} is ready ({:?})", staged.version, staged.kind);
        self.set(Some(Update { version: staged.version.clone(), stage: UpdateStage::Ready, kind: Some(staged.kind), waiting_for_idle: false, error: None }));
        *self.staged.lock().unwrap() = Some(staged);
        self.clean_old_builds();
    }

    /// Build `commit` in the build worktree and stage it.
    async fn build(&self, checkout: &Path, commit: &str, version: &str) -> Result<Staged> {
        let builds = self.builds();
        std::fs::create_dir_all(&builds)?;
        let worktree = builds.join("worktree");
        let log = self.space_dir.join("logs").join(format!("build-{version}.log"));
        if !worktree.join(".git").exists() {
            let _ = std::fs::remove_dir_all(&worktree);
            git(checkout, &["worktree", "prune"]).await.ok();
            git(checkout, &["worktree", "add", "--detach", &worktree.display().to_string(), commit]).await?;
        } else {
            git(&worktree, &["checkout", "--detach", "--force", commit]).await?;
            // What a build left that git doesn't track, except the build output and packages.
            git(&worktree, &["clean", "-fdq", "-e", "target", "-e", "node_modules"]).await?;
        }
        let dest = builds.join(version);
        run_logged(&worktree, "just", &["build"], &log).await.with_context(|| format!("Space {version} didn't build; see {}", log.display()))?;
        run_logged(&worktree, "scripts/stage.sh", &[&dest.display().to_string(), &checkout.display().to_string()], &log)
            .await
            .with_context(|| format!("Space {version} couldn't be staged; see {}", log.display()))?;
        let app = dest.join("Space.app");
        warm_up(&app).await;
        let components: Option<Components> = std::fs::read(app.join("Contents/Resources/components.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let kind = match &components {
            Some(c) => c.kind_from(self.running_components.as_ref()).unwrap_or(UpdateKind::App),
            None => UpdateKind::Agent,
        };
        Ok(Staged { version: version.to_string(), app, components, kind })
    }

    /// Keep the running builds (the server's and the app's), the staged one and the one before;
    /// delete the rest.
    fn clean_old_builds(&self) {
        // The app may run from another build than the server (after an App update).
        let current = std::fs::read_to_string(self.builds().join("current")).ok().map(|p| PathBuf::from(p.trim()));
        let keep: Vec<PathBuf> = [running_bundle(), current, self.staged.lock().unwrap().as_ref().map(|s| s.app.clone()), previous_app(&self.space_dir)]
            .into_iter()
            .flatten()
            .filter_map(|app| app.parent().map(Path::to_path_buf))
            .collect();
        let Ok(entries) = std::fs::read_dir(self.builds()) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == "worktree" || keep.iter().any(|k| k == &path) || !path.join("Space.app").is_dir() {
                continue;
            }
            tracing::info!("deleting the old build {}", path.display());
            let _ = std::fs::remove_dir_all(&path);
        }
    }

    /// Switch to the staged build: now, or (an Agent update with `when_idle`) once no thread
    /// is working.
    pub async fn apply(self: Arc<Self>, when_idle: bool) {
        let Some(staged) = self.staged.lock().unwrap().clone() else { return };
        {
            let mut applying = self.applying.lock().unwrap();
            if *applying {
                return;
            }
            *applying = true;
        }
        if staged.kind == UpdateKind::Agent && when_idle {
            let mut waiting = false;
            while self.server.working() > 0 {
                if !waiting {
                    waiting = true;
                    self.set(Some(Update { version: staged.version.clone(), stage: UpdateStage::Ready, kind: Some(staged.kind), waiting_for_idle: true, error: None }));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        if let Err(e) = self.switch(&staged).await {
            tracing::warn!("switching to {}: {e:#}", staged.version);
            *self.applying.lock().unwrap() = false;
            self.set(Some(Update { version: staged.version, stage: UpdateStage::Failed, kind: Some(staged.kind), waiting_for_idle: false, error: Some(format!("{e:#}")) }));
        }
    }

    async fn switch(&self, staged: &Staged) -> Result<()> {
        let app_exe = staged.app.join("Contents/MacOS/Space");
        let new_server = staged.app.join("Contents/Helpers/space-server");
        if !app_exe.is_file() || !new_server.is_file() {
            bail!("the staged build at {} is incomplete", staged.app.display());
        }
        // The build to go back to, if the new app doesn't come up.
        if let Some(running) = running_bundle() {
            let _ = std::fs::write(self.space_dir.join("builds").join("previous"), running.display().to_string());
        }
        let _ = std::fs::write(self.builds().join("current"), staged.app.display().to_string());
        tracing::info!("switching to Space {} ({:?})", staged.version, staged.kind);
        let apps = self.server.app_count();
        match staged.kind {
            UpdateKind::App => {
                self.server.relaunch_apps(&app_exe);
                self.watch_app_comes_up(staged.clone(), apps);
                *self.staged.lock().unwrap() = None;
                *self.applying.lock().unwrap() = false;
                self.set(None);
                Ok(())
            }
            UpdateKind::Server | UpdateKind::Agent => {
                let keeper_changed = staged.components.as_ref().is_none_or(|c| c.keeper_changed(self.running_components.as_ref()));
                if staged.kind == UpdateKind::Agent {
                    // Running turns stop; threads go on from their saved sessions.
                    self.server.stop_agents();
                }
                self.server.relaunch_apps(&app_exe);
                if staged.kind == UpdateKind::Agent && keeper_changed {
                    // A new keeper: everything stops, and the new app starts it all again.
                    self.server.host().flush_saves(crate::FLUSH_TIMEOUT).await;
                    if apps == 0 {
                        open_app(&staged.app);
                    }
                    self.keeper.quit();
                } else {
                    // The keeper runs the new server once this one exits, and holds what the
                    // agents say meanwhile: nothing arrives here after the hang-up, so the last
                    // writes are all there is to finish.
                    self.keeper.replace_server(ServerCommand { program: new_server, args: self.server_args.clone() });
                    self.server.host().hand_over_agents();
                    self.keeper.close();
                    // What arrived before the hang-up is handled, then nothing more is saved.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    self.server.host().freeze_saves();
                    self.server.host().flush_saves(crate::FLUSH_TIMEOUT).await;
                    std::process::exit(0);
                }
                Ok(())
            }
        }
    }

    /// After an App update: if no app of the new build connects in time, bring back the one
    /// before (the new app crashed on start, or never connected).
    fn watch_app_comes_up(&self, staged: Staged, apps_before: usize) {
        if apps_before == 0 {
            return;
        }
        let server = self.server.clone();
        let space_dir = self.space_dir.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + APP_GRACE;
            while tokio::time::Instant::now() < deadline {
                if server.app_versions().iter().any(|v| v == &staged.version) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let Some(previous) = previous_app(&space_dir) else { return };
            tracing::warn!("Space {} didn't come up; going back to {}", staged.version, previous.display());
            let _ = std::fs::write(space_dir.join("builds").join("current"), previous.display().to_string());
            open_app(&previous);
            server.set_update(Some(Update {
                version: staged.version.clone(),
                stage: UpdateStage::Failed,
                kind: Some(UpdateKind::App),
                waiting_for_idle: false,
                error: Some(format!("Space {} didn't start, so Space went back to the build before it. See ~/.space/logs.", staged.version)),
            }));
        });
    }
}

/// Run each executable of a staged build once (it exits at once: no input, or an argument it
/// refuses). macOS looks over a new executable the first time it runs, which takes about a second
/// each; done here, it doesn't hold up switching to the build.
async fn warm_up(app: &Path) {
    let mut exes: Vec<PathBuf> = std::fs::read_dir(app.join("Contents/Helpers")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_none()).collect();
    exes.push(app.join("Contents/MacOS/Space"));
    for exe in exes {
        let child = tokio::process::Command::new(&exe).arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn();
        if let Ok(mut child) = child {
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        }
    }
}

/// The `Space.app` this server runs from, when it runs from one.
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let helpers = exe.parent()?;
    helpers.ends_with("Contents/Helpers").then(|| helpers.parent()?.parent().map(Path::to_path_buf)).flatten()
}

/// The running build's parts, when it's a staged or released bundle.
fn running_components() -> Option<Components> {
    let app = running_bundle()?;
    serde_json::from_slice(&std::fs::read(app.join("Contents/Resources/components.json")).ok()?).ok()
}

/// The build that ran before the last switch.
fn previous_app(space_dir: &Path) -> Option<PathBuf> {
    let path = std::fs::read_to_string(space_dir.join("builds").join("previous")).ok()?;
    Some(PathBuf::from(path.trim())).filter(|p| p.is_dir())
}

/// Open a Space.app (it connects to this server, or starts the next one).
pub fn open_app(app: &Path) {
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg("-n").arg(app).spawn() {
        tracing::warn!("opening {}: {e}", app.display());
    }
}

/// `PATH` with where developer tools live (an app started from the Dock gets a bare one).
fn tool_path() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let extra = [home.join(".cargo/bin"), home.join(".bun/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    let mut parts: Vec<String> = extra.iter().map(|p| p.display().to_string()).collect();
    parts.push(std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()));
    parts.join(":")
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git").arg("-C").arg(dir).args(args).env("PATH", tool_path()).stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `program` in `dir`, its output appended to `log`.
async fn run_logged(dir: &Path, program: &str, args: &[&str], log: &Path) -> Result<()> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let status = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("PATH", tool_path())
        // The build is the worktree's own, wherever this server was built.
        .env_remove("CARGO_TARGET_DIR")
        .env_remove(space_core::process::TARGET_DIR_ENV)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .status()
        .await
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}
