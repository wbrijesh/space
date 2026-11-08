//! Traces and the trajectory seam: the host's facts reach plugins that observe `trace`, tagged
//! with the session's thread, and only those; the app's trajectory calls reach the provider and
//! its `trajectory/changed` comes back as an event; deleting a thread is observed.

use std::path::Path;
use std::time::Duration;

use space_core::{DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, Secrets};
use space_protocol::trace::{GuardVerdict, TraceEvent, TraceRecord, TrajectoryRead};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");
const GUARD: &str = env!("CARGO_BIN_EXE_space-stub-guard");
const TEST_KEYCHAIN: &str = "dev.brijesh.space.test";
const TRACER: &str = "observes=['trace', 'thread/deleted']\nuses=['read:trace']";

/// A bundled root (the stub agent, and stub guards by `(id, manifest extras, config)`) and a
/// user root with the given guards (which need your approval).
fn roots(bundled: &[(&str, &str, &str)], user: &[(&str, &str, &str)]) -> (tempfile::TempDir, tempfile::TempDir) {
    let (b, u) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let write = |root: &Path, dir: &str, manifest: String| {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write(b.path(), "stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{STUB}'\n"));
    for (root, guards) in [(b.path(), bundled), (u.path(), user)] {
        for (id, extras, config) in guards {
            let manifest = format!(
                "id='{id}'\nname='Guard {id}'\nversion='1'\n{extras}\n[run]\ncommand='{GUARD}'\n[run.env]\nSPACE_STUB_GUARD='{config}'\n"
            );
            write(root, id, manifest);
        }
    }
    (b, u)
}

fn host(bundled: &Path, user: &Path, space_dir: &Path) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: bundled.to_path_buf(),
        discovery: DiscoveryOptions {
            bundled_roots: vec![bundled.to_path_buf()],
            user_root: Some(user.to_path_buf()),
            project_root: None,
        },
        secrets: Secrets::new(TEST_KEYCHAIN),
        held_recheck: Duration::from_secs(60),
        space_dir: Some(space_dir.to_path_buf()),
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

/// A trace record a stub guard logged, and which guard.
fn traced(e: &HostEvent) -> Option<(String, TraceRecord)> {
    let HostEvent::PluginLog { plugin_id, line } = e else { return None };
    let json = line.split_once("space-stub-guard: traced ")?.1;
    Some((plugin_id.clone(), serde_json::from_str(json).unwrap()))
}

#[tokio::test(flavor = "multi_thread")]
async fn host_facts_reach_tracers_with_the_thread_and_only_them() {
    let ctx = r#"{"context":"Mode: {mode}","decision":"allow"}"#;
    let (bundled, user) = roots(
        &[("tracer", &format!("provides=['context']\n{TRACER}"), ctx), ("plain", "provides=['context']\nobserves=['turn/ended']", "{}")],
        &[("stranger", &format!("provides=['context']\n{TRACER}"), "{}")],
    );
    let space = tempfile::tempdir().unwrap();
    let (host, mut events) = host(bundled.path(), user.path(), space.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(bundled.path(), None, Some("t1")).await.unwrap();
    assert_eq!(session.prompt("hello").await.unwrap(), "end_turn");

    let mut got: Vec<TraceEvent> = Vec::new();
    while !got.iter().any(|e| matches!(e, TraceEvent::Guard { .. })) {
        let e = next(&mut events, |e| traced(e).is_some()).await;
        let (plugin, record) = traced(&e).unwrap();
        assert_eq!(plugin, "tracer", "only the approved plugin that observes traces");
        assert_eq!((record.thread_id.as_str(), record.session_id.as_str()), ("t1", session.id()));
        got.extend(record.events);
    }
    let TraceEvent::Prompt { text, .. } = &got[0] else { panic!("the prompt first: {got:?}") };
    assert_eq!(text, "hello");
    let context = got.iter().find_map(|e| match e {
        TraceEvent::Context { sections, .. } => Some(sections.clone()),
        _ => None,
    });
    let context = context.expect("the context sent");
    assert_eq!((context[0].plugin.as_str(), context[0].text.starts_with("Mode: ")), ("tracer", true));
    let guard = got.iter().find_map(|e| match e {
        TraceEvent::Guard { verdict, call_id, .. } => Some((*verdict, call_id.clone())),
        _ => None,
    });
    assert_eq!(guard, Some((GuardVerdict::Allow, "tool-1".to_string())));
    // The stranger (from your home folder, not approved) never started.
    assert!(host.status("stranger").is_some_and(|s| s.label() == "not started"));
}

#[tokio::test(flavor = "multi_thread")]
async fn trajectory_calls_reach_the_provider_and_changes_come_back() {
    let (bundled, user) =
        roots(&[("traj", &format!("provides=['trajectory']\n{TRACER}"), r#"{"trajectory":true}"#)], &[]);
    let space = tempfile::tempdir().unwrap();
    let (host, mut events) = host(bundled.path(), user.path(), space.path());
    host.start_services().await;
    assert!(host.has_trajectory());
    let page = host
        .trajectory_read(TrajectoryRead { thread_id: "t9".into(), limit: 50, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(page.rows[0].preview, "t9");

    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(bundled.path(), None, Some("t9")).await.unwrap();
    session.prompt("hi").await.unwrap();
    let changed = next(&mut events, |e| matches!(e, HostEvent::TrajectoryChanged { .. })).await;
    assert!(matches!(changed, HostEvent::TrajectoryChanged { thread_id, .. } if thread_id == "t9"));

    // Deleting the thread is observed (there's no store here, so the delete itself fails).
    let _ = host.delete_thread("t9").await;
    next(&mut events, |e| matches!(e, HostEvent::PluginLog { line, .. } if line.contains("\"thread/deleted\"") && line.contains("t9")))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_trajectory_plugin_the_calls_fail() {
    let (bundled, user) = roots(&[], &[]);
    let space = tempfile::tempdir().unwrap();
    let (host, _events) = host(bundled.path(), user.path(), space.path());
    host.start_services().await;
    assert!(!host.has_trajectory());
    let err = host.trajectory_read(TrajectoryRead { thread_id: "t".into(), limit: 1, ..Default::default() }).await.unwrap_err();
    assert!(err.to_string().contains("no trajectory plugin"), "{err}");
}
