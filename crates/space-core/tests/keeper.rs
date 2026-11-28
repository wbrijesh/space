//! Agents under a keeper outlive the host: a new host takes over a running agent mid-turn, with
//! its open turns, its waiting approval card and fresh requests of its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use space_core::{DiscoveryOptions, EventReceiver, Host, HostEvent, HostOptions, PermissionDecision, SessionEvent};
use space_keeper::KeeperClient;

const STUB: &str = env!("CARGO_BIN_EXE_space-stub-agent");

/// A plugin root with the stub agent, playing `script`.
fn stub_root(script: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    let script_path = tmp.path().join("script.json");
    std::fs::write(&script_path, script).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        format!(
            "id = 'stub'\nname = 'Stub'\nversion = '0.1.0'\nprovides = ['agent']\n[run]\ncommand = '{STUB}'\n[run.env]\nSPACE_STUB_SCRIPT = '{}'\n",
            script_path.display()
        ),
    )
    .unwrap();
    tmp
}

fn host(root: &Path, keeper: KeeperClient) -> (Host, EventReceiver) {
    let options = HostOptions {
        project_dir: root.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root.to_path_buf()], user_root: None, project_root: None },
        secrets: space_core::Secrets::new("dev.brijesh.space.test"),
        held_recheck: space_core::HELD_RECHECK,
        space_dir: None,
        keeper: Some(keeper),
    };
    Host::new(tokio::runtime::Handle::current(), options)
}

/// The next session event in `session` matching `pred`.
async fn next(events: &mut EventReceiver, session: &str, mut pred: impl FnMut(&SessionEvent) -> bool) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = events.recv().await {
            if let HostEvent::Session { session_id, event } = e
                && session_id == session
                && pred(&event)
            {
                return event;
            }
        }
        panic!("event stream closed");
    })
    .await
    .expect("timed out waiting for an event")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

#[test]
fn a_new_host_takes_over_a_running_agent() {
    let root = stub_root(
        r#"{"turns": [
            {"prompt": "hold", "steps": [{"type": "text", "text": "working"}, {"type": "hold"}]},
            {"prompt": "ask", "steps": [{"type": "permission", "tool_id": "t1", "title": "Run it"}, {"type": "text", "text": "done"}]}
        ]}"#,
    );
    let socket = root.path().join("keeper.sock");
    let keeper_rt = runtime();
    {
        let socket = socket.clone();
        keeper_rt.spawn(async move { space_keeper::daemon::run(&socket, None).await.unwrap() });
    }
    while !socket.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }

    // Host A starts two turns: one working, one waiting on an approval card.
    let rt_a = runtime();
    let (held, s1, s2) = rt_a.block_on(async {
        let (keeper, welcome, pipes) = KeeperClient::connect(&socket).await.unwrap();
        assert!(welcome.agents.is_empty() && pipes.is_empty());
        let (host, mut events) = host(root.path(), keeper);
        let agent = host.start_agent("stub").await.unwrap();
        let one = agent.new_session(PathBuf::from(root.path())).await.unwrap();
        let two = agent.new_session(PathBuf::from(root.path())).await.unwrap();
        let (s1, s2) = (one.id().to_string(), two.id().to_string());
        tokio::spawn({
            let one = one.clone();
            async move { one.prompt("hold").await }
        });
        next(&mut events, &s1, |e| matches!(e, SessionEvent::AgentText { text } if text.contains("working"))).await;
        tokio::spawn({
            let two = two.clone();
            async move { two.prompt("ask").await }
        });
        next(&mut events, &s2, |e| matches!(e, SessionEvent::PermissionRequested(_))).await;
        ((host, events, agent, one, two), s1, s2)
    });
    // It crashes: its tasks and sockets end, nothing is stopped.
    rt_a.shutdown_background();
    std::mem::forget(held);

    // Host B takes over.
    let rt_b = runtime();
    rt_b.block_on(async {
        let (keeper, welcome, mut pipes) = KeeperClient::connect(&socket).await.unwrap();
        assert_eq!(welcome.agents.len(), 1);
        let mut open: Vec<_> = welcome.agents[0].open.iter().map(|r| (r.method.clone(), r.session_id.clone().unwrap())).collect();
        open.sort();
        let mut expected = vec![("session/prompt".to_string(), s1.clone()), ("session/prompt".to_string(), s2.clone())];
        expected.sort();
        assert_eq!(open, expected, "both turns are still running");

        let (host, mut events) = host(root.path(), keeper.clone());
        let agent = host.adopt_agent(pipes.remove(0)).await.unwrap();
        let mut running = vec![s1.clone(), s2.clone()];
        running.sort();
        assert_eq!(agent.running_turns(), running);
        let one = agent.adopt_session(&s1, root.path(), None);
        let two = agent.adopt_session(&s2, root.path(), None);
        assert!(one.is_running() && two.is_running());
        assert!(one.prompt("again").await.is_err(), "a turn is running there");

        // The approval card the old host never answered comes back; answering it lets the turn
        // finish, and its end (a reply to the old host's request) arrives.
        let SessionEvent::PermissionRequested(card) = next(&mut events, &s2, |e| matches!(e, SessionEvent::PermissionRequested(_))).await else {
            unreachable!()
        };
        two.resolve_permission(card.request_id, PermissionDecision::Selected("allow".into())).unwrap();
        next(&mut events, &s2, |e| matches!(e, SessionEvent::AgentText { text } if text.contains("done"))).await;
        let end = next(&mut events, &s2, |e| matches!(e, SessionEvent::TurnEnded { .. })).await;
        assert!(matches!(end, SessionEvent::TurnEnded { stop_reason, .. } if stop_reason == "end_turn"));
        assert!(!two.is_running());

        // Stopping the other turn works too.
        one.cancel().unwrap();
        let end = next(&mut events, &s1, |e| matches!(e, SessionEvent::TurnEnded { .. })).await;
        assert!(matches!(end, SessionEvent::TurnEnded { stop_reason, .. } if stop_reason == "cancelled"));

        // And this host's own requests get their own replies.
        assert_eq!(one.prompt("hello").await.unwrap(), "end_turn");
        let three = agent.new_session(PathBuf::from(root.path())).await.unwrap();
        assert_eq!(three.prompt("hello").await.unwrap(), "end_turn");

        keeper.quit();
    });
    keeper_rt.shutdown_timeout(Duration::from_secs(5));
}

