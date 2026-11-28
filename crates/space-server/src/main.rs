//! `space-server --space-dir <dir> --project <dir> [--no-folder]`: the shared Space server, run by
//! the keeper (see `space_server::keeper_host`). It takes over what the server before it left
//! running, then serves apps on `<dir>/server.sock` until told to quit. Its menu bar item says
//! what it's doing and has Open Space and Quit Server….

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use space_core::{DiscoveryOptions, Host, HostOptions};
use space_server::keeper_host::{self, Paths};
use space_server::menubar::{Action, Menu};
use space_server::{Options, Server};
use tokio::sync::mpsc;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let mut args = std::env::args().skip(1);
    let (mut space_dir, mut project, mut no_folder) = (None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--space-dir" => space_dir = args.next().map(PathBuf::from),
            "--project" => project = args.next().map(PathBuf::from),
            "--no-folder" => no_folder = true,
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    let space_dir = space_dir.context("--space-dir is required")?;
    let project = project.or_else(dirs::home_dir).context("no project folder")?;
    let rt = tokio::runtime::Builder::new_multi_thread().thread_name("space-server").enable_all().build()?;
    let menu = Arc::new(Mutex::new(Menu::default()));
    let (actions_tx, actions) = mpsc::unbounded_channel();
    // The server on the runtime; AppKit (the menu bar) on this, the main, thread.
    {
        let menu = menu.clone();
        std::thread::spawn(move || {
            let code = match rt.block_on(run(space_dir, project, no_folder, menu, actions)) {
                Ok(()) => 0,
                Err(e) => {
                    tracing::error!("{e:#}");
                    1
                }
            };
            std::process::exit(code);
        });
    }
    space_server::menubar::run(menu, actions_tx)
}

async fn run(space_dir: PathBuf, project: PathBuf, no_folder: bool, menu: Arc<Mutex<Menu>>, mut actions: mpsc::UnboundedReceiver<Action>) -> Result<()> {
    let paths = Paths::new(&space_dir);
    let (keeper, welcome, pipes) = keeper_host::connect_keeper(&space_dir).await?;
    tracing::info!("server {} ({}) under keeper {}, {} agent(s) to take over", std::process::id(), space_core::build_info::current().version, welcome.pid, pipes.len());
    let mut options = HostOptions::for_project(&project);
    if no_folder {
        options.discovery = DiscoveryOptions::standard(None);
    }
    options.space_dir = Some(space_dir.clone());
    options.keeper = Some(keeper.clone());
    let (host, events) = Host::new(tokio::runtime::Handle::current(), options);
    let quit_keeper = keeper.clone();
    let server = Server::start(
        host,
        events,
        Options {
            sessions_file: Some(paths.sessions.clone()),
            config_file: Some(paths.config.clone()),
            on_quit: Some(Box::new(move || quit_keeper.quit())),
            pid: std::process::id(),
        },
    );
    server.apply_saved_config().await;
    server.adopt(pipes).await;
    tokio::spawn(keep_menu_current(server.clone(), menu));
    {
        let server = server.clone();
        tokio::spawn(async move {
            while let Some(action) = actions.recv().await {
                match action {
                    Action::OpenSpace => open_app(),
                    Action::QuitServer => server.quit().await,
                    Action::ApplyUpdate => {}
                }
            }
        });
    }
    // The keeper gone means everything is gone: so is this server.
    let mut connected = keeper.connected();
    let listening = Arc::clone(&server).listen(&paths.server_socket);
    tokio::select! {
        result = listening => result,
        _ = connected.wait_for(|c| !*c) => {
            tracing::info!("the keeper went away; stopping");
            server.host().flush_saves(space_server::FLUSH_TIMEOUT).await;
            Ok(())
        }
    }
}

/// The menu bar item's lines, each second.
async fn keep_menu_current(server: Arc<Server>, menu: Arc<Mutex<Menu>>) {
    let started = Instant::now();
    let build = space_core::build_info::current();
    let name = if build.is_release() { format!("Space {}", build.version) } else { format!("Space {} · Development", build.version) };
    loop {
        let working = server.working();
        let terminals = server.host().terminal_list().await.map(|t| t.len()).unwrap_or(0);
        let mut status = vec![format!("Up {}", uptime(started.elapsed()))];
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        if working > 0 {
            status.push(format!("{} working", plural(working, "thread", "threads")));
        }
        if terminals > 0 {
            status.push(plural(terminals, "terminal", "terminals"));
        }
        let stops = match (working, terminals) {
            (0, 0) => None,
            (w, 0) => Some(format!("{} working", plural(w, "thread", "threads"))),
            (0, t) => Some(plural(t, "terminal", "terminals")),
            (w, t) => Some(format!("{} working and {}", plural(w, "thread", "threads"), plural(t, "terminal", "terminals"))),
        };
        {
            let mut m = menu.lock().unwrap();
            m.lines = vec![name.clone(), status.join(" · ")];
            m.stops = stops;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// "3 h 12 min", "12 min", "40 s".
fn uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// Open Space: the app this server came with (an app bundle's, or this build's dev bundle).
fn open_app() {
    let Some(app) = app_bundle() else {
        tracing::warn!("no Space app found beside this server");
        return;
    };
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg(&app).spawn() {
        tracing::warn!("opening {}: {e}", app.display());
    }
}

fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Space.app/Contents/Helpers/space-server
    if dir.ends_with("Contents/Helpers") {
        return dir.parent()?.parent().map(Path::to_path_buf);
    }
    Some(dir.join("Space.app")).filter(|p| p.is_dir())
}
