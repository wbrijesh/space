//! `space-session-store`: keeps threads on disk so they survive a restart.
//!
//! ```text
//! <space dir>/threads/<thread id>/
//!   meta.json      the summary the sidebar lists (with the format version)
//!   events.jsonl   the thread's events, appended one per line
//!   agent/         the agent's own files for the session (to load it again)
//!   attachments/   images sent in the thread, named by the sha256 of their bytes
//! ```
//!
//! Events are kept as they are; Space decides what they mean. A line cut short (Space quit
//! mid-write) is skipped when loading, and the next write starts on a fresh line. Each write
//! says where its events start (`first`), so events the store already has are skipped, and an
//! older summary never replaces a newer one. Without a
//! Space folder (tests, scenarios) nothing is kept: writes do nothing and the list is empty.

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use space_plugin::protocol::service::*;
use space_plugin::{Host, Plugin, RpcError};

/// Version of the on-disk format, in `meta.json`.
const FORMAT_VERSION: u64 = 1;
const META: &str = "meta.json";
const EVENTS: &str = "events.jsonl";
const AGENT: &str = "agent";
const ATTACHMENTS: &str = "attachments";
/// A thread folder without a summary (its agent's files for a thread you never wrote in) is
/// removed once it's this old.
const ORPHAN_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Default)]
struct Store {
    /// `<space dir>/threads`; unset when Space keeps nothing on disk.
    root: OnceLock<PathBuf>,
    /// One file operation at a time.
    lock: Mutex<()>,
}

impl Store {
    /// The folder of thread `id` (`None`: nothing is kept on disk).
    fn dir(&self, id: &str) -> Result<Option<PathBuf>, RpcError> {
        let valid = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(RpcError::invalid_params(format!("bad thread id {id:?}")));
        }
        Ok(self.root.get().map(|root| root.join(id)))
    }

    fn list(&self) -> Result<StoredThreads, RpcError> {
        let Some(root) = self.root.get() else { return Ok(StoredThreads::default()) };
        let _guard = self.lock.lock().unwrap();
        let mut threads = Vec::new();
        for entry in fs::read_dir(root).into_iter().flatten().flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            match read_meta(&dir) {
                Ok(Some(meta)) => threads.push(meta),
                Ok(None) if is_older_than(&dir, ORPHAN_AGE) => {
                    if let Err(e) = fs::remove_dir_all(&dir) {
                        eprintln!("space-session-store: removing {}: {e}", dir.display());
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("space-session-store: {}: {}", dir.display(), e.message),
            }
        }
        threads.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        Ok(StoredThreads { threads })
    }

    fn load(&self, id: &str) -> Result<StoredThread, RpcError> {
        let dir = self.dir(id)?.ok_or_else(|| RpcError::invalid_params(format!("no saved thread {id}")))?;
        let _guard = self.lock.lock().unwrap();
        let meta = read_meta(&dir)?.ok_or_else(|| RpcError::invalid_params(format!("no saved thread {id}")))?;
        let events = read_events(&dir, true)?;
        Ok(StoredThread { meta, events })
    }

    fn write(&self, write: StoreWrite) -> Result<(), RpcError> {
        let Some(dir) = self.dir(&write.thread_id)? else { return Ok(()) };
        let _guard = self.lock.lock().unwrap();
        if write.meta.is_none() && !dir.join(META).is_file() {
            return Err(RpcError::invalid_params(format!("thread {} has no summary yet", write.thread_id)));
        }
        fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        // Skip the events it already has.
        let have = read_events(&dir, false)?.len() as u64;
        if write.first > have {
            eprintln!("space-session-store: {}: events {have}..{} are missing", write.thread_id, write.first);
        }
        let skip = have.saturating_sub(write.first) as usize;
        if write.events.len() > skip {
            let path = dir.join(EVENTS);
            let mut file = OpenOptions::new().read(true).append(true).create(true).open(&path).map_err(|e| io(&path, e))?;
            let mut lines = String::new();
            if !ends_with_newline(&mut file) {
                lines.push('\n');
            }
            for event in &write.events[skip..] {
                lines.push_str(&event.to_string());
                lines.push('\n');
            }
            file.write_all(lines.as_bytes()).map_err(|e| io(&path, e))?;
        }
        // A write sent again may carry an older summary.
        let newer = |m: &ThreadMeta| read_meta(&dir).ok().flatten().is_some_and(|saved| saved.updated_ms > m.updated_ms);
        if let Some(meta) = write.meta.filter(|m| !newer(m)) {
            let mut json = serde_json::to_value(meta).map_err(|e| RpcError::internal(e.to_string()))?;
            json["version"] = FORMAT_VERSION.into();
            let (tmp, path) = (dir.join("meta.json.tmp"), dir.join(META));
            fs::write(&tmp, format!("{json:#}\n")).map_err(|e| io(&tmp, e))?;
            fs::rename(&tmp, &path).map_err(|e| io(&path, e))?;
        }
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<(), RpcError> {
        let Some(dir) = self.dir(id)? else { return Ok(()) };
        let _guard = self.lock.lock().unwrap();
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&dir, e)),
            _ => Ok(()),
        }
    }
}

