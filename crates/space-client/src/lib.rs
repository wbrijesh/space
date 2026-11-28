//! The app's side of the Space server ([`space_core::remote`]): [`Host`], [`AgentHandle`] and
//! [`Session`] with the methods of `space_core`'s, each a call to the server. What the app reads
//! without asking (plugins, statuses, catalogs) is a copy the server keeps current.
//!
//! Calls that need no answer go as notifications, which the server handles at once and in order
//! (a setting before the services start, keys in the order typed); the others are requests.
//!
//! The server is either shared ([`connect_shared`]: the real Space's, under a keeper, outliving
//! the app) or private ([`start_private`]: inside this process, for scenarios, test copies and
//! scripts, ending with it).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::FutureExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use space_core::discovery::DiscoveredPlugin;
use space_core::grants::Approval;
use space_core::protocol::service::{CatalogEntry, SettingValues, ThreadMeta};
use space_core::protocol::terminal::{Bytes, TerminalInfo, TerminalOpen, TerminalOpened, TerminalReplay};
use space_core::protocol::trace::{TrajectoryDetail, TrajectoryDetailRequest, TrajectoryPage, TrajectoryRead, TrajectoryStats};
use space_core::protocol::{ConfigureSession, ForkOf, RewindResult, SpaceStatus};
use space_core::remote::{self, AgentInfo, HostState, LiveSession, TerminalData, Welcome};
use space_core::{HostEvent, PermissionDecision, PluginStatus, SessionEvent, SessionId, SettingError, SettingState, TerminalSink, TranscriptItem, UserInput};
use space_plugin::rpc::{Peer, RpcError};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

pub type EventReceiver = mpsc::UnboundedReceiver<HostEvent>;

/// The server, as the app talks to it. Cheap to clone.
#[derive(Clone)]
pub struct Host {
    inner: Arc<Inner>,
}

struct Inner {
    peer: Peer,
    shared: bool,
    welcome: Welcome,
    state: Mutex<HostState>,
    sink: Mutex<Option<TerminalSink>>,
    /// Sessions with a turn running.
    running: Mutex<HashSet<SessionId>>,
    /// Agents running, by plugin.
    alive: Mutex<HashSet<String>>,
    /// The app is quitting: sessions dropped now aren't released (the server keeps them).
    closing: AtomicBool,
    /// The server said it's quitting (Quit Server).
    server_quitting: AtomicBool,
    /// The server asked the app to start again from this executable (an update).
    relaunch: Mutex<Option<PathBuf>>,
    connected: watch::Sender<bool>,
    /// The shared server's socket.
    socket: Mutex<Option<PathBuf>>,
}

