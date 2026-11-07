//! End to end: the real permissions plugin in the host's pipeline, the stub agent as the
//! consumer, and a scripted local server standing in for OpenAI. Covers the fail-closed hold,
//! its recovery (Retry now, a new key, the 5-minute probe with a test interval), key
//! validation and the blockers' contents.
//!
//! Needs the stub agent built next to the plugin (`cargo test --workspace` does that).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use space_core::protocol::service::CatalogEntry;
use space_core::{
    DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, Secrets, SessionEvent, SessionState, SettingError,
    ToolStatus, TranscriptItem,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PLUGIN: &str = env!("CARGO_BIN_EXE_space-permissions");
/// Test-only Keychain services, one per test (they run in parallel).
const KEYCHAIN: &str = "dev.brijesh.space.test.guardrail";
const KEYCHAIN_OUTAGE: &str = "dev.brijesh.space.test.guardrail-outage";

/// What the fake OpenAI answers.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Api {
    Allow,
    Rejected,
    Down,
}

struct Mock {
    url: String,
    api: Arc<Mutex<Api>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    async fn start(api: Api) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (api, requests) = (Arc::new(Mutex::new(api)), Arc::new(Mutex::new(Vec::new())));
        let (a, r) = (api.clone(), requests.clone());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (a, r) = (a.clone(), r.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 16384];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let len = text[..end]
                                .lines()
                                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    r.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
                    let api = *a.lock().unwrap();
                    let (status, body) = match api {
                        Api::Allow => (
                            200,
                            r#"{"model":"gpt-6-luna","answers":[
                                {"type":"choice","name":"verdict","choice":"allow","confidence":0.97,"probabilities":[]},
                                {"type":"choice","name":"concern","choice":"none","confidence":0.9,"probabilities":[]}]}"#
                                .replace('\n', " "),
                        ),
                        Api::Rejected => (401, r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key"}}"#.into()),
                        Api::Down => (503, r#"{"error":{"message":"overloaded"}}"#.into()),
                    };
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                });
            }
        });
        Self { url, api, requests }
    }

    fn set(&self, api: Api) {
        *self.api.lock().unwrap() = api;
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn stub_agent() -> PathBuf {
    let path = Path::new(PLUGIN).with_file_name("space-stub-agent");
    assert!(path.exists(), "{} is missing: build the workspace first (cargo build --workspace)", path.display());
    path
}

/// A plugin root with the stub agent and the permissions plugin pointed at `mock`.
fn root(mock: &Mock, retry_secs: f64) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write(
        "stub",
        format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\n", stub_agent().display()),
    );
    write(
        "permissions",
        format!(
            "id='permissions'\nname='Permissions'\nversion='1'\n\
             provides=['sandbox','context','settings','catalog:permission-modes']\n\
             hooks=[{{event='tools/pre-execute', order=10}}]\nrequired=true\n[run]\ncommand='{PLUGIN}'\n\
             [run.env]\nSPACE_LUNA_BASE_URL='{}'\nSPACE_GUARD_RETRY_SECS='{retry_secs}'\n",
            mock.url
        ),
    );
    tmp
}

fn host(root: &Path, keychain: &str) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
        secrets: Secrets::new(keychain),
        held_recheck: Duration::from_secs(60),
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

