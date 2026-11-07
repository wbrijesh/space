//! Shared protocol types for Space.
//!
//! * [`manifest`] — the `space-plugin.toml` plugin manifest.
//! * [`ext`] — Space-specific ACP extension methods (`_space/*`) and payloads.
//! * [`service`] — the JSON-RPC protocol between the host and service plugins.
//!
//! These Rust types are the only definition of the protocol. TypeScript plugins use bindings
//! generated from them (`plugins/pi-agent/src/generated/protocol.ts`; `just protocol`
//! regenerates it, and a test fails while it is stale).
//! * [`acp`] — re-export of the [`agent_client_protocol`] crate, so downstream crates
//!   use exactly the ACP version Space was built against.

pub mod ext;
pub mod manifest;
pub mod service;
mod typescript;

/// Version of Space's own protocol: the `_space/*` ACP extensions and the service-plugin
/// protocol. Plugins report the version they were built for when they start, and a plugin on
/// another version is refused. Bump it on any incompatible change to either.
pub const PROTOCOL_VERSION: u32 = 1;

/// Re-export of the Agent Client Protocol SDK.
pub use agent_client_protocol as acp;
/// ACP v1 message types (`InitializeRequest`, `SessionNotification`, ...).
pub use agent_client_protocol::v1::schema as acp_schema;

pub use ext::{
    Confine, ConfineResult, ConfigureSession, ContextSection, ModelInfo, PreExecute, PreExecuteResult, SpaceStatus,
    Verdict,
};
pub use manifest::{
    CAP_AGENT, CAP_CONTEXT, CAP_SANDBOX, CAP_SESSION_STORE, CAP_SETTINGS, CATALOG_PERMISSION_MODES, CATALOG_PREFIX, EVENTS,
    HOOK_TOOLS_PRE_EXECUTE, HookSpec, MANIFEST_FILE, Manifest, ManifestError, RunConfig, RunCwd, USE_READ_TRANSCRIPT,
};
