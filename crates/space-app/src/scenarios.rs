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
    /// Also run the real trajectory plugin, and show the selected thread's trajectory.
    pub trajectory: bool,
    /// Open the right pane on Files (the project folder: the repo, or the folder given).
    pub files: bool,
    /// Open the right pane on Changes (the project folder: the repo, or the folder given).
    pub changes: bool,
    /// Also run the real terminal plugin (with a scripted shell: a colour test, then `sh`), and
    /// open the selected thread's terminal pane.
    pub terminal: bool,
    /// Also run the real session store, keeping threads in a temp folder of the scenario's own
    /// that outlives the run: launch it again to see what was saved.
    pub store: bool,
    /// Threads already in the store when it starts (with `store`; replacing what it kept).
    pub saved: Vec<SavedThread>,
    /// More plugins to run from the repo's `plugins/` (`alerts`, `titles`, …).
    pub plugins: &'static [&'static str],
    /// Plugins of the scenario's own, as (folder, manifest): `bundled` ones run; `home` ones
    /// are from your home folder, so they wait for your approval.
    pub bundled: &'static [(&'static str, &'static str)],
    pub home: &'static [(&'static str, &'static str)],
}

/// A thread saved before the scenario starts: made `created` days ago, marked done `done` days
/// ago (or still open), maybe pinned. One exchange: its title, and a short reply.
pub struct SavedThread {
    pub title: &'static str,
    pub created: f64,
    pub done: Option<f64>,
    pub pinned: bool,
}

