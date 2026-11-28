//! `space-server --space-dir <dir> --project <dir> [--no-folder]`: the shared Space server, run by
//! the keeper (see `space_server::keeper_host`). It takes over what the server before it left
//! running, then serves apps on `<dir>/server.sock` until told to quit.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use space_core::{DiscoveryOptions, Host, HostOptions};
use space_server::keeper_host::{self, Paths};
use space_server::{Options, Server};

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
    rt.block_on(run(space_dir, project, no_folder))
}

async fn run(space_dir: PathBuf, project: PathBuf, no_folder: bool) -> Result<()> {
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
