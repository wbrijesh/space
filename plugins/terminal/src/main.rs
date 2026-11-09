//! `space-terminal-plugin`: runs shells on PTYs for the app's terminal pane.
//!
//! Each terminal is a login shell (`$SHELL`) on its own PTY, in its own session, started in the
//! thread's project folder. Output is read on a thread and sent to the host in batches (every
//! 4 ms or 64 KB), numbered in order; the newest few MB are kept so the app can draw a terminal
//! from scratch (`terminal/attach`). The program in front (`tcgetpgrp`) is the tab's title.
//! Closing a terminal hangs up its session and, two seconds later, kills whatever is left. When
//! the host goes away (Space quits) every shell is stopped the same way.

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use space_plugin::protocol::service::*;
use space_plugin::protocol::terminal::*;
use space_plugin::{Host, Plugin, RpcError};

/// Output is sent at least this often while it flows...
const FLUSH_EVERY: Duration = Duration::from_millis(4);
/// ...or as soon as this much is waiting.
const MAX_BATCH: usize = 64 * 1024;
/// The newest output kept per terminal for `terminal/attach`.
const REPLAY_MAX: usize = 2 * 1024 * 1024;
/// How often the program in front is checked (for titles).
const TITLE_EVERY: Duration = Duration::from_millis(400);
/// After hanging up a terminal, what's left is killed this much later.
const KILL_AFTER: Duration = Duration::from_secs(2);
/// Runs this instead of the login shell (tests and scenarios): a program and its arguments,
/// separated by spaces.
const SHELL_ENV: &str = "SPACE_TERMINAL_SHELL";

struct Terminal {
    thread_id: String,
    title: String,
    master: Box<dyn MasterPty + Send>,
    /// To the writer thread.
    input: mpsc::Sender<Vec<u8>>,
    /// The shell's pid, which is also its session and process group.
    pid: Option<i32>,
    replay: VecDeque<u8>,
    seq: u64,
    /// `Some` once the shell ended (with its exit code when known).
    exited: Option<Option<i32>>,
}

#[derive(Default)]
struct State {
    terminals: Mutex<HashMap<String, Terminal>>,
    host: OnceLock<Host>,
    next: AtomicU64,
    /// Overrides the login shell (tests).
    shell: OnceLock<Vec<String>>,
}

impl State {
    fn command(&self, cwd: &Path) -> (CommandBuilder, String) {
        let custom = self.shell.get().cloned().or_else(|| {
            std::env::var(SHELL_ENV).ok().map(|s| s.split_whitespace().map(String::from).collect::<Vec<_>>()).filter(|v| !v.is_empty())
        });
        let (mut cmd, name) = match custom {
            Some(argv) => {
                let mut cmd = CommandBuilder::new(&argv[0]);
                cmd.args(&argv[1..]);
                (cmd, base_name(&argv[0]))
            }
            // The user's login shell ($SHELL, else the password database), as a login shell.
            None => {
                let cmd = CommandBuilder::new_default_prog();
                let name = base_name(&cmd.get_shell());
                (cmd, name)
            }
        };
        cmd.cwd(cwd);
        for (k, _) in std::env::vars_os() {
            if k.to_str().is_some_and(|k| k.starts_with("SPACE_")) {
                cmd.env_remove(k);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Space");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        (cmd, name)
    }

    fn open(self: &Arc<Self>, open: TerminalOpen) -> Result<TerminalOpened, String> {
        let size = PtySize { rows: open.rows.max(2), cols: open.cols.max(2), pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).map_err(|e| format!("couldn't open a terminal: {e}"))?;
        let cwd = Path::new(&open.cwd);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let (cmd, title) = self.command(if cwd.is_dir() { cwd } else { Path::new(&home) });
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| format!("couldn't start the shell: {e}"))?;
        drop(pair.slave);
        let pid = child.process_id().map(|p| p as i32);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let id = format!("term-{}-{}", std::process::id(), self.next.fetch_add(1, Ordering::Relaxed) + 1);

        // Writer: input in order, off the connection's reader (a busy shell may not read).
        let (input, keys) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            while let Ok(bytes) = keys.recv() {
                if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        // Reader: raw chunks to the batcher.
        let (chunks_tx, chunks) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if chunks_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        // Batcher: numbered batches to the host; then, once the PTY is closed, the exit code.
        let (state, bid) = (self.clone(), id.clone());
        thread::spawn(move || {
            while let Ok(first) = chunks.recv() {
                let mut batch = first;
                let started = Instant::now();
                while batch.len() < MAX_BATCH {
                    let left = FLUSH_EVERY.saturating_sub(started.elapsed());
                    match chunks.recv_timeout(left) {
                        Ok(more) => batch.extend_from_slice(&more),
                        Err(_) => break,
                    }
                }
                state.push_output(&bid, batch);
            }
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            state.exited(&bid, code);
        });

        self.terminals.lock().unwrap().insert(
            id.clone(),
            Terminal {
                thread_id: open.thread_id,
                title: title.clone(),
                master: pair.master,
                input,
                pid,
                replay: VecDeque::new(),
                seq: 0,
                exited: None,
            },
        );
        Ok(TerminalOpened { terminal_id: id, title })
    }

    /// Keep a batch for replays and send it (under the lock, so an attach sees all of it or
    /// none of it).
    fn push_output(&self, id: &str, data: Vec<u8>) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(t) = terminals.get_mut(id) else { return };
        t.seq += 1;
        t.replay.extend(&data);
        if t.replay.len() > REPLAY_MAX {
            // Drop the oldest, up to a line end so the replay starts at a line.
            let mut cut = t.replay.len() - REPLAY_MAX;
            while cut < t.replay.len() && t.replay[cut - 1] != b'\n' {
                cut += 1;
            }
            t.replay.drain(..cut);
        }
        if let Some(host) = self.host.get() {
            host.terminal_output(id, t.seq, data);
        }
    }

    fn exited(&self, id: &str, code: Option<i32>) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(t) = terminals.get_mut(id) else { return };
        t.exited = Some(code);
        if let Some(host) = self.host.get() {
            host.terminal_exited(id, code);
        }
    }