fn saved(title: &'static str, created: f64, done: Option<f64>) -> SavedThread {
    SavedThread { title, created, done, pinned: false }
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
    /// Repo-relative images sent with `prompt`, and in the draft.
    pub images: &'static [&'static str],
    pub draft_images: &'static [&'static str],
    /// Pinned to the top of the sidebar.
    pub pinned: bool,
    /// Quoted by `prompt`, and in the composer.
    pub quote: Option<&'static str>,
    pub draft_quote: Option<&'static str>,
    /// Sent after `prompt`, one per turn as each ends, with what the stub streams back.
    pub followups: Vec<(&'static str, Vec<Step>)>,
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
```rust\nlet agent = host.start_agent(\"claude-agent\").await?;\nlet session = agent.new_session(dir).await?;\n```\n\n\
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

/// A turn whose model request waits for the network: the reply that broke off has left.
fn offline_thread() -> ScenarioThread {
    use space_core::protocol::{RetryKind, RetryState};
    thread(
        "Summarize what changed in the session reducer",
        vec![
            thought("Read the reducer first."),
            tool("t1", "read: crates/space-core/src/session.rs", "read", "completed", Some("350 lines")),
            say("The reducer now keeps"),
            Step::Retry { state: None, discard: true },
            Step::Retry {
                state: Some(RetryState {
                    kind: RetryKind::Offline,
                    attempt: 2,
                    reason: "API Error: Connection dropped (ECONNRESET). A VPN, firewall, or security software can cause this.".into(),
                }),
                discard: false,
            },
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
            name: "offline",
            description: "A turn waiting for the network after its reply broke off (\"Waiting for the network…\").",
            threads: vec![offline_thread()],
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
            name: "pinned-threads",
            description: "Six threads, two of them pinned: the pinned ones lead the sidebar (⌘-numbers follow it).",
            threads: vec![
                ScenarioThread { pinned: true, ..done_thread("Release checklist", "Tag, upload, update the cask.") },
                done_thread("Bump the gpui-kit version", "Bumped `gpui-kit` to 0.9.0; all tests pass."),
                ScenarioThread { pinned: true, ..done_thread("Guardrail notes", "Fail closed: a held call never runs unchecked.") },
                done_thread("Fix the flaky stub test", "The test raced the stub's exit; it now waits for the status."),
                error_thread(),
                explain_thread(),
            ],
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
            description: "Two finished threads; the selected one shows its trajectory.",
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
            name: "quote",
            description: "A reply to select and quote; a sent message with a quote, and one in the composer.",
            threads: vec![
                ScenarioThread {
                    quote: Some("Later roots win on duplicate ids."),
                    draft_quote: Some("Invalid manifests are reported, not fatal."),
                    draft: Some("Where are they reported?".into()),
                    ..done_thread("So a project plugin replaces a bundled one with the same id?", "Yes. The project's `.space/plugins` is scanned last, so its plugin replaces the bundled one, and the replaced one is logged.")
                },
            ],
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
            name: "terminal",
            description: "The terminal pane beside a finished thread: the real terminal plugin running a scripted shell \
                          (a colour test and a prompt, then sh).",
            threads: vec![explain_thread()],
            terminal: true,
            ..Default::default()
        },
        Scenario {
            name: "files",
            description: "The right pane's Files view beside a finished thread, on the project folder (the repo, \
                          or the folder given after the scenario name).",
            threads: vec![explain_thread()],
            files: true,
            ..Default::default()
        },
        Scenario {
            name: "changes",
            description: "The right pane's Changes view beside a finished thread, on the project folder (the \
                          repo, or the folder given after the scenario name). The stub writes commit messages.",
            threads: vec![explain_thread()],
            changes: true,
            ..Default::default()
        },
        Scenario {
            name: "rewind",
            description: "Three finished turns that wrote files under `rewind-demo/` in the project folder (give a \
                          scratch folder after the scenario name): hover a message for ✎ Edit and ↺ Rewind, or Esc \
                          Esc in the empty composer. Threads are saved in a temp folder kept between launches.",
            threads: vec![rewind_thread()],
            store: true,
            ..Default::default()
        },
        Scenario {
            name: "side-chat",
            description: "A finished thread to open the side chat beside (⌘⇧L, or /btw and a space). Threads are \
                          saved in a temp folder kept between launches, so a side chat is there again next time.",
            threads: vec![explain_thread()],
            store: true,
            ..Default::default()
        },
        Scenario {
            name: "done-threads",
            description: "Saved threads, two pinned, and done ones under the sidebar's Done header, from today \
                          back to last year (never deleted). Click a done thread to reopen it. Threads are saved \
                          in a temp folder, set back to this at each launch.",
            store: true,
            settings: Some(r#"{ "threads": { "keepDoneDays": null } }"#),
            saved: vec![
                SavedThread { pinned: true, ..saved("Release checklist", 20., None) },
                SavedThread { pinned: true, ..saved("Guardrail notes", 3., None) },
                saved("Fix the failing smoke test in space-core", 0.1, None),
                saved("Bump the gpui-kit version", 1.2, None),
                saved("Explain how plugin discovery works", 6., None),
                saved("Add a context ring to the composer", 2., Some(0.05)),
                saved("Fix the flaky stub test", 1.5, Some(1.)),
                saved("Terminal plugin with alacritty", 9., Some(3.)),
                saved("Greeting exchange", 12., Some(5.)),
                saved("Sketch the plugin manifest", 30., Some(15.)),
                saved("Release 0.1.0", 70., Some(45.)),
                saved("First prototype", 420., Some(400.)),
            ],
            ..Default::default()
        },
        Scenario {
            name: "settings",
            description: "Settings (⌘,) with the stub's models and a saved default, the bundled plugins' settings, \
                          a guard with a key (no page of its own: under its row on Plugins), a plugin waiting for \
                          approval, and days typed before there were choices (45: shown as 90 days).",
            threads: vec![explain_thread()],
            settings_page: true,
            settings: Some(
                r#"{ "newThreads": { "model": { "provider": "stub", "id": "stub-fast" }, "thinkingLevel": "medium" },
                     "threads": { "keepDoneDays": 45 },
                     "plugins": { "settings": { "trajectory": { "keep_days": "45" } } } }"#,
            ),
            store: true,
            plugins: &["alerts", "titles", "trajectory"],
            bundled: &[("guard", GUARD_MANIFEST)],
            home: &[("notes", NOTES_MANIFEST)],
            ..Default::default()
        },
    ]
}

/// The `settings` scenario's guard: the stub guard with an API key setting.
const GUARD_MANIFEST: &str = r#"id = "guard"
name = "Guard"
version = "0.1.0"
description = "Checks each operation in Guarded mode."
provides = ["settings"]

[run]
command = "${target}/space-stub-guard"

[run.env]
SPACE_STUB_GUARD = '{ "secret": "api_key" }'
"#;

/// The `settings` scenario's plugin from your home folder, waiting for approval (never started).
const NOTES_MANIFEST: &str = r#"id = "notes"
name = "Notes"
version = "0.1.0"
description = "Keeps a note of what each thread decided."
provides = ["settings"]
observes = ["turn/ended"]
uses = ["read:transcript", "host/notify"]

[run]
command = "${target}/space-stub-guard"
"#;

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

/// Three turns, each changing files, to go back through.
fn rewind_thread() -> ScenarioThread {
    let write = |path: &str, text: &str| Step::Write { path: format!("rewind-demo/{path}"), text: text.into() };
    ScenarioThread {
        followups: vec![
            ("Also remember its width", vec![thought("Keep it in the window state."), write("sidebar.rs", "resizable\nremembered\n"), write("settings.rs", "sidebar_width\n"), say("Done: the width is saved per window and restored at launch.")]),
            ("Add a minimum width of 200 px", vec![write("sidebar.rs", "resizable\nremembered\nmin 200\n"), say("Done: it stops at 200 px.")]),
        ],
        ..thread("Make the sidebar resizable", vec![thought("A drag handle on its edge."), write("sidebar.rs", "resizable\n"), say("Done. Drag the divider to resize it.")])
    }
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
            .filter_map(|t| {
                // The agent gets the quote before the message.
                let input = space_core::UserInput { quote: t.quote.map(Into::into), ..space_core::UserInput::text(t.prompt?) };
                Some(ScriptedTurn { prompt: input.prompt_text(), steps: t.steps.clone() })
            })
            .chain(scenario.threads.iter().flat_map(|t| t.followups.iter().map(|(p, steps)| ScriptedTurn { prompt: (*p).into(), steps: steps.clone() })))
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
    let store = root.join("session-store");
    let _ = std::fs::remove_dir_all(&store);
    if scenario.store {
        let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x"))
            .context("the session store lives in the repo; run scenarios from a checkout")?;
        std::fs::create_dir_all(&store)?;
        std::fs::copy(repo.join("plugins/session-store/space-plugin.toml"), store.join("space-plugin.toml"))?;
    }
    for id in scenario.plugins {
        let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x"))
            .context("the bundled plugins live in the repo; run scenarios from a checkout")?;
        std::fs::create_dir_all(root.join(id))?;
        std::fs::copy(repo.join("plugins").join(id).join("space-plugin.toml"), root.join(id).join("space-plugin.toml"))?;
    }
    let home = std::env::temp_dir().join(format!("space-scenario-home-{}", std::process::id()));
    for (dir, (folder, manifest)) in scenario.bundled.iter().map(|p| (&root, p)).chain(scenario.home.iter().map(|p| (&home, p))) {
        std::fs::create_dir_all(dir.join(folder))?;
        std::fs::write(dir.join(folder).join("space-plugin.toml"), manifest)?;
    }
    // Scenario-only Keychain items.
    let secrets = space_core::Secrets::new("dev.brijesh.space.scenario");
    let settings_path = root.join("settings.json");
    match scenario.settings {
        Some(json) => std::fs::write(&settings_path, json)?,
        None => {
            let _ = std::fs::remove_file(&settings_path);
        }
    }

    let host = HostOptions {
        project_dir: project_dir.to_path_buf(),
        discovery: DiscoveryOptions {
            bundled_roots: vec![root],
            user_root: (!scenario.home.is_empty()).then_some(home),
            project_root: None,
        },
        secrets,
        held_recheck: space_core::HELD_RECHECK,
        // The store's threads: never `~/.space`.
        space_dir: scenario.store.then(|| std::env::temp_dir().join(format!("space-scenario-store-{}", scenario.name))),
    };
    if let Some(dir) = host.space_dir.as_ref().filter(|_| !scenario.saved.is_empty()) {
        write_saved(&dir.join("threads"), &scenario.saved, project_dir)?;
    }
    let repo = space_core::discovery::find_repo_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("x")).unwrap_or_default();
    let seed = scenario
        .threads
        .iter()
        .map(|t| SeedThread {
            prompt: t.prompt.map(Into::into),
            cwd: t.subdir.map(|d| project_dir.join(d)),
            draft: t.draft.clone(),
            mode: None,
            scrolled_up: t.scrolled_up,
            images: t.images.iter().map(|p| repo.join(p)).collect(),
            draft_images: t.draft_images.iter().map(|p| repo.join(p)).collect(),
            pinned: t.pinned,
            quote: t.quote.map(Into::into),
            draft_quote: t.draft_quote.map(Into::into),
            followups: t.followups.iter().map(|(p, _)| (*p).to_string()).collect(),
        })
        .collect();
    let options = Options {
        project_dir: project_dir.to_path_buf(),
        last_folder: false,
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
        open_files: scenario.files,
        open_changes: scenario.changes,
    };
    Ok((host, options))
}

