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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_plugin::rpc::{self, Peer, RpcError};
use space_protocol::acp::{self, Agent, ByteStreams, Client};
use space_protocol::acp_schema as schema;
use space_protocol::ext::CONTEXT_META_KEY;
use space_protocol::service::{self as svc, CatalogEntry, HistoryItem, SettingField, SettingKind, SettingValues};
use space_protocol::{
    CAP_CONTEXT, CAP_SANDBOX, CAP_SETTINGS, CATALOG_PERMISSION_MODES, Confine, ConfineResult, ConfigureSession,
    ContextSection, HOOK_TOOLS_PRE_EXECUTE, PreExecute, PreExecuteResult, SpaceStatus, Verdict,
};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::convert;
use crate::discovery::{self, DiscoveredPlugin, DiscoveryOptions, DiscoveryReport};
use crate::process::{self, ExitKind, PluginProcess};
use crate::secrets::Secrets;
use crate::session::{
    PermissionDecision, PermissionKind, PermissionOption, PermissionRequest, SessionEvent, SessionId, ToolCallPatch,
};

/// How long one hook listener may take before the call is held. Generous: a listener may
/// retry a slow network call within it.
const HOOK_TIMEOUT: Duration = Duration::from_secs(15);
/// `settings/validate` may reach the network.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(20);
/// This many crashes within [`CRASH_WINDOW`] and a held call says the plugin keeps crashing.
const CRASH_LIMIT: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(300);
/// Other service calls (confine, context, catalogs, settings).
const SERVICE_TIMEOUT: Duration = Duration::from_secs(5);
/// A service that exited is restarted on next use, at most this often.
const RESTART_BACKOFF: Duration = Duration::from_secs(3);
/// Thread items kept for hook listeners.
const MAX_HISTORY: usize = 200;
/// Option ids of the approval card the pipeline shows.
const ASK_ALLOW: &str = "allow";
const ASK_REJECT: &str = "reject";

type Conn = acp::v1::ConnectionTo<Agent>;

/// Lifecycle of a plugin process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    Stopped,
    Starting,
    Running,
    /// Exited unexpectedly or failed to start.
    Crashed(String),
}

impl PluginStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PluginStatus::Stopped => "stopped",
            PluginStatus::Starting => "starting",
            PluginStatus::Running => "running",
            PluginStatus::Crashed(_) => "crashed",
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
}

/// A running service plugin.
struct Service {
    plugin: DiscoveredPlugin,
    peer: Peer,
    _process: PluginProcess,
    dead: Arc<AtomicBool>,
    fields: Vec<SettingField>,
}

impl Service {
    fn alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst) && !self.peer.is_closed()
    }
}

/// Per-session facts for hook listeners.
struct SessionMeta {
    cwd: PathBuf,
    /// The permission mode chosen for the session (`None`: the catalog's default).
    mode: Option<String>,
    history: VecDeque<HistoryItem>,
    signals: HoldSignals,
}

/// What a held call of a session waits on.
#[derive(Clone, Default)]
struct HoldSignals {
    /// Run the chain again now.
    wake: Arc<tokio::sync::Notify>,
    /// Bumped when the user stops the turn: held calls give up.
    stops: Arc<AtomicU64>,
}

