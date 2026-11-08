//! `space-trajectory`: keeps every thread's trace and answers the app's Trajectory tab.
//!
//! ```text
//! <space dir>/trajectory/<thread id>/<session id>.jsonl   one trace batch per line, as received
//! ```
//!
//! The host hands it every trace batch in order (`trace/record`); it appends the batch to the
//! session's file and marks the thread's layout stale. A read builds the layout again when
//! needed ([`project::Projection`]) and pages through it from the end. While batches arrive it
//! tells the host the thread changed, at most every 200 ms. Without a Space folder (tests,
//! scenarios) traces live in memory only. Nothing leaves the machine.

mod project;

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use project::{Projection, SessionLog};
use space_plugin::protocol::service::*;
use space_plugin::protocol::trace::*;
use space_plugin::{Host, Plugin, RpcError};

/// Days to keep a thread's trace after it last changed (unset: forever).
const KEEP_DAYS: &str = "keep_days";
/// Whether streamed chunk times are kept.
const CHUNK_TIMING: &str = "chunk_timing";
/// `trajectory/changed` at most this often per thread.
const NOTIFY_EVERY: Duration = Duration::from_millis(200);
const SWEEP_EVERY: Duration = Duration::from_secs(24 * 3600);
/// A page holds at most this many rows.
const MAX_PAGE: usize = 500;

/// One thread's trace and its layout.
#[derive(Default)]
struct Thread {
    sessions: Vec<SessionLog>,
    version: u64,
    projection: Option<Projection>,
}

impl Thread {
    fn projection(&mut self) -> &Projection {
        if self.projection.is_none() {
            let mut sessions = self.sessions.clone();
            sessions.sort_by_key(SessionLog::started);
            self.sessions = sessions;
            self.projection = Some(Projection::build(&self.sessions));
        }
        self.projection.as_ref().expect("built")
    }
}

#[derive(Default)]
struct Notices {
    /// When each thread was last announced, and whether an announcement is scheduled.
    last: HashMap<String, Instant>,
    scheduled: HashMap<String, bool>,
}

#[derive(Default)]
struct Trajectory {
    /// `<space dir>/trajectory`; unset: memory only.
    root: OnceLock<PathBuf>,
    host: OnceLock<Host>,
    threads: Mutex<HashMap<String, Thread>>,
    notices: Mutex<Notices>,
    keep_days: Mutex<Option<u64>>,
    chunk_timing: Mutex<bool>,
}

/// Thread and session ids become folder and file names: only letters, digits, `-` and `_`.
fn safe(id: &str) -> Option<&str> {
    let ok = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(id)
}

/// A thread's key ("unknown" when the host didn't know it).
fn thread_key(id: &str) -> String {
    if id.is_empty() { "unknown".to_string() } else { id.to_string() }
}

fn apply_settings(this: &Trajectory, values: &SettingValues) {
    *this.keep_days.lock().unwrap() = values.get(KEEP_DAYS).and_then(|v| v.trim().parse().ok()).filter(|d: &u64| *d > 0);
    *this.chunk_timing.lock().unwrap() = values.get(CHUNK_TIMING).is_none_or(|v| v != "false");
}

impl Trajectory {
    /// The thread's trace, loaded from disk the first time.
    fn with_thread<R>(&self, id: &str, f: impl FnOnce(&mut Thread) -> R) -> R {
        let key = thread_key(id);
        let mut threads = self.threads.lock().unwrap();
        let thread = threads.entry(key.clone()).or_insert_with(|| self.load(&key));
        f(thread)
    }

    fn dir(&self, thread: &str) -> Option<PathBuf> {
        Some(self.root.get()?.join(safe(thread)?))
    }

