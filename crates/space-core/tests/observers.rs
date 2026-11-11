//! Observed events and the host services plugins call: events reach the plugins that
//! `observe` them, in order, without holding up a turn; `host/notify` and `host/open-url`
//! reach the app as events.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use space_core::{DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PermissionDecision, Secrets, SessionEvent};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");
const GUARD: &str = env!("CARGO_BIN_EXE_space-stub-guard");
const TEST_KEYCHAIN: &str = "dev.brijesh.space.test";
const ALL: &str = "observes=['session/created', 'turn/started', 'turn/ended', 'tool/held', 'tool/result', 'thread/titled']";

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
                "id='{id}'\nname='Guard {id}'\nversion='1'\nprovides=['context']\n{extras}\n[run]\ncommand='{GUARD}'\n\
                 [run.env]\nSPACE_STUB_GUARD='{config}'\n"
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
        held_recheck: Duration::from_secs(60),
        space_dir: None,
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

async fn next(events: &mut EventReceiver, mut pred: impl FnMut(&HostEvent) -> bool) -> HostEvent {
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(e) = events.recv().await {
            if pred(&e) {
                return e;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("timed out waiting for event")
}

/// The JSON of an observed event a stub guard logged.
fn observed(e: &HostEvent) -> Option<(String, serde_json::Value)> {
    let HostEvent::PluginLog { plugin_id, line } = e else { return None };
    let json = line.split_once("space-stub-guard: observed ")?.1;
    Some((plugin_id.clone(), serde_json::from_str(json).unwrap()))
}

#[tokio::test(flavor = "multi_thread")]
async fn observers_are_told_what_they_observe_in_order() {
    let root = root(&[
        ("all", &format!("{ALL}\nuses=['read:transcript']"), "{}"),
        ("plain", "observes=['tool/result', 'turn/ended']", "{}"),
        ("deaf", "", "{}"),
    ]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    host.set_watching(Some(session.id().to_string()));
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");

    let mut seen: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let done = |seen: &HashMap<String, Vec<serde_json::Value>>, id: &str| {
        seen.get(id).is_some_and(|v| v.iter().any(|e| e["event"] == "turn/ended"))
    };
    while !(done(&seen, "all") && done(&seen, "plain")) {
        let e = next(&mut events, |e| observed(e).is_some()).await;
        let (id, event) = observed(&e).unwrap();
        seen.entry(id).or_default().push(event);
    }
    let names = |id: &str| seen[id].iter().map(|e| e["event"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names("all"), ["session/created", "turn/started", "tool/result", "turn/ended"]);
    assert_eq!(names("plain"), ["tool/result", "turn/ended"]);
    assert!(!seen.contains_key("deaf"));

    let ended = seen["all"].last().unwrap();
    assert_eq!((ended["sessionId"].as_str(), ended["stopReason"].as_str()), (Some(session.id()), Some("end_turn")));
    assert_eq!(ended["watching"], true);
    // The tool's output is transcript: only with `read:transcript`.
    assert!(seen["all"][2]["output"].as_str().is_some_and(|o| o.contains("echo stub")), "{:?}", seen["all"][2]);
    assert!(seen["plain"][0].get("output").is_none(), "{:?}", seen["plain"][0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn observers_are_told_of_approval_cards() {
    let ask = r#"{"decision":"ask","reason":"Unsure about {tool}"}"#;
    let root = root(&[
        ("guard", "hooks=[{event='tools/pre-execute', order=10}]", ask),
        ("reader", "observes=['permission/requested']\nuses=['read:transcript']", "{}"),
        ("plain", "observes=['permission/requested']", "{}"),
    ]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let s = session.clone();
    let turn = tokio::spawn(async move { s.prompt("hello").await });

    let mut seen: HashMap<String, serde_json::Value> = HashMap::new();
    let mut request = None;
    while request.is_none() || seen.len() < 2 {
        let e = next(&mut events, |e| observed(e).is_some() || matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. })).await;
        match (&e, observed(&e)) {
            (_, Some((id, event))) => drop(seen.insert(id, event)),
            (HostEvent::Session { event: SessionEvent::PermissionRequested(req), .. }, _) => request = Some(req.request_id),
            _ => {}
        }
    }
    for id in ["reader", "plain"] {
        let event = &seen[id];
        assert_eq!((event["event"].as_str(), event["sessionId"].as_str(), event["watching"].as_bool()), (Some("permission/requested"), Some(session.id()), Some(false)));
    }
    // The guard's reason can quote the call: only with `read:transcript`.
    assert_eq!(seen["reader"]["reason"], "Unsure about bash");
    assert!(seen["plain"].get("reason").is_none(), "{:?}", seen["plain"]);

    session.resolve_permission(request.unwrap(), PermissionDecision::Selected("allow".into())).unwrap();
    assert_eq!(turn.await.unwrap().unwrap(), "end_turn");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_observer_holds_nothing_up() {
    let root = root(&[("slow", "observes=['turn/started', 'turn/ended']", r#"{"observeDelayMs":60000}"#)]);
    let (host, mut events) = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    // It takes a minute over its first event; three turns finish long before that.
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..3 {
            assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");
        }
    })
    .await
    .expect("turns waited for an observer");
    // Only the first event reached it; the rest wait in its own queue.
    let (mut told, mut ended) = (0, 0);
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
        told += usize::from(observed(&e).is_some());
        ended += usize::from(matches!(e, HostEvent::Session { event: SessionEvent::TurnEnded { .. }, .. }));
    }
    assert_eq!((told, ended), (1, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_and_open_url_reach_the_app() {
    let call = |method: &str, params: &str| format!(r#"{{"call":"{method}","callParams":{params}}}"#);
    let notify = call("host/notify", r#"{"title":"Done","sessionId":"s1","sound":true}"#);
    let root = root(&[
        ("notifier", "uses=['host/notify']", &notify),
        ("sneaky", "", &notify),
        ("opener", "uses=['host/open-url']", &call("host/open-url", r#"{"url":"https://example.com/docs"}"#)),
        ("file-opener", "uses=['host/open-url']", &call("host/open-url", r#"{"url":"file:///etc/hosts"}"#)),
    ]);
    let (host, mut events) = host(root.path());
    host.start_services().await;

    let (mut notified, mut opened, mut answers) = (Vec::new(), Vec::new(), HashMap::new());
    while answers.len() < 4 || notified.is_empty() || opened.is_empty() {
        match next(&mut events, |e| {
            matches!(e, HostEvent::Notify { .. } | HostEvent::OpenUrl { .. })
                || matches!(e, HostEvent::PluginLog { line, .. } if line.contains(" -> "))
        })
        .await
        {
            HostEvent::Notify { plugin_id, notify } => notified.push((plugin_id, notify)),
            HostEvent::OpenUrl { plugin_id, url } => opened.push((plugin_id, url)),
            HostEvent::PluginLog { plugin_id, line } => {
                answers.insert(plugin_id, line);
            }
            _ => unreachable!(),
        }
    }
    let (from, n) = &notified[0];
    assert_eq!(from, "notifier");
    assert_eq!((n.title.as_str(), n.session_id.as_deref(), n.banner, n.sound), ("Done", Some("s1"), true, true));
    assert_eq!(opened, [("opener".to_string(), "https://example.com/docs".to_string())]);
    assert!(answers["notifier"].ends_with("-> ok") && answers["opener"].ends_with("-> ok"), "{answers:?}");
    assert!(answers["sneaky"].contains("(-32003)"), "{answers:?}");
    assert!(answers["file-opener"].contains("only http(s) URLs"), "{answers:?}");
    // Nothing else got through.
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
        assert!(!matches!(e, HostEvent::Notify { .. } | HostEvent::OpenUrl { .. }), "{e:?}");
    }
}
