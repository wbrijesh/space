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
    /// Also run the real permissions plugin (its Keychain items use a scenario-only prefix, so
    /// no decision-model key is set).
    pub permissions: bool,
    /// Also run the real trajectory plugin, and open the selected thread's Trajectory tab.
    pub trajectory: bool,
    /// Also run the real terminal plugin (with a scripted shell: a colour test, then `sh`), and
    /// open the selected thread's terminal pane.
    pub terminal: bool,
    /// With `permissions`: a decision-model key is set (in a scenario-only Keychain item) but
    /// OpenAI can't be reached, so Workspace Write and Full access calls are paused.
    pub model_offline: bool,
}

#[derive(Default)]
pub struct ScenarioThread {
    /// Sent as the first message; `steps` is what the stub streams back.
    pub prompt: Option<&'static str>,
    pub steps: Vec<Step>,
    /// Project-relative folder for the thread (default: the project).
    pub subdir: Option<&'static str>,
    pub draft: Option<String>,
    /// Permission mode for the thread (with `permissions`).
    pub mode: Option<&'static str>,
    /// Hold the transcript this many px above the bottom (as if scrolled up) once it's that long.
    pub scrolled_up: Option<f32>,
    /// Repo-relative images sent with `prompt`, and in the draft.
    pub images: &'static [&'static str],
    pub draft_images: &'static [&'static str],
}

