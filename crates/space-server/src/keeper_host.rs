//! The shared server: the one the real Space app uses, in Space's own folder, under a keeper so
//! its agents outlive it. Files in the folder: `keeper.sock`, `server.sock`, `server-sessions.json`
//! (open sessions, for the next server), `server-config.json` (the app's plugin choices) and
//! `logs/` (`keeper.log`, `server.log`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use space_keeper::{KeeperClient, ServerCommand};

/// Where the shared server's files go in a Space folder.
pub struct Paths {
    pub keeper_socket: PathBuf,
    pub server_socket: PathBuf,
    pub sessions: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    pub fn new(space_dir: &Path) -> Paths {
        Paths {
            keeper_socket: space_keeper::daemon::socket_path(space_dir),
            server_socket: space_core::remote::socket_path(space_dir),
            sessions: space_dir.join("server-sessions.json"),
            config: space_dir.join("server-config.json"),
            logs: space_dir.join("logs"),
        }
    }
}

/// Start the keeper (which starts the server) unless one is running, and wait until the server
/// answers on its socket. `server` is how to start it.
pub async fn ensure_running(space_dir: &Path, keeper_exe: &Path, server: &ServerCommand) -> Result<()> {
    let paths = Paths::new(space_dir);
    if tokio::net::UnixStream::connect(&paths.server_socket).await.is_ok() {
        return Ok(());
    }
    if tokio::net::UnixStream::connect(&paths.keeper_socket).await.is_err() {
        space_keeper::client::start(keeper_exe, &paths.keeper_socket, &paths.logs.join("keeper.log"), Some(server)).await?;
    }
    // A keeper without a server (the server is starting, or keeps failing): wait a while.
    for _ in 0..300 {
        if tokio::net::UnixStream::connect(&paths.server_socket).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("the Space server didn't start; see {}", paths.logs.join("server.log").display())
}

/// Connect to the keeper that started this server.
pub async fn connect_keeper(space_dir: &Path) -> Result<(KeeperClient, space_keeper::Welcome, Vec<space_keeper::AgentPipe>)> {
    let paths = Paths::new(space_dir);
    KeeperClient::connect(&paths.keeper_socket).await.context("the server runs under a keeper; none answers")
}