impl Host {
    /// Talk to a server over `reader`/`writer`. `shared`: a server that outlives the app (its
    /// shutdown is a no-op from here).
    pub async fn connect<R, W>(reader: R, writer: W, shared: bool) -> Result<(Host, EventReceiver)>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (events_tx, events) = mpsc::unbounded_channel::<HostEvent>();
        let slot: Arc<Mutex<Option<std::sync::Weak<Inner>>>> = Arc::new(Mutex::new(None));
        // Before the welcome, notifications wait here (none should come).
        let notes = {
            let slot = slot.clone();
            Arc::new(move |method: String, params: Value| {
                let Some(inner) = slot.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade) else { return };
                inner.on_note(&method, params, &events_tx);
            })
        };
        let handler = Arc::new(|method: String, _params: Value| async move { Err(RpcError::method_not_found(&method)) }.boxed());
        let (peer, task) = Peer::spawn(reader, writer, handler, notes);
        let version = space_core::build_info::current().version.clone();
        let welcome: Welcome = call(&peer, "server.hello", json!([remote::SERVER_PROTOCOL, version])).await?;
        let (connected, _) = watch::channel(true);
        let inner = Arc::new(Inner {
            peer,
            shared,
            state: Mutex::new(welcome.state.clone()),
            sink: Mutex::new(None),
            running: Mutex::new(welcome.sessions.iter().filter(|s| s.running).map(|s| s.session_id.clone()).collect()),
            alive: Mutex::new(welcome.agents.iter().map(|a| a.plugin_id.clone()).collect()),
            closing: AtomicBool::new(false),
            server_quitting: AtomicBool::new(false),
            relaunch: Mutex::new(None),
            welcome,
            connected,
            socket: Mutex::new(None),
        });
        *slot.lock().unwrap() = Some(Arc::downgrade(&inner));
        {
            let inner = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let _ = task.await;
                tracing::info!("the connection to the server ended");
                if let Some(inner) = inner.upgrade() {
                    inner.connected.send_replace(false);
                }
            });
        }
        Ok((Host { inner }, events))
    }

    /// Whether this is the shared server (it outlives the app).
    pub fn is_shared(&self) -> bool {
        self.inner.shared
    }

    /// The server's process id and build.
    pub fn server_info(&self) -> (u32, String) {
        (self.inner.welcome.pid, self.inner.welcome.version.clone())
    }

    /// Whether the connection is up; changes once, when it ends.
    pub fn connected(&self) -> watch::Receiver<bool> {
        self.inner.connected.subscribe()
    }

    /// Whether the connection ended.
    pub fn is_disconnected(&self) -> bool {
        !*self.inner.connected.borrow()
    }

    /// Whether the server said it's quitting (Quit Server): the app quits too.
    pub fn server_quitting(&self) -> bool {
        self.inner.server_quitting.load(Ordering::SeqCst)
    }

    /// Whether a shared server answers again (after this connection ended).
    pub fn server_answers(&self) -> bool {
        self.inner.socket.lock().unwrap().as_ref().is_some_and(|s| std::os::unix::net::UnixStream::connect(s).is_ok())
    }

    /// Sessions that were open when this app connected (an earlier app made them).
    pub fn live_sessions(&self) -> Vec<LiveSession> {
        self.inner.welcome.sessions.clone()
    }

    /// Agents that were running when this app connected.
    pub fn live_agents(&self) -> Vec<AgentHandle> {
        self.inner.welcome.agents.iter().map(|info| AgentHandle { host: self.clone(), info: Arc::new(info.clone()) }).collect()
    }

    /// The app is quitting: sessions it drops from now on stay with the server.
    pub fn close(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
    }

    /// Switch to the ready update: now, or (one that stops agents) once no thread is working.
    pub async fn apply_update(&self, when_idle: bool) -> Result<()> {
        self.request("server.apply_update", json!([when_idle])).await
    }

    /// The update waiting for the app to start again from it (the server asked).
    pub fn relaunch_into(&self) -> Option<PathBuf> {
        self.inner.relaunch.lock().unwrap().clone()
    }

    /// Stop the shared server and everything it runs (agents, terminals).
    pub async fn quit_server(&self) -> Result<()> {
        self.request::<()>("server.quit", json!([])).await
    }

    async fn request<R: DeserializeOwned>(&self, method: &str, params: Value) -> Result<R> {
        call(&self.inner.peer, method, params).await
    }

    fn note(&self, method: &str, params: impl Serialize) {
        self.inner.peer.notify(method, params);
    }

    fn state<T>(&self, read: impl FnOnce(&HostState) -> T) -> T {
        read(&self.inner.state.lock().unwrap())
    }

    // ----- what the app reads without asking --------------------------------------------

    pub fn plugins(&self) -> Vec<DiscoveredPlugin> {
        self.state(|s| s.plugins.clone())
    }

    pub fn plugin(&self, id: &str) -> Option<DiscoveredPlugin> {
        self.state(|s| s.plugins.iter().find(|p| p.id() == id).cloned())
    }

    /// Service plugins (not agents).
    pub fn service_plugins(&self) -> Vec<DiscoveredPlugin> {
        self.state(|s| s.plugins.iter().filter(|p| !p.manifest.is_agent()).cloned().collect())
    }

    pub fn status(&self, plugin_id: &str) -> Option<PluginStatus> {
        self.state(|s| s.statuses.get(plugin_id).cloned())
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        self.state(|s| s.disabled.iter().any(|d| d == plugin_id))
    }

    pub fn approval(&self, plugin_id: &str) -> Approval {
        self.state(|s| s.approvals.get(plugin_id).copied().unwrap_or(Approval::Pending))
    }

    pub fn catalog(&self, name: &str) -> Vec<CatalogEntry> {
        self.state(|s| s.catalogs.get(name).cloned().unwrap_or_default())
    }

    pub fn plugin_settings(&self, plugin_id: &str) -> Vec<SettingState> {
        self.state(|s| s.plugin_settings.get(plugin_id).cloned().unwrap_or_default())
    }

    pub fn has_session_store(&self) -> bool {
        self.state(|s| s.has_session_store)
    }

    pub fn has_terminal(&self) -> bool {
        self.state(|s| s.has_terminal)
    }

    pub fn has_trajectory(&self) -> bool {
        self.state(|s| s.has_trajectory)
    }

    pub fn terminal_plugin(&self) -> Option<String> {
        self.state(|s| s.terminal_plugin.clone())
    }

    pub fn project_dir(&self) -> PathBuf {
        self.state(|s| s.project_dir.clone())
    }

    /// A new build of Space on its way or ready.
    pub fn update(&self) -> Option<remote::Update> {
        self.state(|s| s.update.clone())
    }

    // ----- calls without an answer ------------------------------------------------------

    pub fn discover(&self) {
        self.note("host.discover", json!([]));
    }

    pub fn set_disabled(&self, ids: impl IntoIterator<Item = String>) {
        let ids: Vec<String> = ids.into_iter().collect();
        self.inner.state.lock().unwrap().disabled.clone_from(&ids);
        self.note("host.set_disabled", json!([ids]));
    }

    pub fn set_text_settings(&self, plugin_id: &str, values: SettingValues) {
        self.note("host.set_text_settings", json!([plugin_id, values]));
    }

    pub fn set_watching(&self, session_id: Option<SessionId>) {
        self.note("host.set_watching", json!([session_id]));
    }

    pub fn answer_approval(&self, plugin_id: &str, approved: bool) -> Result<()> {
        self.note("host.answer_approval", json!([plugin_id, approved]));
        Ok(())
    }

    pub fn save_thread_meta(&self, meta: ThreadMeta) {
        self.note("host.save_thread_meta", json!([meta]));
    }

    pub fn record_event(&self, thread_id: &str, event: &SessionEvent) {
        self.note("host.record_event", json!([thread_id, event]));
    }

    pub fn thread_renamed(&self, session_id: &str, title: &str) {
        self.note("host.thread_renamed", json!([session_id, title]));
    }

    pub fn terminal_input(&self, terminal_id: &str, data: Vec<u8>) {
        self.note("host.terminal_input", json!([terminal_id, Bytes(data)]));
    }

    pub fn terminal_resize(&self, terminal_id: &str, cols: u16, rows: u16) {
        self.note("host.terminal_resize", json!([terminal_id, cols, rows]));
    }

    /// Where terminal output goes; `true` from it puts a [`HostEvent::TerminalOutput`] in the
    /// event stream (draw again).
    pub fn set_terminal_sink(&self, sink: TerminalSink) {
        *self.inner.sink.lock().unwrap() = Some(sink);
    }

    /// Stop every plugin: a private server's (Space is quitting). The shared server goes on.
    pub fn shutdown(&self) {
        if !self.inner.shared {
            let _ = futures::executor::block_on(tokio_timeout(Duration::from_secs(5), self.request::<()>("host.shutdown", json!([]))));
        }
    }

    /// Wait (from outside an async context) for every thread's writes, at most `timeout`.
    pub fn flush_saves_blocking(&self, timeout: Duration) {
        let call = self.request::<()>("host.flush_saves", json!([timeout.as_millis() as u64]));
        let _ = futures::executor::block_on(tokio_timeout(timeout + Duration::from_secs(1), call));
    }

    // ----- calls with an answer ---------------------------------------------------------

    pub async fn start_services(&self) {
        let _ = self.request::<()>("host.start_services", json!([])).await;
    }

    pub async fn set_setting(&self, plugin_id: &str, key: &str, value: Option<String>, force: bool) -> Result<(), SettingError> {
        match self.request::<Result<(), SettingError>>("host.set_setting", json!([plugin_id, key, value, force])).await {
            Ok(result) => result,
            Err(e) => Err(SettingError::Failed(format!("{e:#}"))),
        }
    }

    pub async fn retry_plugin(&self, plugin_id: &str) {
        let _ = self.request::<()>("host.retry_plugin", json!([plugin_id])).await;
    }

    pub async fn restart_service(&self, plugin_id: &str) -> Result<()> {
        self.request("host.restart_service", json!([plugin_id])).await
    }

    pub async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        self.request("host.list_threads", json!([])).await
    }

    /// A saved thread, whole (including what the server hasn't written yet), from here on
    /// followed: its session events come after this. The `u64` is how many events it has.
    pub async fn load_thread(&self, thread_id: &str) -> Result<(ThreadMeta, Vec<SessionEvent>, u64)> {
        let (meta, events): (ThreadMeta, Vec<SessionEvent>) = self.request("host.open_thread", json!([thread_id])).await?;
        let count = events.len() as u64;
        Ok((meta, events, count))
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<()> {
        self.request("host.delete_thread", json!([thread_id])).await
    }

    pub async fn flush_saves(&self, timeout: Duration) {
        let _ = self.request::<()>("host.flush_saves", json!([timeout.as_millis() as u64])).await;
    }

    pub async fn thread_attachments_dir(&self, thread_id: &str) -> PathBuf {
        match self.request("host.thread_attachments_dir", json!([thread_id])).await {
            Ok(dir) => dir,
            Err(_) => std::env::temp_dir().join("space-attachments").join(thread_id),
        }
    }

    pub async fn generate_title(&self, session_id: &str, first_message: &str, timeout: Duration) -> Option<String> {
        self.request("host.generate_title", json!([session_id, first_message, timeout.as_millis() as u64])).await.ok().flatten()
    }

    pub async fn complete(&self, session_id: &str, prompt: &str) -> Result<String, String> {
        match self.request::<Result<String, String>>("host.complete", json!([session_id, prompt])).await {
            Ok(result) => result,
            Err(e) => Err(format!("{e:#}")),
        }
    }

    pub async fn trajectory_read(&self, read: TrajectoryRead) -> Result<TrajectoryPage> {
        self.request("host.trajectory_read", json!([read])).await
    }

    pub async fn trajectory_stats(&self, thread_id: &str) -> Result<TrajectoryStats> {
        self.request("host.trajectory_stats", json!([thread_id])).await
    }

    pub async fn trajectory_detail(&self, request: TrajectoryDetailRequest) -> Result<TrajectoryDetail> {
        self.request("host.trajectory_detail", json!([request])).await
    }

    pub async fn terminal_open(&self, open: TerminalOpen) -> Result<TerminalOpened> {
        self.request("host.terminal_open", json!([open])).await
    }

    pub async fn terminal_close(&self, terminal_id: &str) -> Result<()> {
        self.request("host.terminal_close", json!([terminal_id])).await
    }

    pub async fn terminal_attach(&self, terminal_id: &str) -> Result<TerminalReplay> {
        self.request("host.terminal_attach", json!([terminal_id])).await
    }

    pub async fn terminal_list(&self) -> Result<Vec<TerminalInfo>> {
        self.request("host.terminal_list", json!([])).await
    }

    /// Start an agent plugin, or get the one already running.
    pub async fn start_agent(&self, plugin_id: &str) -> Result<AgentHandle> {
        let info: AgentInfo = self.request("host.start_agent", json!([plugin_id])).await?;
        self.inner.alive.lock().unwrap().insert(info.plugin_id.clone());
        Ok(AgentHandle { host: self.clone(), info: Arc::new(info) })
    }

    /// A proxy for a session an earlier app left open.
    pub fn adopt_session(&self, live: &LiveSession) -> Option<Session> {
        let info = self.inner.welcome.agents.iter().find(|a| a.plugin_id == live.plugin_id)?;
        let agent = AgentHandle { host: self.clone(), info: Arc::new(info.clone()) };
        Some(agent.session_from(live))
    }
}

