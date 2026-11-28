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
}

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