impl Plugin for Store {
    async fn initialize(&self, init: Initialize, _host: Host) -> Result<Initialized, RpcError> {
        match init.space_dir {
            Some(dir) => {
                let _ = self.root.set(Path::new(&dir).join("threads"));
            }
            None => eprintln!("space-session-store: no Space folder; threads stay in memory"),
        }
        Ok(Initialized::default())
    }

    async fn store_list(&self) -> Result<StoredThreads, RpcError> {
        self.list()
    }

    async fn store_load(&self, thread: ThreadRef) -> Result<StoredThread, RpcError> {
        self.load(&thread.thread_id)
    }

    async fn store_write(&self, write: StoreWrite) -> Result<(), RpcError> {
        self.write(write)
    }

    async fn store_delete(&self, thread: ThreadRef) -> Result<(), RpcError> {
        self.delete(&thread.thread_id)
    }

    async fn store_data_dir(&self, thread: ThreadRef) -> Result<DataDir, RpcError> {
        let dir = self.dir(&thread.thread_id)?.ok_or_else(|| RpcError::unavailable("nothing is kept on disk"))?;
        Ok(DataDir {
            data_dir: dir.join(AGENT).display().to_string(),
            attachments_dir: Some(dir.join(ATTACHMENTS).display().to_string()),
        })
    }
}

/// A thread folder's summary (`None`: it has none).
fn read_meta(dir: &Path) -> Result<Option<ThreadMeta>, RpcError> {
    let path = dir.join(META);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&path, e)),
    };
    let json: Value = serde_json::from_str(&text).map_err(|e| RpcError::internal(format!("{}: {e}", path.display())))?;
    if json["version"].as_u64().is_some_and(|v| v > FORMAT_VERSION) {
        return Err(RpcError::internal(format!("{} was saved by a newer Space", path.display())));
    }
    serde_json::from_value(json).map(Some).map_err(|e| RpcError::internal(format!("{}: {e}", path.display())))
}

/// A thread folder's events (lines that don't parse are skipped, and logged with `log`).
fn read_events(dir: &Path, log: bool) -> Result<Vec<Value>, RpcError> {
    let path = dir.join(EVENTS);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io(&path, e)),
    };
    let mut events = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(v) => events.push(v),
            Err(e) if log => eprintln!("space-session-store: {} line {}: skipped ({e})", path.display(), n + 1),
            Err(_) => {}
        }
    }
    Ok(events)
}

/// Whether the file is empty or ends a line.
fn ends_with_newline(file: &mut fs::File) -> bool {
    let mut last = [0u8; 1];
    match file.seek(SeekFrom::End(-1)) {
        Ok(_) => file.read_exact(&mut last).is_err() || last[0] == b'\n',
        Err(_) => true,
    }
}

fn is_older_than(path: &Path, age: Duration) -> bool {
    let modified = fs::metadata(path).and_then(|m| m.modified()).unwrap_or_else(|_| SystemTime::now());
    modified.elapsed().is_ok_and(|e| e > age)
}

fn io(path: &Path, e: std::io::Error) -> RpcError {
    RpcError::internal(format!("{}: {e}", path.display()))
}