impl Inner {
    fn on_note(&self, method: &str, params: Value, events: &mpsc::UnboundedSender<HostEvent>) {
        match method {
            remote::EVENT => {
                let event: HostEvent = match serde_json::from_value(params) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("unreadable event from the server: {e}");
                        return;
                    }
                };
                match &event {
                    HostEvent::Session { session_id, event: SessionEvent::TurnStarted } => {
                        self.running.lock().unwrap().insert(session_id.clone());
                    }
                    HostEvent::Session { session_id, event: SessionEvent::TurnEnded { .. } } => {
                        self.running.lock().unwrap().remove(session_id);
                    }
                    HostEvent::PluginStatus { plugin_id, status } => {
                        let mut alive = self.alive.lock().unwrap();
                        match status {
                            PluginStatus::Running | PluginStatus::Starting => {}
                            _ => {
                                alive.remove(plugin_id);
                            }
                        }
                        self.state.lock().unwrap().statuses.insert(plugin_id.clone(), status.clone());
                    }
                    _ => {}
                }
                let _ = events.send(event);
            }
            remote::QUITTING => self.server_quitting.store(true, Ordering::SeqCst),
            remote::STATE => match serde_json::from_value::<HostState>(params) {
                Ok(state) => {
                    let changed = self.state.lock().unwrap().update != state.update;
                    *self.state.lock().unwrap() = state;
                    if changed {
                        let _ = events.send(HostEvent::UpdateChanged);
                    }
                }
                Err(e) => tracing::warn!("unreadable state from the server: {e}"),
            },
            remote::RELAUNCH => {
                if let Ok(exe) = serde_json::from_value::<PathBuf>(params) {
                    *self.relaunch.lock().unwrap() = Some(exe);
                    let _ = events.send(HostEvent::UpdateChanged);
                }
            }
            remote::TERMINAL_DATA => {
                let Ok(data) = serde_json::from_value::<TerminalData>(params) else { return };
                let sink = self.sink.lock().unwrap().clone();
                if sink.is_some_and(|sink| sink(&data.id, data.seq, &data.data.0)) {
                    let _ = events.send(HostEvent::TerminalOutput { terminal_id: data.id });
                }
            }
            _ => {}
        }
    }
}

