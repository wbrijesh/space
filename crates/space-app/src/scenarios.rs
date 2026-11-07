//! Dev scenarios: boot the real app shell into a named state, backed by the stub agent
//! playing a script (`space_core::stub_script`). Turns can hold indefinitely (thinking, a
//! running tool, a pending permission), so a scenario stays put for as long as you look.
//!
//! `Space --scenario <name>` (or Dev ▸ Relaunch in Scenario); `Space --scenario list`.
//! Adding one: append a [`Scenario`] to [`all`] (see docs/ui-development.md).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use space_core::stub_script::{SCRIPT_ENV, ScriptedTurn, Step, StubScript};
use space_core::{DiscoveryOptions, HostOptions};

use crate::app::{Options, SeedThread};

#[derive(Default)]
pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    /// Opened in order (the sidebar shows the last one on top).
    pub threads: Vec<ScenarioThread>,
    /// Index of the thread to select (default: the last).
    pub select: Option<usize>,
    /// Open on the Settings page.
    pub settings_page: bool,
    /// Contents of the scenario's settings file (a temp file; never `~/.space/settings.json`).
    pub settings: Option<&'static str>,
}

#[derive(Default)]
pub struct ScenarioThread {
    /// Sent as the first message; `steps` is what the stub streams back.
    pub prompt: Option<&'static str>,
    pub steps: Vec<Step>,
    /// Project-relative folder for the thread (default: the project).
    pub subdir: Option<&'static str>,
    pub draft: Option<String>,
    /// Hold the transcript this many px above the bottom (as if scrolled up) once it's that long.
    pub scrolled_up: Option<f32>,
}

// ----- step helpers -------------------------------------------------------------------------

fn thought(t: &str) -> Step {
    Step::Thought { text: t.into() }
}
fn say(t: &str) -> Step {
    Step::Text { text: t.into() }
}
fn tool(id: &str, title: &str, kind: &str, status: &str, output: Option<&str>) -> Step {
    Step::Tool { id: id.into(), title: title.into(), kind: kind.into(), status: status.into(), output: output.map(Into::into) }
}
fn context(tokens: u64) -> Step {
    Step::Context { tokens }
}
fn thread(prompt: &'static str, steps: Vec<Step>) -> ScenarioThread {
    ScenarioThread { prompt: Some(prompt), steps, ..Default::default() }
}

const TEST_OUTPUT: &str = "running 6 tests\ntest configure_switches_model_and_thinking ... ok\ntest crash_is_reported ... ok\n\
test full_turn_streams_and_ends ... ok\ntest permission_allow_and_reject ... ok\ntest result: ok. 6 passed; 0 failed";

const EXPLAIN: &str = "Plugin discovery scans three roots in order: the bundled `plugins/` directory, \
`~/.space/plugins` and the project's `.space/plugins`. Later roots win on duplicate ids.\n\n\
**What happens on start**\n\n\
- The manifest's `run.command` is resolved on `PATH` and spawned with piped stdio.\n\
- The harness performs the ACP `initialize` handshake.\n\
- stderr lines are kept in a ring buffer for the logs panel.\n\n\
```rust\nlet agent = host.start_agent(\"pi-agent\").await?;\nlet session = agent.new_session(dir).await?;\n```\n\n\
Invalid manifests are reported, not fatal.";

fn explain_thread() -> ScenarioThread {
    thread(
        "Explain how plugin discovery works, with a short code example",
        vec![
            thought("Read discovery.rs, then summarize the order and the start sequence."),
            tool("t1", "read: crates/space-core/src/discovery.rs", "read", "completed", Some("194 lines")),
            say(EXPLAIN),
            context(41_000),
        ],
    )
}

