//! The server's side of the keeper: connect, start agents, and talk to each one through an
//! ordinary byte stream ([`AgentPipe::io`]), as if it were the agent's stdin and stdout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};

use crate::proto::*;

/// Bytes buffered in each agent's in-process pipe.
const PIPE_BUFFER: usize = 1 << 20;

/// Something that happened to an agent besides its output.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Stderr(String),
    /// It ended (`None`: killed by a signal), or the keeper is gone (`Lost`).
    Exited(Option<i32>),
    /// The connection to the keeper ended: the agent can't be reached any more.
    Lost,
}

/// One agent the keeper holds, as the server sees it.
pub struct AgentPipe {
    pub name: String,
    pub pid: u32,
    /// The agent's stdin (write) and stdout (read).
    pub io: DuplexStream,
    pub events: mpsc::UnboundedReceiver<AgentEvent>,
    /// What the keeper knew of it at connect (adopted agents only).
    pub info: Option<AgentInfo>,
}

/// A connection to a keeper. Cheap to clone.
#[derive(Clone)]
pub struct KeeperClient {
    tx: mpsc::UnboundedSender<ToKeeper>,
    shared: Arc<Shared>,
    /// Ends the connection once what's been sent is written ([`KeeperClient::close`]).
    close: Arc<tokio::sync::Notify>,
}

struct Shared {
    slots: Mutex<HashMap<String, Slot>>,
    spawns: Mutex<HashMap<String, oneshot::Sender<Result<u32, String>>>>,
    connected: watch::Sender<bool>,
}

struct Slot {
    /// Lines from the agent, with their number, for the pipe's writer.
    lines: mpsc::UnboundedSender<(Option<u64>, String)>,
    events: mpsc::UnboundedSender<AgentEvent>,
}

/// What a keeper said when the server connected.
#[derive(Debug, Clone)]
pub struct Welcome {
    pub pid: u32,
    pub agents: Vec<AgentInfo>,
    pub server: Option<ServerCommand>,
    pub previous_server: Option<ServerCommand>,
}

impl std::fmt::Debug for KeeperClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeeperClient").finish_non_exhaustive()
    }
}

impl KeeperClient {
    /// Connect to the keeper at `socket`. Every agent it holds comes back as a pipe to adopt
    /// (or kill); their waiting lines are already flowing into those pipes.
    pub async fn connect(socket: &Path) -> Result<(KeeperClient, Welcome, Vec<AgentPipe>)> {
        let stream = UnixStream::connect(socket).await.with_context(|| format!("connecting to the keeper at {}", socket.display()))?;
        let (read, mut write) = stream.into_split();
        let hello = serde_json::to_string(&ToKeeper::Hello { protocol: KEEPER_PROTOCOL })?;
        write.write_all(format!("{hello}\n").as_bytes()).await?;
        let mut lines = BufReader::new(read).lines();
        let first = lines.next_line().await?.context("the keeper hung up")?;
        let (pid, agents, server, previous_server) = match serde_json::from_str::<FromKeeper>(&first)? {
            FromKeeper::Welcome { pid, agents, server, previous_server, .. } => (pid, agents, server, previous_server),
            FromKeeper::Refused { reason } => bail!("the keeper refused this server: {reason}"),
            other => bail!("unexpected first message from the keeper: {other:?}"),
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<ToKeeper>();
        let (connected, _) = watch::channel(true);
        let close = Arc::new(tokio::sync::Notify::new());
        let client = KeeperClient {
            tx: tx.clone(),
            shared: Arc::new(Shared { slots: Mutex::new(HashMap::new()), spawns: Mutex::new(HashMap::new()), connected }),
            close: close.clone(),
        };
        // Pipes for the adopted agents exist before their backlog is read.
        let pipes = agents.iter().map(|info| client.open_slot(&info.agent, info.pid, Some(info.clone()))).collect();
        tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    msg = rx.recv() => msg,
                    _ = close.notified() => {
                        // What was sent before, then hang up: the keeper keeps the agents' lines
                        // for the next server.
                        while let Ok(msg) = rx.try_recv() {
                            let line = serde_json::to_string(&msg).expect("serializable");
                            let _ = write.write_all(format!("{line}\n").as_bytes()).await;
                        }
                        let _ = write.shutdown().await;
                        break;
                    }
                };
                let Some(msg) = msg else { break };
                let line = serde_json::to_string(&msg).expect("serializable");
                if write.write_all(line.as_bytes()).await.is_err() || write.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        });
        let shared = client.shared.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<FromKeeper>(&line) {
                    Ok(msg) => shared.handle(msg),
                    Err(e) => tracing::warn!("unreadable message from the keeper: {e}"),
                }
            }
            shared.lost();
        });
        Ok((client, Welcome { pid, agents, server, previous_server }, pipes))
    }

    /// Start `name` under the keeper.
    pub async fn spawn(&self, name: &str, spec: SpawnSpec) -> Result<AgentPipe> {
        let (done, wait) = oneshot::channel();
        self.shared.spawns.lock().unwrap().insert(name.to_string(), done);
        // The pipe exists before the agent can write anything.
        let mut pipe = self.open_slot(name, 0, None);
        self.send(ToKeeper::Spawn { agent: name.to_string(), spec })?;
        match wait.await {
            Ok(Ok(pid)) => {
                pipe.pid = pid;
                Ok(pipe)
            }
            Ok(Err(e)) => {
                self.shared.slots.lock().unwrap().remove(name);
                Err(anyhow!(e))
            }
            Err(_) => Err(anyhow!("the keeper went away while starting `{name}`")),
        }
    }

    /// Stop an agent, or forget one that ended.
    pub fn kill(&self, name: &str) {
        let _ = self.send(ToKeeper::Kill { agent: name.to_string() });
    }

    /// Stop every agent and the keeper.
    pub fn quit(&self) {
        let _ = self.send(ToKeeper::Quit);
    }

    /// Hang up (this server is about to exit): the keeper holds every agent's lines from here on
    /// for the next server. What was sent before is written first.
    pub fn close(&self) {
        self.close.notify_one();
    }

    /// Run `server` from now on, once the current one exits.
    pub fn replace_server(&self, server: ServerCommand) {
        let _ = self.send(ToKeeper::ReplaceServer { server });
    }

    /// Whether the connection is up; changes once, when it ends.
    pub fn connected(&self) -> watch::Receiver<bool> {
        self.shared.connected.subscribe()
    }

    fn send(&self, msg: ToKeeper) -> Result<()> {
        self.tx.send(msg).map_err(|_| anyhow!("the keeper connection is closed"))
    }

    fn open_slot(&self, name: &str, pid: u32, info: Option<AgentInfo>) -> AgentPipe {
        let (ours, theirs) = tokio::io::duplex(PIPE_BUFFER);
        let (lines_tx, mut lines_rx) = mpsc::unbounded_channel::<(Option<u64>, String)>();
        let (events_tx, events) = mpsc::unbounded_channel();
        self.shared.slots.lock().unwrap().insert(name.to_string(), Slot { lines: lines_tx, events: events_tx });
        let (mut reader, mut writer) = tokio::io::split(theirs);
        // Agent → server: into the pipe, then acknowledged.
        let tx = self.tx.clone();
        let agent = name.to_string();
        tokio::spawn(async move {
            while let Some((seq, line)) = lines_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() || writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if let Some(seq) = seq {
                    let _ = tx.send(ToKeeper::Ack { agent: agent.clone(), seq });
                }
            }
            // The keeper side is gone: the pipe reads end-of-file.
            let _ = writer.shutdown().await;
        });
        // Server → agent.
        let tx = self.tx.clone();
        let agent = name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(&mut reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(ToKeeper::Send { agent: agent.clone(), line }).is_err() {
                    break;
                }
            }
        });
        AgentPipe { name: name.to_string(), pid, io: ours, events, info }
    }
}

