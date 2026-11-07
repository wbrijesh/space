//! End to end: the real titles plugin behind the host, the stub agent as the agent. A thread is
//! named with its own model, or the one the setting picks; a failing or slow model leaves it
//! unnamed (Space keeps the first-line title); without `agent/complete` in `uses` it can't ask.
//!
//! Needs the stub agent built next to the plugin (`cargo test --workspace` does that).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use space_core::{DiscoveryOptions, Host, HostOptions, Secrets, TITLE_TIMEOUT};

const PLUGIN: &str = env!("CARGO_BIN_EXE_space-titles");

fn stub_agent() -> PathBuf {
    let path = Path::new(PLUGIN).with_file_name("space-stub-agent");
    assert!(path.is_file(), "build the stub agent first (cargo test --workspace)");
    path
}

/// A plugin root with the stub agent and the titles plugin, which `uses` these.
fn root(uses: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |dir: &str, manifest: String| {
        let d = tmp.path().join("plugins").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("space-plugin.toml"), manifest).unwrap();
    };
    write("stub", format!("id='stub'\nname='Stub'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\n", stub_agent().display()));
    write(
        "titles",
        format!("id='titles'\nname='Titles'\nversion='1'\nprovides=['session-title', 'settings']\nuses={uses}\n[run]\ncommand='{PLUGIN}'\n"),
    );
    tmp
}

fn host(root: &Path) -> Host {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.join("plugins")], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.titles"),
        held_recheck: Duration::from_secs(60),
        space_dir: None,
    };
    Host::new(tokio::runtime::Handle::current(), options).0
}

#[tokio::test(flavor = "multi_thread")]
async fn threads_are_named_with_their_model_or_the_chosen_one() {
    let root = root("['read:transcript', 'agent/complete']");
    let host = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();

    // The thread's own model.
    let title = host.generate_title(session.id(), "please fix the login page", TITLE_TIMEOUT).await;
    assert_eq!(title.as_deref(), Some("Title by stub-model"));

    // The "Title model" setting takes precedence.
    host.set_setting("titles", "model", Some("stub/stub-fast".into()), false).await.unwrap();
    let title = host.generate_title(session.id(), "please fix the login page", TITLE_TIMEOUT).await;
    assert_eq!(title.as_deref(), Some("Title by stub-fast"));

    // A failing model: no title.
    assert_eq!(host.generate_title(session.id(), "titlefail", TITLE_TIMEOUT).await, None);

    // A model that doesn't answer: no title once the wait is over.
    let start = Instant::now();
    assert_eq!(host.generate_title(session.id(), "titlehang", Duration::from_millis(500)).await, None);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_agent_complete_in_uses_it_cant_ask() {
    let root = root("['read:transcript']");
    let host = host(root.path());
    host.start_services().await;
    let agent = host.start_agent("stub").await.unwrap();
    let session = agent.new_session(root.path()).await.unwrap();
    assert_eq!(host.generate_title(session.id(), "please fix the login page", TITLE_TIMEOUT).await, None);
}