    /// A thread's sessions from disk (none when nothing is kept). A line cut short is skipped.
    fn load(&self, thread: &str) -> Thread {
        let Some(dir) = self.dir(thread) else { return Thread::default() };
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|_| path.extension().is_some_and(|e| e == "jsonl")) else {
                continue;
            };
            let text = fs::read_to_string(&path).unwrap_or_default();
            let mut log = SessionLog { id: id.to_string(), events: Vec::new() };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<TraceBatch>(line) {
                    Ok(batch) => log.events.extend(batch.events),
                    Err(e) => eprintln!("space-trajectory: {} skipped a line: {e}", path.display()),
                }
            }
            sessions.push(log);
        }
        Thread { sessions, version: 1, projection: None }
    }

    /// Append a batch to its session's file.
    fn append(&self, thread: &str, session: &str, batch: &TraceBatch) {
        let (Some(dir), Some(session)) = (self.dir(thread), safe(session)) else { return };
        let line = match serde_json::to_string(batch) {
            Ok(l) => l,
            Err(e) => return eprintln!("space-trajectory: {e}"),
        };
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(&dir)?;
            let mut file = OpenOptions::new().create(true).append(true).open(dir.join(format!("{session}.jsonl")))?;
            writeln!(file, "{line}")
        };
        if let Err(e) = write() {
            eprintln!("space-trajectory: writing {}: {e}", dir.display());
        }
    }

    /// Tell the host the thread changed, at most every [`NOTIFY_EVERY`]: a change inside that
    /// window is announced at its end.
    fn announce(self: &Arc<Self>, thread: &str) {
        let Some(host) = self.host.get() else { return };
        let mut notices = self.notices.lock().unwrap();
        if notices.scheduled.get(thread).copied().unwrap_or(false) {
            return;
        }
        let since = notices.last.get(thread).map(Instant::elapsed);
        match since {
            Some(e) if e < NOTIFY_EVERY => {
                notices.scheduled.insert(thread.to_string(), true);
                let (this, thread, wait) = (self.clone(), thread.to_string(), NOTIFY_EVERY - e);
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    this.notices.lock().unwrap().scheduled.insert(thread.clone(), false);
                    this.announce(&thread);
                });
            }
            _ => {
                notices.last.insert(thread.to_string(), Instant::now());
                drop(notices);
                let version = self.threads.lock().unwrap().get(&thread_key(thread)).map_or(0, |t| t.version);
                host.trajectory_changed(thread, version);
            }
        }
    }

    /// Remove the traces of threads that haven't changed for `keep_days`.
    fn sweep(&self) {
        let (Some(root), Some(days)) = (self.root.get(), *self.keep_days.lock().unwrap()) else { return };
        let cutoff = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
        for entry in fs::read_dir(root).into_iter().flatten().flatten() {
            let dir = entry.path();
            if dir.is_dir() && newest_change(&dir).is_some_and(|t| t < cutoff) {
                let _ = fs::remove_dir_all(&dir);
                if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                    self.threads.lock().unwrap().remove(name);
                }
            }
        }
    }
}

/// When anything in `dir` last changed.
fn newest_change(dir: &Path) -> Option<SystemTime> {
    fs::read_dir(dir).ok()?.flatten().filter_map(|e| e.metadata().ok()?.modified().ok()).max()
}

/// Shared state behind the plugin (the SDK wants `Plugin` on a plain value).
#[derive(Default, Clone)]
struct Plugged(Arc<Trajectory>);