impl Shared {
    fn handle(&self, msg: FromKeeper) {
        match msg {
            FromKeeper::Welcome { .. } | FromKeeper::Refused { .. } => {}
            FromKeeper::Spawned { agent, pid } => {
                if let Some(done) = self.spawns.lock().unwrap().remove(&agent) {
                    let _ = done.send(Ok(pid));
                }
            }
            FromKeeper::SpawnFailed { agent, error } => {
                if let Some(done) = self.spawns.lock().unwrap().remove(&agent) {
                    let _ = done.send(Err(error));
                }
            }
            FromKeeper::Recv { agent, seq, line } => {
                if let Some(slot) = self.slots.lock().unwrap().get(&agent) {
                    let _ = slot.lines.send((seq, line));
                }
            }
            FromKeeper::Stderr { agent, line } => {
                if let Some(slot) = self.slots.lock().unwrap().get(&agent) {
                    let _ = slot.events.send(AgentEvent::Stderr(line));
                }
            }
            FromKeeper::Exited { agent, code } => {
                // Its pipe closes: the connection on it ends.
                if let Some(slot) = self.slots.lock().unwrap().remove(&agent) {
                    let _ = slot.events.send(AgentEvent::Exited(code));
                }
            }
        }
    }

    /// The keeper connection ended: every pipe closes.
    fn lost(&self) {
        for (_, slot) in self.slots.lock().unwrap().drain() {
            let _ = slot.events.send(AgentEvent::Lost);
        }
        self.spawns.lock().unwrap().clear();
        self.connected.send_replace(false);
    }
}

/// Start a keeper for `socket` from `exe`, detached from this process (its own session, so it
/// outlives the server and the app), logging to `log`, running `server`. Returns once it
/// answers.
pub async fn start(exe: &Path, socket: &Path, log: &Path, server: Option<&ServerCommand>) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--socket").arg(socket);
    if let Some(server) = server {
        cmd.arg("--server-log").arg(log.with_file_name("server.log")).arg("--server").arg(&server.program).args(&server.args);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| format!("starting {}", exe.display()))?;
    for _ in 0..100 {
        if UnixStream::connect(socket).await.is_ok() {
            // Reaped by whoever outlives it; don't leave a zombie while we run.
            std::thread::spawn(move || child.wait());
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("the keeper exited at once ({status}); see {}", log.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("the keeper didn't answer on {}", socket.display())
}

/// The keeper executable next to this one (`space-keeper`).
pub fn exe_beside_current() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name("space-keeper"))
}
