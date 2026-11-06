//! `Space` — the Space desktop app.
//!
//! ```text
//! Space [PROJECT_DIR]                 open the window (project defaults to the cwd)
//! Space --smoke "PROMPT" [--plugin ID] [--allow] [PROJECT_DIR]
//!                                     headless: start an agent, prompt once, print events
//! ```
//!
//! The app starts the agent plugin (`pi-agent`, or `SPACE_AGENT=<plugin id>`) on launch and
//! opens an empty thread. Env: `SPACE_AUTOPROMPT="..."` sends that prompt in the first
//! thread; `SPACE_PLUGIN_DIRS` overrides the bundled plugin roots.
//!
//! Dev only: `SPACE_DEMO=1` seeds a few threads with scripted prompts (for the stub agent),
//! `SPACE_SCRIPT=file` replays input commands (see `devscript`).

mod app;
mod assets;
mod devscript;
mod format;
mod theme;
mod view;

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
                println!("usage: Space [--smoke PROMPT [--plugin ID] [--allow]] [PROJECT_DIR]");
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

    let options = app::Options {
        project_dir: args.project_dir,
        agent: std::env::var("SPACE_AGENT").ok().filter(|s| !s.is_empty()),
        demo: std::env::var_os("SPACE_DEMO").is_some_and(|v| v != "0"),
        autoprompt: std::env::var("SPACE_AUTOPROMPT").ok().filter(|p| !p.trim().is_empty()),
    };
    app::run(host, events, options);
    drop(runtime);
    Ok(())
}
