//! Plugin permissions: a plugin gets only what its manifest declares, and plugins from your
//! home or project folders wait for your approval.

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::{
    Approval, DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PluginStatus, Secrets, SessionEvent,
};

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");
const GUARD: &str = env!("CARGO_BIN_EXE_space-stub-guard");
const TEST_KEYCHAIN: &str = "dev.brijesh.space.test";

/// A guard plugin `id` in `root` (`extras`: more manifest lines; `config`: `SPACE_STUB_GUARD`).
fn guard(root: &Path, id: &str, extras: &str, config: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest =
        format!("id='{id}'\nname='Guard {id}'\nversion='1'\n{extras}\n[run]\ncommand='{GUARD}'\n[run.env]\nSPACE_STUB_GUARD='{config}'\n");
    std::fs::write(dir.join("space-plugin.toml"), manifest).unwrap();
}

fn host(bundled: &Path, user: Option<PathBuf>, space_dir: Option<PathBuf>) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: bundled.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![bundled.to_path_buf()], user_root: user, project_root: None },
        secrets: Secrets::new(TEST_KEYCHAIN),
        held_recheck: Duration::from_secs(60),
        space_dir,
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

/// The next stderr line of plugin `id` containing `text`.
async fn log_line(events: &mut EventReceiver, id: &str, text: &str) -> String {
    let e = next(events, |e| matches!(e, HostEvent::PluginLog { plugin_id, line } if plugin_id == id && line.contains(text))).await;
    let HostEvent::PluginLog { line, .. } = e else { unreachable!() };
    line
}

async fn status(events: &mut EventReceiver, id: &str, pred: impl Fn(&PluginStatus) -> bool) -> PluginStatus {
    let e = next(events, |e| matches!(e, HostEvent::PluginStatus { plugin_id, status } if plugin_id == id && pred(status))).await;
    let HostEvent::PluginStatus { status, .. } = e else { unreachable!() };
    status
}

