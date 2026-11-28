//! What the Space app and the Space server say to each other: JSON-RPC over a Unix socket (one
//! message per line, [`space_plugin::rpc`]). The app calls the host's methods by name
//! (`host.*`, `agent.*`, `session.*`); the server reports the host's events (`event`), the
//! state the app reads without asking (`state`) and terminal output (`terminal.data`).
//!
//! The server keeps running when the app quits: its agents keep working and its threads keep
//! being saved. A thread's session events reach the app only once the app has opened the thread
//! (`host.open_thread`, or a session it made itself), so a thread that went on while no app was
//! there is read whole, then followed, with every event exactly once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use space_protocol::service::{CatalogEntry, SettingValues};

use crate::discovery::DiscoveredPlugin;
use crate::grants::Approval;
use crate::host::{PluginStatus, SettingState};

/// Bumped when a change to these messages would confuse the other side. An app that speaks
/// another version than the server is told so and doesn't half-work.
pub const SERVER_PROTOCOL: u32 = 1;

/// Notification: a [`crate::HostEvent`].
pub const EVENT: &str = "event";
/// Notification: the [`HostState`] changed.
pub const STATE: &str = "state";
/// Notification: [`TerminalData`].
pub const TERMINAL_DATA: &str = "terminal.data";
/// Notification: the server is quitting (Quit Server): the app quits too.
pub const QUITTING: &str = "server.quitting";

/// What the app reads without asking: a copy kept current by [`STATE`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostState {
    pub plugins: Vec<DiscoveredPlugin>,
    pub statuses: BTreeMap<String, PluginStatus>,
    pub disabled: Vec<String>,
    pub approvals: BTreeMap<String, Approval>,
    pub catalogs: BTreeMap<String, Vec<CatalogEntry>>,
    pub plugin_settings: BTreeMap<String, Vec<SettingState>>,
    pub has_session_store: bool,
    pub has_terminal: bool,
    pub has_trajectory: bool,
    pub terminal_plugin: Option<String>,
    pub project_dir: PathBuf,
    /// A new build of Space, while one is on its way or ready.
    pub update: Option<Update>,
}

/// A new build of Space: found, being built (development) or downloaded (release), ready to
/// switch to, or failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Update {
    /// Its version ("e4f5a6b", or "0.1.9").
    pub version: String,
    pub stage: UpdateStage,
    /// What switching to it costs (once ready).
    pub kind: Option<UpdateKind>,
    /// Waiting for every thread to finish before restarting (Restart when idle).
    pub waiting_for_idle: bool,
    /// Why it isn't ready (didn't build, didn't download), with where the log is.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateStage {
    /// Being built or downloaded.
    Getting,
    Ready,
    Failed,
}

/// What switching to a build costs, by what changed in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateKind {
    /// Only the window: the app relaunches. Nothing stops.
    App,
    /// The server (and maybe the app): it restarts, the app relaunches. Nothing stops.
    Server,
    /// An agent, the keeper or the protocol between server and agents: agents restart, so
    /// running turns stop (or it waits until none runs).
    Agent,
}

/// What each part of a build is (`components.json`, from `scripts/components.sh`):
/// comparing two says what an update between them changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Components {
    pub app: String,
    pub server: String,
    pub agent: String,
    #[serde(default)]
    pub keeper: String,
    pub protocol: u32,
}

impl Components {
    /// What switching from `running` (unknown: `None`) to `self` costs; `None`: nothing changed.
    pub fn kind_from(&self, running: Option<&Components>) -> Option<UpdateKind> {
        let Some(old) = running else { return Some(UpdateKind::Agent) };
        if old.agent != self.agent || old.keeper != self.keeper || old.protocol != self.protocol {
            Some(UpdateKind::Agent)
        } else if old.server != self.server {
            Some(UpdateKind::Server)
        } else if old.app != self.app {
            Some(UpdateKind::App)
        } else {
            None
        }
    }

    /// Whether the keeper changed (everything restarts, not only the agents).
    pub fn keeper_changed(&self, running: Option<&Components>) -> bool {
        running.is_none_or(|old| old.keeper != self.keeper)
    }
}

/// Notification: start the app again from this executable (an update).
pub const RELAUNCH: &str = "server.relaunch";

/// The answer to `server.hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    /// The server's process id (0 for one inside the app).
    pub pid: u32,
    /// Its build ([`crate::build_info::Build::version`]).
    pub version: String,
    pub state: HostState,
    /// Agents running now (an app that opens later finds them here).
    pub agents: Vec<AgentInfo>,
    /// Sessions open now, with the thread each was made for.
    pub sessions: Vec<LiveSession>,
}

/// A running agent, as an app sees it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub plugin_id: String,
    pub agent_name: Option<String>,
    pub can_load_session: bool,
    pub accepts_images: bool,
    pub accepts_embedded_text: bool,
    pub can_steer: bool,
    pub can_rewind: bool,
}

/// An open session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSession {
    pub session_id: String,
    pub plugin_id: String,
    pub thread_id: Option<String>,
    pub cwd: PathBuf,
    /// A turn is running.
    pub running: bool,
    pub mode: String,
}

/// Terminal output (`data` in base64).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalData {
    pub id: String,
    pub seq: u64,
    pub data: space_protocol::terminal::Bytes,
}

/// `host.set_text_settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSettings {
    pub plugin_id: String,
    pub values: SettingValues,
}

/// The socket a server listens on, in a Space data folder.
pub fn socket_path(space_dir: &std::path::Path) -> PathBuf {
    space_dir.join("server.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_changed_decides_what_an_update_costs() {
        let base = Components { app: "a".into(), server: "s".into(), agent: "g".into(), keeper: "k".into(), protocol: 5 };
        let with = |f: fn(&mut Components)| {
            let mut c = base.clone();
            f(&mut c);
            c
        };
        assert_eq!(base.kind_from(Some(&base)), None);
        assert_eq!(with(|c| c.app = "a2".into()).kind_from(Some(&base)), Some(UpdateKind::App));
        assert_eq!(with(|c| c.server = "s2".into()).kind_from(Some(&base)), Some(UpdateKind::Server));
        // The app changing too doesn't make a server update cost more.
        assert_eq!(with(|c| { c.server = "s2".into(); c.app = "a2".into() }).kind_from(Some(&base)), Some(UpdateKind::Server));
        assert_eq!(with(|c| c.agent = "g2".into()).kind_from(Some(&base)), Some(UpdateKind::Agent));
        assert_eq!(with(|c| c.protocol = 6).kind_from(Some(&base)), Some(UpdateKind::Agent));
        let keeper = with(|c| c.keeper = "k2".into());
        assert_eq!(keeper.kind_from(Some(&base)), Some(UpdateKind::Agent));
        assert!(keeper.keeper_changed(Some(&base)) && !base.keeper_changed(Some(&base)));
        // Not knowing what runs: the most careful.
        assert_eq!(base.kind_from(None), Some(UpdateKind::Agent));
    }
}
