//! `space` — the Space desktop app.
//!
//! ```text
//! space [PROJECT_DIR]                 open the window (project defaults to the cwd)
//! space --smoke "PROMPT" [--plugin ID] [--allow] [PROJECT_DIR]
//!                                     headless: start an agent, prompt once, print events
//! ```
//!
//! Env: `SPACE_AUTOSTART=1` starts the first agent and a session on launch;
//! `SPACE_AUTOPROMPT="..."` additionally sends that prompt. `SPACE_PLUGIN_DIRS` overrides
//! the bundled plugin roots.

mod app;
mod theme;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use space_core::smoke::{self, SmokeOptions};
use space_core::{Host, HostOptions};

struct Args {
    project_dir: PathBuf,
    smoke: Option<String>,
    plugin: Option<String>,
    allow: bool,
}

fn parse_args() -> Result<Args> {
    let mut project_dir = None;
    let mut smoke = None;
    let mut plugin = None;
    let mut allow = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--smoke" => smoke = Some(args.next().context("--smoke needs a prompt")?),
            "--plugin" => plugin = Some(args.next().context("--plugin needs an id")?),
            "--allow" => allow = true,
            "-h" | "--help" => {
                println!("usage: space [--smoke PROMPT [--plugin ID] [--allow]] [PROJECT_DIR]");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => anyhow::bail!("unknown flag {arg}"),
            _ => project_dir = Some(PathBuf::from(arg)),
        }
    }
    let project_dir = match project_dir {
        Some(p) => p.canonicalize().with_context(|| format!("project dir {}", p.display()))?,
        None => std::env::current_dir()?,
    };
    Ok(Args { project_dir, smoke, plugin, allow })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args()?;
    // space-core runs on its own tokio runtime; GPUI keeps its own executors.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("space-core")
        .enable_all()
        .build()?;
    let (host, events) = Host::new(runtime.handle().clone(), HostOptions::for_project(&args.project_dir));

    if let Some(prompt) = args.smoke {
        let opts = SmokeOptions {
            plugin_id: args.plugin,
            cwd: args.project_dir,
            prompt,
            allow_permissions: args.allow,
            timeout: Duration::from_secs(180),
        };
        let state = runtime.block_on(smoke::run(&host, events, opts))?;
        println!("ok: {} transcript item(s)", state.items.len());
        return Ok(());
    }

    app::run(host, events, args.project_dir);
    drop(runtime);
    Ok(())
}