/// Put `threads` in the session store's folder `dir`, in place of what was there.
fn write_saved(dir: &Path, threads: &[SavedThread], project_dir: &Path) -> Result<()> {
    use space_core::SessionEvent;
    use space_core::protocol::service::ThreadMeta;
    let _ = std::fs::remove_dir_all(dir);
    let now = crate::app::now_ms();
    let ago = |days: f64| now - (days * 86_400_000.) as u64;
    for (ix, t) in threads.iter().enumerate() {
        let id = format!("scenario-{ix}");
        let meta = ThreadMeta {
            id: id.clone(),
            title: t.title.into(),
            project: project_dir.display().to_string(),
            created_ms: ago(t.created),
            updated_ms: ago(t.done.unwrap_or(t.created)),
            titled: true,
            done_ms: t.done.map(ago),
            pinned_ms: t.pinned.then(|| ago(t.created)),
            ..Default::default()
        };
        let mut json = serde_json::to_value(meta)?;
        json["version"] = 1.into();
        let events = [
            space_core::UserInput::text(t.title).into_message(Some(ago(t.created)), false),
            SessionEvent::TurnStarted,
            SessionEvent::AgentText { text: "Done.".into() },
            SessionEvent::turn_ended("end_turn"),
        ];
        let lines: Vec<String> = events.iter().map(serde_json::to_string).collect::<Result<_, _>>()?;
        let thread = dir.join(id);
        std::fs::create_dir_all(&thread)?;
        std::fs::write(thread.join("meta.json"), format!("{json:#}\n"))?;
        std::fs::write(thread.join("events.jsonl"), lines.join("\n") + "\n")?;
    }
    Ok(())
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