impl SessionMeta {
    fn new(cwd: PathBuf) -> Self {
        Self { cwd, mode: None, history: VecDeque::new(), signals: HoldSignals::default() }
    }
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
            }),
        };
        host.discover();
        (host, rx)
    }

    pub fn project_dir(&self) -> &Path {
        &self.inner.options.project_dir
    }

    /// Re-scan plugin directories, replacing the registry. Invalid manifests are returned
    /// in the report (and logged) rather than failing discovery.
    pub fn discover(&self) -> DiscoveryReport {
        let report = discovery::discover(&self.inner.options.discovery);
        for e in &report.errors {
            tracing::warn!("{e}");
        }
        *self.inner.plugins.lock().unwrap() = report.plugins.clone();
        report
    }

    pub fn plugins(&self) -> Vec<DiscoveredPlugin> {
        self.inner.plugins.lock().unwrap().clone()
    }

    pub fn plugin(&self, id: &str) -> Option<DiscoveredPlugin> {
        self.inner.plugins.lock().unwrap().iter().find(|p| p.id() == id).cloned()
    }

    /// Service plugins (not agents), in discovery order.
    pub fn service_plugins(&self) -> Vec<DiscoveredPlugin> {
        self.plugins().into_iter().filter(|p| !p.manifest.is_agent()).collect()
    }

    /// Turn plugins off (by id); every other plugin is on. Disabled services stop at once,
    /// and their hooks, seams, catalogs and context go with them. `required` plugins stay on.
    pub fn set_disabled(&self, ids: impl IntoIterator<Item = String>) {
        let required: HashSet<String> =
            self.plugins().into_iter().filter(|p| p.manifest.required).map(|p| p.id().to_string()).collect();
        let ids: HashSet<String> = ids.into_iter().filter(|id| !required.contains(id)).collect();
        *self.inner.disabled.lock().unwrap() = ids.clone();
        for id in ids {
            self.inner.stop_service(&id);
        }
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        self.inner.disabled.lock().unwrap().contains(plugin_id)
    }

    /// Start every enabled service plugin that isn't running. Failures are reported as
    /// [`HostEvent::PluginStatus`] and don't stop the others.
    pub async fn start_services(&self) {
        let inner = self.inner.clone();
        let _ = self
            .inner
            .rt
            .spawn(async move {
                let ids: Vec<String> = inner.enabled_services().into_iter().map(|p| p.id().to_string()).collect();
                futures::future::join_all(ids.iter().map(|id| inner.service(id))).await;
            })
            .await;
    }

    /// Stop and start a service plugin again.
    pub async fn restart_service(&self, plugin_id: &str) -> Result<()> {
        self.inner.stop_service(plugin_id);
        self.inner.started_at.lock().unwrap().remove(plugin_id);
        let inner = self.inner.clone();
        let id = plugin_id.to_string();
        self.inner.rt.spawn(async move { inner.service(&id).await.map(|_| ()) }).await?.map_err(|e| anyhow!(e))
    }

    /// "Retry now" on a blocker of `plugin_id`: tell it (starting it first if it isn't
    /// running), and run held calls through the chain again.
    pub async fn retry_plugin(&self, plugin_id: &str) {
        let inner = self.inner.clone();
        let id = plugin_id.to_string();
        let _ = self
            .inner
            .rt
            .spawn(async move {
                inner.started_at.lock().unwrap().remove(&id);
                match inner.service(&id).await {
                    Ok(service) => service.peer.notify(svc::PLUGIN_RETRY, serde_json::json!({})),
                    Err(e) => tracing::warn!("retry `{id}`: {e}"),
                }
                inner.wake_held();
            })
            .await;
    }

    /// The entries of catalog `name` (empty until its provider has started).
    pub fn catalog(&self, name: &str) -> Vec<CatalogEntry> {
        self.inner.catalogs.lock().unwrap().get(name).cloned().unwrap_or_default()
    }

    /// Text settings the app saved for `plugin_id` (call before [`Host::start_services`]).
    pub fn set_text_settings(&self, plugin_id: &str, values: SettingValues) {
        self.inner.text_settings.lock().unwrap().insert(plugin_id.to_string(), values);
    }

    /// The settings of a running service plugin, and which are set.
    pub fn plugin_settings(&self, plugin_id: &str) -> Vec<SettingState> {
        let service = self.inner.services.lock().unwrap().get(plugin_id).cloned();
        service.map(|s| self.inner.setting_states(&s)).unwrap_or_default()
    }

    /// Change one setting of a service plugin (`None` clears it). A new value is first checked
    /// by the plugin (`settings/validate`): an invalid one isn't stored, and one that couldn't
    /// be checked is stored only with `force`. Secrets go to the Keychain; a text value is kept
    /// for the plugin (the app persists it). The plugin is told every setting's new value.
    pub async fn set_setting(
        &self,
        plugin_id: &str,
        key: &str,
        value: Option<String>,
        force: bool,
    ) -> Result<(), SettingError> {
        let inner = self.inner.clone();
        let (plugin_id, key) = (plugin_id.to_string(), key.to_string());
        let failed = |e: anyhow::Error| SettingError::Failed(format!("{e:#}"));
        self.inner
            .rt
            .spawn(async move {
                let service = inner.service(&plugin_id).await.map_err(SettingError::Failed)?;
                let field = service
                    .fields
                    .iter()
                    .find(|f| f.key == key)
                    .ok_or_else(|| SettingError::Failed(format!("`{plugin_id}` has no setting `{key}`")))?;
                let value = value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
                if let Some(v) = &value {
                    let check = svc::SettingCheck { key: key.clone(), value: v.clone() };
                    match service.peer.call::<_, svc::SettingVerdict>(svc::SETTINGS_VALIDATE, &check, Some(VALIDATE_TIMEOUT)).await {
                        Ok(verdict) => match verdict.status {
                            svc::SettingStatus::Ok => {}
                            svc::SettingStatus::Invalid => {
                                return Err(SettingError::Invalid(verdict.message.unwrap_or_else(|| "Not accepted.".into())));
                            }
                            svc::SettingStatus::Unverifiable if !force => {
                                let message = verdict.message.unwrap_or_else(|| "It couldn't be checked right now.".into());
                                return Err(SettingError::Unverifiable(message));
                            }
                            svc::SettingStatus::Unverifiable => {}
                        },
                        // A plugin that doesn't validate accepts everything.
                        Err(e) if e.code == RpcError::METHOD_NOT_FOUND => {}
                        Err(e) if force => tracing::warn!("validating `{plugin_id}`.{key}: {e}"),
                        Err(e) => return Err(SettingError::Unverifiable(format!("It couldn't be checked ({}).", e.message))),
                    }
                }
                match field.kind {
                    SettingKind::Secret => {
                        let secrets = inner.options.secrets.clone();
                        let (id, k, v) = (plugin_id.clone(), key.clone(), value.clone());
                        tokio::task::spawn_blocking(move || secrets.set(&id, &k, v.as_deref()))
                            .await
                            .map_err(|e| failed(anyhow!(e)))?
                            .map_err(failed)?;
                    }
                    SettingKind::Text => {
                        let mut text = inner.text_settings.lock().unwrap();
                        let map = text.entry(plugin_id.clone()).or_default();
                        match value {
                            Some(v) => map.insert(key.clone(), v),
                            None => map.remove(&key),
                        };
                    }
                }
                let (values, unreadable) = inner.setting_values(&service).await;
                service.peer.notify(svc::SETTINGS_CHANGED, svc::SettingsChanged { values, unreadable });
                inner.emit(HostEvent::PluginSettings {
                    plugin_id: plugin_id.clone(),
                    settings: inner.setting_states(&service),
                });
                Ok(())
            })
            .await
            .map_err(|e| failed(anyhow!(e)))?
    }

    /// Spawn an `agent` plugin and complete the ACP `initialize` handshake.
    pub async fn start_agent(&self, plugin_id: &str) -> Result<AgentHandle> {
        let plugin = self.plugin(plugin_id).with_context(|| format!("unknown plugin `{plugin_id}`"))?;
        if !plugin.manifest.is_agent() {
            bail!("plugin `{plugin_id}` is a service plugin, not an agent");
        }
        let inner = self.inner.clone();
        let id = plugin_id.to_string();
        inner.emit_status(&id, PluginStatus::Starting);
        let result = self.inner.rt.spawn(start_agent(inner.clone(), plugin)).await;
        let result = result.map_err(|e| anyhow!("agent start task failed: {e}")).and_then(|r| r);
        match &result {
            Ok(_) => inner.emit_status(&id, PluginStatus::Running),
            Err(e) => inner.emit_status(&id, PluginStatus::Crashed(format!("{e:#}"))),
        }
        result
    }
}