fn working_thread() -> ScenarioThread {
    thread(
        "Refactor the session reducer to keep tool calls in order",
        vec![
            thought("Tool updates for unknown ids are appended at the end; check where that happens."),
            tool("t1", "read: crates/space-core/src/session.rs", "read", "completed", Some("350 lines")),
            say("The reducer materializes updates for unknown tool calls at the end of the transcript, which is why \
                 they can show up out of order. I'll keep a position index so late updates land"),
            Step::Hold,
        ],
    )
}

fn thinking_thread() -> ScenarioThread {
    thread(
        "Why does the composer flicker when it grows?",
        vec![thought("The composer decides its layout from the previous frame's bounds, so a wrap that"), Step::Hold],
    )
}

fn tool_thread() -> ScenarioThread {
    thread(
        "Run the full test suite",
        vec![
            thought("Run the workspace tests first."),
            tool("t1", "bash: cargo test --workspace", "execute", "in_progress", None),
            Step::Hold,
        ],
    )
}

fn permission_thread() -> ScenarioThread {
    thread(
        "Clean the incremental build cache",
        vec![
            thought("Deleting files needs the user's go-ahead."),
            tool("t1", "bash: rm -rf target/debug/incremental", "execute", "pending", None),
            Step::Permission { tool_id: "t1".into(), title: "bash: rm -rf target/debug/incremental".into() },
            tool("t1", "bash: rm -rf target/debug/incremental", "execute", "completed", Some("")),
            say("Removed the incremental cache (1.2 GB)."),
        ],
    )
}

fn error_thread() -> ScenarioThread {
    thread(
        "Summarize the open pull requests",
        vec![
            thought("List the PRs with gh, then summarize."),
            Step::Fail { message: "The model request failed: overloaded (529). Try again in a minute.".into() },
        ],
    )
}

fn done_thread(prompt: &'static str, reply: &str) -> ScenarioThread {
    thread(
        prompt,
        vec![
            thought("Check the code first."),
            tool("t1", "bash: cargo test -p space-core", "execute", "completed", Some(TEST_OUTPUT)),
            say(reply),
        ],
    )
}