async fn call<R: DeserializeOwned>(peer: &Peer, method: &str, params: Value) -> Result<R> {
    let value = peer.request(method, params, None).await.map_err(|e| anyhow!("{}", e.message))?;
    serde_json::from_value(value).map_err(|e| anyhow!("unreadable answer to {method}: {e}"))
}

async fn tokio_timeout<T>(timeout: Duration, fut: impl std::future::Future<Output = T>) -> Option<T> {
    futures::select_biased! {
        out = fut.fuse() => Some(out),
        _ = futures_timer(timeout).fuse() => None,
    }
}

/// A timer that works off the tokio runtime (the app's thread blocks on it).
async fn futures_timer(d: Duration) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

/// A running agent. Cheap to clone.
#[derive(Clone)]
pub struct AgentHandle {
    host: Host,
    info: Arc<AgentInfo>,
}

impl AgentHandle {
    pub fn plugin_id(&self) -> &str {
        &self.info.plugin_id
    }

    pub fn agent_name(&self) -> Option<&str> {
        self.info.agent_name.as_deref()
    }

    pub fn is_running(&self) -> bool {
        self.host.inner.alive.lock().unwrap().contains(&self.info.plugin_id)
    }

    pub fn can_load_session(&self) -> bool {
        self.info.can_load_session
    }

