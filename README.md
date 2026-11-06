# Space

A native macOS development environment built around projects, bots and knowledge. v0 is the
foundation: a Rust/[GPUI](https://gpui.rs) **harness** that hosts out-of-process **plugins** over the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP), plus a first agent plugin built on the
pi SDK. See [docs/architecture.md](docs/architecture.md) for the contract (manifest, discovery, ACP subset,
`_space/*` extensions).

```
crates/space-protocol   manifest types (space-plugin.toml), _space/* extension types, ACP re-exports
crates/space-core       discovery · process supervisor · ACP client · session model (no UI deps)
crates/space-ui         design tokens · animation clock · components (+ previews and gallery, `dev`)
crates/space-app        GPUI desktop app, binary `Space` (app state, wiring, dev scenarios)
plugins/pi-agent        TypeScript ACP agent on the pi SDK (Claude CLI subscription)
```

## Prerequisites

- macOS with **Xcode** installed, including the **Metal toolchain** (GPUI compiles Metal shaders).
  `.cargo/config.toml` points `DEVELOPER_DIR` at `/Applications/Xcode.app`, so `xcode-select` can stay
  on the Command Line Tools. Check: `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun metal --version`.
  If it is missing: `xcodebuild -downloadComponent MetalToolchain`.
- Rust (stable, edition 2024; tested with 1.98).
- Node ≥ 24 and pnpm (for `plugins/pi-agent`).
- The `claude` CLI, signed in. pi-agent uses your Claude subscription through it, so prompts cost usage.
- Optional: [`just`](https://github.com/casey/just).

## Commands

| | |
|---|---|
| `just build` | `cargo build --workspace` + `pnpm install && pnpm build` in `plugins/pi-agent` |
| `just run [DIR]` | open the app on a project directory (default: this repo), from `target/debug/Space.app` |
| `just bundle [debug\|release]` | wrap the built binary in `target/<profile>/Space.app` (bundle id `dev.brijesh.space`, icon) |
| `just demo "Reply with exactly: pong"` | open the app and send the prompt in the first thread |
| `just gallery` | dev: component gallery: tokens, every component state, animation-clock controls |
| `just scenario name=many-threads` | dev: the real app in a held, scripted state (`name=list` lists them) |
| `just release` | release build without dev tooling |
| `just test` | all Rust tests; they use the scripted `space-stub-agent`, so they make no model calls |
| `just lint` | clippy with `-D warnings` |
| `just smoke ["PROMPT"]` | headless end-to-end run against pi-agent, printing events (billed) |
| `just smoke-stub` | the same headless run against the stub agent (free) |

Without just, run `cargo run -p space-app -- [PROJECT_DIR]` (no bundle: no system notifications, sound only) or
`cargo run -p space-app -- --smoke "PROMPT" [--plugin ID] [--allow] [PROJECT_DIR]`.

Environment:

- `SPACE_AGENT=<plugin id>` picks the agent plugin (default `pi-agent`, else the first agent found).
- `SPACE_PLUGIN_DIRS=dir1:dir2` replaces the bundled plugin roots (`<repo>/plugins`).
  `~/.space/plugins` and `<project>/.space/plugins` are still scanned.
- `SPACE_AUTOPROMPT="…"` sends that prompt in the first thread once its session is up.
- `SPACE_NOTIFY=0` turns off the "agent finished" alert. When a turn ends, Space plays a sound if you're
  watching that thread, otherwise it posts a macOS notification (bundled app only); clicking it opens
  the thread. Scenarios are silent unless `SPACE_NOTIFY=1`.
- `RUST_LOG=space_core=debug` turns on logging (stderr).
- Dev builds: `SPACE_CLOCK=paused|pin:0.35|speed:0.25` starts the animation clock in that state;
  `SPACE_GALLERY_PAGE=<name>` opens a gallery page; `SPACE_SCRIPT=<file>` replays input. See
  [docs/ui-development.md](docs/ui-development.md) for the gallery, scenarios and the clock.

## App icon

`crates/space-app/resources/AppIcon.{png,icns}` is a **placeholder** (graphite tile, dashed frame,
circle/square/triangle) until there's a real logo. Regenerate with `python3 scripts/make_placeholder_icon.py`.

## Using the app

The app starts the agent plugin in the background on launch and opens a new thread. There is no
plugin list; the agent's status sits in the sidebar footer.

- **Sidebar**: a plain list of thread titles (first line of your first message), newest first. A
  spinner shows while a thread works; a dot marks news you haven't seen (blue: reply, red: error,
  yellow: waiting for your input). ⌘B hides the sidebar.
- **Title bar**: sidebar toggle, back/forward through the threads you visited (⌘[ / ⌘]), new
  thread (⌘N), and the current thread's title and folder. Drag it to move the window.
- **Composer**: one row, **+** · text · send, while the text fits on one line. Longer text
  switches the card to two zones: the text across the full width (growing upward to ten lines,
  then scrolling) and **+** / send in a row below; it collapses again when the text fits. Enter
  sends, Shift+Enter adds a newline. While the agent works, send
  becomes Stop (`session/cancel`). **+** attaches files or folders (sent as ACP `resource_link`s).
  Below the card: the project folder (switch it while the thread is empty, otherwise pick a folder
  to start a new thread there), the model and thinking effort (both switch the session through
  `_space/session/configure`, and new threads inherit your last choice), and a context ring
  (hover for token counts).
- **Transcript**: your messages as bubbles, replies as markdown, thinking collapsed as
  "Thought for Ns", tool calls as compact rows that expand to their output, permission cards
  (Allow/Reject).
- **Logs**: the terminal icon in the sidebar footer (or ⌘J) opens the agent's stderr. If the agent
  exits, the thread shows the error with Restart and Logs.

## space-core API sketch

```rust
let (host, mut events) = Host::new(tokio_handle, HostOptions::for_project(dir));
let agent: AgentHandle = host.start_agent("pi-agent").await?;   // spawn + ACP initialize
let session: Session = agent.new_session(dir).await?;           // session/new
session.prompt("hi").await?;   // streams HostEvent::Session { event: SessionEvent::.. }; returns stop reason
session.cancel()?;             // session/cancel
session.resolve_permission(request_id, PermissionDecision::Selected(option_id))?;
// UI side: SessionState::apply(&event) folds events into a transcript.
```