/// Fold events until `pred` holds for one (and return the latest permission-modes catalog).
async fn until(
    events: &mut EventReceiver,
    state: &mut SessionState,
    catalog: &mut Vec<CatalogEntry>,
    secs: u64,
    mut pred: impl FnMut(&HostEvent, &SessionState) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(e) = events.recv().await {
            match &e {
                HostEvent::Session { session_id, event } if *session_id == state.id => state.apply(event),
                HostEvent::Catalog { name, entries } if name == "permission-modes" => catalog.clone_from(entries),
                _ => {}
            }
            if pred(&e, state) {
                return;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("timed out waiting");
}

fn ended(e: &HostEvent, _: &SessionState) -> bool {
    matches!(e, HostEvent::Session { event: SessionEvent::TurnEnded { .. }, .. })
}

fn held(_: &HostEvent, s: &SessionState) -> bool {
    s.held().is_some()
}

fn last_tool(state: &SessionState) -> space_core::ToolCallInfo {
    state
        .items
        .iter()
        .rev()
        .find_map(|i| match i {
            TranscriptItem::Tool(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap()
}

fn workspace_write(catalog: &[CatalogEntry]) -> CatalogEntry {
    catalog.iter().find(|e| e.id == "workspace-write").cloned().expect("workspace-write")
}

/// Workspace Write as the host has it once `pred` holds (the catalog refreshes asynchronously).
async fn wait_ww(host: &Host, pred: impl Fn(&CatalogEntry) -> bool) -> CatalogEntry {
    for _ in 0..100 {
        let ww = workspace_write(&host.catalog("permission-modes"));
        if pred(&ww) {
            return ww;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("workspace-write never matched: {:?}", workspace_write(&host.catalog("permission-modes")));
}

#[tokio::test(flavor = "multi_thread")]
async fn key_rejected_mid_thread_holds_until_retry_or_a_new_key() {
    let mock = Mock::start(Api::Allow).await;
    let root = root(&mock, 300.0);
    Secrets::new(KEYCHAIN).set("permissions", "openai_api_key", None).unwrap();
    let (host, mut events) = host(root.path(), KEYCHAIN);
    host.start_services().await;
    let mut catalog = host.catalog("permission-modes");
    // No key: Workspace Write isn't ready, and says how to fix it.
    let ww = workspace_write(&catalog);
    assert!(!ww.ready);
    let b = ww.blocker.unwrap();
    assert!(b.text.contains("Add an OpenAI API key"), "{}", b.text);
    assert_eq!(b.actions.iter().map(|a| a.action.as_str()).collect::<Vec<_>>(), ["open-settings:permissions", "use-mode:read-only"]);

    // A rejected key is never stored.
    mock.set(Api::Rejected);
    assert_eq!(
        host.set_setting("permissions", "openai_api_key", Some("sk-bad".into()), false).await,
        Err(SettingError::Invalid("OpenAI rejected this key.".into()))
    );
    assert_eq!(Secrets::new(KEYCHAIN).get("permissions", "openai_api_key").unwrap(), None);
    // A working one is, and the modes become ready without a second check.
    mock.set(Api::Allow);
    let before = mock.count();
    host.set_setting("permissions", "openai_api_key", Some("sk-good".into()), false).await.unwrap();
    wait_ww(&host, |e| e.ready).await;
    assert_eq!(mock.count(), before + 1, "validated once, not probed again");

    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    session.set_permission_mode("workspace-write");
    let mut state = SessionState::new(session.id());
    session.prompt("hello").await.unwrap();
    until(&mut events, &mut state, &mut catalog, 15, ended).await;
    assert_eq!(last_tool(&state).status, ToolStatus::Completed, "reviewed and allowed");

    // The key stops working mid-thread: the next call is held, never asked or run.
    mock.set(Api::Rejected);
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("again").await });
    until(&mut events, &mut state, &mut catalog, 15, held).await;
    assert!(state.held().unwrap().starts_with("OpenAI rejected your API key"), "{:?}", state.held());
    assert!(state.pending_permission().is_none());
    let ww = wait_ww(&host, |e| !e.ready).await;
    let b = ww.blocker.unwrap();
    assert!(b.last_attempt_ms.is_some() && b.next_attempt_ms.is_some(), "{b:?}");
    let next_in = b.next_attempt_ms.unwrap() - b.last_attempt_ms.unwrap();
    assert!((299_000..=301_000).contains(&next_in), "next try in 5 min, got {next_in} ms");
    assert!(b.actions.iter().any(|a| a.action == "retry:permissions"));

    // Retry now while still rejected: one probe, still held.
    let n = mock.count();
    host.retry_plugin("permissions").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(mock.count(), n + 1, "Retry now probes at once");
    assert!(state.held().is_some());
    // OpenAI takes the key again; Retry now releases the held call.
    mock.set(Api::Allow);
    host.retry_plugin("permissions").await;
    until(&mut events, &mut state, &mut catalog, 15, ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(last_tool(&state).status, ToolStatus::Completed);

    // Rejected again; this time a new key releases it.
    mock.set(Api::Rejected);
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("third").await });
    until(&mut events, &mut state, &mut catalog, 15, held).await;
    mock.set(Api::Allow);
    host.set_setting("permissions", "openai_api_key", Some("sk-new".into()), false).await.unwrap();
    until(&mut events, &mut state, &mut catalog, 15, ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(last_tool(&state).status, ToolStatus::Completed);
    wait_ww(&host, |e| e.ready).await;

    // No file contents or outputs ever went out.
    for r in mock.requests.lock().unwrap().iter() {
        assert!(!r.contains("argv:"), "a tool output was sent: {r}");
    }
    Secrets::new(KEYCHAIN).set("permissions", "openai_api_key", None).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_outage_holds_calls_and_the_probe_resumes_them() {
    let mock = Mock::start(Api::Down).await;
    let root = root(&mock, 1.0);
    Secrets::new(KEYCHAIN_OUTAGE).set("permissions", "openai_api_key", None).unwrap();
    let (host, mut events) = host(root.path(), KEYCHAIN_OUTAGE);
    host.start_services().await;
    // Saving can't be checked while OpenAI is down: only with "Save anyway".
    let r = host.set_setting("permissions", "openai_api_key", Some("sk-x".into()), false).await;
    assert!(matches!(r, Err(SettingError::Unverifiable(ref m)) if m.contains("Couldn't reach OpenAI")), "{r:?}");
    host.set_setting("permissions", "openai_api_key", Some("sk-x".into()), true).await.unwrap();

    let mut catalog = Vec::new();
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    session.set_permission_mode("full-access");
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    // First "Checking…", then, once the probe fails, why.
    until(&mut events, &mut state, &mut catalog, 15, |_, s| s.held().is_some_and(|r| r.starts_with("Can't reach OpenAI"))).await;
    assert!(state.held().unwrap().contains("HTTP 503"), "{:?}", state.held());
    let b = wait_ww(&host, |e| !e.ready).await.blocker.unwrap();
    assert!(b.next_attempt_ms.unwrap() - b.last_attempt_ms.unwrap() <= 1_100, "{b:?}");
    // Probes keep failing every interval (1 s here) while it's down.
    let n = mock.count();
    tokio::time::sleep(Duration::from_millis(2600)).await;
    assert!(mock.count() >= n + 2, "probed on schedule: {} -> {}", n, mock.count());
    assert_eq!(last_tool(&state).status, ToolStatus::Pending);
    // Back up: the next probe releases the call.
    mock.set(Api::Allow);
    until(&mut events, &mut state, &mut catalog, 15, ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(last_tool(&state).status, ToolStatus::Completed);
    wait_ww(&host, |e| e.ready).await;
    assert!(host.catalog("permission-modes").iter().all(|e| e.ready));
    Secrets::new(KEYCHAIN_OUTAGE).set("permissions", "openai_api_key", None).unwrap();
}
