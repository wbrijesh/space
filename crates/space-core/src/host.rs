//! The plugin host: registry, agent processes, ACP client connections and sessions.
//!
//! All async methods run their work on the tokio runtime handed to [`Host::new`], so they
//! can be awaited from any executor (e.g. GPUI's). Everything observable is reported as
//! [`HostEvent`]s on the receiver returned by [`Host::new`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use space_protocol::acp::{self, Agent, ByteStreams, Client};
use space_protocol::acp_schema as schema;
use space_protocol::{PluginKind, SpaceStatus};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::convert;
use crate::discovery::{self, DiscoveredPlugin, DiscoveryOptions, DiscoveryReport};
use crate::process::{self, ExitKind, PluginProcess};
use crate::session::{PermissionDecision, PermissionRequest, SessionEvent, SessionId};

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
}

pub type EventReceiver = mpsc::UnboundedReceiver<HostEvent>;

#[derive(Debug, Clone)]
pub struct HostOptions {
    /// Default project directory (used for `cwd = "project"` and project plugins).
    pub project_dir: PathBuf,
    pub discovery: DiscoveryOptions,
}

impl HostOptions {
    /// Standard discovery locations for `project_dir`.
    pub fn for_project(project_dir: impl Into<PathBuf>) -> Self {
        let project_dir = project_dir.into();
        Self { discovery: DiscoveryOptions::standard(Some(&project_dir)), project_dir }
    }
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
}

impl Host {
    /// Create a host whose background work runs on `rt`. Runs an initial discovery.
    pub fn new(rt: Handle, options: HostOptions) -> (Self, EventReceiver) {
        let (events, rx) = mpsc::unbounded_channel();
        let host = Host { inner: Arc::new(HostInner { rt, events, options, plugins: Mutex::new(Vec::new()) }) };
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

    /// Spawn an `agent` plugin and complete the ACP `initialize` handshake.
    pub async fn start_agent(&self, plugin_id: &str) -> Result<AgentHandle> {
        let plugin = self.plugin(plugin_id).with_context(|| format!("unknown plugin `{plugin_id}`"))?;
        if plugin.manifest.kind != PluginKind::Agent {
            bail!("plugin `{plugin_id}` is a {} plugin, not an agent", plugin.manifest.kind);
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
        let request_id = self.next_permission.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let session_id = req.session_id.0.to_string();
        self.permissions.lock().unwrap().insert(request_id, PendingPermission { session_id: session_id.clone(), tx });
        let request = PermissionRequest {
            request_id,
            tool_call: convert::tool_call_patch(req.tool_call.clone()),
            options: convert::permission_options(&req.options),
        };
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
                let status = match kind {
                    ExitKind::Killed | ExitKind::Exited(Some(0)) => PluginStatus::Stopped,
                    ExitKind::Exited(code) => {
                        let code = code.map_or("a signal".to_string(), |c| format!("code {c}"));
                        let last = tail.last().map(|l| format!(": {l}")).unwrap_or_default();
                        PluginStatus::Crashed(format!("exited with {code}{last}"))
                    }
                };
                exit_shared.host.emit_status(&exit_shared.plugin_id, status);
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
        host.emit_session(&id, SessionEvent::Created { plugin_id: self.inner.plugin_id.clone(), cwd });
        Ok(Session { inner: Arc::new(SessionInner { id, agent: self.clone(), running: AtomicBool::new(false) }) })
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

    /// Send a text prompt and wait for the turn to end. Streams progress as
    /// [`HostEvent::Session`] events. Returns the ACP stop reason (`end_turn`, `cancelled`, ...).
    pub async fn prompt(&self, text: impl Into<String>) -> Result<String> {
        let text = text.into();
        if self.inner.running.swap(true, Ordering::SeqCst) {
            bail!("a prompt is already running in this session");
        }
        let host = self.inner.agent.inner.shared.host.clone();
        let id = self.inner.id.clone();
        host.emit_session(&id, SessionEvent::UserMessage { text: text.clone() });
        host.emit_session(&id, SessionEvent::TurnStarted);

        let agent = self.inner.agent.clone();
        let task_id = id.clone();
        let result = host
            .rt
            .spawn(async move {
                let request = schema::PromptRequest::new(
                    task_id,
                    vec![schema::ContentBlock::Text(schema::TextContent::new(text))],
                );
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
        agent.shared.cancel_permissions(&self.inner.id);
        agent
            .conn
            .send_notification(schema::CancelNotification::new(self.inner.id.clone()))
            .map_err(|e| anyhow!("session/cancel failed: {e}"))
    }

    pub fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        self.inner.agent.resolve_permission(request_id, decision)
    }
}