impl Plugin for Plugged {
    async fn initialize(&self, init: Initialize, host: Host) -> Result<Initialized, RpcError> {
        if let Some(dir) = &init.space_dir {
            let _ = self.0.root.set(PathBuf::from(dir).join("trajectory"));
        }
        apply_settings(&self.0, &init.settings);
        let _ = self.0.host.set(host);
        let this = self.0.clone();
        tokio::spawn(async move {
            loop {
                this.sweep();
                tokio::time::sleep(SWEEP_EVERY).await;
            }
        });
        Ok(Initialized::default())
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            fields: vec![
                SettingField {
                    key: KEEP_DAYS.into(),
                    label: "Keep traces for (days)".into(),
                    description: "Traces of threads that haven't changed for this long are deleted. Empty: keep them.".into(),
                    kind: SettingKind::Text,
                    placeholder: Some("Forever".into()),
                    default: None,
                },
                SettingField {
                    key: CHUNK_TIMING.into(),
                    label: "Record streamed chunk timing".into(),
                    description: "When each part of a reply arrived, for the request's timing graph.".into(),
                    kind: SettingKind::Switch,
                    placeholder: None,
                    default: Some("true".into()),
                },
            ],
        }
    }

    async fn validate_setting(&self, check: SettingCheck) -> Result<SettingVerdict, RpcError> {
        if check.key == KEEP_DAYS && !check.value.trim().parse::<u64>().is_ok_and(|d| d > 0) {
            return Ok(SettingVerdict::invalid("Enter a number of days, or leave it empty to keep traces."));
        }
        Ok(SettingVerdict::ok())
    }

    fn settings_changed(&self, changed: SettingsChanged) {
        apply_settings(&self.0, &changed.values);
        self.0.sweep();
    }

    async fn observe(&self, event: Observed) {
        if let Observed::ThreadDeleted { thread_id } = event {
            self.0.threads.lock().unwrap().remove(&thread_key(&thread_id));
            if let Some(dir) = self.0.dir(&thread_id) {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    async fn trace_record(&self, record: TraceRecord) -> Result<(), RpcError> {
        let TraceRecord { thread_id, session_id, mut events } = record;
        if !*self.0.chunk_timing.lock().unwrap() {
            events.retain(|e| !matches!(e, TraceEvent::Chunks { .. }));
        }
        if events.is_empty() {
            return Ok(());
        }
        let batch = TraceBatch { session_id: session_id.clone(), events };
        // Loaded (from disk, the first time) before the batch is appended, so it isn't read twice.
        self.0.with_thread(&thread_id, |t| {
            self.0.append(&thread_key(&thread_id), &session_id, &batch);
            match t.sessions.iter_mut().find(|s| s.id == session_id) {
                Some(s) => s.events.extend(batch.events),
                None => t.sessions.push(SessionLog { id: session_id, events: batch.events }),
            }
            t.version += 1;
            t.projection = None;
        });
        self.0.announce(&thread_key(&thread_id));
        Ok(())
    }

    async fn trajectory_read(&self, read: TrajectoryRead) -> Result<TrajectoryPage, RpcError> {
        let limit = (read.limit as usize).clamp(1, MAX_PAGE);
        Ok(self.0.with_thread(&read.thread_id, |t| {
            let version = t.version;
            let p = t.projection();
            let indices: Vec<usize> = match read.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                Some(q) => p.search(q),
                None => (0..p.rows.len()).collect(),
            };
            let end = read
                .before
                .as_deref()
                .and_then(|id| indices.iter().position(|&i| p.rows[i].id == id))
                .unwrap_or(indices.len());
            let start = end.saturating_sub(limit);
            let rows: Vec<TrajectoryRow> = indices[start..end].iter().map(|&i| p.rows[i].clone()).collect();
            TrajectoryPage {
                requests: p.requests_of(&rows),
                older: (start > 0).then(|| p.rows[indices[start]].id.clone()),
                rows,
                version,
                stats: p.stats.clone(),
            }
        }))
    }

    async fn trajectory_detail(&self, request: TrajectoryDetailRequest) -> Result<TrajectoryDetail, RpcError> {
        self.0
            .with_thread(&request.thread_id, |t| {
                t.projection();
                let p = t.projection.as_ref().expect("built");
                p.detail(&request.id, &t.sessions)
            })
            .ok_or_else(|| RpcError::invalid_params(format!("no row {:?}", request.id)))
    }

    async fn trajectory_stats(&self, thread: ThreadRef) -> Result<TrajectoryStats, RpcError> {
        Ok(self.0.with_thread(&thread.thread_id, |t| t.projection().stats.clone()))
    }
}

fn main() {
    space_plugin::run(Plugged::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(thread: &str, session: &str, events: Vec<TraceEvent>) -> TraceRecord {
        TraceRecord { thread_id: thread.into(), session_id: session.into(), events }
    }

    fn prompt(at: u64, text: &str) -> TraceEvent {
        TraceEvent::Prompt { at, text: text.into(), files: vec![], images: vec![] }
    }

    fn plugin_at(root: &Path) -> Plugged {
        let p = Plugged::default();
        let _ = p.0.root.set(root.to_path_buf());
        *p.0.chunk_timing.lock().unwrap() = true;
        p
    }

    fn read(thread: &str, before: Option<String>, limit: u32, query: Option<&str>) -> TrajectoryRead {
        TrajectoryRead { thread_id: thread.into(), before, limit, query: query.map(Into::into) }
    }

    #[tokio::test]
    async fn pages_from_the_end_with_a_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plugin_at(tmp.path());
        let events: Vec<_> = (0..5).map(|i| prompt(i, &format!("message {i}"))).collect();
        p.trace_record(record("t1", "s1", events)).await.unwrap();
        let last = p.trajectory_read(read("t1", None, 2, None)).await.unwrap();
        assert_eq!(last.rows.iter().map(|r| r.preview.as_str()).collect::<Vec<_>>(), ["message 3", "message 4"]);
        let older = p.trajectory_read(read("t1", last.older.clone(), 2, None)).await.unwrap();
        assert_eq!(older.rows.iter().map(|r| r.preview.as_str()).collect::<Vec<_>>(), ["message 1", "message 2"]);
        let first = p.trajectory_read(read("t1", older.older.clone(), 2, None)).await.unwrap();
        assert_eq!((first.rows.len(), first.older), (1, None));
        // Search pages the same way, over the matches.
        let found = p.trajectory_read(read("t1", None, 10, Some("MESSAGE 3"))).await.unwrap();
        assert_eq!(found.rows.len(), 1);
        assert!(last.version > 0);
    }

    #[tokio::test]
    async fn keeps_batches_on_disk_and_loads_them_again() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plugin_at(tmp.path());
        p.trace_record(record("t1", "s1", vec![prompt(1, "one")])).await.unwrap();
        p.trace_record(record("t1", "s1", vec![prompt(2, "two")])).await.unwrap();
        let file = tmp.path().join("t1/s1.jsonl");
        assert_eq!(fs::read_to_string(&file).unwrap().lines().count(), 2);
        // A fresh plugin (Space restarted) reads it back.
        let again = plugin_at(tmp.path());
        let page = again.trajectory_read(read("t1", None, 10, None)).await.unwrap();
        assert_eq!(page.rows.len(), 2);
        let detail = again.trajectory_detail(TrajectoryDetailRequest { thread_id: "t1".into(), id: page.rows[1].id.clone() }).await.unwrap();
        assert_eq!(detail.text.as_deref(), Some("two"));
        assert_eq!(again.trajectory_stats(ThreadRef { thread_id: "t1".into() }).await.unwrap().turns, 0);
    }

    #[tokio::test]
    async fn deleting_a_thread_removes_its_trace() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plugin_at(tmp.path());
        p.trace_record(record("t1", "s1", vec![prompt(1, "one")])).await.unwrap();
        p.observe(Observed::ThreadDeleted { thread_id: "t1".into() }).await;
        assert!(!tmp.path().join("t1").exists());
        assert!(p.trajectory_read(read("t1", None, 10, None)).await.unwrap().rows.is_empty());
    }

    #[tokio::test]
    async fn chunk_timing_can_be_left_out_and_old_traces_are_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plugin_at(tmp.path());
        apply_settings(&p.0, &SettingValues::from([(CHUNK_TIMING.to_string(), "false".to_string())]));
        p.trace_record(record("t1", "s1", vec![TraceEvent::Chunks { request: 1, stream: ChunkKind::Text, t0: 1, dt: vec![1] }]))
            .await
            .unwrap();
        assert!(!tmp.path().join("t1").exists(), "nothing but chunks: nothing kept");
        p.trace_record(record("t2", "s1", vec![prompt(1, "one")])).await.unwrap();
        // Make it old, then keep 1 day.
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        fs::File::options().write(true).open(tmp.path().join("t2/s1.jsonl")).unwrap().set_modified(old).unwrap();
        apply_settings(&p.0, &SettingValues::from([(KEEP_DAYS.to_string(), "1".to_string())]));
        p.0.sweep();
        assert!(!tmp.path().join("t2").exists());
        assert!(matches!(p.validate_setting(SettingCheck { key: KEEP_DAYS.into(), value: "soon".into() }).await.unwrap().status, SettingStatus::Invalid));
    }

    #[test]
    fn ids_that_arent_names_are_not_kept() {
        assert_eq!((safe("t-1_a"), safe("../x"), safe("")), (Some("t-1_a"), None, None));
        assert_eq!(thread_key(""), "unknown");
    }
}