/// Every scenario, in menu order.
pub fn all() -> Vec<Scenario> {
    let twelve = (1..=12).map(|i| format!("{i}. step {i} of the release checklist")).collect::<Vec<_>>().join("\n");
    vec![
        Scenario { name: "empty", description: "A fresh window: one empty thread.", ..Default::default() },
        Scenario {
            name: "thread-working",
            description: "A turn streaming a reply (held mid-sentence).",
            threads: vec![working_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "thread-thinking",
            description: "A turn held while thinking.",
            threads: vec![thinking_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "tool-running",
            description: "A turn held while a tool runs.",
            threads: vec![tool_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "permission-pending",
            description: "A turn waiting for a permission answer.",
            threads: vec![permission_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "error",
            description: "A turn that failed.",
            threads: vec![error_thread()],
            ..Default::default()
        },
        Scenario {
            name: "many-threads",
            description: "Nine threads in mixed states; a finished one selected.",
            threads: vec![
                done_thread("Bump the gpui-kit version", "Bumped `gpui-kit` to 0.9.0; all tests pass."),
                error_thread(),
                done_thread("Fix the flaky stub test", "The test raced the stub's exit; it now waits for the status."),
                ScenarioThread { subdir: Some("crates/space-core"), ..tool_thread() },
                permission_thread(),
                thinking_thread(),
                done_thread("Add a context ring to the composer", "Added a 16px ring with the token counts in its tooltip."),
                working_thread(),
                explain_thread(),
            ],
            select: Some(8),
            ..Default::default()
        },
        Scenario {
            name: "long-composer",
            description: "A finished thread with a 12-line draft in the composer.",
            threads: vec![ScenarioThread { draft: Some(twelve), ..explain_thread() }],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "long-transcript",
            description: "A thread longer than the window, scrolled to the end (bottom fade over text).",
            threads: vec![long_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "scrolled-up",
            description: "A long thread held scrolled up (scroll-to-bottom button) while the reply streams below.",
            threads: vec![streaming_long_thread()],
            select: None,
            ..Default::default()
        },
        Scenario {
            name: "settings",
            description: "The Settings page (⌘,) with the stub's models and a saved default.",
            threads: vec![explain_thread()],
            settings_page: true,
            settings: Some(
                r#"{ "newThreads": { "model": { "provider": "stub", "id": "stub-fast" }, "thinkingLevel": "medium" } }"#,
            ),
            ..Default::default()
        },
    ]
}

/// The explain reply three times over, so the transcript always overflows the window.
fn long_thread() -> ScenarioThread {
    let mut steps = Vec::new();
    for i in 0..3 {
        steps.push(tool(&format!("t{i}"), "read: crates/space-core/src/discovery.rs", "read", "completed", Some("194 lines")));
        steps.push(say(EXPLAIN));
    }
    steps.push(context(120_000));
    thread("Walk me through plugin discovery in detail", steps)
}

/// A long finished stretch, then a reply that keeps streaming a sentence every ~0.6 s for a few
/// minutes and holds; the view is held scrolled up above it.
fn streaming_long_thread() -> ScenarioThread {
    let mut steps = Vec::new();
    for i in 0..3 {
        let title = "read: crates/space-core/src/session.rs";
        steps.push(tool(&format!("t{i}"), title, "read", "completed", Some("350 lines")));
        steps.push(say(EXPLAIN));
    }
    steps.push(say("\n\n**Streaming below** (scroll down or press ⌘↓ to follow):\n\n"));
    for i in 1..=300 {
        steps.push(Step::Sleep { ms: 600 });
        steps.push(say(&format!("Line {i}: another sentence arrives below the fold while you read above.\n\n")));
    }
    steps.push(Step::Hold);
    ScenarioThread {
        scrolled_up: Some(600.),
        ..thread("Walk me through the session reducer while it keeps going", steps)
    }
}

pub fn find(name: &str) -> Option<Scenario> {
    all().into_iter().find(|s| s.name == name)
}

pub fn list() -> String {
    all().iter().map(|s| format!("{:<20} {}", s.name, s.description)).collect::<Vec<_>>().join("\n")
}

/// The stub agent binary: next to this executable, else `SPACE_STUB_AGENT`, else the
/// workspace's debug build.
fn stub_agent() -> Result<PathBuf> {
    let candidates = [
        std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("space-stub-agent"))),
        std::env::var_os("SPACE_STUB_AGENT").map(PathBuf::from),
        Some(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/space-stub-agent"))),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file()).context(
        "space-stub-agent not found; build it with `cargo build -p space-core --bin space-stub-agent`",
    )
}

/// The stub's script for `scenario`.
pub fn script(scenario: &Scenario) -> StubScript {
    StubScript {
        turns: scenario
            .threads
            .iter()
            .filter_map(|t| t.prompt.map(|p| ScriptedTurn { prompt: p.into(), steps: t.steps.clone() }))
            .collect(),
    }
}

/// Write the stub plugin + script under a temp dir and return host and app options that
/// boot the scenario.
pub fn prepare(scenario: &Scenario, project_dir: &Path) -> Result<(HostOptions, Options)> {
    let root = std::env::temp_dir().join(format!("space-scenario-{}", std::process::id()));
    let plugin = root.join("stub");
    std::fs::create_dir_all(&plugin)?;
    let script_path = root.join("script.json");
    std::fs::write(&script_path, serde_json::to_string_pretty(&script(scenario))?)?;
    let stub = stub_agent()?;
    let manifest = format!(
        "id = \"stub\"\nname = \"Stub Agent\"\nversion = \"0.1.0\"\nprovides = [\"agent\"]\n\n[run]\ncommand = {:?}\n\n[run.env]\n{SCRIPT_ENV} = {:?}\n",
        stub.display().to_string(),
        script_path.display().to_string(),
    );
    std::fs::write(plugin.join("space-plugin.toml"), manifest)?;
    let settings_path = root.join("settings.json");
    match scenario.settings {
        Some(json) => std::fs::write(&settings_path, json)?,
        None => {
            let _ = std::fs::remove_file(&settings_path);
        }
    }

    let host = HostOptions {
        project_dir: project_dir.to_path_buf(),
        discovery: DiscoveryOptions { bundled_roots: vec![root], user_root: None, project_root: None },
        secrets: space_core::Secrets::new("dev.brijesh.space.scenario"),
    };
    let seed = scenario
        .threads
        .iter()
        .map(|t| SeedThread {
            prompt: t.prompt.map(Into::into),
            cwd: t.subdir.map(|d| project_dir.join(d)),
            draft: t.draft.clone(),
            scrolled_up: t.scrolled_up,
        })
        .collect();
    let options = Options {
        project_dir: project_dir.to_path_buf(),
        agent: Some("stub".into()),
        seed,
        select: scenario.select,
        // Scripted turns end instantly; alerts only on request (`SPACE_NOTIFY=1`).
        notify: std::env::var("SPACE_NOTIFY").is_ok_and(|v| v == "1"),
        // Never the user's settings file.
        settings_path: Some(settings_path),
        open_settings: scenario.settings_page,
    };
    Ok((host, options))
}

/// Resolve a `--scenario` argument.
pub fn resolve(name: &str) -> Result<Scenario> {
    match find(name) {
        Some(s) => Ok(s),
        None => bail!("unknown scenario `{name}`; available:\n{}", list()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_are_well_formed() {
        let all = all();
        for wanted in [
            "empty",
            "thread-working",
            "thread-thinking",
            "tool-running",
            "permission-pending",
            "error",
            "many-threads",
            "long-composer",
            "scrolled-up",
            "settings",
        ] {
            assert!(find(wanted).is_some(), "missing scenario {wanted}");
        }
        let mut names = std::collections::HashSet::new();
        for s in &all {
            assert!(names.insert(s.name), "duplicate {}", s.name);
            assert!(s.select.is_none_or(|i| i < s.threads.len()), "{}: select out of range", s.name);
            // Every prompt is unique so the stub can find its script.
            let prompts: Vec<_> = s.threads.iter().filter_map(|t| t.prompt).collect();
            let unique: std::collections::HashSet<_> = prompts.iter().collect();
            assert_eq!(prompts.len(), unique.len(), "{}: duplicate prompts", s.name);
        }
        assert!(list().lines().count() == all.len());
        assert!(resolve("nope").is_err());
    }

    #[test]
    fn script_round_trips_through_json() {
        let s = find("permission-pending").unwrap();
        let script = script(&s);
        let json = serde_json::to_string(&script).unwrap();
        let back: StubScript = serde_json::from_str(&json).unwrap();
        assert_eq!(back.turns.len(), 1);
        assert!(back.turn("Clean the incremental build cache").is_some());
    }

    #[test]
    fn prepare_writes_plugin_and_seed() {
        if stub_agent().is_err() {
            return; // stub not built in this test environment
        }
        let dir = tempfile_dir();
        let (host, options) = prepare(&find("many-threads").unwrap(), &dir).unwrap();
        let manifest = std::fs::read_to_string(host.discovery.bundled_roots[0].join("stub/space-plugin.toml")).unwrap();
        assert!(manifest.contains(SCRIPT_ENV));
        assert_eq!(options.seed.len(), 9);
        assert_eq!(options.agent.as_deref(), Some("stub"));
        assert!(options.settings_path.unwrap().starts_with(std::env::temp_dir()));

        let (_, options) = prepare(&find("settings").unwrap(), &dir).unwrap();
        assert!(options.open_settings);
        let store = crate::settings::SettingsStore::load(options.settings_path);
        assert_eq!(store.settings.new_threads.model.unwrap().id, "stub-fast");
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("space-scenario-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
