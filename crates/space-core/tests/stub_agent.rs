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
            TranscriptItem::User { text: t, .. } => format!("user:{t}"),
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

#[tokio::test(flavor = "multi_thread")]
async fn configure_switches_model_and_thinking() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    let status = session
        .configure(space_core::protocol::ConfigureSession {
            model: Some("stub-fast".into()),
            thinking_level: Some("high".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(status.model.as_deref(), Some("stub-fast"));
    assert_eq!(status.thinking_level.as_deref(), Some("high"));
    assert_eq!(status.available_models.as_ref().map(Vec::len), Some(2));
    // The response is also folded into session state.
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::Session { event: SessionEvent::Status(s), .. } if s.model.as_deref() == Some("stub-fast"))
    })
    .await;
    assert_eq!(state.status.thinking_level.as_deref(), Some("high"));
    assert_eq!(state.status.current_model().and_then(|m| m.name.as_deref()), Some("Stub Fast"));

    let err = session
        .configure(space_core::protocol::ConfigureSession { model: Some("nope".into()), ..Default::default() })
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("configure"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn set_mode_round_trips() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    // session/new advertised the modes; Write is current.
    let modes = session.modes().cloned().expect("stub advertises modes");
    assert_eq!(modes.current, "write");
    assert!(modes.has("read") && modes.has("write"));
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::Session { event: SessionEvent::Modes(_), .. })
    })
    .await;
    assert_eq!(state.modes.as_ref().map(|m| m.current.as_str()), Some("write"));

    session.set_mode("read").await.unwrap();
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::Session { event: SessionEvent::ModeChanged { .. }, .. })
    })
    .await;
    assert_eq!(state.modes.as_ref().map(|m| m.current.as_str()), Some("read"));

    // The next prompt runs in read mode (the stub logs it).
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
    assert!(agent.logs().iter().any(|l| l.contains("prompt \"hello\" (mode read)")), "{:?}", agent.logs());

    let err = session.set_mode("plan").await.unwrap_err();
    assert!(format!("{err:#}").contains("set_mode"), "{err:#}");
    session.set_mode("write").await.unwrap();
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::Session { event: SessionEvent::ModeChanged { mode_id }, .. } if mode_id == "write")
    })
    .await;
    assert_eq!(state.modes.unwrap().current, "write");
}