#[test]
fn dropping_an_agent_stops_it_and_an_exit_is_reported() {
    let root = stub_root(r#"{"turns": []}"#);
    let socket = root.path().join("keeper.sock");
    let rt = runtime();
    rt.block_on(async {
        let socket2 = socket.clone();
        tokio::spawn(async move { space_keeper::daemon::run(&socket2, None).await.unwrap() });
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (keeper, _, _) = KeeperClient::connect(&socket).await.unwrap();
        let (host, mut events) = host(root.path(), keeper.clone());
        let agent = host.start_agent("stub").await.unwrap();
        agent.stop();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(e) = events.recv().await {
                if matches!(e, HostEvent::PluginStatus { status: space_core::PluginStatus::Stopped, .. }) {
                    return;
                }
            }
        })
        .await
        .expect("the stop is reported");
        assert!(!agent.is_running());
        // A new connection finds nothing held.
        let (_, welcome, _) = KeeperClient::connect(&socket).await.unwrap();
        assert!(welcome.agents.is_empty(), "{:?}", welcome.agents);
        keeper.quit();
    });
}

#[test]
fn a_planned_handover_ends_no_turn_and_the_next_host_gets_its_end() {
    let root = stub_root(r#"{"turns": [{"prompt": "slow", "steps": [{"type": "text", "text": "a"}, {"type": "sleep", "ms": 800}, {"type": "text", "text": "b"}]}]}"#);
    let socket = root.path().join("keeper.sock");
    let keeper_rt = runtime();
    {
        let socket = socket.clone();
        keeper_rt.spawn(async move { space_keeper::daemon::run(&socket, None).await.unwrap() });
    }
    while !socket.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let rt = runtime();
    rt.block_on(async {
        let (keeper, _, _) = KeeperClient::connect(&socket).await.unwrap();
        let (host, mut events) = host(root.path(), keeper.clone());
        let agent = host.start_agent("stub").await.unwrap();
        let session = agent.new_session(PathBuf::from(root.path())).await.unwrap();
        let sid = session.id().to_string();
        tokio::spawn({
            let session = session.clone();
            async move { session.prompt("slow").await }
        });
        next(&mut events, &sid, |e| matches!(e, SessionEvent::AgentText { .. })).await;
        // Hand over, as an update does.
        host.hand_over_agents();
        keeper.close();
        let ended_here = tokio::time::timeout(Duration::from_millis(500), next(&mut events, &sid, |e| matches!(e, SessionEvent::TurnEnded { .. } | SessionEvent::Error { .. }))).await;
        assert!(ended_here.is_err(), "the old host reports no end: {ended_here:?}");
        std::mem::forget((host, agent, session));
        // The turn ends while no host is there: its end waits in the keeper.
        tokio::time::sleep(Duration::from_millis(900)).await;

        let (keeper, _, mut pipes) = KeeperClient::connect(&socket).await.unwrap();
        let (next_host, mut events) = self::host(root.path(), keeper.clone());
        let agent = next_host.adopt_agent(pipes.remove(0)).await.unwrap();
        let _session = agent.adopt_session(&sid, root.path(), None);
        let end = next(&mut events, &sid, |e| matches!(e, SessionEvent::TurnEnded { .. })).await;
        assert!(matches!(end, SessionEvent::TurnEnded { stop_reason, .. } if stop_reason == "end_turn"));
        keeper.quit();
    });
    keeper_rt.shutdown_timeout(Duration::from_secs(5));
}
