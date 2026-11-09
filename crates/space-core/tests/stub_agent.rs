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
        format!("id = 'stub'\nname = 'Stub'\nversion = '0.1.0'\nprovides = ['agent']\n[run]\ncommand = '{STUB}'\n"),
    )
    .unwrap();
    tmp
}

fn host(root: &Path) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
        secrets: space_core::Secrets::new("dev.brijesh.space.test"),
        held_recheck: space_core::HELD_RECHECK,
        space_dir: None,
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
    // The message and the turn end carry times; the turn end, the agent's usage.
    let ended = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
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
            TranscriptItem::TurnEnd { reason, .. } => format!("turn-end:{reason}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user:hello",
            "thought:Thinking about the request.",
            "tool:execute:done:stub\nargv: /bin/sh -c echo stub",
            "agent:Hello from the stub agent.",
            "turn-end:end_turn"
        ]
    );
    let TranscriptItem::User { at_ms: Some(sent), .. } = state.items[0] else { panic!("no time on {:?}", state.items[0]) };
    let Some(TranscriptItem::TurnEnd { at_ms: Some(at), usage: Some(usage), .. }) = state.items.last() else {
        panic!("no time or usage on {:?}", state.items.last())
    };
    assert!(sent <= *at && *at <= ended + 1000, "{sent} {at} {ended}");
    assert_eq!((usage.total(), usage.cache_hit_percent(), usage.models.as_slice()), (9501, Some(86), &["stub/stub-model".to_string()][..]));
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
    assert!(matches!(state.items.last(), Some(TranscriptItem::TurnEnd { reason, .. }) if reason == "cancelled"));
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
        "id='nobuild'\nname='N'\nversion='1'\nprovides=['agent']\n[run]\ncommand='node'\nargs=['dist/main.js']\n",
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
async fn an_agent_on_another_protocol_version_is_refused() {
    let tmp = stub_root();
    let manifest = tmp.path().join("stub/space-plugin.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}[run.env]\nSPACE_STUB_PROTOCOL = '999'\n")).unwrap();
    let (host, mut events) = host(tmp.path());
    let err = host.start_agent("stub").await.err().expect("refused");
    assert!(err.to_string().contains("protocol v999"), "{err:#}");
    let (mut state, mut seen) = (SessionState::default(), Vec::new());
    until(&mut events, &mut state, &mut seen, |e| {
        matches!(e, HostEvent::PluginStatus { status: PluginStatus::NotStarted(m), .. } if m.contains("v999"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn images_go_as_image_blocks_after_the_message_event() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    assert!(agent.accepts_images());
    let session = agent.new_session(PathBuf::from(root.path())).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    let path = root.path().join("abc.png");
    std::fs::write(&path, b"12345").unwrap();
    let image = space_core::ImageRef { path: path.clone(), mime: "image/png".into(), width: 2, height: 1, name: Some("shot.png".into()) };
    let stop = session.prompt_with("what is this", vec![], vec![image.clone()]).await.unwrap();
    assert_eq!(stop, "end_turn");
    until(&mut events, &mut state, &mut seen, turn_ended).await;

    let TranscriptItem::User { text, images, at_ms, .. } = &state.items[0] else { panic!("{:?}", state.items[0]) };
    assert_eq!((text.as_str(), images.as_slice(), at_ms.is_some()), ("what is this", &[image][..], true));
    let reply: String = state.items.iter().filter_map(|i| if let TranscriptItem::Agent(t) = i { Some(t.as_str()) } else { None }).collect();
    assert!(reply.starts_with(&format!("Saw 1 image (image/png 5 bytes file://{}). ", path.display())), "{reply}");

    // A missing file fails the send before anything is shown.
    let gone = space_core::ImageRef { path: root.path().join("gone.png"), mime: "image/png".into(), width: 1, height: 1, name: None };
    assert!(session.prompt_with("again", vec![], vec![gone]).await.is_err());
    assert!(!session.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_without_images_refuses_them() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        format!(
            "id = 'stub'\nname = 'Stub'\nversion = '0.1.0'\nprovides = ['agent']\n[run]\ncommand = '/usr/bin/env'\nargs = ['SPACE_STUB_NO_IMAGES=1', '{STUB}']\n"
        ),
    )
    .unwrap();
    let (host, _events) = host(tmp.path());
    let agent = host.start_agent("stub").await.unwrap();
    assert!(!agent.accepts_images());
    let session = agent.new_session(PathBuf::from(tmp.path())).await.unwrap();
    let image = space_core::ImageRef { path: tmp.path().join("a.png"), mime: "image/png".into(), width: 1, height: 1, name: None };
    let err = session.prompt_with("look", vec![], vec![image]).await.unwrap_err();
    assert!(err.to_string().contains("can't read images"), "{err}");
}

/// The app stops an idle agent: the stop reads as Stopped (not a crash), and the session goes
/// on in a new agent process, loaded from its data folder.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_stopped_on_purpose_is_stopped_and_its_session_loads_again() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let data = root.path().join("thread-data");
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(PathBuf::from(root.path()), Some(data.clone()), Some("t1")).await.unwrap();
    let id = session.id().to_string();
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");

    agent.stop();
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = events.recv().await {
            if let HostEvent::PluginStatus { status, .. } = e
                && !matches!(status, PluginStatus::Running | PluginStatus::Starting)
            {
                return status;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("the agent's exit is reported");
    assert_eq!(status, PluginStatus::Stopped);
    assert!(!agent.is_running());

    // Woken: a new process loads the same session and remembers the earlier prompt.
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.load_session(&id, PathBuf::from(root.path()), Some(data), Some("t1")).await.unwrap();
    assert_eq!(session.id(), id);
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();
    assert_eq!(session.prompt("recall").await.unwrap(), "end_turn");
    until(&mut events, &mut state, &mut seen, turn_ended).await;
    let reply: String = state.items.iter().filter_map(|i| if let TranscriptItem::Agent(t) = i { Some(t.as_str()) } else { None }).collect();
    assert!(reply.contains("I remember 1 earlier prompt(s)."), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_steered_message_joins_the_running_turn_at_its_next_step() {
    let root = stub_root();
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    assert!(agent.can_steer());
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let mut seen = Vec::new();

    // Nothing runs: refused (the app sends it as a normal message instead).
    assert!(session.steer("s0".into(), "too early".into(), vec![], vec![]).await.is_err());

    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("slow please").await });
    until(&mut events, &mut state, &mut seen, |e| matches!(e, HostEvent::Session { event: SessionEvent::AgentText { .. }, .. })).await;
    session.steer("s1".into(), "also check the tests".into(), vec![], vec![]).await.unwrap();
    until(&mut events, &mut state, &mut seen, |e| matches!(e, HostEvent::Steered { steer_id, .. } if steer_id == "s1")).await;
    // The message is in the transcript before what the agent says after it.
    let users: Vec<&str> =
        state.items.iter().filter_map(|i| if let TranscriptItem::User { text, .. } = i { Some(text.as_str()) } else { None }).collect();
    assert_eq!(users, ["slow please", "also check the tests"]);
    until(&mut events, &mut state, &mut seen, turn_ended).await;
    assert_eq!(turn.await.unwrap().unwrap(), "end_turn");
    let reply: String = state.items.iter().filter_map(|i| if let TranscriptItem::Agent(t) = i { Some(t.as_str()) } else { None }).collect();
    assert!(reply.contains("Steered: also check the tests."), "{reply}");

    // Taken back before the next step: never delivered.
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("slow again").await });
    // Once the agent is replying (Space reports the turn started before the prompt reaches it).
    until(&mut events, &mut state, &mut seen, |e| matches!(e, HostEvent::Session { event: SessionEvent::TurnStarted, .. })).await;
    until(&mut events, &mut state, &mut seen, |e| matches!(e, HostEvent::Session { event: SessionEvent::AgentText { .. }, .. })).await;
    session.steer("s2".into(), "never mind".into(), vec![], vec![]).await.unwrap();
    assert_eq!(session.unsteer().await.unwrap(), ["s2"]);
    until(&mut events, &mut state, &mut seen, turn_ended).await;
    turn.await.unwrap().unwrap();
    assert!(!seen.iter().any(|e| matches!(e, HostEvent::Steered { steer_id, .. } if steer_id == "s2")));
    agent.stop();
}
