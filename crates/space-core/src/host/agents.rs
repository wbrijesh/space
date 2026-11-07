//! Agent plugins: the ACP client connection, sessions and permission requests.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use space_protocol::acp::{self, Agent, ByteStreams, Client};
use space_protocol::acp_schema as schema;
use space_protocol::ext::{CONTEXT_META_KEY, DATA_DIR_META_KEY, PROTOCOL_VERSION_META_KEY};
use space_protocol::service::HistoryItem;
use space_protocol::{Complete, CompleteResult, Confine, ConfineResult, ConfigureSession, PROTOCOL_VERSION, PreExecute, SpaceStatus};
use tokio::sync::oneshot;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::pipeline::{SessionMeta, pre_execute};
use super::{Host, HostEvent, HostInner, PluginStatus, Refused, check_protocol, exit_status, start_failure};
use crate::convert;
use crate::discovery::DiscoveredPlugin;
use crate::process::{self, PluginProcess};
use crate::session::{PermissionDecision, PermissionOption, PermissionRequest, SessionEvent, SessionId, ToolCallPatch};

type Conn = acp::v1::ConnectionTo<Agent>;

impl Host {
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
            Err(e) => inner.emit_status(&id, start_failure(e)),
        }
        result
    }
}

/// State shared between an agent's handle and its ACP handlers.
pub(super) struct Shared {
    pub(super) host: Arc<HostInner>,
    plugin_id: String,
    next_permission: AtomicU64,
    permissions: Mutex<HashMap<u64, PendingPermission>>,
    /// Sessions being loaded: the history an agent replays while loading is already on screen.
    loading: Mutex<HashSet<SessionId>>,
    /// The process exited or was stopped.
    dead: AtomicBool,
}

struct PendingPermission {
    session_id: SessionId,
    tx: oneshot::Sender<PermissionDecision>,
}

impl Shared {
    pub(super) fn on_session_notification(&self, n: schema::SessionNotification) {
        if self.loading.lock().unwrap().contains(n.session_id.0.as_ref()) {
            return;
        }
        if let Some(event) = convert::session_update(n.update) {
            self.host.emit_session(&n.session_id.0, event);
        }
    }

    pub(super) fn on_permission_request(&self, req: &schema::RequestPermissionRequest) -> oneshot::Receiver<PermissionDecision> {
        self.request_permission(
            &req.session_id.0,
            convert::tool_call_patch(req.tool_call.clone()),
            convert::permission_options(&req.options),
            None,
        )
    }