    pub fn accepts_images(&self) -> bool {
        self.info.accepts_images
    }

    pub fn accepts_embedded_text(&self) -> bool {
        self.info.accepts_embedded_text
    }

    pub fn can_steer(&self) -> bool {
        self.info.can_steer
    }

    pub fn can_rewind(&self) -> bool {
        self.info.can_rewind
    }

    /// Recent stderr lines.
    pub fn logs(&self) -> Vec<String> {
        let call = self.host.request("agent.logs", json!([self.plugin_id()]));
        futures::executor::block_on(tokio_timeout(Duration::from_secs(2), call)).and_then(Result::ok).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.host.inner.alive.lock().unwrap().remove(&self.info.plugin_id);
        self.host.note("agent.stop", json!([self.plugin_id()]));
    }

    pub async fn new_session(&self, cwd: impl Into<PathBuf>) -> Result<Session> {
        self.new_session_in(cwd, None).await
    }

    pub async fn new_session_in(&self, cwd: impl Into<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
        self.create(cwd.into(), thread_id, None, None).await
    }

    pub async fn fork_session(&self, fork: &ForkOf, cwd: impl Into<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
        self.create(cwd.into(), thread_id, None, Some(fork)).await
    }

    pub async fn new_side_session(&self, side_of: &str, cwd: impl Into<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
        self.create(cwd.into(), thread_id, Some(side_of), None).await
    }

