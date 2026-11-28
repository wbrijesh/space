//! What the server and the keeper say to each other: one JSON object per line over the keeper's
//! Unix socket. Agent traffic travels inside as raw JSON-RPC lines (`line`), untouched except for
//! the ids the keeper translates ([`crate::daemon`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bumped when a change to these messages would confuse the other side. A server that speaks
/// another version is told so and disconnected; it must not half-work.
pub const KEEPER_PROTOCOL: u32 = 1;

/// Method of the notification the keeper turns a reply into when the server that sent the request
/// is gone (a turn's `session/prompt` that ran across a server restart).
pub const ORPHAN_REPLY: &str = "_keeper/orphanReply";

/// From the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToKeeper {
    /// First message of every connection.
    Hello { protocol: u32 },
    /// Start a process the keeper will hold. `agent` is the server's name for it, unique among
    /// living processes.
    Spawn { agent: String, spec: SpawnSpec },
    /// A JSON-RPC line for the agent.
    Send { agent: String, line: String },
    /// Every line from `agent` numbered `seq` or lower has been received.
    Ack { agent: String, seq: u64 },
    /// Stop the agent, or forget one that already ended.
    Kill { agent: String },
    /// Stop every agent and the keeper.
    Quit,
    /// Run this server from now on (an update): the keeper starts it once the current one exits,
    /// and goes back to the current one if the new one keeps failing.
    ReplaceServer { server: ServerCommand },
}

/// How the keeper starts the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerCommand {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

/// How to start a process: everything resolved by the server (the keeper knows no manifests).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment (the keeper's own isn't passed on).
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// From the keeper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromKeeper {
    /// The answer to [`ToKeeper::Hello`]: what the keeper holds.
    Welcome {
        protocol: u32,
        pid: u32,
        agents: Vec<AgentInfo>,
        /// The server it runs, and the one before it (kept to go back to).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<ServerCommand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_server: Option<ServerCommand>,
    },
    /// The server speaks another protocol; the keeper hangs up after this.
    Refused { reason: String },
    Spawned { agent: String, pid: u32 },
    SpawnFailed { agent: String, error: String },
    /// A JSON-RPC line from the agent. `seq` is set on the agent's own lines (acknowledge
    /// them); a line the keeper answers itself (`initialize` from its cache) has none.
    Recv {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        line: String,
    },
    /// A line the agent wrote to stderr.
    Stderr { agent: String, line: String },
    /// The agent ended (`code`: `None` when killed by a signal).
    Exited { agent: String, code: Option<i32> },
}

/// One process the keeper holds, as a new server learns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent: String,
    pub pid: u32,
    /// `None` while it runs; its exit once it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited: Option<Exit>,
    /// Requests an earlier server sent that the agent hasn't answered yet (each running turn's
    /// `session/prompt`). Their replies arrive as [`ORPHAN_REPLY`] notifications.
    #[serde(default)]
    pub open: Vec<OpenRequest>,
    /// Its last stderr lines.
    #[serde(default)]
    pub stderr: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    pub code: Option<i32>,
}

/// A request an earlier server sent that's still open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub method: String,
    /// `params.sessionId`, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sessionId")]
    pub session_id: Option<String>,
}

/// The params of an [`ORPHAN_REPLY`] notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrphanReply {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}
