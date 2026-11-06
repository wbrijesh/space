//! Space plugin host (no UI dependencies).
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use space_core::{Host, HostOptions};
//! let (host, mut events) = Host::new(tokio::runtime::Handle::current(), HostOptions::for_project("."));
//! let agent = host.start_agent("pi-agent").await?;
//! let session = agent.new_session(".").await?;
//! tokio::spawn(async move { while let Some(e) = events.recv().await { println!("{e:?}"); } });
//! let stop_reason = session.prompt("Reply with exactly: pong").await?;
//! # Ok(()) }
//! ```

mod convert;
pub mod discovery;
mod host;
pub mod process;
pub mod session;
pub mod smoke;

pub use discovery::{DiscoveredPlugin, DiscoveryOptions, DiscoveryReport, PluginSource};
pub use host::{AgentHandle, EventReceiver, Host, HostEvent, HostOptions, PluginStatus, Session};
pub use session::{
    PermissionDecision, PermissionKind, PermissionOption, PermissionRequest, SessionEvent, SessionId,
    SessionState, ToolCallInfo, ToolCallPatch, ToolStatus, TranscriptItem,
};
pub use space_protocol as protocol;