    async fn create(&self, cwd: PathBuf, thread_id: Option<&str>, side_of: Option<&str>, fork: Option<&ForkOf>) -> Result<Session> {
        let live: LiveSession = self.host.request("agent.new_session", json!([self.plugin_id(), cwd, thread_id, side_of, fork])).await?;
        Ok(self.session_from(&live))
    }

    pub async fn load_session(&self, session_id: &str, cwd: impl Into<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
        let live: LiveSession = self.host.request("agent.load_session", json!([self.plugin_id(), session_id, cwd.into(), thread_id, None::<String>])).await?;
        Ok(self.session_from(&live))
    }

    pub async fn load_side_session(&self, session_id: &str, side_of: &str, cwd: impl Into<PathBuf>, thread_id: Option<&str>) -> Result<Session> {
        let live: LiveSession = self.host.request("agent.load_session", json!([self.plugin_id(), session_id, cwd.into(), thread_id, side_of])).await?;
        Ok(self.session_from(&live))
    }

    pub fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        self.host.note("agent.resolve_permission", json!([self.plugin_id(), request_id, decision]));
        Ok(())
    }

    fn session_from(&self, live: &LiveSession) -> Session {
        if live.running {
            self.host.inner.running.lock().unwrap().insert(live.session_id.clone());
        }
        Session { inner: Arc::new(SessionInner { id: live.session_id.clone(), agent: self.clone(), mode: Mutex::new(live.mode.clone()) }) }
    }
}

/// A session on an agent. Cheap to clone; dropping the last clone lets the server forget it
/// (unless the app is quitting: then the server keeps it for the next app).
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    id: SessionId,
    agent: AgentHandle,
    mode: Mutex<String>,
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        let host = &self.agent.host;
        if !host.inner.closing.load(Ordering::SeqCst) {
            host.note("session.release", json!([self.id]));
        }
    }
}

impl Session {
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn agent(&self) -> &AgentHandle {
        &self.inner.agent
    }

    fn host(&self) -> &Host {
        &self.inner.agent.host
    }

