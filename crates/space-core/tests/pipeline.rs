//! The host's tool pipeline, sandbox seam, context, catalogs and plugin settings, end to end:
//! the stub agent as the consumer, `space-stub-guard` service plugins as listeners.

use std::path::Path;
use std::time::Duration;

use space_core::protocol::service::SettingKind;
use space_core::{
    SettingError, DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PermissionDecision, PluginStatus, Secrets,
    SessionEvent, SessionState, ToolCallInfo, TranscriptItem,
};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");
const GUARD: &str = env!("CARGO_BIN_EXE_space-stub-guard");
const TEST_KEYCHAIN: &str = "dev.brijesh.space.test";

/// A plugin root with the stub agent and one stub guard per `(id, manifest extras, config)`.
fn root(guards: &[(&str, &str, &str)]) -> tempfile::TempDir {
    root_with_script(guards, None)
}

/// Like [`root`], with the stub agent playing `script` (a [`space_core::stub_script`] JSON).
fn root_with_script(guards: &[(&str, &str, &str)], script: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    let env = script.map_or(String::new(), |json| {
        let path = tmp.path().join("script.json");
        std::fs::write(&path, json).unwrap();
        format!("[run.env]\nSPACE_STUB_SCRIPT='{}'\n", path.display())
    });
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{STUB}'\n{env}"));
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
    host_rechecking(root, Duration::from_secs(60))
}

/// A host whose held calls run the chain again every `recheck` (else only when woken).
fn host_rechecking(root: &Path, recheck: Duration) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
        secrets: Secrets::new(TEST_KEYCHAIN),
        held_recheck: recheck,
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

fn held(e: &HostEvent) -> bool {
    matches!(e, HostEvent::Session { event: SessionEvent::ToolCallHeld { reason: Some(_), .. }, .. })
}

