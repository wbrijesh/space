//! The bundled TypeScript plugins run from one Bun executable (`plugins/space-ts.ts`, compiled
//! by `just build` into `target/<profile>/space-ts`). This starts the real claude-agent through it
//! (ACP initialize, and session/new when the Claude Code CLI is signed in here; no model call),
//! and checks that the entry's plugin list matches the plugin folders whose manifest runs it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::protocol::Manifest;
use space_core::{DiscoveryOptions, Host, HostOptions, Secrets};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_agent_starts_from_the_bun_executable() {
    let exe = space_core::process::target_dir().join("space-ts");
    if !exe.is_file() {
        eprintln!("skipped: {} is missing (run `just build`)", exe.display());
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/claude");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        format!("id='claude'\nname='Claude'\nversion='1'\nprovides=['agent']\n[run]\ncommand='{}'\nargs=['claude-agent']\n", exe.display()),
    )
    .unwrap();
    let options = HostOptions {
        project_dir: tmp.path().to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![tmp.path().join("plugins")], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.space-ts"),
        held_recheck: Duration::from_secs(60),
        space_dir: None,
        keeper: None,
    };
    let (host, _events) = Host::new(tokio::runtime::Handle::current(), options);
    let agent = tokio::time::timeout(Duration::from_secs(30), host.start_agent("claude")).await.expect("starts in time").unwrap();
    assert!(agent.accepts_images());
    assert!(agent.can_load_session());
    assert!(agent.can_steer());
    match tokio::time::timeout(Duration::from_secs(60), agent.new_session(tmp.path().to_path_buf())).await.expect("in time") {
        Ok(session) => assert!(!session.id().is_empty()),
        // A machine without a signed-in Claude Code CLI can't open a session.
        Err(e) if format!("{e:#}").contains("Claude Code") => eprintln!("skipped session/new: {e:#}"),
        Err(e) => panic!("session/new failed: {e:#}"),
    }
}

#[test]
fn the_entry_lists_exactly_the_plugins_that_run_it() {
    let repo = repo();
    let entry = std::fs::read_to_string(repo.join("plugins/space-ts.ts")).unwrap();
    // `"claude-agent": () => import("./claude-agent/src/main.ts"),` → "claude-agent"
    let mut listed: Vec<String> = entry
        .lines()
        .filter(|l| l.contains("=> import("))
        .filter_map(|l| l.trim().strip_prefix('"').and_then(|l| l.split('"').next()).map(str::to_string))
        .collect();
    listed.sort();
    let mut running: Vec<String> = std::fs::read_dir(repo.join("plugins"))
        .unwrap()
        .flatten()
        .filter_map(|e| Manifest::load_from_dir(&e.path()).ok())
        .filter(|m| m.run.command.ends_with("/space-ts"))
        .map(|m| m.run.args.first().cloned().unwrap_or_default())
        .collect();
    running.sort();
    assert!(!listed.is_empty());
    assert_eq!(listed, running, "plugins/space-ts.ts and the manifests that run `${{target}}/space-ts` must agree");
}
