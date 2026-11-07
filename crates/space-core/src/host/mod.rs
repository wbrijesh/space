//! The plugin host: registry, agent and service processes, ACP client connections, sessions,
//! and the tool pipeline.
//!
//! All async methods run their work on the tokio runtime handed to [`Host::new`], so they
//! can be awaited from any executor (e.g. GPUI's). Everything observable is reported as
//! [`HostEvent`]s on the receiver returned by [`Host::new`].
//!
//! **Agents** (`provides = ["agent"]`) speak ACP. **Service plugins** (everything else) speak
//! the plain JSON-RPC protocol of [`space_protocol::service`]; they start with
//! [`Host::start_services`] and are restarted on next use if they exit. The host routes by
//! capability, not by plugin id:
//!
//! * `_space/tools/preExecute` from an agent runs the `tools/pre-execute` hook chain (lowest
//!   `order` first). `deny` wins; `ask` is remembered and the user is asked after the last
//!   listener; `allow`/`next` pass on. A listener that answers `unavailable`, fails, times out
//!   or isn't running **holds** the call (fail closed): the agent waits, and the whole chain
//!   runs again when a listener sends `hooks/ready`, a service (re)starts, the session's mode
//!   changes, or every [`HostOptions::held_recheck`]; stopping the turn ends the wait.
//! * `required` service plugins can't be turned off.
//! * `_space/sandbox/confine` goes to the `sandbox` provider (none: the argv is unchanged; a
//!   failing provider: an error, so the agent doesn't spawn).
//! * Before each prompt, every `context` provider adds hidden sections (`_meta`).
//! * Catalogs (`catalog:<name>`) and settings (`settings`) are cached and reported as events.
//!
//! Files: [`registry`] (who is discovered, turned off, provides what), [`services`] (service
//! processes, settings, catalogs, seams), [`pipeline`] (the hook chain and held calls),
//! [`agents`] (ACP connections and sessions).

mod agents;
mod pipeline;
mod registry;
mod services;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use space_protocol::PROTOCOL_VERSION;
use space_protocol::service::{CatalogEntry, SettingField, SettingValues};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::discovery::{DiscoveredPlugin, DiscoveryOptions};
use crate::process::ExitKind;
use crate::secrets::Secrets;
use crate::session::{SessionEvent, SessionId};

pub use agents::{AgentHandle, Session};
use pipeline::SessionMeta;
use services::Service;

/// Lifecycle of a plugin process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    Stopped,
    Starting,
    Running,
    /// Exited unexpectedly or failed to start.
    Crashed(String),
    /// Space won't start it, and says why (e.g. it was built for another protocol version).
    NotStarted(String),
}

impl PluginStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PluginStatus::Stopped => "stopped",
            PluginStatus::Starting => "starting",
            PluginStatus::Running => "running",
            PluginStatus::Crashed(_) => "crashed",
            PluginStatus::NotStarted(_) => "not started",
        }
    }
}

/// Everything the host reports.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    PluginStatus { plugin_id: String, status: PluginStatus },
    /// One stderr line from a plugin.
    PluginLog { plugin_id: String, line: String },
    Session { session_id: SessionId, event: SessionEvent },
    /// A catalog's current entries (empty once its provider is disabled).
    Catalog { name: String, entries: Vec<CatalogEntry> },
    /// A service plugin's settings, and which of them are set.
    PluginSettings { plugin_id: String, settings: Vec<SettingState> },
}

/// One plugin setting as the Settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingState {
    pub field: SettingField,
    pub set: bool,
    /// The value of a text setting (secrets are never handed out).
    pub value: Option<String>,
}

pub type EventReceiver = mpsc::UnboundedReceiver<HostEvent>;

#[derive(Debug, Clone)]
pub struct HostOptions {
    /// Default project directory (used for `cwd = "project"` and project plugins).
    pub project_dir: PathBuf,
    pub discovery: DiscoveryOptions,
    /// Where plugin secrets live.
    pub secrets: Secrets,
    /// A held tool call runs the hook chain again at least this often (in case no listener
    /// says when it's ready, e.g. a crashed plugin to restart).
    pub held_recheck: Duration,
}

/// Default of [`HostOptions::held_recheck`].
pub const HELD_RECHECK: Duration = Duration::from_secs(10);

impl HostOptions {
    /// Standard discovery locations for `project_dir`.
    pub fn for_project(project_dir: impl Into<PathBuf>) -> Self {
        let project_dir = project_dir.into();
        Self {
            discovery: DiscoveryOptions::standard(Some(&project_dir)),
            project_dir,
            secrets: Secrets::default(),
            held_recheck: HELD_RECHECK,
        }
    }
}

/// Why a plugin setting wasn't saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    /// The plugin rejected the value; nothing was stored.
    #[error("{0}")]
    Invalid(String),
    /// The plugin couldn't check the value right now; save again with `force` to keep it.
    #[error("{0}")]
    Unverifiable(String),
    #[error("{0}")]
    Failed(String),
}

/// The plugin host. Cheap to clone.
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

