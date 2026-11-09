//! End to end: the host starts the real terminal plugin, opens a shell, types into it, gets its
//! output through the sink, attaches, resizes, sees the exit and closes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use space_core::protocol::terminal::TerminalOpen;
use space_core::{DiscoveryOptions, Host, HostEvent, HostOptions, Secrets};

const PLUGIN: &str = env!("CARGO_BIN_EXE_space-terminal-plugin");

fn setup() -> (tempfile::TempDir, Host, space_core::EventReceiver) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/terminal");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("space-plugin.toml"),
        format!(
            "id='terminal'\nname='Terminal'\nversion='1'\nprovides=['terminal']\nobserves=['thread/deleted']\n[run]\ncommand='{PLUGIN}'\n[run.env]\nSPACE_TERMINAL_SHELL='/bin/sh'\n"
        ),
    )
    .unwrap();
    let options = HostOptions {
        project_dir: tmp.path().to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![tmp.path().join("plugins")], user_root: None, project_root: None },
        secrets: Secrets::new("dev.brijesh.space.test.terminal"),
        held_recheck: Duration::from_secs(60),
        space_dir: None,
    };
    let (host, events) = Host::new(tokio::runtime::Handle::current(), options);
    (tmp, host, events)
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_through_the_host() {
    let (tmp, host, mut events) = setup();
    host.start_services().await;
    assert!(host.has_terminal());
    assert_eq!(host.terminal_plugin().as_deref(), Some("terminal"));

    let seen: Arc<Mutex<Vec<u8>>> = Arc::default();
    let last_seq = Arc::new(Mutex::new(0u64));
    {
        let (seen, last_seq) = (seen.clone(), last_seq.clone());
        host.set_terminal_sink(Arc::new(move |_id, seq, data| {
            let mut last = last_seq.lock().unwrap();
            assert!(seq > *last, "in order");
            *last = seq;
            seen.lock().unwrap().extend_from_slice(data);
            true
        }));
    }
    let opened = host
        .terminal_open(TerminalOpen { thread_id: "t1".into(), cwd: tmp.path().display().to_string(), cols: 80, rows: 24 })
        .await
        .unwrap();
    let id = opened.terminal_id;
    host.terminal_input(&id, b"echo e2e-$((1+1))\n".to_vec());
    let text = || String::from_utf8_lossy(&seen.lock().unwrap()).to_string();
    wait_for("the echo", || text().contains("e2e-2")).await;
    // The sink asked for a redraw: the app hears about it.
    let told = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(HostEvent::TerminalOutput { terminal_id }) = events.recv().await {
                return terminal_id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(told, id);

    let replay = host.terminal_attach(&id).await.unwrap();
    assert!(String::from_utf8_lossy(&replay.data.0).contains("e2e-2"));
    assert!(replay.seq >= 1);

    host.terminal_resize(&id, 100, 30);
    host.terminal_input(&id, b"stty size\n".to_vec());
    wait_for("the new size", || text().contains("30 100")).await;

    let list = host.terminal_list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].thread_id, "t1");

    host.terminal_input(&id, b"exit 7\n".to_vec());
    let code = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(HostEvent::TerminalExited { exit_code, .. }) = events.recv().await {
                return exit_code;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, Some(7));
    host.terminal_close(&id).await.unwrap();
    assert!(host.terminal_list().await.unwrap().is_empty());
    host.shutdown();
}
