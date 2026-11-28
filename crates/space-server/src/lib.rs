//! The Space server: the host ([`space_core::Host`]) with its agents, sessions and saved threads,
//! serving the app over [`space_core::remote`]. It outlives the app: quitting the app leaves its
//! agents working and its threads being saved, and the next app picks everything up.
//!
//! [`Server`] keeps what the app's calls made (agents by plugin, sessions by id) so a later app
//! can find them, and passes each app the host's events: a thread's session events only once that
//! app opened the thread, so it reads the thread whole and then follows it, every event once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::FutureExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use space_core::remote::{self, AgentInfo, HostState, LiveSession, TerminalData, Welcome};
use space_core::{AgentHandle, EventReceiver, Host, HostEvent, Session};
use space_plugin::rpc::{self, Peer, RpcError};
use tokio::io::{AsyncRead, AsyncWrite};

pub mod keeper_host;

/// How long a quitting server waits for its last writes.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// The host and what apps made on it.
pub struct Server {
    host: Host,
    agents: Mutex<HashMap<String, AgentHandle>>,
    sessions: Mutex<HashMap<String, Entry>>,
    conns: Mutex<Vec<Arc<Conn>>>,
    /// Where open sessions are written, for the next server to take over (keeper mode).
    sessions_file: Option<PathBuf>,
    /// What `server.quit` does (stop the keeper and everything with it); `None`: nothing.
    on_quit: Option<Box<dyn Fn() + Send + Sync>>,
    pid: u32,
    /// Where the app's plugin choices are kept, so a server started without an app (after a
    /// crash or an update) starts the same plugins.
    config_file: Option<PathBuf>,
    config: Mutex<SavedConfig>,
}

/// The plugin choices an app last gave ([`Server::config_file`]).
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct SavedConfig {
    pub disabled: Vec<String>,
    pub text_settings: std::collections::BTreeMap<String, space_protocol::service::SettingValues>,
}

struct Entry {
    session: Session,
    plugin_id: String,
    thread_id: Option<String>,
    cwd: PathBuf,
}

/// One connected app.
struct Conn {
    peer: OnceLock<Peer>,
    /// Threads whose session events this app gets.
    opened: Mutex<HashSet<String>>,
    /// Threads being opened: their events start with the host's marker.
    opening: Mutex<HashSet<String>>,
    last_state: Mutex<Option<HostState>>,
}

impl Conn {
    fn notify(&self, method: &str, params: impl Serialize) {
        if let Some(peer) = self.peer.get() {
            peer.notify(method, params);
        }
    }

    fn wants(&self, thread: Option<&str>) -> bool {
        thread.is_none_or(|t| self.opened.lock().unwrap().contains(t))
    }
}

/// A session as written for the next server ([`Server::sessions_file`]).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SavedSession {
    pub session_id: String,
    pub plugin_id: String,
    pub thread_id: Option<String>,
    pub cwd: PathBuf,
    pub mode: String,
}

/// How a server is set up.
#[derive(Default)]
pub struct Options {
    /// Write open sessions here (and read them back when taking over).
    pub sessions_file: Option<PathBuf>,
    pub on_quit: Option<Box<dyn Fn() + Send + Sync>>,
    /// This server's process id to report (0: it runs inside the app).
    pub pid: u32,
    /// Keep the app's plugin choices here ([`Server::apply_saved_config`]).
    pub config_file: Option<PathBuf>,
}