fn main() {
    space_plugin::run(Store::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &Path) -> Store {
        let store = Store::default();
        let _ = store.root.set(root.to_path_buf());
        store
    }

    fn meta(id: &str, updated_ms: u64) -> ThreadMeta {
        ThreadMeta { id: id.into(), title: id.into(), project: "/p".into(), updated_ms, ..Default::default() }
    }

    /// Append `events` after what's saved (as Space does).
    fn write(store: &Store, id: &str, meta: Option<ThreadMeta>, events: &[Value]) -> Result<(), RpcError> {
        let first = store.dir(id)?.map_or(Ok(0), |d| read_events(&d, false).map(|e| e.len() as u64))?;
        store.write(StoreWrite { thread_id: id.into(), meta, events: events.to_vec(), first })
    }

    #[test]
    fn a_write_sent_again_keeps_each_event_once_and_the_newest_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let n = |n: u64| serde_json::json!({ "n": n });
        let send = |first, meta, events: Vec<Value>| store.write(StoreWrite { thread_id: "a".into(), meta, events, first });
        send(0, Some(meta("new", 2)), vec![n(1), n(2)]).unwrap();
        // Retried after a failure that had written the events; then a quit save repeating it.
        send(0, Some(meta("old", 1)), vec![n(1), n(2)]).unwrap();
        send(1, None, vec![n(2), n(3)]).unwrap();
        assert_eq!(store.load("a").unwrap().events, [n(1), n(2), n(3)]);
        assert_eq!(store.load("a").unwrap().meta.title, "new");
    }

    #[test]
    fn appends_lists_and_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        // Nothing is written before the thread has a summary.
        assert!(write(&store, "a", None, &[serde_json::json!(1)]).is_err());
        write(&store, "a", Some(meta("a", 1)), &[serde_json::json!({"n": 1})]).unwrap();
        write(&store, "a", None, &[serde_json::json!({"n": 2}), serde_json::json!({"n": 3})]).unwrap();
        write(&store, "b", Some(meta("b", 5)), &[]).unwrap();
        let ids: Vec<_> = store.list().unwrap().threads.into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["b", "a"]);
        let a = store.load("a").unwrap();
        assert_eq!(a.events, [serde_json::json!({"n": 1}), serde_json::json!({"n": 2}), serde_json::json!({"n": 3})]);
        let on_disk: Value = serde_json::from_str(&fs::read_to_string(tmp.path().join("a/meta.json")).unwrap()).unwrap();
        assert_eq!(on_disk["version"], FORMAT_VERSION);
        store.delete("a").unwrap();
        assert!(!tmp.path().join("a").exists());
        store.delete("a").unwrap();
    }

    #[test]
    fn a_cut_short_line_is_skipped_and_the_next_write_starts_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        write(&store, "a", Some(meta("a", 1)), &[serde_json::json!({"n": 1})]).unwrap();
        let mut f = OpenOptions::new().append(true).open(tmp.path().join("a/events.jsonl")).unwrap();
        f.write_all(br#"{"n": 2, "cut"#).unwrap();
        assert_eq!(store.load("a").unwrap().events, [serde_json::json!({"n": 1})]);
        write(&store, "a", None, &[serde_json::json!({"n": 3})]).unwrap();
        assert_eq!(store.load("a").unwrap().events, [serde_json::json!({"n": 1}), serde_json::json!({"n": 3})]);
    }

    #[test]
    fn without_a_folder_nothing_is_kept() {
        let store = Store::default();
        write(&store, "a", Some(meta("a", 1)), &[serde_json::json!(1)]).unwrap();
        assert!(store.list().unwrap().threads.is_empty());
        assert!(store.load("a").is_err());
    }

    #[test]
    fn rejects_paths_as_ids_and_prunes_old_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        for bad in ["../x", "a/b", "", "."] {
            assert!(store.dir(bad).is_err(), "{bad}");
        }
        let data = store.dir("fresh").unwrap().unwrap().join(AGENT);
        fs::create_dir_all(&data).unwrap();
        assert!(store.list().unwrap().threads.is_empty());
        // A young orphan stays (its thread may just not have a message yet).
        assert!(tmp.path().join("fresh").exists());
    }
}