    pub fn is_running(&self) -> bool {
        self.host().inner.running.lock().unwrap().contains(&self.inner.id)
    }

    pub fn set_permission_mode(&self, mode: impl Into<String>) {
        let mode = mode.into();
        mode.clone_into(&mut self.inner.mode.lock().unwrap());
        self.host().note("session.set_permission_mode", json!([self.id(), mode]));
    }

    pub fn permission_mode(&self) -> String {
        self.inner.mode.lock().unwrap().clone()
    }

    pub fn seed_history(&self, items: &[TranscriptItem]) {
        self.host().note("session.seed_history", json!([self.id(), items]));
    }

    pub async fn prompt(&self, text: impl Into<String>) -> Result<String> {
        self.prompt_with(UserInput::text(text)).await
    }

    pub async fn prompt_with(&self, input: UserInput) -> Result<String> {
        self.host().request("session.prompt", json!([self.id(), input])).await
    }

    pub async fn steer(&self, id: String, input: UserInput) -> Result<()> {
        self.host().request("session.steer", json!([self.id(), id, input])).await
    }

    pub async fn unsteer(&self) -> Result<Vec<String>> {
        self.host().request("session.unsteer", json!([self.id()])).await
    }

    pub async fn rewind(&self, message_id: &str, restore_files: bool) -> Result<RewindResult> {
        self.host().request("session.rewind", json!([self.id(), message_id, restore_files])).await
    }

    pub fn cancel(&self) -> Result<()> {
        self.host().note("session.cancel", json!([self.id()]));
        Ok(())
    }

    pub fn resolve_permission(&self, request_id: u64, decision: PermissionDecision) -> Result<()> {
        self.host().note("session.resolve_permission", json!([self.id(), request_id, decision]));
        Ok(())
    }

    pub async fn configure(&self, request: ConfigureSession) -> Result<SpaceStatus> {
        self.host().request("session.configure", json!([self.id(), request])).await
    }
}

// ----- servers ----------------------------------------------------------------------------

/// A server inside this process, for scenarios, test copies and scripts: it ends with the app,
/// and so do its agents. Must be called inside a tokio runtime.
pub async fn start_private(host: space_core::Host, events: space_core::EventReceiver) -> Result<(Host, EventReceiver)> {
    let server = space_server::Server::start(host, events, space_server::Options::default());
    let (app_side, server_side) = tokio::io::duplex(1 << 20);
    let (server_read, server_write) = tokio::io::split(server_side);
    tokio::spawn(server.serve(server_read, server_write));
    let (read, write) = tokio::io::split(app_side);
    Host::connect(read, write, false).await
}

/// The shared server of the Space folder `space_dir`: started (with its keeper) if it isn't
/// running, from the executables beside this one. Must be called inside a tokio runtime.
pub async fn connect_shared(space_dir: &Path, project: &Path, no_folder: bool) -> Result<(Host, EventReceiver)> {
    let helpers = helpers_dir(&std::env::current_exe()?);
    let mut args = vec!["--space-dir".to_string(), space_dir.display().to_string(), "--project".to_string(), project.display().to_string()];
    if no_folder {
        args.push("--no-folder".into());
    }
    let server = space_keeper::ServerCommand { program: helpers.join("space-server"), args };
    space_server::keeper_host::ensure_running(space_dir, &helpers.join("space-keeper"), &server).await?;
    let socket = remote::socket_path(space_dir);
    let stream = tokio::net::UnixStream::connect(&socket).await?;
    let (read, write) = stream.into_split();
    let (host, events) = Host::connect(read, write, true).await?;
    *host.inner.socket.lock().unwrap() = Some(socket);
    Ok((host, events))
}

/// Where the server and keeper executables are: where the bundled plugins' are (an installed
/// app's `Contents/Helpers`, else the `target/<profile>` this build runs from).
pub fn helpers_dir(_exe: &Path) -> PathBuf {
    space_core::process::target_dir()
}
