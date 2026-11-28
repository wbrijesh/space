//! The keeper process: the parent of every agent, so agents outlive the server.
//!
//! It does three things and nothing else:
//!
//! 1. **Parent.** It starts each agent (as the server describes it) and holds its pipes, which
//!    it never closes: agents keep running while servers come and go.
//! 2. **Relay with a waiting room.** It passes lines between each agent and the connected
//!    server. A line from an agent is kept until the server acknowledges it, and sent again to
//!    the next server if it isn't; past [`QUEUE_LIMIT`] the keeper stops reading the agent, which
//!    then waits. Nothing is dropped.
//! 3. **Notepad.** It reads just enough of each JSON-RPC line (`id`, `method`) to know which
//!    requests are open in each direction, and smooths a change of server over:
//!    - each server numbers its requests its own way; the keeper gives the agent ids unique for
//!      its whole life, and maps replies back;
//!    - a reply to a request from a server that's gone becomes an [`ORPHAN_REPLY`] notification
//!      (a running turn's end reaches the new server);
//!    - the agent's first `initialize` answer is kept, and a later server's `initialize` is
//!      answered from it (an agent is initialized once);
//!    - an agent's request an earlier server received but never answered (a tool call waiting
//!      for the guard chain or for you) is sent again to the next server.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::{Notify, mpsc};

use crate::proto::*;

/// Unacknowledged bytes from one agent before the keeper stops reading it.
pub const QUEUE_LIMIT: usize = 64 << 20;
/// Stderr lines kept per agent for a new server.
const STDERR_KEEP: usize = 200;

/// Run the keeper on `socket` until a server sends [`ToKeeper::Quit`]. Fails if another keeper
/// already answers there.
pub async fn run(socket: &Path) -> Result<()> {
    let listener = bind(socket).await?;
    let keeper = Arc::new(Keeper { state: Mutex::new(State::default()), quit: Notify::new() });
    tracing::info!("keeper {} listening on {}", std::process::id(), socket.display());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accepting a server")?;
                tokio::spawn(keeper.clone().serve(stream));
            }
            _ = keeper.quit.notified() => break,
        }
    }
    keeper.stop_all().await;
    let _ = std::fs::remove_file(socket);
    tracing::info!("keeper quit");
    Ok(())
}

/// Listen on `socket`, replacing a stale socket file but never a live keeper.
async fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        if UnixStream::connect(socket).await.is_ok() {
            bail!("a keeper is already running on {}", socket.display());
        }
        std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

struct Keeper {
    state: Mutex<State>,
    quit: Notify,
}

#[derive(Default)]
struct State {
    /// Bumped for every server that connects.
    generation: u64,
    conn: Option<Conn>,
    agents: HashMap<String, Agent>,
}

/// The connected server.
struct Conn {
    generation: u64,
    tx: mpsc::UnboundedSender<String>,
}

impl Conn {
    fn send(&self, msg: &FromKeeper) {
        let _ = self.tx.send(serde_json::to_string(msg).expect("serializable"));
    }
}

struct Agent {
    pid: u32,
    stdin: mpsc::UnboundedSender<String>,
    exited: Option<Option<i32>>,
    /// Number of the last line read from the agent.
    seq: u64,
    /// Every line up to here was received by a server.
    acked: u64,
    /// Lines not yet acknowledged, oldest first.
    queue: VecDeque<Entry>,
    queue_bytes: usize,
    /// Wakes the agent's reader once the queue has room again.
    room: Arc<Notify>,
    /// The agent's requests a server received but hasn't answered, by line number.
    open_from_agent: BTreeMap<u64, (String, String)>,
    /// The agent's requests answered while still queued (by id key).
    answered: HashSet<String>,
    /// The servers' requests the agent hasn't answered, by the id the agent saw.
    pending: HashMap<u64, Pending>,
    next_id: u64,
    /// The agent's answer to its first `initialize`.
    init: Option<Value>,
    stderr: VecDeque<String>,
}

struct Entry {
    seq: u64,
    line: String,
    kind: Kind,
}

enum Kind {
    /// A request from the agent, with its id key.
    Request(String),
    /// A reply to the request the agent knows as this id.
    Response(u64),
    Other,
}

/// A server's request on its way to, or at, the agent.
struct Pending {
    generation: u64,
    id: Value,
    method: String,
    session_id: Option<String>,
}

/// A JSON-RPC id as a map key.
fn id_key(id: &Value) -> String {
    id.to_string()
}