struct HostInner {
    rt: Handle,
    events: mpsc::UnboundedSender<HostEvent>,
    options: HostOptions,
    plugins: Mutex<Vec<DiscoveredPlugin>>,
    /// Plugin ids turned off in Settings.
    disabled: Mutex<HashSet<String>>,
    /// Running service plugins by id.
    services: Mutex<HashMap<String, Arc<Service>>>,
    /// When each service was last (re)started, for the restart backoff.
    started_at: Mutex<HashMap<String, Instant>>,
    /// One start at a time per service: concurrent callers wait for it instead of failing.
    start_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Text settings by plugin id (the app persists them; secrets live in the Keychain).
    text_settings: Mutex<HashMap<String, SettingValues>>,
    /// Catalog entries by catalog name.
    catalogs: Mutex<HashMap<String, Vec<CatalogEntry>>>,
    /// What the pipeline knows of each session.
    sessions: Mutex<HashMap<SessionId, SessionMeta>>,
    /// Recent crash times by service id.
    crashes: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// The last status reported for each plugin.
    statuses: Mutex<HashMap<String, PluginStatus>>,
}

impl Host {
    /// Create a host whose background work runs on `rt`. Runs an initial discovery.
    pub fn new(rt: Handle, options: HostOptions) -> (Self, EventReceiver) {
        let (events, rx) = mpsc::unbounded_channel();
        let host = Host {
            inner: Arc::new(HostInner {
                rt,
                events,
                options,
                plugins: Mutex::new(Vec::new()),
                disabled: Mutex::new(HashSet::new()),
                services: Mutex::new(HashMap::new()),
                started_at: Mutex::new(HashMap::new()),
                start_locks: Mutex::new(HashMap::new()),
                text_settings: Mutex::new(HashMap::new()),
                catalogs: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                crashes: Mutex::new(HashMap::new()),
                statuses: Mutex::new(HashMap::new()),
            }),
        };
        host.discover();
        (host, rx)
    }

    pub fn project_dir(&self) -> &Path {
        &self.inner.options.project_dir
    }
}

impl HostInner {
    fn emit(&self, event: HostEvent) {
        let _ = self.events.send(event);
    }

    fn emit_status(&self, plugin_id: &str, status: PluginStatus) {
        self.statuses.lock().unwrap().insert(plugin_id.to_string(), status.clone());
        self.emit(HostEvent::PluginStatus { plugin_id: plugin_id.to_string(), status });
    }

    /// A plugin process ended. Killing a plugin that failed to start or was refused doesn't
    /// hide why.
    fn emit_exit(&self, plugin_id: &str, status: PluginStatus) {
        let current = self.statuses.lock().unwrap().get(plugin_id).cloned();
        if status == PluginStatus::Stopped && matches!(current, Some(PluginStatus::Crashed(_) | PluginStatus::NotStarted(_))) {
            return;
        }
        self.emit_status(plugin_id, status);
    }

    fn emit_session(&self, session_id: &str, event: SessionEvent) {
        self.emit(HostEvent::Session { session_id: session_id.to_string(), event });
    }
}

/// Why Space won't run a plugin: its status becomes [`PluginStatus::NotStarted`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Refused(String);

/// The status for a plugin that failed to start.
fn start_failure(e: &anyhow::Error) -> PluginStatus {
    match e.downcast_ref::<Refused>() {
        Some(r) => PluginStatus::NotStarted(r.0.clone()),
        None => PluginStatus::Crashed(format!("{e:#}")),
    }
}

/// Refuse a plugin built for another [`PROTOCOL_VERSION`]. `None`: it didn't say.
fn check_protocol(version: Option<u32>) -> Result<(), Refused> {
    match version {
        Some(PROTOCOL_VERSION) => Ok(()),
        Some(v) => Err(Refused(format!("Built for Space protocol v{v}; this Space speaks v{PROTOCOL_VERSION}."))),
        None => Err(Refused(format!("It doesn't say which Space protocol it speaks (this Space speaks v{PROTOCOL_VERSION})."))),
    }
}

fn exit_status(kind: ExitKind, tail: &[String]) -> PluginStatus {
    match kind {
        ExitKind::Killed | ExitKind::Exited(Some(0)) => PluginStatus::Stopped,
        ExitKind::Exited(code) => {
            let code = code.map_or("a signal".to_string(), |c| format!("code {c}"));
            let last = tail.last().map(|l| format!(": {l}")).unwrap_or_default();
            PluginStatus::Crashed(format!("exited with {code}{last}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_versions() {
        assert!(check_protocol(Some(PROTOCOL_VERSION)).is_ok());
        let other = check_protocol(Some(PROTOCOL_VERSION + 1)).unwrap_err();
        assert!(other.0.contains(&format!("v{}", PROTOCOL_VERSION + 1)));
        assert!(check_protocol(None).is_err());
        let refused: anyhow::Error = other.into();
        assert!(matches!(start_failure(&refused), PluginStatus::NotStarted(_)));
        assert!(matches!(start_failure(&anyhow::anyhow!("boom")), PluginStatus::Crashed(_)));
    }
}