    /// Tabs follow the program in front of each shell.
    fn update_titles(&self) {
        let mut terminals = self.terminals.lock().unwrap();
        for (id, t) in terminals.iter_mut().filter(|(_, t)| t.exited.is_none()) {
            let Some(name) = t.master.process_group_leader().and_then(process_name) else { continue };
            if name != t.title {
                t.title = name;
                if let Some(host) = self.host.get() {
                    host.terminal_title(id, &t.title);
                }
            }
        }
    }

    fn close(&self, id: &str) -> bool {
        let Some(t) = self.terminals.lock().unwrap().remove(id) else { return false };
        hang_up(t);
        true
    }

    fn close_all(&self, thread: Option<&str>) {
        let gone: Vec<Terminal> = {
            let mut terminals = self.terminals.lock().unwrap();
            let ids: Vec<String> =
                terminals.iter().filter(|(_, t)| thread.is_none_or(|th| t.thread_id == th)).map(|(id, _)| id.clone()).collect();
            ids.iter().filter_map(|id| terminals.remove(id)).collect()
        };
        for t in gone {
            hang_up(t);
        }
    }
}

/// Hang up a terminal's session (the shell, its jobs, the program in front), then kill what's
/// left after [`KILL_AFTER`].
fn hang_up(t: Terminal) {
    let groups: Vec<i32> = [t.pid, t.master.process_group_leader()].into_iter().flatten().filter(|p| *p > 1).collect();
    for g in &groups {
        // SAFETY: plain syscalls on process groups we started.
        unsafe { libc::kill(-g, libc::SIGHUP) };
    }
    // Closing the PTY hangs up whatever else is attached to it.
    drop(t.master);
    drop(t.input);
    thread::spawn(move || {
        thread::sleep(KILL_AFTER);
        for g in groups {
            unsafe { libc::kill(-g, libc::SIGKILL) };
        }
    });
}

fn base_name(path: &str) -> String {
    Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).trim_start_matches('-').to_string()
}

/// A process's executable name.
#[cfg(target_os = "macos")]
fn process_name(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is as large as the call allows.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| base_name(&String::from_utf8_lossy(&buf[..n as usize])))
}

#[cfg(not(target_os = "macos"))]
fn process_name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

/// The SDK wants `Plugin` on a plain value.
#[derive(Default, Clone)]
struct Plugged(Arc<State>);

impl Plugin for Plugged {
    async fn initialize(&self, _init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        let _ = self.0.host.set(host);
        let state = Arc::downgrade(&self.0);
        thread::spawn(move || {
            while let Some(state) = state.upgrade() {
                state.update_titles();
                drop(state);
                thread::sleep(TITLE_EVERY);
            }
        });
        Ok(Initialized::default())
    }

    async fn observe(&self, event: Observed) {
        if let Observed::ThreadDeleted { thread_id } = event {
            self.0.close_all(Some(&thread_id));
        }
    }

    async fn terminal_open(&self, open: TerminalOpen) -> Result<TerminalOpened, RpcError> {
        let state = self.0.clone();
        tokio::task::spawn_blocking(move || state.open(open)).await.map_err(|e| RpcError::internal(e.to_string()))?.map_err(RpcError::internal)
    }

    fn terminal_input(&self, input: TerminalInput) {
        if let Some(t) = self.0.terminals.lock().unwrap().get(&input.terminal_id) {
            let _ = t.input.send(input.data.0);
        }
    }