impl Agent {
    /// The line as the server of `generation` should get it.
    fn deliver(&self, entry: &Entry, generation: u64) -> String {
        if let Kind::Response(id) = entry.kind
            && let Some(p) = self.pending.get(&id)
            && let Ok(mut v) = serde_json::from_str::<Value>(&entry.line)
        {
            if p.generation == generation {
                v["id"] = p.id.clone();
                return v.to_string();
            }
            let reply = OrphanReply { method: p.method.clone(), session_id: p.session_id.clone(), result: v.get("result").cloned(), error: v.get("error").cloned() };
            return json!({"jsonrpc": "2.0", "method": ORPHAN_REPLY, "params": reply}).to_string();
        }
        entry.line.clone()
    }

    fn info(&self, name: &str, generation: u64) -> AgentInfo {
        let mut open: Vec<_> = self.pending.iter().filter(|(_, p)| p.generation < generation && p.method != "initialize").collect();
        open.sort_by_key(|(id, _)| **id);
        AgentInfo {
            agent: name.to_string(),
            pid: self.pid,
            exited: self.exited.map(|code| Exit { code }),
            open: open.into_iter().map(|(_, p)| OpenRequest { method: p.method.clone(), session_id: p.session_id.clone() }).collect(),
            stderr: self.stderr.iter().cloned().collect(),
        }
    }

    /// Everything a new server must get from this agent: requests the last server never answered,
    /// then every line not yet acknowledged.
    fn backlog(&self, name: &str, generation: u64) -> Vec<FromKeeper> {
        let open = self.open_from_agent.iter().map(|(seq, (_, line))| (*seq, line.clone()));
        let queued = self.queue.iter().map(|e| (e.seq, self.deliver(e, generation)));
        open.chain(queued).map(|(seq, line)| FromKeeper::Recv { agent: name.to_string(), seq: Some(seq), line }).collect()
    }

    /// A line from the agent: numbered, classified and queued until acknowledged.
    fn read(&mut self, line: String) {
        self.seq += 1;
        let kind = match serde_json::from_str::<Value>(&line) {
            Ok(v) => match (v.get("method").is_some(), v.get("id")) {
                (true, Some(id)) => Kind::Request(id_key(id)),
                (false, Some(id)) => match id.as_u64().filter(|id| self.pending.contains_key(id)) {
                    Some(id) => {
                        if self.pending[&id].method == "initialize" && self.init.is_none() {
                            self.init = v.get("result").cloned();
                        }
                        Kind::Response(id)
                    }
                    None => Kind::Other,
                },
                _ => Kind::Other,
            },
            Err(_) => Kind::Other,
        };
        self.queue_bytes += line.len();
        self.queue.push_back(Entry { seq: self.seq, line, kind });
    }

    fn ack(&mut self, seq: u64) {
        if seq <= self.acked {
            return;
        }
        self.acked = seq.min(self.seq);
        while self.queue.front().is_some_and(|e| e.seq <= self.acked) {
            let entry = self.queue.pop_front().expect("front");
            self.queue_bytes -= entry.line.len();
            match entry.kind {
                Kind::Request(key) => {
                    if !self.answered.remove(&key) {
                        self.open_from_agent.insert(entry.seq, (key, entry.line));
                    }
                }
                Kind::Response(id) => {
                    self.pending.remove(&id);
                }
                Kind::Other => {}
            }
        }
        if self.queue_bytes < QUEUE_LIMIT {
            self.room.notify_one();
        }
    }

    /// A line from the server: translate its request id, answer `initialize` from the cache, or
    /// note an answer to one of the agent's requests. Returns a line the keeper answers itself.
    fn on_server_line(&mut self, generation: u64, line: &str) -> Option<String> {
        let Ok(mut v) = serde_json::from_str::<Value>(line) else {
            let _ = self.stdin.send(line.to_string());
            return None;
        };
        let method = v.get("method").and_then(Value::as_str).map(str::to_string);
        match (method, v.get("id").cloned()) {
            (Some(method), Some(id)) => {
                if method == "initialize"
                    && let Some(init) = &self.init
                {
                    return Some(json!({"jsonrpc": "2.0", "id": id, "result": init}).to_string());
                }
                self.next_id += 1;
                let session_id = v.pointer("/params/sessionId").and_then(Value::as_str).map(str::to_string);
                self.pending.insert(self.next_id, Pending { generation, id, method, session_id });
                v["id"] = json!(self.next_id);
                let _ = self.stdin.send(v.to_string());
            }
            (None, Some(id)) => {
                let key = id_key(&id);
                let before = self.open_from_agent.len();
                self.open_from_agent.retain(|_, (k, _)| *k != key);
                if self.open_from_agent.len() == before {
                    self.answered.insert(key);
                }
                let _ = self.stdin.send(line.to_string());
            }
            _ => {
                let _ = self.stdin.send(line.to_string());
            }
        }
        None
    }
}

