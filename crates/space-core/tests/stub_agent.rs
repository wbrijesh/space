//! End-to-end tests of the host against the `space-stub-agent` binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::{
    DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PermissionDecision, PluginStatus, SessionEvent,
    SessionState, ToolStatus, TranscriptItem,
};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");

/// A temp plugin root containing one plugin `stub` that runs the stub agent binary.
fn stub_root() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        format!("id = 'stub'\nname = 'Stub'\nversion = '0.1.0'\nkind = 'agent'\n[run]\ncommand = '{STUB}'\n"),
    )
    .unwrap();
    tmp
}

fn host(root: &Path) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

/// Receive events until `pred` matches, folding session events into `state`.
async fn until(
    events: &mut EventReceiver,
    state: &mut SessionState,
    seen: &mut Vec<HostEvent>,
    mut pred: impl FnMut(&HostEvent) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = events.recv().await {
            if let HostEvent::Session { session_id, event } = &e
                && *session_id == state.id
            {
                state.apply(event);
            }
            let done = pred(&e);
            seen.push(e);
            if done {
                return;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("timed out waiting for event");
}

fn turn_ended(e: &HostEvent) -> bool {
    matches!(e, HostEvent::Session { event: SessionEvent::TurnEnded { .. }, .. })
}

#[tokio::test(flavor = "multi_thread")]
async fn full_turn_streams_and_ends() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    assert_eq!(host.plugins().len(), 1);

    let agent = host.start_agent("stub").await.unwrap();
    assert_eq!(agent.agent_name(), Some("space-stub-agent"));
    let session = agent.new_session(PathBuf::from(root.path())).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    let stop = session.prompt("hello").await.unwrap();
    assert_eq!(stop, "end_turn");
    until(&mut events, &mut state, &mut seen, turn_ended).await;

    assert!(seen.iter().any(|e| matches!(e, HostEvent::PluginStatus { status: PluginStatus::Running, .. })));
    assert_eq!(state.status.model.as_deref(), Some("stub-model"));
    assert_eq!(state.status.thinking_level.as_deref(), Some("low"));
    assert!(!state.running);
    let kinds: Vec<_> = state
        .items
        .iter()
        .map(|i| match i {
            TranscriptItem::User(t) => format!("user:{t}"),
            TranscriptItem::Thought(t) => format!("thought:{t}"),
            TranscriptItem::Tool(t) => format!("tool:{}:{}:{}", t.kind, t.status.as_str(), t.output),
            TranscriptItem::Agent(t) => format!("agent:{t}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user:hello",
            "thought:Thinking about the request.",
            "tool:execute:done:stub",
            "agent:Hello from the stub agent."
        ]
    );
    assert!(agent.logs().iter().any(|l| l.contains("prompt \"hello\"")));

    // Stopping reports the plugin as stopped.
    agent.stop();
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::PluginStatus { status: PluginStatus::Stopped, .. })
    })
    .await;
    assert!(!agent.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_allow_and_reject() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    for (answer, expected) in [("allow", ToolStatus::Completed), ("reject", ToolStatus::Failed)] {
        let s = session.clone();
        let turn = tokio::spawn(async move { s.prompt("needs permission").await });
        until(&mut events, &mut state, &mut seen, |e| {
            matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. })
        })
        .await;
        let req = state.pending_permission().cloned().expect("pending permission");
        assert_eq!(req.options.len(), 2);
        assert_eq!(req.tool_call.title.as_deref(), Some("bash: echo stub"));
        session.resolve_permission(req.request_id, PermissionDecision::Selected(answer.into())).unwrap();
        until(&mut events, &mut state, &mut seen, turn_ended).await;
        assert_eq!(turn.await.unwrap().unwrap(), "end_turn");
        assert!(state.pending_permission().is_none());
        let last_tool = state
            .items
            .iter()
            .rev()
            .find_map(|i| match i {
                TranscriptItem::Tool(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(last_tool.status, expected, "answer {answer}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_turn() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("slow please").await });
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::Session { event: SessionEvent::AgentText { .. }, .. })
    })
    .await;
    assert!(session.is_running());
    session.cancel().unwrap();
    until(&mut events, &mut state, &mut seen, turn_ended).await;
    assert_eq!(turn.await.unwrap().unwrap(), "cancelled");
    assert!(matches!(state.items.last(), Some(TranscriptItem::TurnEnd(r)) if r == "cancelled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_is_reported() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    let err = session.prompt("crash now").await.unwrap_err();
    assert!(err.to_string().contains("session/prompt failed"), "{err:#}");
    // Crash status and the turn's error can arrive in either order.
    let (mut crashed, mut ended) = (false, false);
    until(&mut events, &mut state, &mut seen, |e| {
        crashed |= matches!(e, HostEvent::PluginStatus { status: PluginStatus::Crashed(_), .. });
        ended |= turn_ended(e);
        crashed && ended
    })
    .await;
    let crashed = seen.iter().find_map(|e| match e {
        HostEvent::PluginStatus { status: PluginStatus::Crashed(m), .. } => Some(m.clone()),
        _ => None,
    });
    assert!(crashed.unwrap().contains("code 3"));
    assert!(state.items.iter().any(|i| matches!(i, TranscriptItem::Error(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn unbuilt_plugin_fails_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("nobuild");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        "id='nobuild'\nname='N'\nversion='1'\nkind='agent'\n[run]\ncommand='node'\nargs=['dist/main.js']\n",
    )
    .unwrap();
    let (host, _events) = host(tmp.path());
    let err = host.start_agent("nobuild").await.err().unwrap();
    assert!(format!("{err:#}").contains("is the plugin built"), "{err:#}");
    assert!(host.start_agent("missing").await.is_err());
}
