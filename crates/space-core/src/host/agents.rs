//! Agent plugins: the ACP client connection, sessions and permission requests.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use space_protocol::acp::{self, Agent, ByteStreams, Client};
use space_protocol::acp_schema as schema;
use space_protocol::ext::{CONTEXT_META_KEY, DATA_DIR_META_KEY, PROTOCOL_VERSION_META_KEY, STEER_META_KEY, USAGE_META_KEY};
use space_protocol::service::HistoryItem;
use space_protocol::trace::{ContextFact, TraceBatch, TraceEvent};
use space_protocol::{
    Complete, CompleteResult, Confine, ConfineResult, ConfigureSession, PROTOCOL_VERSION, PreExecute, SpaceStatus, SteerImage, SteerSession, SteerText,
    Retry, Steered, UnsteerSession,
};
use tokio::sync::oneshot;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::pipeline::{SessionMeta, pre_execute};
use super::{Host, HostEvent, HostInner, PluginStatus, Refused, check_protocol, exit_status, start_failure};
use crate::{convert, now_ms};
use crate::discovery::DiscoveredPlugin;
use crate::process::{self, PluginProcess};
use crate::session::{TextRef, UserInput, PermissionDecision, PermissionOption, PermissionRequest, SessionEvent, SessionId, ToolCallPatch};

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
    /// Messages sent to steer a running turn, by session, until the agent delivers them.
    steers: Mutex<HashMap<SessionId, Vec<PendingSteer>>>,
}

/// A steering message the agent hasn't handed to the model yet.
struct PendingSteer {
    id: String,
    input: UserInput,
}

struct PendingPermission {
    session_id: SessionId,
    tx: oneshot::Sender<PermissionDecision>,
}