impl Keeper {
    async fn serve(self: Arc<Self>, stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let hello = match lines.next_line().await {
            Ok(Some(line)) => serde_json::from_str::<ToKeeper>(&line).ok(),
            _ => return,
        };
        if !matches!(hello, Some(ToKeeper::Hello { protocol: KEEPER_PROTOCOL })) {
            let reason = format!("this keeper speaks protocol {KEEPER_PROTOCOL}");
            let refused = serde_json::to_string(&FromKeeper::Refused { reason }).expect("serializable");
            let _ = write.write_all(format!("{refused}\n").as_bytes()).await;
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let generation = {
            let mut state = self.state.lock().unwrap();
            state.generation += 1;
            let generation = state.generation;
            let conn = Conn { generation, tx };
            let mut names: Vec<_> = state.agents.keys().cloned().collect();
            names.sort();
            let agents = names.iter().map(|n| state.agents[n].info(n, generation)).collect();
            conn.send(&FromKeeper::Welcome { protocol: KEEPER_PROTOCOL, pid: std::process::id(), agents });
            for name in &names {
                for msg in state.agents[name].backlog(name, generation) {
                    conn.send(&msg);
                }
            }
            // The server before this one is replaced: dropping its sender ends its writer.
            state.conn = Some(conn);
            generation
        };
        tracing::info!("server connected (generation {generation})");
        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write.write_all(line.as_bytes()).await.is_err() || write.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        });
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<ToKeeper>(&line) {
                Ok(msg) => self.clone().handle(generation, msg),
                Err(e) => tracing::warn!("unreadable message from the server: {e}"),
            }
            if !self.is_current(generation) {
                break;
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.conn.as_ref().is_some_and(|c| c.generation == generation) {
                state.conn = None;
            }
        }
        writer.abort();
        tracing::info!("server disconnected (generation {generation})");
    }

    fn is_current(&self, generation: u64) -> bool {
        self.state.lock().unwrap().conn.as_ref().is_some_and(|c| c.generation == generation)
    }

    fn handle(self: Arc<Self>, generation: u64, msg: ToKeeper) {
        match msg {
            ToKeeper::Hello { .. } => {}
            ToKeeper::Spawn { agent, spec } => self.spawn(agent, spec),
            ToKeeper::Send { agent, line } => {
                let mut state = self.state.lock().unwrap();
                let current = state.conn.as_ref().filter(|c| c.generation == generation).map(|c| c.tx.clone());
                let Some(a) = state.agents.get_mut(&agent) else { return };
                if let Some(answer) = a.on_server_line(generation, &line)
                    && let Some(tx) = current
                {
                    let msg = FromKeeper::Recv { agent, seq: None, line: answer };
                    let _ = tx.send(serde_json::to_string(&msg).expect("serializable"));
                }
            }
            ToKeeper::Ack { agent, seq } => {
                if let Some(a) = self.state.lock().unwrap().agents.get_mut(&agent) {
                    a.ack(seq);
                }
            }
            ToKeeper::Kill { agent } => {
                let mut state = self.state.lock().unwrap();
                match state.agents.get(&agent) {
                    Some(a) if a.exited.is_some() => {
                        state.agents.remove(&agent);
                    }
                    Some(a) => kill(a.pid),
                    None => {}
                }
            }
            ToKeeper::Quit => self.quit.notify_one(),
        }
    }

    fn spawn(self: Arc<Self>, name: String, spec: SpawnSpec) {
        let result = self.clone().start(&name, spec);
        let state = self.state.lock().unwrap();
        if let Some(conn) = &state.conn {
            match result {
                Ok(pid) => conn.send(&FromKeeper::Spawned { agent: name, pid }),
                Err(e) => conn.send(&FromKeeper::SpawnFailed { agent: name, error: format!("{e:#}") }),
            }
        }
    }

    fn start(self: Arc<Self>, name: &str, spec: SpawnSpec) -> Result<u32> {
        if self.state.lock().unwrap().agents.get(name).is_some_and(|a| a.exited.is_none()) {
            bail!("`{name}` is already running");
        }
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", spec.program.display()))?;
        let pid = child.id().context("no pid")?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let room = Arc::new(Notify::new());
        self.state.lock().unwrap().agents.insert(
            name.to_string(),
            Agent {
                pid,
                stdin: stdin_tx,
                exited: None,
                seq: 0,
                acked: 0,
                queue: VecDeque::new(),
                queue_bytes: 0,
                room: room.clone(),
                open_from_agent: BTreeMap::new(),
                answered: HashSet::new(),
                pending: HashMap::new(),
                next_id: 0,
                init: None,
                stderr: VecDeque::new(),
            },
        );
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let stdout_done = tokio::spawn(self.clone().read_stdout(name.to_string(), stdout, room));
        let stderr_done = tokio::spawn(self.clone().read_stderr(name.to_string(), stderr));
        let keeper = self.clone();
        let agent = name.to_string();
        tokio::spawn(async move {
            let name = agent;
            let status = child.wait().await;
            let _ = stdout_done.await;
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), stderr_done).await;
            let code = status.ok().and_then(|s| s.code());
            let mut state = keeper.state.lock().unwrap();
            let conn = state.conn.as_ref().map(|c| c.tx.clone());
            if let Some(a) = state.agents.get_mut(&name) {
                a.exited = Some(code);
                if let Some(tx) = conn {
                    let _ = tx.send(serde_json::to_string(&FromKeeper::Exited { agent: name.clone(), code }).expect("serializable"));
                }
            }
            tracing::info!("`{name}` exited ({code:?})");
        });
        tracing::info!("started `{name}` (pid {pid})");
        Ok(pid)
    }

    async fn read_stdout(self: Arc<Self>, name: String, stdout: tokio::process::ChildStdout, room: Arc<Notify>) {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            loop {
                let full = self.state.lock().unwrap().agents.get(&name).is_some_and(|a| a.queue_bytes >= QUEUE_LIMIT);
                if !full {
                    break;
                }
                room.notified().await;
            }
            let Ok(Some(line)) = lines.next_line().await else { break };
            if line.trim().is_empty() {
                continue;
            }
            let mut state = self.state.lock().unwrap();
            let State { conn, agents, .. } = &mut *state;
            let Some(a) = agents.get_mut(&name) else { break };
            a.read(line);
            if let Some(conn) = conn {
                let entry = a.queue.back().expect("just read");
                conn.send(&FromKeeper::Recv { agent: name.clone(), seq: Some(entry.seq), line: a.deliver(entry, conn.generation) });
            }
        }
    }

    async fn read_stderr(self: Arc<Self>, name: String, stderr: tokio::process::ChildStderr) {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut state = self.state.lock().unwrap();
            let State { conn, agents, .. } = &mut *state;
            let Some(a) = agents.get_mut(&name) else { break };
            if a.stderr.len() == STDERR_KEEP {
                a.stderr.pop_front();
            }
            a.stderr.push_back(line.clone());
            if let Some(conn) = conn {
                conn.send(&FromKeeper::Stderr { agent: name.clone(), line });
            }
        }
    }

    /// Quit: stop every agent, politely first.
    async fn stop_all(&self) {
        let pids: Vec<u32> = self.state.lock().unwrap().agents.values().filter(|a| a.exited.is_none()).map(|a| a.pid).collect();
        for pid in &pids {
            signal(*pid, libc::SIGTERM);
        }
        for _ in 0..20 {
            if self.state.lock().unwrap().agents.values().all(|a| a.exited.is_some()) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        for pid in pids {
            signal(pid, libc::SIGKILL);
        }
    }
}

