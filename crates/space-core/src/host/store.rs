//! The `session-store` seam: saved threads. The app decides what a thread is and when to save
//! it; the store keeps each thread's summary and its events, and the folder its agent may keep
//! the session's own files in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use space_protocol::CAP_SESSION_STORE;
use space_protocol::service::{self as svc, ThreadMeta};

use super::{Host, HostInner};
use crate::session::SessionEvent;

/// Loading a long thread reads a large file.
const STORE_TIMEOUT: Duration = Duration::from_secs(20);

/// One thread's save at quit ([`Host::save_threads_blocking`]).
pub struct ThreadSave {
    pub id: String,
    pub meta: ThreadMeta,
    /// The event number of the first of `events`.
    pub first: u64,
    pub events: Vec<SessionEvent>,
}

/// A thread's folders ([`Host::thread_dirs`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadDirs {
    pub data: Option<PathBuf>,
    pub attachments: PathBuf,
}

impl Host {
    /// Whether a session store is installed (on and approved).
    pub fn has_session_store(&self) -> bool {
        !self.inner.providers(CAP_SESSION_STORE).is_empty()
    }

    /// Every saved thread, newest first.
    pub async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let list: svc::StoredThreads = self.store_call(svc::STORE_LIST, serde_json::json!({})).await?;
        Ok(list.threads)
    }

    /// A saved thread's summary, its events (events this Space can't read are left out) and
    /// how many events the store has (the `first` of the next write).
    pub async fn load_thread(&self, thread_id: &str) -> Result<(ThreadMeta, Vec<SessionEvent>, u64)> {
        let stored: svc::StoredThread = self.store_call(svc::STORE_LOAD, thread_ref(thread_id)).await?;
        let count = stored.events.len() as u64;
        let events = stored
            .events
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).map_err(|err| tracing::warn!("skipping a saved event of {thread_id}: {err}")).ok())
            .collect();
        Ok((stored.meta, events, count))
    }

    /// Append `events` to a thread, the first being its event number `first` (events the store
    /// already has are skipped), and, with `meta`, replace its summary.
    pub async fn save_thread(&self, thread_id: &str, meta: Option<ThreadMeta>, first: u64, events: &[SessionEvent]) -> Result<()> {
        let events = events.iter().map(serde_json::to_value).collect::<Result<_, _>>()?;
        let write = svc::StoreWrite { thread_id: thread_id.to_string(), meta, events, first };
        let _: serde_json::Value = self.store_call(svc::STORE_WRITE, write).await?;
        Ok(())
    }

    /// Save threads, then delete threads, while Space quits (from a thread outside the
    /// runtime), waiting at most `timeout` in all. Call before [`Host::shutdown`], which stops
    /// the store.
    pub fn save_threads_blocking(&self, saves: Vec<ThreadSave>, deletes: Vec<String>, timeout: Duration) {
        if saves.is_empty() && deletes.is_empty() {
            return;
        }
        let host = self.clone();
        let saved = self.inner.rt.block_on(tokio::time::timeout(timeout, async move {
            for save in saves {
                if let Err(e) = host.save_thread(&save.id, Some(save.meta), save.first, &save.events).await {
                    tracing::warn!("saving thread {}: {e:#}", save.id);
                }
            }
            for id in deletes {
                if let Err(e) = host.delete_thread(&id).await {
                    tracing::warn!("deleting thread {id}: {e:#}");
                }
            }
        }));
        if saved.is_err() {
            tracing::warn!("quitting before every thread was saved");
        }
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<()> {
        let _: serde_json::Value = self.store_call(svc::STORE_DELETE, thread_ref(thread_id)).await?;
        Ok(())
    }

    /// The thread's folders: the one its agent may keep the session's own files in (`None`: no
    /// store, or it couldn't say; the agent then keeps the session in memory), and the one its
    /// attached images are kept in (a temporary folder when the store keeps none).
    pub async fn thread_dirs(&self, thread_id: &str) -> ThreadDirs {
        let temp = || std::env::temp_dir().join("space-attachments").join(thread_id);
        if !self.has_session_store() {
            return ThreadDirs { data: None, attachments: temp() };
        }
        match self.store_call::<_, svc::DataDir>(svc::STORE_DATA_DIR, thread_ref(thread_id)).await {
            Ok(d) => ThreadDirs { data: Some(PathBuf::from(d.data_dir)), attachments: d.attachments_dir.map_or_else(temp, PathBuf::from) },
            Err(e) => {
                tracing::debug!("{e:#}");
                ThreadDirs { data: None, attachments: temp() }
            }
        }
    }

    async fn store_call<P: Serialize + Send + Sync + 'static, R: DeserializeOwned + Send + 'static>(&self, method: &'static str, params: P) -> Result<R> {
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move { inner.store_call(method, &params).await }).await?
    }
}

impl HostInner {
    async fn store_call<P: Serialize, R: DeserializeOwned>(self: &Arc<Self>, method: &str, params: &P) -> Result<R> {
        let plugin = self.providers(CAP_SESSION_STORE).into_iter().next().ok_or_else(|| anyhow!("no session store is installed"))?;
        self.call_service(plugin.id(), method, params, STORE_TIMEOUT)
            .await
            .map_err(|e| anyhow!("{} ({method}): {e}", plugin.manifest.name))
    }
}

fn thread_ref(thread_id: &str) -> svc::ThreadRef {
    svc::ThreadRef { thread_id: thread_id.to_string() }
}