#[tokio::test(flavor = "multi_thread")]
async fn host_services_need_uses() {
    let root = tempfile::tempdir().unwrap();
    guard(root.path(), "undeclared", "provides=['context']", r#"{"call":"host/notify"}"#);
    guard(root.path(), "declared", "provides=['context']\nuses=['host/notify']", r#"{"call":"host/notify"}"#);
    let (host, mut events) = host(root.path(), None, None);
    host.start_services().await;
    // Both answers, in whichever order they come.
    let mut answers = std::collections::HashMap::new();
    while answers.len() < 2 {
        if let HostEvent::PluginLog { plugin_id, line } =
            next(&mut events, |e| matches!(e, HostEvent::PluginLog { line, .. } if line.contains("host/notify ->"))).await
        {
            answers.insert(plugin_id, line);
        }
    }
    assert!(answers["undeclared"].contains("isn't in the plugin's `uses` (-32003)"), "{answers:?}");
    assert!(!answers["declared"].contains("-32003"), "{answers:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_read_transcript_sees_what_the_user_wrote() {
    let root = tempfile::tempdir().unwrap();
    let stub = root.path().join("stub");
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::write(stub.join("space-plugin.toml"), format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{STUB}'\n"))
        .unwrap();
    let ask = r#"{"decision":"ask","reason":"[{history}:{said}]"}"#;
    guard(root.path(), "reader", "provides=['context']\nuses=['read:transcript']\nhooks=[{event='tools/pre-execute', order=1}]", ask);
    guard(root.path(), "blind", "provides=['context']\nhooks=[{event='tools/pre-execute', order=2}]", ask);
    let (host, mut events) = host(root.path(), None, None);
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    let turn = tokio::spawn(async move { session.prompt("hello").await });
    let e = next(&mut events, |e| matches!(e, HostEvent::Session { event: SessionEvent::PermissionRequested(_), .. })).await;
    let HostEvent::Session { event: SessionEvent::PermissionRequested(request), .. } = e else { unreachable!() };
    // Both see that there was a message; only the reader sees its text.
    assert_eq!(request.reason.as_deref(), Some("[1:hello] [1:]"));
    agent.stop();
    let _ = turn.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_get_a_clean_environment() {
    // Cargo sets CARGO and CARGO_MANIFEST_DIR for test runs.
    let root = tempfile::tempdir().unwrap();
    let config = r#"{"printEnv":["HOME","CARGO","CARGO_MANIFEST_DIR"]}"#;
    guard(root.path(), "env", "provides=['context']\nenv=['CARGO_MANIFEST_DIR']", config);
    let (host, mut events) = host(root.path(), None, None);
    host.start_services().await;
    let line = log_line(&mut events, "env", "env ").await;
    assert!(line.ends_with("env HOME=set CARGO=unset CARGO_MANIFEST_DIR=set"), "{line}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_plugin_gets_only_its_own_secrets() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (format!("own-a-{}", std::process::id()), format!("own-b-{}", std::process::id()));
    guard(root.path(), &a, "provides=['settings']", r#"{"secret":"api_key"}"#);
    guard(root.path(), &b, "provides=['settings']", r#"{"secret":"api_key"}"#);
    let (host, mut events) = host(root.path(), None, None);
    host.start_services().await;
    host.set_setting(&a, "api_key", Some("sk-a".into()), false).await.unwrap();
    host.restart_service(&a).await.unwrap();
    let line_a = log_line(&mut events, &a, "initialized with 1").await;
    host.restart_service(&b).await.unwrap();
    let line_b = log_line(&mut events, &b, "initialized with").await;
    let secrets = Secrets::new(TEST_KEYCHAIN);
    secrets.set(&a, "api_key", None).unwrap();
    assert!(line_a.contains("1 setting(s)"));
    assert!(line_b.contains("0 setting(s)"), "{line_b}");
}

#[tokio::test(flavor = "multi_thread")]
async fn user_plugins_wait_for_approval_and_ask_again_for_more() {
    let tmp = tempfile::tempdir().unwrap();
    let (bundled, user, space_dir) = (tmp.path().join("bundled"), tmp.path().join("user"), tmp.path().join("space"));
    let grants = space_dir.join(space_core::GRANTS_FILE);
    std::fs::create_dir_all(&bundled).unwrap();
    guard(&bundled, "sb", "provides=['sandbox']", "{}");
    guard(&user, "extra", "provides=['context']\nrequires=['sandbox']", "{}");
    // A user plugin that isn't approved provides nothing.
    guard(&user, "lists", "provides=['catalog:x']", r#"{"catalog":[{"id":"a","name":"A"}]}"#);

    let (host, mut events) = host(&bundled, Some(user.clone()), Some(space_dir.clone()));
    assert_eq!(host.approval("sb"), Approval::Trusted);
    assert_eq!(host.approval("extra"), Approval::Pending);
    host.start_services().await;
    let s = status(&mut events, "extra", |s| matches!(s, PluginStatus::NotStarted(_))).await;
    assert_eq!(s, PluginStatus::NotStarted("Waiting for your approval in Settings → Plugins.".into()));
    assert!(host.catalog("x").is_empty());

    host.answer_approval("extra", true).unwrap();
    host.start_services().await;
    status(&mut events, "extra", |s| *s == PluginStatus::Running).await;
    assert!(grants.is_file());
    host.shutdown();

    // Saved. Asking for more (here, to observe an event) asks again.
    let (host, _) = self::host(&bundled, Some(user.clone()), Some(space_dir.clone()));
    assert_eq!(host.approval("extra"), Approval::Approved);
    guard(&user, "extra", "provides=['context']\nrequires=['sandbox']\nobserves=['turn/ended']", "{}");
    let (host, mut events) = self::host(&bundled, Some(user.clone()), Some(space_dir.clone()));
    assert_eq!(host.approval("extra"), Approval::Pending);
    host.answer_approval("extra", false).unwrap();
    let s = status(&mut events, "extra", |s| matches!(s, PluginStatus::NotStarted(_))).await;
    assert_eq!(s, PluginStatus::NotStarted("You didn't approve it.".into()));
    assert_eq!(host.approval("extra"), Approval::Denied);
    // Asking for less keeps the answer.
    guard(&user, "extra", "provides=['context']", "{}");
    let (host, _) = self::host(&bundled, Some(user), Some(space_dir));
    assert_eq!(host.approval("extra"), Approval::Denied);
}
