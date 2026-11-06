//! Shared protocol types for Space.
//!
//! * [`manifest`] — the `space-plugin.toml` plugin manifest.
//! * [`ext`] — Space-specific ACP extension methods (`_space/*`) and payloads.
//! * [`acp`] — re-export of the [`agent_client_protocol`] crate, so downstream crates
//!   use exactly the ACP version Space was built against.

pub mod ext;
pub mod manifest;

/// Re-export of the Agent Client Protocol SDK.
pub use agent_client_protocol as acp;
/// ACP v1 message types (`InitializeRequest`, `SessionNotification`, ...).
pub use agent_client_protocol::v1::schema as acp_schema;

pub use ext::{ConfigureSession, ModelInfo, SpaceStatus};
pub use manifest::{Manifest, ManifestError, PluginKind, RunConfig, RunCwd, MANIFEST_FILE};