fn released(e: &HostEvent) -> bool {
    matches!(e, HostEvent::Session { event: SessionEvent::ToolCallHeld { reason: None, .. }, .. })
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
async fn a_crashing_listener_holds_the_call_until_it_runs_again() {
    // Case 13: the guard exits on every call until `ok` exists. The call waits (never asks),
    // the reason turns to "keeps crashing" after repeated crashes, and the call goes through
    // once the guard runs again.
    let tmp = tempfile::tempdir().unwrap();
    let ok = tmp.path().join("ok");
    let config = format!(r#"{{"decision":"allow","crashUntil":"{}"}}"#, ok.display());
    let manifest = format!("{HOOK}\nrequired=true");
    let root = root(&[("guard", &manifest, &config)]);
    let (host, mut events) = host_rechecking(root.path(), Duration::from_millis(300));
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    until(&mut events, &mut state, held).await;
    assert!(state.held().unwrap().contains("couldn't check this call"), "{:?}", state.held());
    // Restarts are 3 s apart; after the third crash the reason says so.
    let keeps = |e: &HostEvent| {
        matches!(e, HostEvent::Session { event: SessionEvent::ToolCallHeld { reason: Some(r), .. }, .. } if r.contains("keeps crashing"))
    };
    tokio::time::timeout(Duration::from_secs(30), until(&mut events, &mut state, keeps)).await.expect("keeps crashing");
    assert!(state.pending_permission().is_none(), "a failing guard never asks");
    // A required guard can't be turned off.
    host.set_disabled(["guard".to_string()]);
    assert!(!host.is_disabled("guard"));
    std::fs::write(&ok, "").unwrap();
    tokio::time::timeout(Duration::from_secs(30), until(&mut events, &mut state, turn_ended)).await.expect("turn ended");
    turn.await.unwrap().unwrap();
    assert_eq!(tool(&state).status, space_core::ToolStatus::Completed);
    assert_eq!(state.held(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_call_resumes_when_the_listener_is_ready() {
    // `hooks/ready` releases it at once (the recheck interval here is a minute).
    let tmp = tempfile::tempdir().unwrap();
    let ready = tmp.path().join("ready");
    let config = format!(r#"{{"decision":"allow","reason":"key rejected","readyFile":"{}"}}"#, ready.display());
    let root = root(&[("guard", HOOK, &config)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    until(&mut events, &mut state, held).await;
    assert_eq!(state.held(), Some("key rejected"));
    assert!(state.running);
    // Still held a moment later: nothing ran.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(tool(&state).status, space_core::ToolStatus::Pending);
    std::fs::write(&ready, "").unwrap();
    let started = std::time::Instant::now();
    until(&mut events, &mut state, released).await;
    assert!(started.elapsed() < Duration::from_secs(5), "released by hooks/ready, not the recheck");
    until(&mut events, &mut state, turn_ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(tool(&state).status, space_core::ToolStatus::Completed);
    // The call reaches the history once, as allowed.
    let logs = agent.logs();
    assert!(logs.iter().any(|l| l.contains("argv:")) || tool(&state).output.contains("argv:"), "{logs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_turn_ends_a_held_call() {
    let root = root(&[("guard", HOOK, r#"{"decision":"unavailable","reason":"offline"}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    until(&mut events, &mut state, held).await;
    session.cancel().unwrap();
    until(&mut events, &mut state, turn_ended).await;
    turn.await.unwrap().unwrap();
    let t = tool(&state);
    assert_ne!(t.status, space_core::ToolStatus::Completed, "the held call never ran: {t:?}");
    assert_eq!(state.held(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_mode_rechecks_held_calls() {
    // Held in workspace-write; the user switches the thread to read-only, where the guard
    // decides: the call goes through the whole chain again under the new mode.
    let root = root(&[("guard", HOOK, r#"{"decision":"deny","reason":"denied in {mode}","holdModes":["workspace-write"]}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    session.set_permission_mode("workspace-write");
    let mut state = SessionState::new(session.id());
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });
    until(&mut events, &mut state, held).await;
    session.set_permission_mode("read-only");
    until(&mut events, &mut state, turn_ended).await;
    turn.await.unwrap().unwrap();
    assert_eq!(tool(&state).blocked.as_deref(), Some("denied in read-only"));
}

#[tokio::test(flavor = "multi_thread")]
async fn one_recovery_releases_every_held_call_in_turn_order() {
    // Two sessions held at once, one with two calls in a row: one `hooks/ready` lets them all go.
    let tmp = tempfile::tempdir().unwrap();
    let ready = tmp.path().join("ready");
    let config = format!(r#"{{"decision":"allow","reason":"down","readyFile":"{}"}}"#, ready.display());
    let script = r#"{"turns":[{"prompt":"two","steps":[
        {"type":"guard","id":"a","title":"bash: first","tool":"bash","args":{"command":"first"},"output":"1"},
        {"type":"guard","id":"b","title":"bash: second","tool":"bash","args":{"command":"second"},"output":"2"}]}]}"#;
    let root = root_with_script(&[("guard", HOOK, &config)], Some(&script.replace('\n', " ")));
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let one = agent.new_session(root.path()).await.unwrap();
    let two = agent.new_session(root.path()).await.unwrap();
    let mut s1 = SessionState::new(one.id());
    let mut s2 = SessionState::new(two.id());
    let (o, t) = (one.clone(), two.clone());
    let turn1 = tokio::spawn(async move { o.prompt("two").await });
    let turn2 = tokio::spawn(async move { t.prompt("hello").await });
    // Fold both sessions' events until each has a held call.
    tokio::time::timeout(Duration::from_secs(15), async {
        while s1.held().is_none() || s2.held().is_none() {
            if let Some(HostEvent::Session { session_id, event }) = events.recv().await {
                if session_id == s1.id { s1.apply(&event) } else if session_id == s2.id { s2.apply(&event) }
            }
        }
    })
    .await
    .expect("both held");
    std::fs::write(&ready, "").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while s1.running || s2.running {
            if let Some(HostEvent::Session { session_id, event }) = events.recv().await {
                if session_id == s1.id { s1.apply(&event) } else if session_id == s2.id { s2.apply(&event) }
            }
        }
    })
    .await
    .expect("both turns ended");
    turn1.await.unwrap().unwrap();
    turn2.await.unwrap().unwrap();
    let outputs: Vec<(String, String)> = s1
        .items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Tool(t) => Some((t.id.clone(), t.output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, [("a".to_string(), "1".to_string()), ("b".to_string(), "2".to_string())]);
    assert_eq!(tool(&s2).status, space_core::ToolStatus::Completed);
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
    let config = r#"{"secret":"api_key","needsSecret":["b"],"invalid":["bad"],"unverifiable":["offline"],
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
    assert!(!b.ready && b.blocker.as_ref().unwrap().actions[0].action == "open-settings:guard");
    let settings = host.plugin_settings("guard");
    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0].field.kind, SettingKind::Secret);

    // The default mode applies to sessions that haven't picked one.
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    assert_eq!(session.permission_mode(), "b");

    // The plugin checks a value before it's stored: rejected ones never are; ones it couldn't
    // check are stored only when forced.
    assert_eq!(
        host.set_setting("guard", "api_key", Some("bad".into()), false).await,
        Err(SettingError::Invalid("That key was rejected.".into()))
    );
    assert_eq!(secrets.get("guard", "api_key").unwrap(), None);
    assert!(matches!(host.set_setting("guard", "api_key", Some("offline".into()), false).await, Err(SettingError::Unverifiable(_))));
    assert_eq!(secrets.get("guard", "api_key").unwrap(), None);
    host.set_setting("guard", "api_key", Some("offline".into()), true).await.unwrap();
    assert_eq!(secrets.get("guard", "api_key").unwrap().as_deref(), Some("offline"));
    host.set_setting("guard", "api_key", Some("sk-test".into()), false).await.unwrap();
    until(&mut events, &mut state, |e| {
        matches!(e, HostEvent::Catalog { entries, .. } if entries.iter().all(|e| e.ready))
    })
    .await;
    assert!(host.plugin_settings("guard")[0].set);
    assert_eq!(secrets.get("guard", "api_key").unwrap().as_deref(), Some("sk-test"));
    host.set_setting("guard", "api_key", None, false).await.unwrap();
    assert_eq!(secrets.get("guard", "api_key").unwrap(), None);
    assert!(host.set_setting("guard", "nope", None, false).await.is_err());

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

/// Wait for plugin `id` to report a status matching `pred`; the statuses seen on the way, in order.
async fn status_of(
    events: &mut EventReceiver,
    seen: &mut Vec<(String, PluginStatus)>,
    id: &str,
    pred: impl Fn(&PluginStatus) -> bool,
) -> PluginStatus {
    let mut state = SessionState::default();
    let e = until(events, &mut state, |e| {
        if let HostEvent::PluginStatus { plugin_id, status } = e {
            seen.push((plugin_id.clone(), status.clone()));
            return plugin_id == id && pred(status);
        }
        false
    })
    .await;
    let HostEvent::PluginStatus { status, .. } = e else { unreachable!() };
    status
}

fn not_started(s: &PluginStatus) -> bool {
    matches!(s, PluginStatus::NotStarted(_))
}

const SANDBOX: &str = "provides=['sandbox']";
const NEEDS_SANDBOX: &str = "provides=['context']\nrequires=['sandbox']";

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_starts_after_what_it_requires() {
    let root = root(&[("dep", NEEDS_SANDBOX, "{}"), ("sb", SANDBOX, "{}")]);
    let (host, mut events) = host(root.path());
    // Only the dependent is asked for: its provider starts first.
    host.restart_service("dep").await.unwrap();
    let mut seen = Vec::new();
    status_of(&mut events, &mut seen, "dep", |s| *s == PluginStatus::Running).await;
    let at = |id: &str, status: PluginStatus| seen.iter().position(|(i, s)| i == id && *s == status).unwrap();
    assert!(at("sb", PluginStatus::Running) < at("dep", PluginStatus::Starting), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unmet_requirements_and_cycles_are_not_started() {
    let cycle_a = "provides=['catalog:a']\nrequires=['catalog:b']";
    let cycle_b = "provides=['catalog:b']\nrequires=['catalog:a']";
    let root = root(&[("dep", NEEDS_SANDBOX, "{}"), ("a", cycle_a, "{}"), ("b", cycle_b, "{}")]);
    // The agent needs a sandbox too.
    let stub = root.path().join("stub/space-plugin.toml");
    let text = std::fs::read_to_string(&stub).unwrap().replace("provides=['agent']\n", "provides=['agent']\nrequires=['sandbox']\n");
    std::fs::write(&stub, text).unwrap();
    let (host, mut events) = host(root.path());
    host.start_services().await;
    // Every start has been tried by now.
    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let HostEvent::PluginStatus { plugin_id, status } = e {
            seen.push((plugin_id, status));
        }
    }
    let last = |id: &str| seen.iter().rev().find(|(i, _)| i == id).map(|(_, s)| s.clone());
    assert_eq!(last("dep"), Some(PluginStatus::NotStarted("Needs a sandbox provider.".into())));
    let circle = PluginStatus::NotStarted("Its requirements go in a circle: Guard a → Guard b → Guard a.".into());
    assert_eq!(last("a"), Some(circle));
    let err = host.start_agent("stub").await.err().expect("not started");
    assert_eq!(err.to_string(), "Needs a sandbox provider.");
    status_of(&mut events, &mut seen, "stub", not_started).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn when_a_provider_crashes_its_dependents_stop_and_come_back() {
    // `sb` crashes on its first confine (from the agent's tool call); the host stops `dep` and
    // the agent, which both require a sandbox, starts `sb` again, and `dep` with it.
    let tmp = tempfile::tempdir().unwrap();
    let config = format!(r#"{{"crashUntil":"{}"}}"#, tmp.path().join("never").display());
    let root = root(&[("dep", NEEDS_SANDBOX, "{}"), ("sb", SANDBOX, &config)]);
    let stub = root.path().join("stub/space-plugin.toml");
    let text = std::fs::read_to_string(&stub).unwrap().replace("provides=['agent']\n", "provides=['agent']\nrequires=['sandbox']\n");
    std::fs::write(&stub, text).unwrap();
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let turn = tokio::spawn(async move { session.prompt("hello").await });
    let mut seen = Vec::new();
    let s = status_of(&mut events, &mut seen, "dep", not_started).await;
    assert_eq!(s, PluginStatus::NotStarted("Waiting for Guard sb.".into()));
    status_of(&mut events, &mut seen, "stub", not_started).await;
    let _ = turn.await;
    assert!(!agent.is_running());
    // Restarted after the backoff, and `dep` with it.
    status_of(&mut events, &mut seen, "sb", |s| *s == PluginStatus::Running).await;
    status_of(&mut events, &mut seen, "dep", |s| *s == PluginStatus::Running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_a_provider_off_stops_its_dependents_first() {
    let root = root(&[("dep", NEEDS_SANDBOX, "{}"), ("sb", SANDBOX, "{}")]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let mut seen = Vec::new();
    status_of(&mut events, &mut seen, "dep", |s| *s == PluginStatus::Running).await;
    host.set_disabled(["sb".to_string()]);
    let s = status_of(&mut events, &mut seen, "dep", not_started).await;
    assert_eq!(s, PluginStatus::NotStarted("Waiting for Guard sb.".into()));
    host.set_disabled([]);
    host.start_services().await;
    status_of(&mut events, &mut seen, "dep", |s| *s == PluginStatus::Running).await;
    host.shutdown();
    status_of(&mut events, &mut seen, "sb", |s| *s == PluginStatus::Stopped).await;
}
