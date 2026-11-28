//! The app and the server: a turn keeps going after its app leaves, and the next app finds it
//! running, reads the thread whole and follows the rest of it, every event once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::protocol::service::ThreadMeta;
use space_core::{DiscoveryOptions, HostEvent, HostOptions, Secrets, SessionEvent, SessionState};
use space_server::{Options, Server};

fn target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
}

/// Plugins: the stub agent playing `script`, and the session store.
fn root(script: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("script.json");
    std::fs::write(&script_path, script).unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join("plugins").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    let (stub, store) = (target().join("space-stub-agent"), target().join("space-session-store"));
    assert!(stub.is_file() && store.is_file(), "build the workspace first (cargo test --workspace)");
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\n[run.env]\nSPACE_STUB_SCRIPT='{}'\n", stub.display(), script_path.display()));
    write("session-store", format!("id='session-store'\nname='Store'\nversion='1'\nprovides=['session-store']\nuses=['read:transcript']\n[run]\ncommand='{}'\n", store.display()));
    tmp
}

async fn connect(server: &std::sync::Arc<Server>) -> (space_client::Host, space_client::EventReceiver) {
    let (app_side, server_side) = tokio::io::duplex(1 << 20);
    let (r, w) = tokio::io::split(server_side);
    tokio::spawn(server.clone().serve(r, w));
    let (r, w) = tokio::io::split(app_side);
    space_client::Host::connect(r, w, true).await.unwrap()
}

/// Session events until `pred` holds for one (included).
async fn until(events: &mut space_client::EventReceiver, mut pred: impl FnMut(&SessionEvent) -> bool) -> Vec<SessionEvent> {
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = events.recv().await {
            if let HostEvent::Session { event, .. } = e {
                let done = pred(&event);
                seen.push(event);
                if done {
                    return;
                }
            }
        }
        panic!("events ended");
    })
    .await
    .expect("timed out");
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_outlives_its_app_and_the_next_app_follows_it() {
    let root = root(
        r#"{"turns": [{"prompt": "slow", "steps": [
            {"type": "text", "text": "first "}, {"type": "sleep", "ms": 600}, {"type": "text", "text": "second"}
        ]}]}"#,
    );
    let options = HostOptions {
        project_dir: root.path().to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.path().join("plugins")], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.client"),
        held_recheck: Duration::from_secs(60),
        space_dir: Some(root.path().join("space")),
        keeper: None,
    };
    let (core, core_events) = space_core::Host::new(tokio::runtime::Handle::current(), options);
    let server = Server::start(core, core_events, Options::default());

    // App A starts a slow turn, then quits while it runs.
    let (a, mut a_events) = connect(&server).await;
    a.start_services().await;
    assert!(a.has_session_store(), "the state arrived");
    let agent = a.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(root.path(), Some("t1")).await.unwrap();
    a.save_thread_meta(ThreadMeta { id: "t1".into(), title: "slow".into(), project: root.path().display().to_string(), ..Default::default() });
    tokio::spawn({
        let session = session.clone();
        async move { session.prompt("slow").await }
    });
    until(&mut a_events, |e| matches!(e, SessionEvent::AgentText { text } if text.contains("first"))).await;
    assert!(session.is_running());
    a.close();
    drop((session, agent, a, a_events));

    // App B finds the session running, reads the thread and follows the rest.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (b, mut b_events) = connect(&server).await;
    let live = b.live_sessions();
    assert_eq!(live.len(), 1, "{live:?}");
    assert!(live[0].running && live[0].thread_id.as_deref() == Some("t1"));
    let session = b.adopt_session(&live[0]).unwrap();
    assert!(session.is_running());
    let (_, loaded, _) = b.load_thread("t1").await.unwrap();
    let after = until(&mut b_events, |e| matches!(e, SessionEvent::TurnEnded { .. })).await;
    let mut state = SessionState::new("t1");
    loaded.iter().chain(&after).for_each(|e| state.apply(e));
    let text: String = state.items.iter().filter_map(|i| if let space_core::TranscriptItem::Agent(t) = i { Some(t.as_str()) } else { None }).collect();
    assert_eq!(text, "first second", "every event once");
    assert!(!state.running && !session.is_running());

    // And B can go on in it.
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
}