impl Server {
    /// Serve `host`, whose events arrive on `events`. Must be called inside a tokio runtime.
    pub fn start(host: Host, events: EventReceiver, options: Options) -> Arc<Server> {
        let server = Arc::new(Server {
            host: host.clone(),
            agents: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            conns: Mutex::new(Vec::new()),
            sessions_file: options.sessions_file,
            on_quit: options.on_quit,
            pid: options.pid,
            config: Mutex::new(options.config_file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()),
            config_file: options.config_file,
        });
        {
            let weak = Arc::downgrade(&server);
            host.set_terminal_sink(Arc::new(move |id, seq, data| {
                if let Some(server) = weak.upgrade() {
                    let out = TerminalData { id: id.to_string(), seq, data: space_protocol::terminal::Bytes(data.to_vec()) };
                    for conn in server.conns() {
                        conn.notify(remote::TERMINAL_DATA, &out);
                    }
                }
                // The app decides whether to draw again.
                false
            }));
        }
        tokio::spawn(server.clone().pump(events));
        server
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Give the host the plugin choices the last app made, and start the service plugins.
    pub async fn apply_saved_config(&self) {
        let config = self.config.lock().unwrap().clone();
        self.host.set_disabled(config.disabled.iter().cloned());
        for (id, values) in config.text_settings {
            self.host.set_text_settings(&id, values);
        }
        self.host.start_services().await;
    }

    fn save_config(&self, change: impl FnOnce(&mut SavedConfig)) {
        let mut config = self.config.lock().unwrap();
        change(&mut config);
        let Some(file) = &self.config_file else { return };
        let tmp = file.with_extension("tmp");
        let written = serde_json::to_vec_pretty(&*config).map_err(anyhow::Error::from).and_then(|b| Ok(std::fs::write(&tmp, b)?)).and_then(|()| Ok(std::fs::rename(&tmp, file)?));
        if let Err(e) = written {
            tracing::warn!("keeping the plugin choices: {e:#}");
        }
    }

    /// Take over what an earlier server left running: the agents its keeper holds, and the
    /// sessions it wrote down. Their running turns end as they would have.
    pub async fn adopt(&self, pipes: Vec<space_keeper::AgentPipe>) {
        let saved: Vec<SavedSession> = self
            .sessions_file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| tracing::warn!("reading the open sessions: {e}")).ok())
            .unwrap_or_default();
        for pipe in pipes {
            let name = pipe.name.clone();
            match self.host.adopt_agent(pipe).await {
                Ok(agent) => {
                    let plugin_id = agent.plugin_id().to_string();
                    let mut agents = self.agents.lock().unwrap();
                    if let Some(older) = agents.insert(plugin_id.clone(), agent) {
                        // Two of one agent: keep the later one.
                        older.stop();
                    }
                    tracing::info!("took over `{name}`");
                }
                Err(e) => tracing::warn!("couldn't take over `{name}`: {e:#}"),
            }
        }
        let agents = self.agents.lock().unwrap().clone();
        for s in saved {
            let Some(agent) = agents.get(&s.plugin_id) else { continue };
            let session = agent.adopt_session(&s.session_id, &s.cwd, s.thread_id.as_deref());
            session.set_permission_mode(s.mode.clone());
            let entry = Entry { session, plugin_id: s.plugin_id, thread_id: s.thread_id, cwd: s.cwd };
            self.sessions.lock().unwrap().insert(s.session_id, entry);
        }
        self.write_sessions();
    }

    /// Serve one app on `reader`/`writer` until it disconnects.
    pub async fn serve<R, W>(self: Arc<Self>, reader: R, writer: W)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let conn = Arc::new(Conn {
            peer: OnceLock::new(),
            opened: Mutex::new(HashSet::new()),
            opening: Mutex::new(HashSet::new()),
            last_state: Mutex::new(None),
        });
        let handler: rpc::RequestHandler = {
            let (server, conn) = (self.clone(), conn.clone());
            Arc::new(move |method, params| {
                let (server, conn) = (server.clone(), conn.clone());
                async move {
                    let result = server.handle(&conn, &method, params).await;
                    if method != "server.hello" {
                        server.push_state(&conn);
                    }
                    result
                }
                .boxed()
            })
        };
        let notes: rpc::NotificationHandler = {
            let (server, conn) = (self.clone(), conn.clone());
            Arc::new(move |method, params| {
                server.handle_note(&method, params);
                server.push_state(&conn);
            })
        };
        let (peer, task) = Peer::spawn(reader, writer, handler, notes);
        let _ = conn.peer.set(peer);
        self.conns.lock().unwrap().push(conn.clone());
        let _ = task.await;
        self.conns.lock().unwrap().retain(|c| !Arc::ptr_eq(c, &conn));
        tracing::info!("an app disconnected");
    }

    /// Accept apps on `socket` until `stop` resolves.
    pub async fn listen(self: Arc<Self>, socket: &Path) -> Result<()> {
        if socket.exists() {
            if tokio::net::UnixStream::connect(socket).await.is_ok() {
                anyhow::bail!("a server is already running on {}", socket.display());
            }
            std::fs::remove_file(socket)?;
        }
        let listener = tokio::net::UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        loop {
            let (stream, _) = listener.accept().await?;
            let (read, write) = stream.into_split();
            tokio::spawn(self.clone().serve(read, write));
        }
    }

    fn conns(&self) -> Vec<Arc<Conn>> {
        self.conns.lock().unwrap().clone()
    }

    async fn pump(self: Arc<Self>, mut events: EventReceiver) {
        while let Some(event) = events.recv().await {
            let conns = self.conns();
            match &event {
                HostEvent::ThreadOpened { thread_id } => {
                    for conn in &conns {
                        if conn.opening.lock().unwrap().remove(thread_id) {
                            conn.opened.lock().unwrap().insert(thread_id.clone());
                        }
                    }
                    continue;
                }
                HostEvent::Session { session_id, .. } => {
                    let thread = self.host.session_thread(session_id);
                    for conn in conns.iter().filter(|c| c.wants(thread.as_deref())) {
                        conn.notify(remote::EVENT, &event);
                    }
                    continue;
                }
                _ => {}
            }
            for conn in &conns {
                conn.notify(remote::EVENT, &event);
            }
            if matches!(event, HostEvent::PluginStatus { .. } | HostEvent::Catalog { .. } | HostEvent::PluginSettings { .. }) {
                for conn in &conns {
                    self.push_state(conn);
                }
            }
        }
    }

    /// What an app reads without asking.
    pub fn state(&self) -> HostState {
        let host = &self.host;
        let plugins = host.plugins();
        let mut state = HostState {
            has_session_store: host.has_session_store(),
            has_terminal: host.has_terminal(),
            has_trajectory: host.has_trajectory(),
            terminal_plugin: host.terminal_plugin(),
            project_dir: host.project_dir().to_path_buf(),
            ..Default::default()
        };
        for p in &plugins {
            let id = p.id().to_string();
            if let Some(status) = host.status(&id) {
                state.statuses.insert(id.clone(), status);
            }
            if host.is_disabled(&id) {
                state.disabled.push(id.clone());
            }
            state.approvals.insert(id.clone(), host.approval(&id));
            let settings = host.plugin_settings(&id);
            if !settings.is_empty() {
                state.plugin_settings.insert(id.clone(), settings);
            }
            for name in p.manifest.provides.iter().filter_map(|c| c.strip_prefix("catalog:")) {
                state.catalogs.insert(name.to_string(), host.catalog(name));
            }
        }
        state.plugins = plugins;
        state
    }

    fn push_state(&self, conn: &Conn) {
        let state = self.state();
        let mut last = conn.last_state.lock().unwrap();
        if last.as_ref() != Some(&state) {
            conn.notify(remote::STATE, &state);
            *last = Some(state);
        }
    }

    fn live_sessions(&self) -> Vec<LiveSession> {
        let sessions = self.sessions.lock().unwrap();
        let mut live: Vec<_> = sessions
            .values()
            .filter(|e| e.session.agent().is_running())
            .map(|e| LiveSession {
                session_id: e.session.id().to_string(),
                plugin_id: e.plugin_id.clone(),
                thread_id: e.thread_id.clone(),
                cwd: e.cwd.clone(),
                running: e.session.is_running(),
                mode: e.session.permission_mode(),
            })
            .collect();
        live.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        live
    }

    fn write_sessions(&self) {
        let Some(file) = &self.sessions_file else { return };
        let saved: Vec<SavedSession> = self
            .live_sessions()
            .into_iter()
            .map(|s| SavedSession { session_id: s.session_id, plugin_id: s.plugin_id, thread_id: s.thread_id, cwd: s.cwd, mode: s.mode })
            .collect();
        let tmp = file.with_extension("tmp");
        let written = serde_json::to_vec_pretty(&saved).map_err(anyhow::Error::from).and_then(|b| Ok(std::fs::write(&tmp, b)?)).and_then(|()| Ok(std::fs::rename(&tmp, file)?));
        if let Err(e) = written {
            tracing::warn!("writing the open sessions: {e:#}");
        }
    }

    fn add_session(&self, conn: &Conn, session: Session, plugin_id: &str, thread_id: Option<String>, cwd: PathBuf) -> LiveSession {
        if let Some(t) = &thread_id {
            conn.opened.lock().unwrap().insert(t.clone());
        }
        let live = LiveSession {
            session_id: session.id().to_string(),
            plugin_id: plugin_id.to_string(),
            thread_id: thread_id.clone(),
            cwd: cwd.clone(),
            running: session.is_running(),
            mode: session.permission_mode(),
        };
        self.sessions.lock().unwrap().insert(live.session_id.clone(), Entry { session, plugin_id: plugin_id.to_string(), thread_id, cwd });
        self.write_sessions();
        live
    }

    fn session(&self, id: &str) -> Result<Session, RpcError> {
        self.sessions.lock().unwrap().get(id).map(|e| e.session.clone()).ok_or_else(|| RpcError::invalid_params(format!("no session {id}")))
    }

    fn agent(&self, plugin_id: &str) -> Result<AgentHandle, RpcError> {
        self.agents.lock().unwrap().get(plugin_id).cloned().ok_or_else(|| RpcError::invalid_params(format!("agent `{plugin_id}` isn't running")))
    }

    fn agent_info(agent: &AgentHandle) -> AgentInfo {
        AgentInfo {
            plugin_id: agent.plugin_id().to_string(),
            agent_name: agent.agent_name().map(str::to_string),
            can_load_session: agent.can_load_session(),
            accepts_images: agent.accepts_images(),
            accepts_embedded_text: agent.accepts_embedded_text(),
            can_steer: agent.can_steer(),
            can_rewind: agent.can_rewind(),
        }
    }

    fn welcome(&self) -> Welcome {
        let agents = self.agents.lock().unwrap().values().filter(|a| a.is_running()).map(Self::agent_info).collect();
        Welcome {
            protocol: remote::SERVER_PROTOCOL,
            pid: self.pid,
            version: space_core::build_info::current().version.clone(),
            state: self.state(),
            agents,
            sessions: self.live_sessions(),
        }
    }

    /// A call that needs no answer, handled at once and in order with the app's other calls
    /// (a setting before the services start, keys in the order typed).
    fn handle_note(&self, method: &str, params: Value) {
        let host = &self.host;
        match method {
            "host.discover" => {
                host.discover();
            }
            "host.set_disabled" => {
                let Ok((ids,)) = args::<(Vec<String>,)>(params) else { return };
                self.save_config(|c| c.disabled = ids.clone());
                host.set_disabled(ids);
            }
            "host.set_text_settings" => {
                let Ok((plugin_id, values)) = args::<(String, space_protocol::service::SettingValues)>(params) else { return };
                self.save_config(|c| {
                    c.text_settings.insert(plugin_id.clone(), values.clone());
                });
                host.set_text_settings(&plugin_id, values);
            }
            "host.set_watching" => {
                let Ok((session_id,)) = args::<(Option<String>,)>(params) else { return };
                host.set_watching(session_id);
            }
            "host.answer_approval" => {
                let Ok((id, approved)) = args::<(String, bool)>(params) else { return };
                if let Err(e) = host.answer_approval(&id, approved) {
                    tracing::warn!("{e:#}");
                }
            }
            "host.save_thread_meta" => {
                let Ok((meta,)) = args::<(space_protocol::service::ThreadMeta,)>(params) else { return };
                host.save_thread_meta(meta);
            }
            "host.record_event" => {
                let Ok((thread_id, event)) = args::<(String, space_core::SessionEvent)>(params) else { return };
                host.record_event(&thread_id, &event);
            }
            "host.thread_renamed" => {
                let Ok((session_id, title)) = args::<(String, String)>(params) else { return };
                host.thread_renamed(&session_id, &title);
            }
            "host.terminal_input" => {
                let Ok((id, data)) = args::<(String, space_protocol::terminal::Bytes)>(params) else { return };
                host.terminal_input(&id, data.0);
            }
            "host.terminal_resize" => {
                let Ok((id, cols, rows)) = args::<(String, u16, u16)>(params) else { return };
                host.terminal_resize(&id, cols, rows);
            }
            "agent.stop" => {
                let Ok((plugin_id,)) = args::<(String,)>(params) else { return };
                if let Some(agent) = self.agents.lock().unwrap().remove(&plugin_id) {
                    agent.stop();
                }
                self.sessions.lock().unwrap().retain(|_, e| e.plugin_id != plugin_id);
                self.write_sessions();
            }
            "agent.resolve_permission" => {
                let Ok((plugin_id, request_id, decision)) = args::<(String, u64, space_core::PermissionDecision)>(params) else { return };
                if let Err(e) = match self.agent(&plugin_id) { Ok(a) => a, Err(_) => return }.resolve_permission(request_id, decision) {
                    tracing::warn!("{method}: {e:#}");
                }
            }
            "session.cancel" => {
                let Ok((id,)) = args::<(String,)>(params) else { return };
                if let Err(e) = match self.session(&id) { Ok(s) => s, Err(_) => return }.cancel() {
                    tracing::warn!("{method}: {e:#}");
                }
            }
            "session.resolve_permission" => {
                let Ok((id, request_id, decision)) = args::<(String, u64, space_core::PermissionDecision)>(params) else { return };
                if let Err(e) = match self.session(&id) { Ok(s) => s, Err(_) => return }.resolve_permission(request_id, decision) {
                    tracing::warn!("{method}: {e:#}");
                }
            }
            "session.set_permission_mode" => {
                let Ok((id, mode)) = args::<(String, String)>(params) else { return };
                match self.session(&id) { Ok(s) => s, Err(_) => return }.set_permission_mode(mode);
                self.write_sessions();
            }
            "session.seed_history" => {
                let Ok((id, items)) = args::<(String, Vec<space_core::TranscriptItem>)>(params) else { return };
                match self.session(&id) { Ok(s) => s, Err(_) => return }.seed_history(&items);
            }
            "session.release" => {
                let Ok((id,)) = args::<(String,)>(params) else { return };
                let released = self.sessions.lock().unwrap().remove(&id).is_some();
                if released {
                    self.write_sessions();
                }
            }
            other => tracing::warn!("unknown notification `{other}`"),
        }
    }

    async fn handle(&self, conn: &Conn, method: &str, params: Value) -> Result<Value, RpcError> {
        let host = &self.host;
        match method {
            "server.hello" => {
                let protocol: (u32,) = args(params)?;
                if protocol.0 != remote::SERVER_PROTOCOL {
                    return Err(RpcError::new(-32001, format!("this server speaks protocol {}", remote::SERVER_PROTOCOL)));
                }
                let welcome = self.welcome();
                *conn.last_state.lock().unwrap() = Some(welcome.state.clone());
                ok(welcome)
            }
            "server.quit" => {
                host.flush_saves(FLUSH_TIMEOUT).await;
                match &self.on_quit {
                    Some(quit) => quit(),
                    None => host.shutdown(),
                }
                ok(())
            }
            "host.start_services" => {
                host.start_services().await;
                ok(())
            }
            "host.shutdown" => {
                host.shutdown();
                ok(())
            }
            "host.set_setting" => {
                let (plugin_id, key, value, force): (String, String, Option<String>, bool) = args(params)?;
                ok(host.set_setting(&plugin_id, &key, value, force).await)
            }
            "host.retry_plugin" => {
                let (id,): (String,) = args(params)?;
                host.retry_plugin(&id).await;
                ok(())
            }
            "host.restart_service" => {
                let (id,): (String,) = args(params)?;
                host.restart_service(&id).await.map_err(internal)?;
                ok(())
            }
            "host.list_threads" => ok(host.list_threads().await.map_err(internal)?),
            "host.open_thread" => {
                let (id,): (String,) = args(params)?;
                conn.opening.lock().unwrap().insert(id.clone());
                let opened = host.open_thread(&id).await;
                if opened.is_err() {
                    conn.opening.lock().unwrap().remove(&id);
                }
                ok(opened.map_err(internal)?)
            }
            "host.delete_thread" => {
                let (id,): (String,) = args(params)?;
                host.delete_thread(&id).await.map_err(internal)?;
                ok(())
            }
            "host.flush_saves" => {
                let (ms,): (u64,) = args(params)?;
                host.flush_saves(Duration::from_millis(ms)).await;
                ok(())
            }
            "host.thread_attachments_dir" => {
                let (id,): (String,) = args(params)?;
                ok(host.thread_attachments_dir(&id).await)
            }
            "host.generate_title" => {
                let (session_id, first, ms): (String, String, u64) = args(params)?;
                ok(host.generate_title(&session_id, &first, Duration::from_millis(ms)).await)
            }
            "host.complete" => {
                let (session_id, prompt): (String, String) = args(params)?;
                ok(host.complete(&session_id, &prompt).await)
            }
            "host.trajectory_read" => {
                let (read,): (space_protocol::trace::TrajectoryRead,) = args(params)?;
                ok(host.trajectory_read(read).await.map_err(internal)?)
            }
            "host.trajectory_stats" => {
                let (id,): (String,) = args(params)?;
                ok(host.trajectory_stats(&id).await.map_err(internal)?)
            }
            "host.trajectory_detail" => {
                let (req,): (space_protocol::trace::TrajectoryDetailRequest,) = args(params)?;
                ok(host.trajectory_detail(req).await.map_err(internal)?)
            }
            "host.terminal_open" => {
                let (open,): (space_protocol::terminal::TerminalOpen,) = args(params)?;
                ok(host.terminal_open(open).await.map_err(internal)?)
            }
            "host.terminal_close" => {
                let (id,): (String,) = args(params)?;
                host.terminal_close(&id).await.map_err(internal)?;
                ok(())
            }
            "host.terminal_attach" => {
                let (id,): (String,) = args(params)?;
                ok(host.terminal_attach(&id).await.map_err(internal)?)
            }
            "host.terminal_list" => ok(host.terminal_list().await.map_err(internal)?),
            "host.start_agent" => {
                let (plugin_id,): (String,) = args(params)?;
                let running = self.agents.lock().unwrap().get(&plugin_id).filter(|a| a.is_running()).cloned();
                let agent = match running {
                    Some(agent) => agent,
                    None => {
                        let agent = host.start_agent(&plugin_id).await.map_err(internal)?;
                        self.agents.lock().unwrap().insert(plugin_id.clone(), agent.clone());
                        agent
                    }
                };
                ok(Self::agent_info(&agent))
            }
            "agent.logs" => {
                let (plugin_id,): (String,) = args(params)?;
                ok(self.agents.lock().unwrap().get(&plugin_id).map(|a| a.logs()).unwrap_or_default())
            }
            "agent.new_session" => {
                let (plugin_id, cwd, thread_id, side_of, fork): (String, PathBuf, Option<String>, Option<String>, Option<space_protocol::ForkOf>) = args(params)?;
                let agent = self.agent(&plugin_id)?;
                if let Some(t) = &thread_id {
                    conn.opened.lock().unwrap().insert(t.clone());
                }
                let session = match (&side_of, &fork) {
                    (_, Some(fork)) => agent.fork_session(fork, &cwd, thread_id.as_deref()).await,
                    (Some(main), None) => agent.new_side_session(main, &cwd, thread_id.as_deref()).await,
                    (None, None) => agent.new_session_in(&cwd, thread_id.as_deref()).await,
                }
                .map_err(internal)?;
                ok(self.add_session(conn, session, &plugin_id, thread_id, cwd))
            }
            "agent.load_session" => {
                let (plugin_id, session_id, cwd, thread_id, side_of): (String, String, PathBuf, Option<String>, Option<String>) = args(params)?;
                let agent = self.agent(&plugin_id)?;
                if let Some(t) = &thread_id {
                    conn.opened.lock().unwrap().insert(t.clone());
                }
                let session = match &side_of {
                    Some(main) => agent.load_side_session(&session_id, main, &cwd, thread_id.as_deref()).await,
                    None => agent.load_session(&session_id, &cwd, thread_id.as_deref()).await,
                }
                .map_err(internal)?;
                ok(self.add_session(conn, session, &plugin_id, thread_id, cwd))
            }
            "session.prompt" => {
                let (id, input): (String, space_core::UserInput) = args(params)?;
                ok(self.session(&id)?.prompt_with(input).await.map_err(internal)?)
            }
            "session.steer" => {
                let (id, steer_id, input): (String, String, space_core::UserInput) = args(params)?;
                self.session(&id)?.steer(steer_id, input).await.map_err(internal)?;
                ok(())
            }
            "session.unsteer" => {
                let (id,): (String,) = args(params)?;
                ok(self.session(&id)?.unsteer().await.map_err(internal)?)
            }
            "session.rewind" => {
                let (id, message_id, restore): (String, String, bool) = args(params)?;
                ok(self.session(&id)?.rewind(&message_id, restore).await.map_err(internal)?)
            }
            "session.configure" => {
                let (id, request): (String, space_protocol::ConfigureSession) = args(params)?;
                ok(self.session(&id)?.configure(request).await.map_err(internal)?)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

fn args<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    rpc::params(params)
}

fn ok<T: Serialize>(value: T) -> Result<Value, RpcError> {
    rpc::result(value)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(format!("{e:#}").trim_end_matches(':').to_string())
}

/// Turn an error from a call into an `anyhow` error.
pub fn rpc_error(e: RpcError) -> anyhow::Error {
    anyhow!("{}", e.message)
}