const ICON: &str = "crates/space-app/resources/AppIcon.png";
const ICON_S: &str = "crates/space-app/resources/AppIcon.icon/Assets/s.png";

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
fn guard(id: &str, title: &str, kind: &str, tool: &str, args: serde_json::Value, output: Option<&str>) -> Step {
    Step::Guard {
        id: id.into(),
        title: title.into(),
        kind: kind.into(),
        tool: tool.into(),
        args,
        output: output.map(Into::into),
    }
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
- stderr lines are kept in a ring buffer for error messages.\n\n\
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

/// A finished turn with two steps and a reply between them (it folds once done).
fn multi_step_thread() -> ScenarioThread {
    thread(
        "Fix the failing test in space-core",
        vec![
            thought("Run the suite first to see what fails."),
            tool("t1", "bash: cargo test -p space-core", "execute", "completed", Some(TEST_OUTPUT)),
            say("One test expects the old error message. Updating it.\n\n"),
            tool("t2", "edit: crates/space-core/tests/stub_agent.rs", "edit", "completed", Some("1 line changed")),
            tool("t3", "bash: cargo test -p space-core", "execute", "completed", Some(TEST_OUTPUT)),
            say("Fixed: the test now expects the new message, and the suite passes."),
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
            name: "collapsed-turns",
            description: "Finished multi-step turns fold their work under \"Completed in …\"; a failed turn stays open; one still runs.",
            threads: vec![working_thread(), error_thread(), multi_step_thread()],
            ..Default::default()
        },
        Scenario {
            name: "trajectory",
            description: "Two finished threads; the selected one shows its Trajectory tab.",
            threads: vec![explain_thread(), done_thread("Fix the failing test in space-core", "Fixed: the test expected the old message.")],
            trajectory: true,
            ..Default::default()
        },
        Scenario {
            name: "images",
            description: "A message with two images and its reply, and an image in the draft.",
            threads: vec![ScenarioThread {
                images: &[ICON, ICON_S],
                draft_images: &[ICON],
                draft: Some("And this one?".into()),
                ..done_thread("What's in these images?", "Both show the Space app icon: an S on an apricot tile.")
            }],
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
            name: "guardrail",
            description: "Permission modes with the real permissions plugin and no decision-model key: a Read Only \
                          thread with a blocked edit, and a Workspace Write thread paused (sending is blocked).",
            threads: vec![read_only_thread(), unchecked_thread()],
            permissions: true,
            ..Default::default()
        },
        Scenario {
            name: "guardrail-offline",
            description: "The real permissions plugin with a key set but OpenAI unreachable: a Workspace Write thread \
                          paused mid-turn, the retrying pill, and a Read Only thread that still works.",
            threads: vec![read_only_thread(), paused_thread()],
            permissions: true,
            model_offline: true,
            ..Default::default()
        },
        Scenario {
            name: "terminal",
            description: "The terminal pane beside a finished thread: the real terminal plugin running a scripted shell \
                          (a colour test and a prompt, then sh).",
            threads: vec![explain_thread()],
            terminal: true,
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

/// Read Only: inspection runs, the edit is blocked by the permissions plugin.
fn read_only_thread() -> ScenarioThread {
    ScenarioThread {
        mode: Some("read-only"),
        ..thread(
            "Why do tool updates sometimes land out of order?",
            vec![
                thought("Inspect the reducer and its tests."),
                guard("t1", "grep: ToolCallUpdate", "search", "grep", serde_json::json!({"pattern": "ToolCallUpdate"}), Some(
                    "crates/space-core/src/session.rs:184",
                )),
                guard("t2", "write: crates/space-core/src/session.rs", "edit", "write", serde_json::json!({
                    "path": "crates/space-core/src/session.rs", "content": "…"
                }), None),
                say("An update for a call the reducer hasn't seen is materialized **at the end** of the transcript. \
                     I couldn't apply the fix here (Read Only); switch the thread to Workspace Write and I'll make it."),
                context(36_000),
            ],
        )
    }
}

/// Workspace Write while OpenAI can't be reached: the first call ran, the next one waits.
fn paused_thread() -> ScenarioThread {
    ScenarioThread {
        mode: Some("workspace-write"),
        ..thread(
            "Rebuild the release binary",
            vec![
                thought("Build it with the release profile."),
                guard("t1", "bash: cargo build --release", "execute", "bash", serde_json::json!({"command": "cargo build --release"}), Some(
                    "Finished `release` profile",
                )),
                say("Built."),
            ],
        )
    }
}

/// Workspace Write without the decision model's key: the call waits (fail closed).
fn unchecked_thread() -> ScenarioThread {
    ScenarioThread {
        mode: Some("workspace-write"),
        ..thread(
            "Upgrade node and rerun the build",
            vec![
                thought("Upgrade node first."),
                guard("t1", "bash: brew upgrade node", "execute", "bash", serde_json::json!({"command": "brew upgrade node"}), Some(
                    "==> Upgrading node",
                )),
                say("Upgraded."),
            ],
        )
    }
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
    let permissions = root.join("permissions");
    let _ = std::fs::remove_dir_all(&permissions);
    let trajectory = root.join("trajectory");
    let _ = std::fs::remove_dir_all(&trajectory);
    if scenario.trajectory {
        let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x"))
            .context("the trajectory plugin lives in the repo; run scenarios from a checkout")?;
        std::fs::create_dir_all(&trajectory)?;
        std::fs::copy(repo.join("plugins/trajectory/space-plugin.toml"), trajectory.join("space-plugin.toml"))?;
    }
    let terminal = root.join("terminal");
    let _ = std::fs::remove_dir_all(&terminal);
    if scenario.terminal {
        let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x"))
            .context("the terminal plugin lives in the repo; run scenarios from a checkout")?;
        std::fs::create_dir_all(&terminal)?;
        let shell = root.join("scenario-shell.sh");
        std::fs::write(&shell, SCENARIO_SHELL)?;
        std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        let mut manifest = std::fs::read_to_string(repo.join("plugins/terminal/space-plugin.toml"))?;
        manifest.push_str(&format!("\n[run.env]\nSPACE_TERMINAL_SHELL = {:?}\n", shell.display().to_string()));
        std::fs::write(terminal.join("space-plugin.toml"), manifest)?;
    }
    if scenario.permissions {
        let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x"))
            .context("the permissions plugin lives in the repo; run scenarios from a checkout")?;
        std::fs::create_dir_all(&permissions)?;
        let mut manifest = std::fs::read_to_string(repo.join("plugins/permissions/space-plugin.toml"))?;
        if scenario.model_offline {
            // Nothing listens on port 9: every check fails as "can't connect".
            manifest.push_str("\n[run.env]\nSPACE_LUNA_BASE_URL = \"http://127.0.0.1:9/v1\"\n");
        }
        std::fs::write(permissions.join("space-plugin.toml"), manifest)?;
    }
    // Scenario-only Keychain items; the offline scenario has its own, with a (fake) key.
    let keychain = if scenario.model_offline { "dev.brijesh.space.scenario.offline" } else { "dev.brijesh.space.scenario" };
    let secrets = space_core::Secrets::new(keychain);
    if scenario.model_offline {
        secrets.set("permissions", "openai_api_key", Some("sk-scenario-offline"))?;
    }
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
        secrets,
        held_recheck: space_core::HELD_RECHECK,
        space_dir: None,
    };
    let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x")).unwrap_or_default();
    let seed = scenario
        .threads
        .iter()
        .map(|t| SeedThread {
            prompt: t.prompt.map(Into::into),
            cwd: t.subdir.map(|d| project_dir.join(d)),
            draft: t.draft.clone(),
            mode: t.mode.map(Into::into),
            scrolled_up: t.scrolled_up,
            images: t.images.iter().map(|p| repo.join(p)).collect(),
            draft_images: t.draft_images.iter().map(|p| repo.join(p)).collect(),
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
        open_trajectory: scenario.trajectory,
        open_terminal: scenario.terminal,
    };
    Ok((host, options))
}

/// The `terminal` scenario's shell: a test run, the 16 colours and styles, then an interactive
/// `sh` with a fixed prompt.
const SCENARIO_SHELL: &str = r#"#!/bin/sh
p() { printf '%b' "$1"; }
prompt='\033[1;34m~/projects/space\033[0m \033[32mmain\033[0m \342\235\257 '
p "${prompt}cargo test -p space-core\r\n"
p "   \033[1;32mCompiling\033[0m space-core v0.1.0\r\n"
p "    \033[1;32mFinished\033[0m test profile in 3.2s\r\n"
p "test session::tests::reducer ... \033[32mok\033[0m\r\n"
p "test result: \033[32mok\033[0m. 28 passed; 0 failed\r\n\r\n"
for i in 0 1 2 3 4 5 6 7; do p "\033[3${i}m colour$i \033[0m"; done; p "\r\n"
for i in 0 1 2 3 4 5 6 7; do p "\033[9${i}m bright$i \033[0m"; done; p "\r\n"
p "\033[1mbold\033[0m \033[3mitalic\033[0m \033[4munderline\033[0m \033[9mstrike\033[0m \033[7m inverse \033[0m \033[48;5;24m 256-colour bg \033[0m\r\n\r\n"
PS1="$(printf '%b' "$prompt")"
export PS1
exec /bin/sh -i
"#;

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
            "guardrail",
            "guardrail-offline",
            "settings",
            "terminal",
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
