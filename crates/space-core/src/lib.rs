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
pub mod grants;
mod host;
pub mod process;
pub mod secrets;
pub mod session;
pub mod smoke;
pub mod stub_script;

pub use discovery::{DiscoveredPlugin, DiscoveryOptions, DiscoveryReport, PluginSource};
pub use grants::Approval;
pub use host::{
    AgentHandle, EventReceiver, GRANTS_FILE, HELD_RECHECK, Host, HostEvent, HostOptions, PluginStatus, Session, SettingError, SettingState,
    TITLE_TIMEOUT, ThreadDirs, ThreadSave,
};
pub use secrets::Secrets;
pub use session::{
    ImageRef, PermissionDecision, PermissionKind, PermissionOption, PermissionRequest, PlanEntry, SessionEvent, SessionId,
    SessionState, ToolCallInfo, ToolCallPatch, ToolStatus, TranscriptItem,
};
pub use space_protocol as protocol;
