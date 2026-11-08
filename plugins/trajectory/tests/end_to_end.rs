//! End to end: the stub agent's own trace and the host's facts go through the host to the real
//! trajectory plugin, which answers `trajectory/read` and `trajectory/detail` for the thread.
//!
//! Needs the stub agent built next to the plugin (`cargo test --workspace` does that).

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::protocol::trace::{RecordKind, RecordStatus, TrajectoryDetailRequest, TrajectoryRead};
use space_core::{DiscoveryOptions, Host, HostEvent, HostOptions, Secrets};

const PLUGIN: &str = env!("CARGO_BIN_EXE_space-trajectory");

fn stub_agent() -> PathBuf {
    let path = Path::new(PLUGIN).with_file_name("space-stub-agent");
    assert!(path.is_file(), "build the stub agent first (cargo test --workspace)");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_shows_up_in_the_trajectory() {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join("plugins").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\n", stub_agent().display()));
    write(
        "trajectory",
        format!(
            "id='trajectory'\nname='Trajectory'\nversion='1'\nprovides=['trajectory']\nobserves=['trace','thread/deleted']\nuses=['read:trace']\n[run]\ncommand='{PLUGIN}'\n"
        ),
    );
    let options = HostOptions {
        project_dir: tmp.path().to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![tmp.path().join("plugins")], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.trajectory"),
        held_recheck: Duration::from_secs(60),
        space_dir: Some(tmp.path().join("space")),
    };
    let (host, mut events) = Host::new(tokio::runtime::Handle::current(), options);
    host.start_services().await;
    assert!(host.has_trajectory());

    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session_in(tmp.path(), None, Some("t1")).await.unwrap();
    assert_eq!(session.prompt("hello trajectory").await.unwrap(), "end_turn");

    // The plugin says when the thread's trajectory changed; wait until the turn is in.
    let read = || host.trajectory_read(TrajectoryRead { thread_id: "t1".into(), before: None, limit: 50, query: None });
    let page = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Some(HostEvent::TrajectoryChanged { .. })) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {}
            let page = read().await.unwrap();
            let done = page.rows.iter().filter(|r| r.kind == RecordKind::Assistant).all(|r| r.status == RecordStatus::Done);
            if page.stats.turns == 1 && page.rows.iter().any(|r| r.kind == RecordKind::Tool) && done {
                return page;
            }
        }
    })
    .await
    .expect("the turn reaches the trajectory");

    let kinds: Vec<RecordKind> = page.rows.iter().map(|r| r.kind).collect();
    assert_eq!(kinds.first(), Some(&RecordKind::System), "{kinds:?}");
    assert!(page.rows.iter().any(|r| r.kind == RecordKind::User && r.preview.contains("hello trajectory")), "{:?}", page.rows);
    assert!(kinds.iter().filter(|k| **k == RecordKind::Assistant).count() >= 2, "{kinds:?}");
    // The stub's two requests add up to the usage it reports for the turn.
    assert_eq!(page.requests.len(), 2);
    assert_eq!(page.stats.steps, 2);
    assert_eq!(page.stats.usage.total(), 9501);
    // The tool row carries the guard's verdict (no guard installed: allowed).
    let tool = page.rows.iter().find(|r| r.kind == RecordKind::Tool).unwrap();
    assert!(tool.guard.is_some(), "{tool:?}");

    // Details: the header's prompt and tools, and a request's timing.
    let system = &page.rows[0];
    let detail = host.trajectory_detail(TrajectoryDetailRequest { thread_id: "t1".into(), id: system.id.clone() }).await.unwrap();
    assert!(detail.header.is_some_and(|h| !h.tools.is_empty()));
    let request = host.trajectory_detail(TrajectoryDetailRequest { thread_id: "t1".into(), id: "request:1".into() }).await.unwrap();
    let r = request.request.expect("request detail");
    assert!(r.summary.first_token_ms.is_some() && r.summary.ended_ms.is_some(), "{:?}", r.summary);

    // Deleting the thread removes its trace.
    host.delete_thread("t1").await.ok();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(read().await.map(|p| p.rows.is_empty()).unwrap_or(true));
}