impl HostInner {
    fn emit(&self, event: HostEvent) {
        let _ = self.events.send(event);
    }

    fn emit_status(&self, plugin_id: &str, status: PluginStatus) {
        self.emit(HostEvent::PluginStatus { plugin_id: plugin_id.to_string(), status });
    }

    fn emit_session(&self, session_id: &str, event: SessionEvent) {
        self.emit(HostEvent::Session { session_id: session_id.to_string(), event });
    }

    fn enabled_services(&self) -> Vec<DiscoveredPlugin> {
        let disabled = self.disabled.lock().unwrap().clone();
        let plugins = self.plugins.lock().unwrap();
        plugins.iter().filter(|p| !p.manifest.is_agent() && !disabled.contains(p.id())).cloned().collect()
    }

    /// Enabled services listening to `event`, in chain order.
    fn hook_listeners(&self, event: &str) -> Vec<DiscoveredPlugin> {
        let mut listeners: Vec<(i32, DiscoveredPlugin)> = self
            .enabled_services()
            .into_iter()
            .filter_map(|p| p.manifest.hook_order(event).map(|o| (o, p)))
            .collect();
        listeners.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id().cmp(b.1.id())));
        listeners.into_iter().map(|(_, p)| p).collect()
    }

    /// The enabled services providing `capability`, in discovery order.
    fn providers(&self, capability: &str) -> Vec<DiscoveredPlugin> {
        self.enabled_services().into_iter().filter(|p| p.manifest.provides(capability)).collect()
    }

    fn stop_service(&self, plugin_id: &str) {
        let Some(service) = self.services.lock().unwrap().remove(plugin_id) else { return };
        service._process.kill();
        let mut catalogs = self.catalogs.lock().unwrap();
        for name in service.plugin.manifest.catalogs() {
            if catalogs.remove(name).is_some() {
                self.emit(HostEvent::Catalog { name: name.to_string(), entries: Vec::new() });
            }
        }
    }

    /// The running service `plugin_id`, started (or restarted) first if needed.
    async fn service(self: &Arc<Self>, plugin_id: &str) -> Result<Arc<Service>, String> {
        if let Some(s) = self.services.lock().unwrap().get(plugin_id).filter(|s| s.alive()) {
            return Ok(s.clone());
        }
        let lock = self.start_locks.lock().unwrap().entry(plugin_id.to_string()).or_default().clone();
        let _starting = lock.lock().await;
        // Started by whoever held the lock before us?
        if let Some(s) = self.services.lock().unwrap().get(plugin_id).filter(|s| s.alive()) {
            return Ok(s.clone());
        }
        if self.disabled.lock().unwrap().contains(plugin_id) {
            return Err(format!("`{plugin_id}` is turned off"));
        }
        let plugin = self
            .plugins
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id() == plugin_id && !p.manifest.is_agent())
            .cloned()
            .ok_or_else(|| format!("no service plugin `{plugin_id}`"))?;
        {
            let mut started = self.started_at.lock().unwrap();
            if started.get(plugin_id).is_some_and(|t| t.elapsed() < RESTART_BACKOFF) {
                return Err(format!("`{plugin_id}` isn't running (it exited and was restarted moments ago)"));
            }
            started.insert(plugin_id.to_string(), Instant::now());
        }
        self.emit_status(plugin_id, PluginStatus::Starting);
        match self.start_service(plugin).await {
            Ok(service) => {
                self.emit_status(plugin_id, PluginStatus::Running);
                Ok(service)
            }
            Err(e) => {
                let message = format!("{e:#}");
                self.emit_status(plugin_id, PluginStatus::Crashed(message.clone()));
                Err(message)
            }
        }
    }

    async fn start_service(self: &Arc<Self>, plugin: DiscoveredPlugin) -> Result<Arc<Service>> {
        let plugin_id = plugin.id().to_string();
        let dead = Arc::new(AtomicBool::new(false));
        let spawned = {
            let weak = Arc::downgrade(self);
            let exit_weak = weak.clone();
            let (log_id, exit_id, exit_dead) = (plugin_id.clone(), plugin_id.clone(), dead.clone());
            process::spawn(
                &plugin,
                &self.options.project_dir,
                move |line| {
                    if let Some(host) = weak.upgrade() {
                        host.emit(HostEvent::PluginLog { plugin_id: log_id.clone(), line });
                    }
                },
                move |kind, tail| {
                    exit_dead.store(true, Ordering::SeqCst);
                    if let Some(host) = exit_weak.upgrade() {
                        let status = exit_status(kind, &tail);
                        if matches!(status, PluginStatus::Crashed(_)) {
                            host.crashed(&exit_id);
                        }
                        host.emit_status(&exit_id, status);
                    }
                },
            )?
        };
        let on_request: rpc::RequestHandler =
            Arc::new(|method: String, _| Box::pin(async move { Err(RpcError::method_not_found(&method)) }));
        let on_notification: rpc::NotificationHandler = {
            let (weak, id) = (Arc::downgrade(self), plugin_id.clone());
            Arc::new(move |method: String, _params| {
                let Some(host) = weak.upgrade() else { return };
                match method.as_str() {
                    svc::CATALOG_CHANGED => {
                        let id = id.clone();
                        host.rt.clone().spawn(async move { host.refresh_catalogs(&id).await });
                    }
                    svc::HOOKS_READY => host.wake_held(),
                    _ => {}
                }
            })
        };
        let (peer, _) = Peer::spawn(spawned.stdout, spawned.stdin, on_request, on_notification);
        let process = spawned.process;
        let logs = process.logs.clone();
        let fail = |what: &str, e: RpcError| {
            let tail = logs.tail(5).join("\n");
            if tail.is_empty() { anyhow!("{what} failed: {e}") } else { anyhow!("{what} failed: {e}\nplugin stderr:\n{tail}") }
        };
        let fields = if plugin.manifest.provides(CAP_SETTINGS) {
            let schema: svc::SettingsSchema = peer
                .call(svc::SETTINGS_SCHEMA, &serde_json::json!({}), Some(SERVICE_TIMEOUT))
                .await
                .map_err(|e| fail(svc::SETTINGS_SCHEMA, e))?;
            schema.fields
        } else {
            Vec::new()
        };
        let mut service = Service { plugin, peer, _process: process, dead, fields };
        let (settings, unreadable) = self.setting_values(&service).await;
        let init = svc::Initialize { protocol_version: svc::PROTOCOL_VERSION, settings, unreadable };
        let _: svc::Initialized = service
            .peer
            .call(svc::INITIALIZE, &init, Some(SERVICE_TIMEOUT))
            .await
            .map_err(|e| fail(svc::INITIALIZE, e))?;
        service.fields.retain(|f| !f.key.is_empty());
        let service = Arc::new(service);
        self.services.lock().unwrap().insert(plugin_id.clone(), service.clone());
        self.refresh_catalogs(&plugin_id).await;
        if !service.fields.is_empty() {
            self.emit(HostEvent::PluginSettings { plugin_id, settings: self.setting_states(&service) });
        }
        // A (re)started listener may decide calls held while it wasn't running.
        self.wake_held();
        Ok(service)
    }

    /// Fetch the catalogs `plugin_id` publishes and report them.
    async fn refresh_catalogs(self: &Arc<Self>, plugin_id: &str) {
        let Some(service) = self.services.lock().unwrap().get(plugin_id).cloned() else { return };
        for name in service.plugin.manifest.catalogs() {
            let request = svc::CatalogName { name: name.to_string() };
            match service.peer.call::<_, svc::Catalog>(svc::CATALOG_GET, &request, Some(SERVICE_TIMEOUT)).await {
                Ok(catalog) => {
                    self.catalogs.lock().unwrap().insert(name.to_string(), catalog.entries.clone());
                    self.emit(HostEvent::Catalog { name: name.to_string(), entries: catalog.entries });
                }
                Err(e) => tracing::warn!("catalog {name} from `{plugin_id}`: {e}"),
            }
        }
    }

    /// Every set setting of `service` (text values, and secrets read from the Keychain), and
    /// the secrets that couldn't be read.
    async fn setting_values(&self, service: &Service) -> (SettingValues, Vec<String>) {
        let mut values = SettingValues::new();
        let mut unreadable = Vec::new();
        let text = self.text_settings.lock().unwrap().get(service.plugin.id()).cloned().unwrap_or_default();
        for field in &service.fields {
            let value = match field.kind {
                SettingKind::Text => text.get(&field.key).cloned(),
                SettingKind::Secret => {
                    let (secrets, id, key) = (self.options.secrets.clone(), service.plugin.id().to_string(), field.key.clone());
                    match tokio::task::spawn_blocking(move || secrets.get(&id, &key)).await {
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) => {
                            tracing::warn!("{e:#}");
                            unreadable.push(field.key.clone());
                            None
                        }
                        Err(_) => None,
                    }
                }
            };
            if let Some(v) = value {
                values.insert(field.key.clone(), v);
            }
        }
        (values, unreadable)
    }

    fn setting_states(&self, service: &Service) -> Vec<SettingState> {
        let text = self.text_settings.lock().unwrap().get(service.plugin.id()).cloned().unwrap_or_default();
        service
            .fields
            .iter()
            .map(|field| {
                let (set, value) = match field.kind {
                    SettingKind::Text => (text.contains_key(&field.key), text.get(&field.key).cloned()),
                    SettingKind::Secret => {
                        let set = self.options.secrets.get(service.plugin.id(), &field.key).ok().flatten().is_some();
                        (set, None)
                    }
                };
                SettingState { field: field.clone(), set, value }
            })
            .collect()
    }

    /// Call a service, starting it first if needed. Errors are plain messages.
    async fn call_service<P: Serialize, R: DeserializeOwned>(
        self: &Arc<Self>,
        plugin_id: &str,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, String> {
        let service = self.service(plugin_id).await?;
        service.peer.call(method, params, Some(timeout)).await.map_err(|e| e.message)
    }

    // ----- sessions and the pipeline ---------------------------------------------------

    fn crashed(&self, plugin_id: &str) {
        let mut crashes = self.crashes.lock().unwrap();
        let times = crashes.entry(plugin_id.to_string()).or_default();
        times.push_back(Instant::now());
        while times.front().is_some_and(|t| t.elapsed() > CRASH_WINDOW) || times.len() > CRASH_LIMIT {
            times.pop_front();
        }
    }

    fn keeps_crashing(&self, plugin_id: &str) -> bool {
        let crashes = self.crashes.lock().unwrap();
        crashes.get(plugin_id).is_some_and(|t| t.iter().filter(|t| t.elapsed() <= CRASH_WINDOW).count() >= CRASH_LIMIT)
    }

    /// Run every held call through the chain again.
    fn wake_held(&self) {
        for meta in self.sessions.lock().unwrap().values() {
            meta.signals.wake.notify_waiters();
        }
    }

    fn signals(&self, session_id: &str) -> HoldSignals {
        self.sessions.lock().unwrap().get(session_id).map(|m| m.signals.clone()).unwrap_or_default()
    }

    /// The session's mode (or the catalog default), project root and history.
    fn session_facts(&self, session_id: &str) -> (String, String, Vec<HistoryItem>) {
        let default_mode = || {
            let catalogs = self.catalogs.lock().unwrap();
            let entries = catalogs.get(CATALOG_PERMISSION_MODES).cloned().unwrap_or_default();
            entries.iter().find(|e| e.default).or(entries.first()).map(|e| e.id.clone()).unwrap_or_default()
        };
        let sessions = self.sessions.lock().unwrap();
        match sessions.get(session_id) {
            Some(meta) => (
                meta.mode.clone().unwrap_or_else(default_mode),
                meta.cwd.display().to_string(),
                meta.history.iter().cloned().collect(),
            ),
            None => (default_mode(), self.options.project_dir.display().to_string(), Vec::new()),
        }
    }

    fn record(&self, session_id: &str, item: HistoryItem) {
        if let Some(meta) = self.sessions.lock().unwrap().get_mut(session_id) {
            if meta.history.len() == MAX_HISTORY {
                meta.history.pop_front();
            }
            meta.history.push_back(item);
        }
    }

    /// Hidden sections from every `context` provider (a failing one is skipped).
    async fn context_sections(self: &Arc<Self>, session_id: &str) -> Vec<ContextSection> {
        let (mode, root, _) = self.session_facts(session_id);
        let request = svc::ContextRequest { session_id: session_id.to_string(), mode, project_root: root };
        let mut sections = Vec::new();
        for plugin in self.providers(CAP_CONTEXT) {
            match self.call_service::<_, svc::ContextSections>(plugin.id(), svc::CONTEXT_GET, &request, SERVICE_TIMEOUT).await
            {
                Ok(s) => sections.extend(s.sections),
                Err(e) => tracing::warn!("context from `{}`: {e}", plugin.id()),
            }
        }
        sections
    }

    /// `_space/sandbox/confine`: the `sandbox` provider's argv, or `argv` when there is none.
    async fn confine(self: &Arc<Self>, req: Confine) -> Result<Vec<String>, String> {
        let Some(plugin) = self.providers(CAP_SANDBOX).into_iter().next() else { return Ok(req.argv) };
        let (mode, root, _) = self.session_facts(&req.session_id);
        let request = svc::ConfineRequest { session_id: req.session_id, mode, project_root: root, argv: req.argv, cwd: req.cwd };
        let confined: svc::Confined = self
            .call_service(plugin.id(), svc::SANDBOX_CONFINE, &request, SERVICE_TIMEOUT)
            .await
            .map_err(|e| format!("{} couldn't prepare the command: {e}", plugin.manifest.name))?;
        if confined.argv.is_empty() {
            return Err(format!("{} returned an empty command", plugin.manifest.name));
        }
        Ok(confined.argv)
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

/// One pass through the `tools/pre-execute` chain.
enum Pass {
    Allow,
    Deny(String),
    /// Ask the user, with the listeners' reasons.
    Ask(Vec<String>),
    /// A listener can't decide right now: wait.
    Hold(String),
}

/// Every listener, in order. A deny wins over a hold, a hold over asks.
async fn run_chain(host: &Arc<HostInner>, hook: &svc::HookCall) -> Pass {
    let (mut asks, mut hold) = (Vec::new(), None);
    for plugin in host.hook_listeners(HOOK_TOOLS_PRE_EXECUTE) {
        let name = &plugin.manifest.name;
        match host.call_service::<_, svc::HookDecision>(plugin.id(), svc::HOOK_PRE_EXECUTE, hook, HOOK_TIMEOUT).await {
            Ok(d) => match d.decision {
                svc::HookVerdict::Deny => {
                    tracing::info!("{} {}: denied by {}", hook.call.tool, hook.call.id, plugin.id());
                    return Pass::Deny(d.reason.unwrap_or_else(|| format!("Blocked by {name}.")));
                }
                svc::HookVerdict::Ask => asks.push(d.reason.unwrap_or_else(|| format!("{name} wants you to check this call."))),
                svc::HookVerdict::Unavailable => {
                    hold.get_or_insert(d.reason.unwrap_or_else(|| format!("{name} can't check tool calls right now.")));
                }
                svc::HookVerdict::Allow | svc::HookVerdict::Next => {}
            },
            Err(e) => {
                let reason = if host.keeps_crashing(plugin.id()) {
                    format!("The {name} plugin keeps crashing, so tool calls can't be checked. See the logs.")
                } else {
                    format!("The {name} plugin couldn't check this call ({e}).")
                };
                hold.get_or_insert(reason);
            }
        }
    }
    match hold {
        Some(reason) => Pass::Hold(reason),
        None if asks.is_empty() => Pass::Allow,
        None => Pass::Ask(asks),
    }
}

/// `_space/tools/preExecute`: run the hook chain; hold the call while a listener can't decide;
/// ask the user if a listener said so.
async fn pre_execute(shared: Arc<Shared>, req: PreExecute) -> PreExecuteResult {
    let host = shared.host.clone();
    let session_id = req.session_id.clone();
    let title = call_title(&req.tool, &req.args);
    let signals = host.signals(&session_id);
    let stops = signals.stops.load(Ordering::SeqCst);
    let stopped = || signals.stops.load(Ordering::SeqCst) != stops;
    let held = |reason: Option<String>| {
        host.emit_session(&session_id, SessionEvent::ToolCallHeld { id: req.tool_call_id.clone(), title: title.clone(), reason })
    };
    let mut held_for: Option<String> = None;
    let (pass, call) = loop {
        // Listen before running the chain, so a wake-up during it isn't lost.
        let wake = signals.wake.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        // Facts are read on every pass: the mode may have changed while the call was held.
        let (mode, root, history) = host.session_facts(&session_id);
        let call = svc::ToolCallFacts {
            id: req.tool_call_id.clone(),
            tool: req.tool.clone(),
            kind: req.kind.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone().unwrap_or_else(|| root.clone()),
        };
        let hook = svc::HookCall { session_id: session_id.clone(), mode, project_root: root, call: call.clone(), history };
        let pass = run_chain(&host, &hook).await;
        let Pass::Hold(reason) = pass else { break (pass, call) };
        if stopped() {
            break (Pass::Hold(reason), call);
        }
        if held_for.as_ref() != Some(&reason) {
            tracing::info!("{} {}: held: {reason}", req.tool, req.tool_call_id);
            held(Some(reason.clone()));
            held_for = Some(reason);
        }
        tokio::select! {
            () = &mut wake => {}
            () = tokio::time::sleep(host.options.held_recheck) => {}
        }
        if stopped() {
            break (Pass::Hold(String::new()), call);
        }
    };
    if held_for.is_some() {
        held(None);
    }
    let record = |allowed: bool| {
        host.record(
            &session_id,
            HistoryItem::Tool { id: call.id.clone(), tool: call.tool.clone(), kind: call.kind.clone(), args: call.args.clone(), allowed },
        )
    };
    let block = |reason: String| {
        host.emit_session(&session_id, SessionEvent::ToolCallBlocked { id: req.tool_call_id.clone(), reason: reason.clone() });
        PreExecuteResult { decision: Verdict::Deny, reason: Some(reason) }
    };
    let asks = match pass {
        Pass::Allow => {
            record(true);
            return PreExecuteResult { decision: Verdict::Allow, reason: None };
        }
        Pass::Deny(reason) => {
            record(false);
            return block(reason);
        }
        // Only when the user stopped the turn while the call was held.
        Pass::Hold(_) => return PreExecuteResult { decision: Verdict::Deny, reason: Some("The turn was stopped.".into()) },
        Pass::Ask(asks) => asks,
    };
    let options = vec![
        PermissionOption { id: ASK_ALLOW.into(), name: "Allow once".into(), kind: PermissionKind::AllowOnce },
        PermissionOption { id: ASK_REJECT.into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
    ];
    let patch = ToolCallPatch { id: req.tool_call_id.clone(), title: Some(title.clone()), kind: Some(req.kind.clone()), ..Default::default() };
    let rx = shared.request_permission(&session_id, patch, options, Some(asks.join(" ")));
    let allowed = matches!(rx.await, Ok(PermissionDecision::Selected(id)) if id == ASK_ALLOW);
    record(allowed);
    if allowed { PreExecuteResult { decision: Verdict::Allow, reason: None } } else { block("The user didn't allow this call.".into()) }
}

/// `bash: ls -la`, `read: src/main.rs`.
fn call_title(tool: &str, args: &serde_json::Value) -> String {
    let detail = ["command", "path", "file_path", "pattern", "url"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "));
    match detail {
        Some(d) if !d.is_empty() => format!("{tool}: {}", d.chars().take(120).collect::<String>()),
        _ => tool.to_string(),
    }
}

/// State shared between an agent's handle and its ACP handlers.
struct Shared {
    host: Arc<HostInner>,
    plugin_id: String,
    next_permission: AtomicU64,
    permissions: Mutex<HashMap<u64, PendingPermission>>,
    /// The process exited or was stopped.
    dead: AtomicBool,
}

struct PendingPermission {
    session_id: SessionId,
    tx: oneshot::Sender<PermissionDecision>,
}

impl Shared {
    fn on_session_notification(&self, n: schema::SessionNotification) {
        if let Some(event) = convert::session_update(n.update) {
            self.host.emit_session(&n.session_id.0, event);
        }
    }

    fn on_permission_request(&self, req: &schema::RequestPermissionRequest) -> oneshot::Receiver<PermissionDecision> {
        self.request_permission(
            &req.session_id.0,
            convert::tool_call_patch(req.tool_call.clone()),
            convert::permission_options(&req.options),
            None,
        )
    }

    /// Show an approval card in the session; the receiver gets the user's answer.
    fn request_permission(
        &self,
        session_id: &str,
        tool_call: ToolCallPatch,
        options: Vec<PermissionOption>,
        reason: Option<String>,
    ) -> oneshot::Receiver<PermissionDecision> {
        let request_id = self.next_permission.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let session_id = session_id.to_string();
        self.permissions.lock().unwrap().insert(request_id, PendingPermission { session_id: session_id.clone(), tx });
        let request = PermissionRequest { request_id, tool_call, options, reason };
        self.host.emit_session(&session_id, SessionEvent::PermissionRequested(request));
        rx
    }

    fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        let pending = self
            .permissions
            .lock()
            .unwrap()
            .remove(&request_id)
            .with_context(|| format!("no pending permission request {request_id}"))?;
        self.host.emit_session(
            &pending.session_id,
            SessionEvent::PermissionResolved { request_id, decision: decision.clone() },
        );
        let _ = pending.tx.send(decision);
        Ok(())
    }

    fn cancel_permissions(&self, session_id: &str) {
        let ids: Vec<u64> = self
            .permissions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.session_id == session_id)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let _ = self.resolve_permission(id, PermissionDecision::Cancelled);
        }
    }
}

async fn start_agent(host: Arc<HostInner>, plugin: DiscoveredPlugin) -> Result<AgentHandle> {
    let plugin_id = plugin.id().to_string();
    let shared = Arc::new(Shared {
        host: host.clone(),
        plugin_id: plugin_id.clone(),
        next_permission: AtomicU64::new(1),
        permissions: Mutex::new(HashMap::new()),
        dead: AtomicBool::new(false),
    });

    let spawned = {
        let log_host = host.clone();
        let log_id = plugin_id.clone();
        let exit_shared = shared.clone();
        process::spawn(
            &plugin,
            &host.options.project_dir,
            move |line| log_host.emit(HostEvent::PluginLog { plugin_id: log_id.clone(), line }),
            move |kind, tail| {
                exit_shared.dead.store(true, Ordering::SeqCst);
                exit_shared.host.emit_status(&exit_shared.plugin_id, exit_status(kind, &tail));
            },
        )?
    };
    let process = spawned.process;

    // Run the ACP client connection on the runtime; hand its context back out.
    let (conn_tx, conn_rx) = oneshot::channel::<Conn>();
    let transport = ByteStreams::new(spawned.stdin.compat_write(), spawned.stdout.compat());
    let driver = Client
        .v1()
        .name("space")
        .on_receive_notification(
            {
                let shared = shared.clone();
                async move |n: schema::SessionNotification, _cx| {
                    shared.on_session_notification(n);
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let shared = shared.clone();
                async move |status: SpaceStatus, _cx| {
                    let session_id = status.session_id.clone();
                    shared.host.emit_session(&session_id, SessionEvent::Status(status));
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                async move |req: schema::RequestPermissionRequest, responder, cx: Conn| {
                    let rx = shared.on_permission_request(&req);
                    // Wait for the user off the dispatch loop so other traffic keeps flowing.
                    cx.spawn(async move {
                        let outcome = match rx.await {
                            Ok(PermissionDecision::Selected(id)) => schema::RequestPermissionOutcome::Selected(
                                schema::SelectedPermissionOutcome::new(id),
                            ),
                            _ => schema::RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(schema::RequestPermissionResponse::new(outcome))
                    })?;
                    Ok(())
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                async move |req: PreExecute, responder, cx: Conn| {
                    let shared = shared.clone();
                    // The user may take a while to answer; keep the dispatch loop free.
                    cx.spawn(async move { responder.respond(pre_execute(shared, req).await) })?;
                    Ok(())
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                async move |req: Confine, responder, cx: Conn| {
                    let host = shared.host.clone();
                    cx.spawn(async move {
                        match host.confine(req).await {
                            Ok(argv) => responder.respond(ConfineResult { argv }),
                            Err(message) => responder.respond_with_error(acp::Error::new(-32603, message)),
                        }
                    })?;
                    Ok(())
                }
            },
            acp::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: Conn| {
            let _ = conn_tx.send(cx.clone());
            cx.incoming_closed().await;
            Ok(())
        });
    {
        let plugin_id = plugin_id.clone();
        tokio::spawn(async move {
            if let Err(e) = driver.await {
                tracing::warn!("ACP connection to `{plugin_id}` ended with error: {e}");
            }
        });
    }
    let conn = conn_rx.await.context("ACP connection closed before it started")?;

    let init = schema::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_info(schema::Implementation::new("space", env!("CARGO_PKG_VERSION")));
    let init = conn
        .send_request(init)
        .block_task()
        .await
        .map_err(|e| acp_error("initialize", e, &process))?;
    let agent_name = init.agent_info.as_ref().map(|i| i.title.clone().unwrap_or_else(|| i.name.clone()));

    Ok(AgentHandle { inner: Arc::new(AgentInner { plugin_id, conn, process, shared, agent_name }) })
}

fn acp_error(what: &str, e: acp::Error, process: &PluginProcess) -> anyhow::Error {
    let tail = process.logs.tail(5).join("\n");
    if tail.is_empty() {
        anyhow!("{what} failed: {e}")
    } else {
        anyhow!("{what} failed: {e}\nplugin stderr:\n{tail}")
    }
}

/// A running agent plugin. Cheap to clone; the process is killed when the last clone drops.
#[derive(Clone)]
pub struct AgentHandle {
    inner: Arc<AgentInner>,
}

struct AgentInner {
    plugin_id: String,
    conn: Conn,
    process: PluginProcess,
    shared: Arc<Shared>,
    agent_name: Option<String>,
}

impl AgentHandle {
    pub fn plugin_id(&self) -> &str {
        &self.inner.plugin_id
    }

    /// Name the agent reported in `initialize`, if any.
    pub fn agent_name(&self) -> Option<&str> {
        self.inner.agent_name.as_deref()
    }

    pub fn is_running(&self) -> bool {
        !self.inner.shared.dead.load(Ordering::SeqCst)
    }

    /// Recent stderr lines.
    pub fn logs(&self) -> Vec<String> {
        self.inner.process.logs.snapshot()
    }

    /// Kill the plugin process.
    pub fn stop(&self) {
        self.inner.process.kill();
    }

    /// `session/new` with `cwd`.
    pub async fn new_session(&self, cwd: impl Into<PathBuf>) -> Result<Session> {
        let cwd = cwd.into();
        let this = self.clone();
        let host = self.inner.shared.host.clone();
        let task_cwd = cwd.clone();
        let response = host
            .rt
            .spawn(async move {
                this.inner
                    .conn
                    .send_request(schema::NewSessionRequest::new(task_cwd))
                    .block_task()
                    .await
                    .map_err(|e| acp_error("session/new", e, &this.inner.process))
            })
            .await??;
        let id = response.session_id.0.to_string();
        host.sessions.lock().unwrap().insert(id.clone(), SessionMeta::new(cwd.clone()));
        host.emit_session(&id, SessionEvent::Created { plugin_id: self.inner.plugin_id.clone(), cwd });
        let inner = SessionInner { id, agent: self.clone(), running: AtomicBool::new(false) };
        Ok(Session { inner: Arc::new(inner) })
    }

    /// Answer a `session/request_permission` from this agent.
    pub fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        self.inner.shared.resolve_permission(request_id, decision)
    }
}

/// An ACP session on an agent. Cheap to clone.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    id: SessionId,
    agent: AgentHandle,
    running: AtomicBool,
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        self.agent.inner.shared.host.sessions.lock().unwrap().remove(&self.id);
    }
}

impl Session {
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn agent(&self) -> &AgentHandle {
        &self.inner.agent
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// Set the session's permission mode (an entry id of the `permission-modes` catalog).
    /// Takes effect for the next tool call, also mid-turn; held calls are checked again under
    /// the new mode.
    pub fn set_permission_mode(&self, mode: impl Into<String>) {
        let mode = mode.into();
        let host = &self.inner.agent.inner.shared.host;
        if let Some(meta) = host.sessions.lock().unwrap().get_mut(&self.inner.id)
            && meta.mode.as_ref() != Some(&mode)
        {
            meta.mode = Some(mode);
            meta.signals.wake.notify_waiters();
        }
    }

    /// The session's permission mode (the catalog's default unless set).
    pub fn permission_mode(&self) -> String {
        self.inner.agent.inner.shared.host.session_facts(&self.inner.id).0
    }

    /// Send a text prompt and wait for the turn to end. Streams progress as
    /// [`HostEvent::Session`] events. Returns the ACP stop reason (`end_turn`, `cancelled`, ...).
    pub async fn prompt(&self, text: impl Into<String>) -> Result<String> {
        self.prompt_with(text, Vec::new()).await
    }

    /// Like [`Session::prompt`], with files attached as ACP `resource_link` content blocks
    /// (`file://` URIs; the agent decides whether and how to read them).
    pub async fn prompt_with(&self, text: impl Into<String>, attachments: Vec<PathBuf>) -> Result<String> {
        let text = text.into();
        if self.inner.running.swap(true, Ordering::SeqCst) {
            bail!("a prompt is already running in this session");
        }
        let host = self.inner.agent.inner.shared.host.clone();
        let id = self.inner.id.clone();
        host.emit_session(&id, SessionEvent::UserMessage { text: text.clone(), attachments: attachments.clone() });
        host.emit_session(&id, SessionEvent::TurnStarted);
        host.record(&id, HistoryItem::User { text: text.clone() });

        let agent = self.inner.agent.clone();
        let task_id = id.clone();
        let task_host = host.clone();
        let result = host
            .rt
            .spawn(async move {
                let sections = task_host.context_sections(&task_id).await;
                let mut blocks = vec![schema::ContentBlock::Text(schema::TextContent::new(text))];
                for path in &attachments {
                    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    blocks.push(schema::ContentBlock::ResourceLink(schema::ResourceLink::new(
                        name,
                        format!("file://{}", path.display()),
                    )));
                }
                let mut request = schema::PromptRequest::new(task_id, blocks);
                if !sections.is_empty() {
                    let mut meta = schema::Meta::new();
                    meta.insert(CONTEXT_META_KEY.into(), serde_json::to_value(&sections).unwrap_or_default());
                    request = request.meta(meta);
                }
                agent
                    .inner
                    .conn
                    .send_request(request)
                    .block_task()
                    .await
                    .map_err(|e| acp_error("session/prompt", e, &agent.inner.process))
            })
            .await
            .map_err(|e| anyhow!("prompt task failed: {e}"))
            .and_then(|r| r);
        self.inner.running.store(false, Ordering::SeqCst);

        match result {
            Ok(response) => {
                let stop_reason = convert::wire_str(&response.stop_reason);
                host.emit_session(&id, SessionEvent::TurnEnded { stop_reason: stop_reason.clone() });
                Ok(stop_reason)
            }
            Err(e) => {
                host.emit_session(&id, SessionEvent::Error { message: format!("{e:#}") });
                host.emit_session(&id, SessionEvent::TurnEnded { stop_reason: "error".into() });
                Err(e)
            }
        }
    }

    /// `session/cancel`: ask the agent to stop the running turn. Pending permission
    /// requests for this session are answered with `cancelled`, as ACP requires.
    pub fn cancel(&self) -> Result<()> {
        let agent = &self.inner.agent.inner;
        let signals = agent.shared.host.signals(&self.inner.id);
        signals.stops.fetch_add(1, Ordering::SeqCst);
        signals.wake.notify_waiters();
        agent.shared.cancel_permissions(&self.inner.id);
        agent
            .conn
            .send_notification(schema::CancelNotification::new(self.inner.id.clone()))
            .map_err(|e| anyhow!("session/cancel failed: {e}"))
    }

    pub fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        self.inner.agent.resolve_permission(request_id, decision)
    }

    /// `_space/session/configure`: switch the session's model and/or thinking level.
    /// `session_id` in `request` is filled in. On success the returned status is also
    /// emitted as a [`SessionEvent::Status`], so folded state picks it up.
    pub async fn configure(&self, mut request: ConfigureSession) -> Result<SpaceStatus> {
        request.session_id.clone_from(&self.inner.id);
        let agent = self.inner.agent.clone();
        let host = agent.inner.shared.host.clone();
        let mut status = host
            .rt
            .spawn(async move {
                agent
                    .inner
                    .conn
                    .send_request(request)
                    .block_task()
                    .await
                    .map_err(|e| acp_error("_space/session/configure", e, &agent.inner.process))
            })
            .await
            .map_err(|e| anyhow!("configure task failed: {e}"))??;
        if status.session_id.is_empty() {
            status.session_id.clone_from(&self.inner.id);
        }
        host.emit_session(&self.inner.id, SessionEvent::Status(status.clone()));
        Ok(status)
    }
}