    /// Show an approval card in the session; the receiver gets the user's answer.
    pub(super) fn request_permission(
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

    pub(super) fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
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

    pub(super) fn cancel_permissions(&self, session_id: &str) {
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
    host.start_requirements(&plugin).await.map_err(Refused)?;
    let plugin_id = plugin.id().to_string();
    let shared = Arc::new(Shared {
        host: host.clone(),
        plugin_id: plugin_id.clone(),
        next_permission: AtomicU64::new(1),
        permissions: Mutex::new(HashMap::new()),
        loading: Mutex::new(HashSet::new()),
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
                exit_shared.host.emit_exit(&exit_shared.plugin_id, exit_status(kind, &tail));
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

    let mut meta = schema::Meta::new();
    meta.insert(PROTOCOL_VERSION_META_KEY.into(), PROTOCOL_VERSION.into());
    let init = schema::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_info(schema::Implementation::new("space", env!("CARGO_PKG_VERSION")))
        .meta(meta);
    let init = conn
        .send_request(init)
        .block_task()
        .await
        .map_err(|e| acp_error("initialize", e, &process))?;
    // An agent that speaks the `_space/*` methods says which protocol version; a plain ACP
    // agent doesn't, and runs without them.
    if let Some(version) = init.meta.as_ref().and_then(|m| m.get(PROTOCOL_VERSION_META_KEY)) {
        check_protocol(version.as_u64().map(|v| v as u32))?;
    }
    let agent_name = init.agent_info.as_ref().map(|i| i.title.clone().unwrap_or_else(|| i.name.clone()));
    let can_load = init.agent_capabilities.load_session;

    let inner = Arc::new(AgentInner { plugin_id: plugin_id.clone(), conn, process, shared, agent_name, can_load });
    host.agents.lock().unwrap().insert(plugin_id, Arc::downgrade(&inner));
    Ok(AgentHandle { inner })
}

impl AgentInner {
    pub(super) fn is_running(&self) -> bool {
        !self.shared.dead.load(Ordering::SeqCst)
    }

    pub(super) fn stop(&self) {
        self.process.kill();
    }

    /// `_space/complete`: one short completion by the agent's model.
    pub(super) async fn complete(&self, request: Complete) -> Result<CompleteResult> {
        self.conn.send_request(request).block_task().await.map_err(|e| acp_error("_space/complete", e, &self.process))
    }
}

/// `_meta` with the session's data folder, if any.
fn data_dir_meta(data_dir: Option<&std::path::Path>) -> Option<schema::Meta> {
    let dir = data_dir?;
    let mut meta = schema::Meta::new();
    meta.insert(DATA_DIR_META_KEY.into(), dir.display().to_string().into());
    Some(meta)
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

pub(super) struct AgentInner {
    plugin_id: String,
    conn: Conn,
    process: PluginProcess,
    shared: Arc<Shared>,
    agent_name: Option<String>,
    /// The agent can load a session again (ACP `loadSession`).
    can_load: bool,
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
        self.inner.is_running()
    }

    /// Recent stderr lines.
    pub fn logs(&self) -> Vec<String> {
        self.inner.process.logs.snapshot()
    }

    /// Kill the plugin process.
    pub fn stop(&self) {
        self.inner.stop();
    }

    /// `session/new` with `cwd`.
    pub async fn new_session(&self, cwd: impl Into<PathBuf>) -> Result<Session> {
        self.new_session_in(cwd, None).await
    }

    /// `session/new` with `cwd`, telling the agent the folder it may keep the session's own
    /// files in (to load it again later).
    pub async fn new_session_in(&self, cwd: impl Into<PathBuf>, data_dir: Option<PathBuf>) -> Result<Session> {
        let cwd = cwd.into();
        let this = self.clone();
        let host = self.inner.shared.host.clone();
        let request = schema::NewSessionRequest::new(cwd.clone()).meta(data_dir_meta(data_dir.as_deref()));
        let response = host
            .rt
            .spawn(async move {
                this.inner
                    .conn
                    .send_request(request)
                    .block_task()
                    .await
                    .map_err(|e| acp_error("session/new", e, &this.inner.process))
            })
            .await??;
        let id = response.session_id.0.to_string();
        host.emit_session(&id, SessionEvent::Created { plugin_id: self.inner.plugin_id.clone(), cwd: cwd.clone() });
        Ok(self.session(id, cwd))
    }

    /// Whether the agent can load a session again ([`AgentHandle::load_session`]).
    pub fn can_load_session(&self) -> bool {
        self.inner.can_load
    }

    /// ACP `session/load`: continue session `session_id` (from an earlier run) in `cwd`. The
    /// history the agent replays while loading isn't reported; the thread already shows it.
    pub async fn load_session(&self, session_id: &str, cwd: impl Into<PathBuf>, data_dir: Option<PathBuf>) -> Result<Session> {
        if !self.inner.can_load {
            bail!("{} can't load a saved session", self.agent_name().unwrap_or(&self.inner.plugin_id));
        }
        let cwd = cwd.into();
        let (this, id) = (self.clone(), session_id.to_string());
        let host = self.inner.shared.host.clone();
        let request = schema::LoadSessionRequest::new(id.clone(), cwd.clone()).meta(data_dir_meta(data_dir.as_deref()));
        self.inner.shared.loading.lock().unwrap().insert(id.clone());
        let response = host
            .rt
            .spawn(async move {
                this.inner
                    .conn
                    .send_request(request)
                    .block_task()
                    .await
                    .map_err(|e| acp_error("session/load", e, &this.inner.process))
            })
            .await;
        self.inner.shared.loading.lock().unwrap().remove(&id);
        response??;
        Ok(self.session(id, cwd))
    }

    fn session(&self, id: SessionId, cwd: PathBuf) -> Session {
        let meta = SessionMeta::new(cwd, self.inner.plugin_id.clone());
        self.inner.shared.host.sessions.lock().unwrap().insert(id.clone(), meta);
        let inner = SessionInner { id, agent: self.clone(), running: AtomicBool::new(false) };
        Session { inner: Arc::new(inner) }
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

    /// Give the tool pipeline's guards what the thread already shows (call after loading a saved
    /// session again), so their first judgements have the same context as later ones.
    pub fn seed_history(&self, items: &[crate::session::TranscriptItem]) {
        self.inner.agent.inner.shared.host.seed_history(&self.inner.id, items);
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
