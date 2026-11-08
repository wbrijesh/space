//! `Space` — the Space desktop app.
//!
//! ```text
//! Space [PROJECT_DIR]                 open the window (project defaults to the cwd)
//! Space --smoke "PROMPT" [--plugin ID] [--allow] [--permission-mode ID] [PROJECT_DIR]
//!                                     headless: start an agent, prompt once, print events
//! Space --gallery                     dev: open the component gallery
//! Space --scenario NAME|list [DIR]    dev: boot into a scripted state (stub agent)
//! ```
//!
//! The app starts the agent plugin (`pi-agent`, or `SPACE_AGENT=<plugin id>`) on launch and
//! opens an empty thread. Env: `SPACE_AUTOPROMPT="..."` sends that prompt in the first
//! thread; `SPACE_PLUGIN_DIRS` overrides the bundled plugin roots; `SPACE_SETTINGS` overrides
//! the settings file (`~/.space/settings.json`). Dev: `SPACE_SCRIPT=file`
//! replays input commands (see `devscript`).

mod app;
mod attach;
mod idle;
mod images;
#[cfg(feature = "dev")]
mod dev;
#[cfg(feature = "dev")]
mod devscript;
mod notify;
mod persist;
mod recall;
#[cfg(feature = "dev")]
mod reload;
#[cfg(feature = "dev")]
mod scenarios;
mod settings;
mod threads;
mod trajectory;
mod view;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use space_core::smoke::{self, SmokeOptions};
use space_core::{Host, HostOptions};

use crate::app::{Launch, Options, SeedThread};

struct Args {
    project_dir: PathBuf,
    smoke: Option<String>,
    plugin: Option<String>,
    allow: bool,
    permission_mode: Option<String>,
    gallery: bool,
    scenario: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut project_dir = None;
    let mut smoke = None;
    let mut plugin = None;
    let mut allow = false;
    let mut permission_mode = None;
    let mut gallery = false;
    let mut scenario = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--smoke" => smoke = Some(args.next().context("--smoke needs a prompt")?),
            "--plugin" => plugin = Some(args.next().context("--plugin needs an id")?),
            "--allow" => allow = true,
            "--permission-mode" => {
                permission_mode = Some(args.next().context("--permission-mode needs a mode id (e.g. read-only)")?)
            }
            "--gallery" => gallery = true,
            "--scenario" => scenario = Some(args.next().context("--scenario needs a name (or `list`)")?),
            "-h" | "--help" => {
                println!(
                    "usage: Space [PROJECT_DIR]\n       Space --smoke PROMPT [--plugin ID] [--allow] [--permission-mode ID] [PROJECT_DIR]\n       \
                     Space --gallery\n       Space --scenario NAME|list [PROJECT_DIR]"
                );
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => anyhow::bail!("unknown flag {arg}"),
            _ => project_dir = Some(PathBuf::from(arg)),
        }
    }
    let project_dir = match project_dir {
        Some(p) => p.canonicalize().with_context(|| format!("project dir {}", p.display()))?,
        // `just run` passes the repo (dx starts the app in its own folder).
        None => match std::env::var_os("SPACE_PROJECT").filter(|p| !p.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir()?,
        },
    };
    Ok(Args { project_dir, smoke, plugin, allow, permission_mode, gallery, scenario })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args()?;
    if (args.gallery || args.scenario.is_some()) && !cfg!(feature = "dev") {
        anyhow::bail!("--gallery and --scenario need a build with the `dev` feature");
    }

    // space-core runs on its own tokio runtime; GPUI keeps its own executors.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("space-core")
        .enable_all()
        .build()?;

    #[allow(unused_mut)]
    let mut host_options = HostOptions::for_project(&args.project_dir);
    // Space's folder (plugin approvals, threads) is the settings file's.
    host_options.space_dir = settings::default_path().and_then(|p| Some(p.parent()?.to_path_buf()));
    #[allow(unused_mut)]
    let mut options = Options {
        project_dir: args.project_dir.clone(),
        agent: std::env::var("SPACE_AGENT").ok().filter(|s| !s.is_empty()),
        seed: std::env::var("SPACE_AUTOPROMPT")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .map(|p| vec![SeedThread { prompt: Some(p), ..Default::default() }])
            .unwrap_or_default(),
        select: None,
        notify: std::env::var("SPACE_NOTIFY").map_or(true, |v| v != "0"),
        settings_path: settings::default_path(),
        open_settings: false,
        open_trajectory: false,
    };
    #[cfg(feature = "dev")]
    if let Some(name) = &args.scenario {
        if name == "list" {
            println!("{}", scenarios::list());
            return Ok(());
        }
        let scenario = scenarios::resolve(name)?;
        (host_options, options) = scenarios::prepare(&scenario, &args.project_dir)?;
    }

    let (host, events) = Host::new(runtime.handle().clone(), host_options);

    if let Some(prompt) = args.smoke {
        let opts = SmokeOptions {
            plugin_id: args.plugin,
            cwd: args.project_dir,
            prompt,
            allow_permissions: args.allow,
            permission_mode: args.permission_mode,
            timeout: Duration::from_secs(180),
        };
        let state = runtime.block_on(smoke::run(&host, events, opts))?;
        println!("ok: {} transcript item(s)", state.items.len());
        return Ok(());
    }

    let launch = if args.gallery { Launch::Gallery } else { Launch::App(options) };
    app::run(host, events, launch);
    drop(runtime);
    Ok(())
}