fn kill(pid: u32) {
    signal(pid, libc::SIGKILL);
}

fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: kill(2) with a pid this keeper started.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

/// Where a keeper's socket lives in a Space data folder.
pub fn socket_path(space_dir: &Path) -> PathBuf {
    space_dir.join("keeper.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> (Agent, mpsc::UnboundedReceiver<String>) {
        let (stdin, rx) = mpsc::unbounded_channel();
        let a = Agent {
            pid: 1,
            stdin,
            exited: None,
            seq: 0,
            acked: 0,
            queue: VecDeque::new(),
            queue_bytes: 0,
            room: Arc::new(Notify::new()),
            open_from_agent: BTreeMap::new(),
            answered: HashSet::new(),
            pending: HashMap::new(),
            next_id: 0,
            init: None,
            stderr: VecDeque::new(),
        };
        (a, rx)
    }

    /// A line from the agent, as the reader takes it.
    fn from_agent(a: &mut Agent, line: Value) -> usize {
        a.read(line.to_string());
        a.queue.len() - 1
    }

    fn json(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn ids_are_translated_and_a_gone_servers_reply_is_an_orphan() {
        let (mut a, mut stdin) = agent();
        // Server 1 starts a turn as its id 0; server 2 as its id 0 too.
        assert!(a.on_server_line(1, r#"{"jsonrpc":"2.0","id":0,"method":"session/prompt","params":{"sessionId":"s1"}}"#).is_none());
        assert!(a.on_server_line(2, r#"{"jsonrpc":"2.0","id":0,"method":"session/prompt","params":{"sessionId":"s2"}}"#).is_none());
        let (first, second) = (json(&stdin.try_recv().unwrap()), json(&stdin.try_recv().unwrap()));
        assert_eq!((first["id"].as_u64(), second["id"].as_u64()), (Some(1), Some(2)), "unique for the agent");

        // Server 2 is connected; it gets its own reply under its own id...
        let mine = from_agent(&mut a, serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}));
        assert_eq!(json(&a.deliver(&a.queue[mine], 2))["id"], 0);
        // ...and server 1's as a notification naming the session.
        let theirs = from_agent(&mut a, serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}));
        let orphan = json(&a.deliver(&a.queue[theirs], 2));
        assert_eq!(orphan["method"], ORPHAN_REPLY);
        assert_eq!(orphan["params"]["sessionId"], "s1");
        assert_eq!(orphan["params"]["result"]["stopReason"], "end_turn");
        assert!(orphan.get("id").is_none());
    }

    #[test]
    fn open_turns_are_listed_for_a_new_server_until_answered() {
        let (mut a, _stdin) = agent();
        a.on_server_line(1, r#"{"id":5,"method":"session/prompt","params":{"sessionId":"s1"}}"#);
        assert!(a.info("x", 1).open.is_empty(), "its own server knows");
        let open = a.info("x", 2).open;
        assert_eq!(open, vec![OpenRequest { method: "session/prompt".into(), session_id: Some("s1".into()) }]);
        from_agent(&mut a, serde_json::json!({"id":1,"result":{}}));
        a.ack(a.seq);
        assert!(a.info("x", 2).open.is_empty());
    }

    #[test]
    fn initialize_is_answered_from_the_cache() {
        let (mut a, mut stdin) = agent();
        assert!(a.on_server_line(1, r#"{"id":0,"method":"initialize","params":{}}"#).is_none());
        stdin.try_recv().unwrap();
        from_agent(&mut a, serde_json::json!({"id":1,"result":{"agentCapabilities":{"loadSession":true}}}));
        let answer = a.on_server_line(2, r#"{"id":7,"method":"initialize","params":{}}"#).expect("from the cache");
        let answer = json(&answer);
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["result"]["agentCapabilities"]["loadSession"], true);
        assert!(stdin.try_recv().is_err(), "the agent isn't asked again");
    }

    #[test]
    fn unacknowledged_lines_and_unanswered_requests_go_to_the_next_server() {
        let (mut a, _stdin) = agent();
        // A tool call the last server received but never answered, then an update it never got.
        from_agent(&mut a, serde_json::json!({"id":"t1","method":"_space/tools/preExecute","params":{}}));
        from_agent(&mut a, serde_json::json!({"method":"session/update","params":{}}));
        a.ack(1);
        let backlog: Vec<_> = a.backlog("x", 2).into_iter().map(|m| match m {
            FromKeeper::Recv { seq, line, .. } => (seq, json(&line)),
            other => panic!("{other:?}"),
        }).collect();
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].1["id"], "t1");
        assert_eq!(backlog[1].1["method"], "session/update");

        // Once answered, the request isn't sent again.
        a.on_server_line(2, r#"{"id":"t1","result":{"decision":"allow"}}"#);
        a.ack(2);
        assert!(a.backlog("x", 3).is_empty());
    }

    #[test]
    fn a_request_answered_before_its_ack_is_not_resent() {
        let (mut a, _stdin) = agent();
        from_agent(&mut a, serde_json::json!({"id":3,"method":"session/request_permission","params":{}}));
        a.on_server_line(1, r#"{"id":3,"result":{}}"#);
        a.ack(1);
        assert!(a.open_from_agent.is_empty() && a.backlog("x", 2).is_empty());
    }
}
