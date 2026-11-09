//! `Space --check-plugins`: start every bundled plugin the way the app does (service plugins,
//! then the agent with one session; no model call), print how each did, and exit non-zero if any
//! failed. The release check runs it on the packaged app; it also tells a user what's broken.

use std::path::Path;
use std::time::Duration;

use space_core::{Host, PluginSource, PluginStatus};

/// How long a plugin may take to start.
const START: Duration = Duration::from_secs(30);

/// Check every bundled plugin; `true` if all started.
pub async fn run(host: &Host, project_dir: &Path, agent_id: Option<String>) -> bool {
    host.start_services().await;
    let plugins: Vec<_> = host.plugins().into_iter().filter(|p| p.source == PluginSource::Bundled).collect();
    if plugins.is_empty() {
        println!("FAIL no bundled plugins found");
        return false;
    }
    let mut ok = true;
    println!("plugins from {}", plugins[0].dir.parent().map_or_else(String::new, |d| d.display().to_string()));
    println!("helpers in {}", space_core::process::target_dir().display());
    for plugin in plugins.iter().filter(|p| !p.manifest.is_agent()) {
        let status = host.status(plugin.id());
        let good = matches!(status, Some(PluginStatus::Running));
        ok &= good;
        println!("{} {} ({}): {status:?}", if good { "ok  " } else { "FAIL" }, plugin.id(), plugin.manifest.name);
    }
    let agent = agent_id
        .or_else(|| plugins.iter().find(|p| p.id() == "pi-agent").map(|p| p.id().to_string()))
        .or_else(|| plugins.iter().find(|p| p.manifest.is_agent()).map(|p| p.id().to_string()));
    let Some(agent) = agent else {
        println!("FAIL no agent plugin");
        return false;
    };
    let started = async {
        let handle = host.start_agent(&agent).await?;
        handle.new_session(project_dir.to_path_buf()).await?;
        anyhow::Ok(())
    };
    match tokio::time::timeout(START, started).await {
        Ok(Ok(())) => println!("ok   {agent}: started, opened a session"),
        Ok(Err(e)) => {
            ok = false;
            println!("FAIL {agent}: {e:#}");
        }
        Err(_) => {
            ok = false;
            println!("FAIL {agent}: didn't start within {}s", START.as_secs());
        }
    }
    host.shutdown();
    ok
}
