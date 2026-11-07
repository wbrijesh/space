//! Shared protocol types for Space.
//!
//! * [`manifest`] — the `space-plugin.toml` plugin manifest.
//! * [`ext`] — Space-specific ACP extension methods (`_space/*`) and payloads.
//! * [`service`] — the JSON-RPC protocol between the host and service plugins.
//! * [`acp`] — re-export of the [`agent_client_protocol`] crate, so downstream crates
//!   use exactly the ACP version Space was built against.

pub mod ext;
pub mod manifest;
pub mod service;

/// Re-export of the Agent Client Protocol SDK.
pub use agent_client_protocol as acp;
/// ACP v1 message types (`InitializeRequest`, `SessionNotification`, ...).
pub use agent_client_protocol::v1::schema as acp_schema;

pub use ext::{
    Confine, ConfineResult, ConfigureSession, ContextSection, ModelInfo, PreExecute, PreExecuteResult, SpaceStatus,
    Verdict,
};
pub use manifest::{
    CAP_AGENT, CAP_CONTEXT, CAP_SANDBOX, CAP_SETTINGS, CATALOG_PERMISSION_MODES, CATALOG_PREFIX, HOOK_TOOLS_PRE_EXECUTE,
    HookSpec, MANIFEST_FILE, Manifest, ManifestError, RunConfig, RunCwd,
};
