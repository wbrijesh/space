//! The host's tool pipeline, sandbox seam, context, catalogs and plugin settings, end to end:
//! the stub agent as the consumer, `space-stub-guard` service plugins as listeners.

use std::path::Path;
use std::time::Duration;

use space_core::protocol::service::SettingKind;
use space_core::{
    DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PermissionDecision, PluginStatus, Secrets,
    SessionEvent, SessionState, ToolCallInfo, TranscriptItem,
};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");
const GUARD: &str = env!("CARGO_BIN_EXE_space-stub-guard");
const TEST_KEYCHAIN: &str = "dev.brijesh.space.test";

/// A plugin root with the stub agent and one stub guard per `(id, manifest extras, config)`.
fn root(guards: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{STUB}'\n"));
    for (id, extras, config) in guards {
        write(
            id,
            format!(
                "id='{id}'\nname='Guard {id}'\nversion='1'\n{extras}\n[run]\ncommand='{GUARD}'\n[run.env]\nSPACE_STUB_GUARD='{config}'\n"
            ),
        );
    }
    tmp
}

fn host(root: &Path) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
        secrets: Secrets::new(TEST_KEYCHAIN),
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

/// Wait for an event matching `pred`, folding session events into `state`.
async fn until(events: &mut EventReceiver, state: &mut SessionState, mut pred: impl FnMut(&HostEvent) -> bool) -> HostEvent {
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(e) = events.recv().await {
            if let HostEvent::Session { session_id, event } = &e
                && *session_id == state.id
            {
                state.apply(event);
            }
            if pred(&e) {
                return e;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("timed out waiting for event")
}

fn turn_ended(e: &HostEvent) -> bool {
    matches!(e, HostEvent::Session { event: SessionEvent::TurnEnded { .. }, .. })
}

fn tool(state: &SessionState) -> ToolCallInfo {
    state
        .items
        .iter()
        .find_map(|i| match i {
            TranscriptItem::Tool(t) => Some(t.clone()),
            _ => None,
        })
        .expect("a tool call")
}

const HOOK: &str = "provides=['context']\nhooks=[{event='tools/pre-execute', order=10}]";

#[tokio::test(flavor = "multi_thread")]
async fn no_listeners_allow_and_argv_is_unchanged() {
    let root = root(&[]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
    until(&mut events, &mut state, turn_ended).await;
    let t = tool(&state);
    assert_eq!(t.status, space_core::ToolStatus::Completed);
    assert!(t.output.ends_with("argv: /bin/sh -c echo stub"), "{}", t.output);
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_blocks_with_the_reason_and_sees_mode_and_history() {
    let root = root(&[("guard", HOOK, r#"{"decision":"deny","reason":"no {tool} in {mode} after {history}"}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    session.set_permission_mode("read-only");
    let mut state = SessionState::new(session.id());
    session.prompt("hello").await.unwrap();
    until(&mut events, &mut state, turn_ended).await;
    let t = tool(&state);
    assert_eq!(t.status, space_core::ToolStatus::Failed);
    // The user's message is the one earlier item.
    assert_eq!(t.output, "blocked: no bash in read-only after 1");
    assert_eq!(t.blocked.as_deref(), Some("no bash in read-only after 1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_puts_the_reason_to_the_user() {
    let root = root(&[("guard", HOOK, r#"{"decision":"ask","reason":"Unsure about {tool}"}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());

    for (answer, done) in [("allow", true), ("reject", false)] {
        let s = session.clone();
        let turn = tokio::spawn(async move { s.prompt("hello").await });
        until(&mut events, &mut state, |e| {
            matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. })
        })
        .await;
        let req = state.pending_permission().cloned().unwrap();
        assert_eq!(req.reason.as_deref(), Some("Unsure about bash"));
        assert_eq!(req.tool_call.title.as_deref(), Some("bash: echo stub"));
        assert_eq!(req.options.len(), 2);
        session.resolve_permission(req.request_id, PermissionDecision::Selected(answer.into())).unwrap();
        until(&mut events, &mut state, turn_ended).await;
        turn.await.unwrap().unwrap();
        let last = state.items.iter().rev().find_map(|i| match i {
            TranscriptItem::Tool(t) => Some(t.clone()),
            _ => None,
        });
        let last = last.unwrap();
        assert_eq!(last.status == space_core::ToolStatus::Completed, done, "{answer}: {}", last.output);
        if !done {
            assert_eq!(last.output, "blocked: The user didn't allow this call.");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_deny_wins_over_an_earlier_ask_and_order_counts() {
    let first = "provides=['context']\nhooks=[{event='tools/pre-execute', order=1}]";
    let second = "provides=['context']\nhooks=[{event='tools/pre-execute', order=2}]";
    let root = root(&[
        ("b-asker", first, r#"{"decision":"ask","reason":"maybe"}"#),
        ("a-denier", second, r#"{"decision":"deny","reason":"never"}"#),
    ]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    session.prompt("hello").await.unwrap();
    until(&mut events, &mut state, turn_ended).await;
    assert!(state.pending_permission().is_none());
    assert!(!state.items.iter().any(|i| matches!(i, TranscriptItem::Permission { .. })), "no card was shown");
    assert_eq!(tool(&state).output, "blocked: never");

    // Turned off, the denier is out of the chain: the asker's question reaches the user.
    host.set_disabled(["a-denier".to_string()]);
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("again").await });
    until(&mut events, &mut state, |e| matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. }))
        .await;
    let req = state.pending_permission().cloned().unwrap();
    session.resolve_permission(req.request_id, PermissionDecision::Selected("allow".into())).unwrap();
    until(&mut events, &mut state, turn_ended).await;
    turn.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_listener_fails_closed_to_the_user() {
    let root = root(&[("guard", HOOK, r#"{"decision":"crash"}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    until(&mut events, &mut state, |e| matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. }))
        .await;
    let req = state.pending_permission().cloned().unwrap();
    assert!(req.reason.as_deref().unwrap().contains("couldn't check this call"), "{:?}", req.reason);
    session.resolve_permission(req.request_id, PermissionDecision::Selected("reject".into())).unwrap();
    until(&mut events, &mut state, turn_ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(tool(&state).status, space_core::ToolStatus::Failed);
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_wraps_the_argv_and_context_reaches_the_agent() {
    let manifest = "provides=['sandbox','context']";
    let root = root(&[("guard", manifest, r#"{"sandbox":["/usr/bin/env","-i"],"context":"Mode is {mode}."}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    session.set_permission_mode("workspace-write");
    let mut state = SessionState::new(session.id());
    session.prompt("hello").await.unwrap();
    until(&mut events, &mut state, turn_ended).await;
    assert!(tool(&state).output.ends_with("argv: /usr/bin/env -i /bin/sh -c echo stub"), "{}", tool(&state).output);
    let logs = agent.logs();
    assert!(
        logs.iter().any(|l| l.contains("context [{\"id\":\"stub_guard\",\"text\":\"Mode is workspace-write.\"}]")),
        "{logs:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn catalogs_default_mode_and_secret_settings() {
    let manifest = "provides=['settings','catalog:permission-modes']";
    let config = r#"{"secret":"api_key","needsSecret":["b"],
        "catalog":[{"id":"a","name":"A"},{"id":"b","name":"B","default":true}]}"#;
    let root = root(&[("guard", manifest, &config.replace('\n', " "))]);
    let secrets = Secrets::new(TEST_KEYCHAIN);
    secrets.set("guard", "api_key", None).unwrap();
    let (host, mut events) = host(root.path());
    let mut state = SessionState::new("none");
    host.start_services().await;
    let entries = host.catalog("permission-modes");
    assert_eq!(entries.len(), 2);
    let b = entries.iter().find(|e| e.id == "b").unwrap();
    assert!(!b.ready && b.blocker.as_ref().unwrap().action.as_deref() == Some("open-settings:guard"));
    let settings = host.plugin_settings("guard");
    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0].field.kind, SettingKind::Secret);

    // The default mode applies to sessions that haven't picked one.
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    assert_eq!(session.permission_mode(), "b");

    host.set_setting("guard", "api_key", Some("sk-test".into())).await.unwrap();
    until(&mut events, &mut state, |e| {
        matches!(e, HostEvent::Catalog { entries, .. } if entries.iter().all(|e| e.ready))
    })
    .await;
    assert!(host.plugin_settings("guard")[0].set);
    assert_eq!(secrets.get("guard", "api_key").unwrap().as_deref(), Some("sk-test"));
    host.set_setting("guard", "api_key", None).await.unwrap();
    assert_eq!(secrets.get("guard", "api_key").unwrap(), None);
    assert!(host.set_setting("guard", "nope", None).await.is_err());

    // Turning the provider off empties its catalog.
    host.set_disabled(["guard".to_string()]);
    assert!(host.catalog("permission-modes").is_empty());
    until(&mut events, &mut state, |e| matches!(e, HostEvent::Catalog { entries, .. } if entries.is_empty())).await;
    let _ = until(&mut events, &mut state, |e| {
        matches!(e, HostEvent::PluginStatus { plugin_id, status: PluginStatus::Stopped } if plugin_id == "guard")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_during_startup_waits_for_the_service() {
    let root = root(&[("guard", HOOK, r#"{"decision":"deny","reason":"checked"}"#)]);
    let (host, mut events) = host(root.path());
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    // Starting the services and the first tool call race; the call must still be checked.
    let starting = {
        let host = host.clone();
        tokio::spawn(async move { host.start_services().await })
    };
    session.prompt("hello").await.unwrap();
    starting.await.unwrap();
    until(&mut events, &mut state, turn_ended).await;
    assert_eq!(tool(&state).blocked.as_deref(), Some("checked"));
}