    fn terminal_resize(&self, resize: TerminalResize) {
        if let Some(t) = self.0.terminals.lock().unwrap().get(&resize.terminal_id) {
            let size = PtySize { rows: resize.rows.max(2), cols: resize.cols.max(2), pixel_width: 0, pixel_height: 0 };
            let _ = t.master.resize(size);
        }
    }

    async fn terminal_attach(&self, terminal: TerminalRef) -> Result<TerminalReplay, RpcError> {
        let terminals = self.0.terminals.lock().unwrap();
        let t = terminals.get(&terminal.terminal_id).ok_or_else(|| RpcError::invalid_params(format!("no terminal {}", terminal.terminal_id)))?;
        Ok(TerminalReplay {
            data: Bytes(t.replay.iter().copied().collect()),
            seq: t.seq,
            title: t.title.clone(),
            exit_code: t.exited.flatten(),
            exited: t.exited.is_some(),
        })
    }

    async fn terminal_list(&self) -> Result<TerminalList, RpcError> {
        let terminals = self.0.terminals.lock().unwrap();
        let mut list: Vec<TerminalInfo> = terminals
            .iter()
            .map(|(id, t)| TerminalInfo { terminal_id: id.clone(), thread_id: t.thread_id.clone(), title: t.title.clone(), exited: t.exited.is_some() })
            .collect();
        list.sort_by_key(|t| natural(&t.terminal_id));
        Ok(TerminalList { terminals: list })
    }

    async fn terminal_close(&self, terminal: TerminalRef) -> Result<(), RpcError> {
        self.0.close(&terminal.terminal_id);
        Ok(())
    }
}

