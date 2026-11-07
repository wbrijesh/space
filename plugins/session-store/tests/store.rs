//! End to end: the real session store behind the host, the stub agent as the agent. A thread
//! saved through the host reads back the same, survives a new host (a restart), and its
//! session loads again and remembers; a store that can't write doesn't stop the conversation.
//!
//! Needs the stub agent built next to the plugin (`cargo test --workspace` does that).

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::protocol::service::ThreadMeta;
use space_core::{DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, Secrets, SessionEvent, SessionState, TranscriptItem};

const PLUGIN: &str = env!("CARGO_BIN_EXE_space-session-store");

fn stub_agent() -> PathBuf {
    let path = Path::new(PLUGIN).with_file_name("space-stub-agent");
    assert!(path.is_file(), "build the stub agent first (cargo test --workspace)");
    path
}

/// A plugin root with the stub agent and the session store.
fn root() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join("plugins").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\n", stub_agent().display()));
    write(
        "session-store",
        format!("id='session-store'\nname='Session Store'\nversion='1'\nprovides=['session-store']\nuses=['read:transcript']\n[run]\ncommand='{PLUGIN}'\n"),
    );
    tmp
}

fn host(root: &Path) -> (Host, EventReceiver) {
    let plugins = root.join("plugins");
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![plugins], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.session-store"),
        held_recheck: Duration::from_secs(60),
        space_dir: Some(root.join("space")),
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

/// The session events reported so far (until the stream is quiet for a moment).
async fn drain(events: &mut EventReceiver) -> Vec<SessionEvent> {
    let mut out = Vec::new();
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
        if let HostEvent::Session { event, .. } = e {
            out.push(event);
        }
    }
    out
}

fn meta(id: &str, session_id: &str, root: &Path) -> ThreadMeta {
    ThreadMeta {
        id: id.into(),
        title: "hello".into(),
        project: root.display().to_string(),
        agent: Some("stub".into()),
        session_id: Some(session_id.into()),
        ..Default::default()
    }
}

fn last_agent_text(state: &SessionState) -> Option<&str> {
    state.items.iter().rev().find_map(|i| match i {
        TranscriptItem::Agent(t) => Some(t.as_str()),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_thread_comes_back_after_a_restart_and_continues() {
    let root = root();
    let (host, mut events) = host(root.path());
    host.start_services().await;
    assert!(host.has_session_store());
    let agent = host.start_agent("stub").await.unwrap();
    let data_dir = host.thread_data_dir("t1").await.expect("a data folder");
    assert!(data_dir.starts_with(root.path().join("space/threads/t1")));
    let session = agent.new_session_in(root.path(), Some(data_dir.clone())).await.unwrap();
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
    let live = drain(&mut events).await;
    let mut shown = SessionState::default();
    live.iter().for_each(|e| shown.apply(e));
    host.save_thread("t1", Some(meta("t1", session.id(), root.path())), 0, &live).await.unwrap();

    // Read back through the host: the same transcript.
    let (saved_meta, saved, count) = host.load_thread("t1").await.unwrap();
    assert_eq!(count, saved.len() as u64);
    assert_eq!(saved_meta.session_id.as_deref(), Some(session.id()));
    let mut replayed = SessionState::default();
    saved.iter().for_each(|e| replayed.apply(e));
    assert_eq!(replayed.items, shown.items);
    let session_id = session.id().to_string();
    drop((session, agent));
    host.shutdown();

    // A line cut short by a quit mid-write is skipped.
    let log = root.path().join("space/threads/t1/events.jsonl");
    std::fs::write(&log, format!("{}{{\"TurnEnd", std::fs::read_to_string(&log).unwrap())).unwrap();

    // A new host (Space restarted) lists it, and the agent continues the session.
    let (host, mut events) = self::host(root.path());
    host.start_services().await;
    let listed = host.list_threads().await.unwrap();
    assert_eq!(listed.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1"]);
    assert_eq!(host.load_thread("t1").await.unwrap().1, saved);
    let agent = host.start_agent("stub").await.unwrap();
    assert!(agent.can_load_session());
    let session = agent.load_session(&session_id, root.path(), host.thread_data_dir("t1").await).await.unwrap();
    assert_eq!(session.id(), session_id);
    // What the agent replays while loading isn't reported (the thread shows it already).
    let replay = drain(&mut events).await;
    assert!(!replay.iter().any(|e| matches!(e, SessionEvent::AgentText { .. })), "{replay:?}");
    assert_eq!(session.prompt("recall").await.unwrap(), "end_turn");
    let mut state = SessionState::default();
    drain(&mut events).await.iter().for_each(|e| state.apply(e));
    assert_eq!(last_agent_text(&state), Some("I remember 1 earlier prompt(s)."));

    // Deleting removes the folder, the agent's files with it.
    host.delete_thread("t1").await.unwrap();
    assert!(!root.path().join("space/threads/t1").exists());
    assert!(host.list_threads().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_that_cant_write_doesnt_stop_the_conversation() {
    let root = root();
    // `threads` is a file, so nothing can be written under it.
    std::fs::create_dir_all(root.path().join("space")).unwrap();
    std::fs::write(root.path().join("space/threads"), "").unwrap();
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(root.path(), host.thread_data_dir("t1").await).await.unwrap();
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
    let live = drain(&mut events).await;
    let err = host.save_thread("t1", Some(meta("t1", session.id(), root.path())), 0, &live).await.unwrap_err();
    assert!(format!("{err:#}").contains("Session Store"), "{err:#}");
    // The session goes on.
    assert_eq!(session.prompt("again").await.unwrap(), "end_turn");
}