impl Shared {
    /// The agent handed a steering message to the model: it joins the transcript here.
    fn on_steered(&self, steered: Steered) {
        let pending = {
            let mut steers = self.steers.lock().unwrap();
            let list = steers.entry(steered.session_id.clone()).or_default();
            let at = list.iter().position(|p| p.id == steered.id);
            at.map(|i| list.remove(i))
        };
        let Some(p) = pending else { return };
        let (host, id, input) = (&self.host, &steered.session_id, p.input);
        let text = input.prompt_text();
        if host.tracing() {
            let path = |p: &PathBuf| p.display().to_string();
            let prompt = TraceEvent::Prompt {
                at: now_ms(),
                text: text.clone(),
                files: input.attachments.iter().chain(input.texts.iter().map(|t| &t.path)).map(path).collect(),
                images: input.images.iter().map(|i| path(&i.path)).collect(),
            };
            host.trace(id, vec![prompt]);
        }
        host.record(id, HistoryItem::User { text });
        host.emit_session(id, input.into_message(Some(now_ms())));
        host.emit(HostEvent::Steered { session_id: steered.session_id, steer_id: steered.id });
    }

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
        steers: Mutex::new(HashMap::new()),
    });

    let launch = host.launch(&plugin_id);
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
                if exit_shared.host.is_current(&exit_shared.plugin_id, launch) {
                    exit_shared.host.emit_exit(&exit_shared.plugin_id, exit_status(kind, &tail));
                }
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
        .on_receive_notification(
            {
                let shared = shared.clone();
                async move |retry: Retry, _cx| {
                    let event = if retry.discard == Some(true) { SessionEvent::AttemptDiscarded } else { SessionEvent::Retry(retry.state) };
                    shared.host.emit_session(&retry.session_id, event);
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let shared = shared.clone();
                async move |steered: Steered, _cx| {
                    shared.on_steered(steered);
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let shared = shared.clone();
                async move |batch: TraceBatch, _cx| {
                    shared.host.trace(&batch.session_id, batch.events);
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
    let images = init.agent_capabilities.prompt_capabilities.image;
    let embedded = init.agent_capabilities.prompt_capabilities.embedded_context;
    let steer = init.meta.as_ref().and_then(|m| m.get(STEER_META_KEY)).and_then(|v| v.as_bool()).unwrap_or(false);

    let inner = Arc::new(AgentInner { plugin_id: plugin_id.clone(), conn, process, shared, agent_name, can_load, images, embedded, steer });
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
/// A long paste as a prompt block: its text inline (an embedded resource), or, for an agent that
/// doesn't take embedded context, a link to its file.
fn text_block(t: &TextRef, embedded: bool) -> Result<schema::ContentBlock> {
    let uri = format!("file://{}", t.path.display());
    if !embedded {
        let name = t.path.file_name().map_or_else(|| "pasted.txt".into(), |n| n.to_string_lossy().into_owned());
        let size = std::fs::metadata(&t.path).map(|m| m.len() as i64).ok();
        let link = schema::ResourceLink::new(name, uri).title("Pasted text".to_string()).mime_type(t.mime.clone()).size(size);
        return Ok(schema::ContentBlock::ResourceLink(link));
    }
    let text = std::fs::read_to_string(&t.path).map_err(|e| anyhow!("reading {}: {e}", t.path.display()))?;
    let contents = schema::TextResourceContents::new(text, uri).mime_type(t.mime.clone());
    Ok(schema::ContentBlock::Resource(schema::EmbeddedResource::new(schema::EmbeddedResourceResource::TextResourceContents(contents))))
}

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
    /// The agent takes images in prompts (ACP `promptCapabilities.image`).
    images: bool,
    /// The agent takes text inline in prompts (ACP `promptCapabilities.embeddedContext`).
    embedded: bool,
    /// The agent takes messages between the steps of a running turn (`_space/session/steer`).
    steer: bool,
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
        self.new_session_in(cwd, None, None).await
    }

    /// `session/new` with `cwd`, telling the agent the folder it may keep the session's own
    /// files in (to load it again later). `thread_id`: the thread the session belongs to (its
    /// traces are filed under it).
    pub async fn new_session_in(&self, cwd: impl Into<PathBuf>, data_dir: Option<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
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
        let session = self.session(id.clone(), cwd.clone(), thread_id);
        host.emit_session(&id, SessionEvent::Created { plugin_id: self.inner.plugin_id.clone(), cwd });
        Ok(session)
    }

    /// Whether the agent can load a session again ([`AgentHandle::load_session`]).
    pub fn can_load_session(&self) -> bool {
        self.inner.can_load
    }

    /// Whether the agent takes images in prompts (ACP `promptCapabilities.image`).
    pub fn accepts_images(&self) -> bool {
        self.inner.images
    }

    /// Whether the agent takes text inline in prompts (ACP `promptCapabilities.embeddedContext`).
    pub fn accepts_embedded_text(&self) -> bool {
        self.inner.embedded
    }

    /// Whether the agent takes messages for a running turn ([`Session::steer`]).
    pub fn can_steer(&self) -> bool {
        self.inner.steer
    }

    /// ACP `session/load`: continue session `session_id` (from an earlier run) in `cwd`. The
    /// history the agent replays while loading isn't reported; the thread already shows it.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: impl Into<PathBuf>,
        data_dir: Option<PathBuf>,
        thread_id: Option<&str>,
    ) -> Result<Session> {
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
        Ok(self.session(id, cwd, thread_id))
    }

    fn session(&self, id: SessionId, cwd: PathBuf, thread_id: Option<&str>) -> Session {
        let mut meta = SessionMeta::new(cwd, self.inner.plugin_id.clone());
        meta.thread_id = thread_id.map(str::to_string);
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
        self.prompt_with(UserInput::text(text)).await
    }

    /// Like [`Session::prompt`], with files attached as ACP `resource_link` content blocks
    /// (`file://` URIs; the agent decides whether and how to read them) and images as ACP
    /// image blocks (the saved file's bytes, and its `file://` URI). Images need an agent that
    /// takes them ([`AgentHandle::accepts_images`]). `texts` (long pastes) go inline as ACP
    /// embedded resources, or as `resource_link`s to their files for an agent that doesn't take
    /// embedded context. A quote goes before the text as a markdown blockquote.
    pub async fn prompt_with(&self, input: UserInput) -> Result<String> {
        let text = input.prompt_text();
        let (attachments, images, texts) = (input.attachments.clone(), input.images.clone(), input.texts.clone());
        if !images.is_empty() && !self.inner.agent.accepts_images() {
            bail!("this agent can't read images");
        }
        // Read before anything is shown: an image that can't be read fails the send, not the turn.
        let image_blocks = images
            .iter()
            .map(|image| {
                let bytes = std::fs::read(&image.path).map_err(|e| anyhow!("reading {}: {e}", image.path.display()))?;
                Ok(schema::ContentBlock::Image(
                    schema::ImageContent::new(BASE64.encode(bytes), image.mime.clone()).uri(format!("file://{}", image.path.display())),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let text_blocks = texts.iter().map(|t| text_block(t, self.inner.agent.accepts_embedded_text())).collect::<Result<Vec<_>>>()?;
        if self.inner.running.swap(true, Ordering::SeqCst) {
            bail!("a prompt is already running in this session");
        }
        let host = self.inner.agent.inner.shared.host.clone();
        let id = self.inner.id.clone();
        if host.tracing() {
            let path = |p: &PathBuf| p.display().to_string();
            let prompt = TraceEvent::Prompt {
                at: now_ms(),
                text: text.clone(),
                files: attachments.iter().chain(texts.iter().map(|t| &t.path)).map(path).collect(),
                images: images.iter().map(|i| path(&i.path)).collect(),
            };
            host.trace(&id, vec![prompt]);
        }
        host.emit_session(&id, input.into_message(Some(now_ms())));
        host.emit_session(&id, SessionEvent::TurnStarted);
        host.record(&id, HistoryItem::User { text: text.clone() });

        let agent = self.inner.agent.clone();
        let task_id = id.clone();
        let task_host = host.clone();
        let result = host
            .rt
            .spawn(async move {
                let sections = task_host.context_sections(&task_id).await;
                if !sections.is_empty() && task_host.tracing() {
                    let facts = sections
                        .iter()
                        .map(|(plugin, s)| ContextFact { plugin: plugin.clone(), id: s.id.clone(), text: s.text.clone() })
                        .collect();
                    task_host.trace(&task_id, vec![TraceEvent::Context { at: now_ms(), sections: facts }]);
                }
                let sections: Vec<_> = sections.into_iter().map(|(_, s)| s).collect();
                // Images and pasted texts first, then the text that refers to them.
                let mut blocks = image_blocks;
                blocks.extend(text_blocks);
                blocks.push(schema::ContentBlock::Text(schema::TextContent::new(text)));
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
                // What the turn used, if the agent says (`_meta["space/usage"]`).
                let usage = response
                    .meta
                    .as_ref()
                    .and_then(|m| m.get(USAGE_META_KEY))
                    .and_then(|u| serde_json::from_value(u.clone()).map_err(|e| tracing::debug!("turn usage: {e}")).ok());
                host.emit_session(&id, SessionEvent::TurnEnded { stop_reason: stop_reason.clone(), at_ms: Some(now_ms()), usage });
                Ok(stop_reason)
            }
            Err(e) => {
                host.emit_session(&id, SessionEvent::Error { message: format!("{e:#}") });
                host.emit_session(&id, SessionEvent::TurnEnded { stop_reason: "error".into(), at_ms: Some(now_ms()), usage: None });
                Err(e)
            }
        }
    }

    /// `_space/session/steer`: a message for the running turn, delivered after its current step
    /// (needs [`AgentHandle::can_steer`]). Returns once the agent has it; when the agent hands it
    /// to the model it joins the transcript and [`HostEvent::Steered`] says so.
    pub async fn steer(&self, id: String, input: UserInput) -> Result<()> {
        let UserInput { attachments, images, texts, .. } = &input;
        if !self.inner.agent.can_steer() {
            bail!("this agent can't take messages during a turn");
        }
        if !images.is_empty() && !self.inner.agent.accepts_images() {
            bail!("this agent can't read images");
        }
        let blocks = images
            .iter()
            .map(|image| {
                let bytes = std::fs::read(&image.path).map_err(|e| anyhow!("reading {}: {e}", image.path.display()))?;
                Ok(SteerImage { data: BASE64.encode(bytes), mime_type: image.mime.clone(), uri: format!("file://{}", image.path.display()) })
            })
            .collect::<Result<Vec<_>>>()?;
        let steer_texts = texts
            .iter()
            .map(|t| {
                let text = std::fs::read_to_string(&t.path).map_err(|e| anyhow!("reading {}: {e}", t.path.display()))?;
                Ok(SteerText { text, mime_type: t.mime.clone(), uri: format!("file://{}", t.path.display()) })
            })
            .collect::<Result<Vec<_>>>()?;
        let request = SteerSession {
            session_id: self.inner.id.clone(),
            id: id.clone(),
            text: input.prompt_text(),
            images: blocks,
            texts: steer_texts,
            attachments: attachments.iter().map(|p| format!("file://{}", p.display())).collect(),
        };
        let shared = self.inner.agent.inner.shared.clone();
        shared.steers.lock().unwrap().entry(self.inner.id.clone()).or_default().push(PendingSteer { id: id.clone(), input });
        let agent = self.inner.agent.clone();
        let result = shared
            .host
            .rt
            .spawn(async move {
                agent.inner.conn.send_request(request).block_task().await.map_err(|e| acp_error("_space/session/steer", e, &agent.inner.process))
            })
            .await
            .map_err(|e| anyhow!("steer task failed: {e}"))
            .and_then(|r| r);
        if result.is_err()
            && let Some(list) = shared.steers.lock().unwrap().get_mut(&self.inner.id)
        {
            list.retain(|p| p.id != id);
        }
        result.map(|_| ())
    }

    /// `_space/session/unsteer`: drop the steering messages the agent hasn't delivered.
    /// Returns their ids.
    pub async fn unsteer(&self) -> Result<Vec<String>> {
        if !self.inner.agent.can_steer() {
            return Ok(Vec::new());
        }
        let request = UnsteerSession { session_id: self.inner.id.clone() };
        let agent = self.inner.agent.clone();
        let shared = agent.inner.shared.clone();
        let result = shared
            .host
            .rt
            .spawn(async move {
                agent.inner.conn.send_request(request).block_task().await.map_err(|e| acp_error("_space/session/unsteer", e, &agent.inner.process))
            })
            .await
            .map_err(|e| anyhow!("unsteer task failed: {e}"))??;
        if let Some(list) = shared.steers.lock().unwrap().get_mut(&self.inner.id) {
            list.retain(|p| !result.ids.contains(&p.id));
        }
        Ok(result.ids)
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