/// Ids in the order they were opened (`term-<pid>-<n>`).
fn natural(id: &str) -> u64 {
    id.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn main() {
    let plugged = Plugged::default();
    let state = plugged.0.clone();
    space_plugin::run(plugged);
    // The host went away (Space quit or restarted the plugin): stop every shell now.
    let gone: Vec<Terminal> = state.terminals.lock().unwrap().drain().map(|(_, t)| t).collect();
    let groups: Vec<i32> =
        gone.iter().flat_map(|t| [t.pid, t.master.process_group_leader()]).flatten().filter(|p| *p > 1).collect();
    for g in &groups {
        unsafe { libc::kill(-g, libc::SIGHUP) };
    }
    drop(gone);
    thread::sleep(Duration::from_millis(300));
    for g in &groups {
        unsafe { libc::kill(-g, libc::SIGKILL) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use space_plugin::Peer;
    use space_plugin::protocol::terminal::{TERMINAL_EXITED, TERMINAL_OUTPUT};
    use space_plugin::rpc::NotificationHandler;

    use super::*;

    #[derive(Default)]
    struct Seen {
        output: HashMap<String, Vec<u8>>,
        last_seq: HashMap<String, u64>,
        exited: HashMap<String, Option<i32>>,
    }

    async fn start() -> (Peer, Arc<StdMutex<Seen>>, Arc<State>) {
        let seen = Arc::new(StdMutex::new(Seen::default()));
        let sink: NotificationHandler = {
            let seen = seen.clone();
            Arc::new(move |method: String, params: serde_json::Value| {
                let mut seen = seen.lock().unwrap();
                match method.as_str() {
                    TERMINAL_OUTPUT => {
                        let out: TerminalOutput = serde_json::from_value(params).unwrap();
                        let last = seen.last_seq.entry(out.terminal_id.clone()).or_default();
                        assert_eq!(out.seq, *last + 1, "output numbered in order");
                        *last = out.seq;
                        seen.output.entry(out.terminal_id).or_default().extend(out.data.0);
                    }
                    TERMINAL_EXITED => {
                        let e: TerminalExited = serde_json::from_value(params).unwrap();
                        seen.exited.insert(e.terminal_id, e.exit_code);
                    }
                    _ => {}
                }
            })
        };
        let plugged = Plugged::default();
        let _ = plugged.0.shell.set(vec!["/bin/sh".into()]);
        let state = plugged.0.clone();
        let peer = space_plugin::testing::connect(plugged, sink);
        let init = Initialize { protocol_version: PROTOCOL_VERSION, settings: Default::default(), unreadable: vec![], space_dir: None };
        let _: Initialized = peer.call(INITIALIZE, &init, None).await.unwrap();
        (peer, seen, state)
    }

    async fn open(peer: &Peer, thread: &str, cwd: &Path) -> String {
        let open = TerminalOpen { thread_id: thread.into(), cwd: cwd.display().to_string(), cols: 80, rows: 24 };
        let opened: TerminalOpened = peer.call(TERMINAL_OPEN, &open, None).await.unwrap();
        assert_eq!(opened.title, "sh");
        opened.terminal_id
    }

    fn type_in(peer: &Peer, id: &str, text: &str) {
        peer.notify(TERMINAL_INPUT, TerminalInput { terminal_id: id.into(), data: Bytes(text.as_bytes().to_vec()) });
    }

    async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[tokio::test]
    async fn runs_a_shell_in_the_folder_and_replays_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (peer, seen, state) = start().await;
        let id = open(&peer, "t1", tmp.path()).await;
        type_in(&peer, &id, "pwd; echo hi-$((40+2))\n");
        let dir = tmp.path().canonicalize().unwrap().display().to_string();
        let out = |seen: &Arc<StdMutex<Seen>>| String::from_utf8_lossy(seen.lock().unwrap().output.get(&id).map_or(&[][..], |v| v)).to_string();
        wait_for("the command's output", || out(&seen).contains("hi-42")).await;
        assert!(out(&seen).contains(&dir), "started in the thread's folder: {}", out(&seen));

        // A replay has everything sent so far, up to the last number.
        let replay: TerminalReplay = peer.call(TERMINAL_ATTACH, &TerminalRef { terminal_id: id.clone() }, None).await.unwrap();
        assert!(String::from_utf8_lossy(&replay.data.0).contains("hi-42"));
        assert_eq!(replay.seq, seen.lock().unwrap().last_seq[&id]);
        assert!(!replay.exited);

        peer.notify(TERMINAL_RESIZE, TerminalResize { terminal_id: id.clone(), cols: 120, rows: 40 });
        type_in(&peer, &id, "stty size\n");
        wait_for("the new size", || out(&seen).contains("40 120")).await;

        let list: TerminalList = peer.call(TERMINAL_LIST, &serde_json::json!({}), None).await.unwrap();
        assert_eq!(list.terminals.len(), 1);
        assert_eq!(list.terminals[0].thread_id, "t1");

        // Closing stops the shell and the program it runs.
        type_in(&peer, &id, "sleep 30\n");
        let pid = state.terminals.lock().unwrap()[&id].pid.unwrap();
        wait_for("sleep to be in front", || {
            state.terminals.lock().unwrap()[&id].master.process_group_leader().is_some_and(|g| g != pid)
        })
        .await;
        let sleeper = state.terminals.lock().unwrap()[&id].master.process_group_leader().unwrap();
        let () = peer.call(TERMINAL_CLOSE, &TerminalRef { terminal_id: id.clone() }, None).await.unwrap();
        wait_for("the shell and sleep to stop", || !alive(pid) && !alive(sleeper)).await;
        let list: TerminalList = peer.call(TERMINAL_LIST, &serde_json::json!({}), None).await.unwrap();
        assert!(list.terminals.is_empty());
    }

    #[tokio::test]
    async fn reports_the_exit_code_and_titles_follow_the_program_in_front() {
        let tmp = tempfile::tempdir().unwrap();
        let (peer, seen, state) = start().await;
        let id = open(&peer, "t1", tmp.path()).await;
        type_in(&peer, &id, "sleep 2\n");
        wait_for("the title to follow sleep", || {
            state.update_titles();
            state.terminals.lock().unwrap()[&id].title == "sleep"
        })
        .await;
        type_in(&peer, &id, "exit 3\n");
        wait_for("the exit", || seen.lock().unwrap().exited.contains_key(&id)).await;
        assert_eq!(seen.lock().unwrap().exited[&id], Some(3));
        let replay: TerminalReplay = peer.call(TERMINAL_ATTACH, &TerminalRef { terminal_id: id }, None).await.unwrap();
        assert!(replay.exited);
        assert_eq!(replay.exit_code, Some(3));
    }

    #[tokio::test]
    async fn deleting_a_thread_closes_its_terminals_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (peer, _seen, state) = start().await;
        let a = open(&peer, "a", tmp.path()).await;
        let b = open(&peer, "b", tmp.path()).await;
        let pid_a = state.terminals.lock().unwrap()[&a].pid.unwrap();
        Plugged(state.clone()).observe(Observed::ThreadDeleted { thread_id: "a".into() }).await;
        wait_for("thread a's shell to stop", || !alive(pid_a)).await;
        let list: TerminalList = peer.call(TERMINAL_LIST, &serde_json::json!({}), None).await.unwrap();
        assert_eq!(list.terminals.iter().map(|t| t.terminal_id.as_str()).collect::<Vec<_>>(), [b.as_str()]);
    }

    #[test]
    fn names() {
        assert_eq!(base_name("/bin/zsh"), "zsh");
        assert_eq!(base_name("-zsh"), "zsh");
        assert_eq!(natural("term-12-7"), 7);
    }
}
