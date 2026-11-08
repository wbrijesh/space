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
plugins/permissions     permission modes: macOS sandbox, decision-model guardrail
plugins/alerts          a notification or sound when an agent finishes or pauses
plugins/session-store   saved threads in ~/.space/threads
plugins/titles          thread titles from the thread's agent model
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
`cargo run -p space-app -- --smoke "PROMPT" [--plugin ID] [--allow] [--permission-mode ID] [PROJECT_DIR]`.

Environment:

- `SPACE_AGENT=<plugin id>` picks the agent plugin (default `pi-agent`, else the first agent found).
- `SPACE_PLUGIN_DIRS=dir1:dir2` replaces the bundled plugin roots (`<repo>/plugins`).
  `~/.space/plugins` and `<project>/.space/plugins` are still scanned.
- `SPACE_AUTOPROMPT="…"` sends that prompt in the first thread once its session is up.
- `SPACE_NOTIFY=0` turns off notifications and sounds. The bundled alerts plugin (`plugins/alerts`) plays a
  sound when a turn ends in the thread you're watching, otherwise Space posts a macOS notification
  (bundled app only); clicking it opens the thread. Both can be turned off in Settings (⌘,) ▸ Alerts.
  Scenarios are silent unless `SPACE_NOTIFY=1`.
- `SPACE_SETTINGS=<file>` uses that settings file instead of `~/.space/settings.json`. Scenarios always
  use a temp file.
- `RUST_LOG=space_core=debug` turns on logging (stderr).
- Dev builds: `SPACE_CLOCK=paused|pin:0.35|speed:0.25` starts the animation clock in that state;
  `SPACE_GALLERY_PAGE=<name>` opens a gallery page; `SPACE_SCRIPT=<file>` replays input. See
  [docs/ui-development.md](docs/ui-development.md) for the gallery, scenarios and the clock.

## App icon

A black S (bottom bar, 68° stem, top bar of the same length, both ends square) on a flat apricot
tile, #FFAF54 (halfway between amber and orange).
`python3 scripts/make_app_icon.py` draws it into `crates/space-app/resources/`:

- `AppIcon.icon`: Icon Composer bundle (the tile colour as its fill, the S as its layer). macOS 26 masks and
  lights it, and uses it for the Dock, app switcher, Finder and notifications.
- `Assets.car`: compiled from it with Xcode's `actool`; `scripts/bundle.sh` ships it.
- `AppIcon.png`: flattened, pre-masked copy, set as the Dock icon only for a bare `cargo run`.

After changing the icon, macOS may keep showing the old one (cached per bundle id). Clear it with
`rm -rf "$(getconf DARWIN_USER_CACHE_DIR)"/com.apple.{dock.iconcache,iconservices,iconservicesagent}; killall iconservicesagent Dock NotificationCenter`.

## Using the app

The app starts the agent plugin in the background on launch and opens a new thread. Settings ▸ Plugins
lists every plugin and its status; if the agent can't start or exits, the thread says so with Restart.

- **Sidebar**: a plain list of thread titles, newest first. A thread shows the first line of your first
  message until the titles plugin names it (with the thread's model, or the one picked in Settings ▸
  Titles); threads are saved and come back after a restart (right-click ▸ Delete Thread removes one). A
  spinner shows while a thread works; a dot marks news you haven't seen (blue: reply, red: error,
  yellow: waiting for your input). ⌘B hides the sidebar.
- **Title bar**: sidebar toggle, back/forward through the threads you visited (⌘[ / ⌘]), new
  thread (⌘N), and the current thread's title and folder. Drag it to move the window.
- **Composer**: one row, **+** · text · send, while the text fits on one line. Longer text
  switches the card to two zones: the text across the full width (growing upward to ten lines,
  then scrolling) and **+** / send in a row below; it collapses again when the text fits. Enter
  sends, Shift+Enter adds a newline. While the agent works, send
  becomes Stop (`session/cancel`). **+** attaches files or folders (sent as ACP `resource_link`s);
  images (PNG, JPEG, GIF, WebP, HEIC, TIFF, BMP) from **+**, a drop on the card or ⌘V become thumbnails
  above the text instead and go to the agent as images (up to 20 per message; each is scaled to at most
  1568 px on its long side and stripped of metadata such as location first).
  Below the card: the project folder (switch it while the thread is empty, otherwise pick a folder
  to start a new thread there), the model and thinking effort (both switch the session through
  `_space/session/configure`; new threads start with the defaults from Settings), and a context ring
  (hover for token counts). Next to the folder, the **permission mode** (shield menu, ⇧⌘R cycles): Read Only
  (commands in the macOS sandbox, no edits), Workspace Write (the default) or Full access (asks to confirm
  once per thread). The modes come from the permissions plugin ([plugins/permissions](plugins/permissions/README.md)):
  in Workspace Write and Full access every tool call is first checked by a decision model (OpenAI; add the
  key in Settings, where it's checked before it's saved). Until the key works, a yellow pill above the input
  says why, with links (Open Settings, Retry now) and when it last tried and tries next, and
  sending is blocked. If the check stops working mid-turn, the tool call waits (**Paused**, never run
  unchecked), the thread shows a pause icon in the sidebar, and it goes on by itself once the check works
  again, the key changes, or you switch the thread to Read Only.
- **Transcript**: your messages as bubbles (their images above them: one shown whole, several as
  tiles; click one to view it large, ← / → for the others, Esc closes), replies as markdown, thinking collapsed as
  "Thought for Ns", tool calls as compact rows that expand to their output, permission cards
  (Allow/Reject). It follows new output while you're at the bottom; scroll up to read and it stays
  put while the agent keeps working, with a round ↓ button above the composer. The button, ⌘↓ or
  View ▸ Scroll to Bottom glide back down and follow again (sending a message jumps there too). In a
  composer with several lines of text, ⌘↓ moves the caret to the end instead.
- **Settings** (⌘, or Space ▸ Settings…): replaces the thread view, sidebar stays. *New threads*: default
  model and thinking level ("Agent default" leaves the agent's choice; existing threads don't change).
  Then each plugin's settings (*Alerts*: notify when an agent finishes, play a sound; *Titles*: the title
  model, "Same as agent" by default). *Notifications*:
  the macOS permission status (with a button to allow it or open System Settings). Esc, ⌘, again, ⌘[, ⌘N or clicking a thread leaves.
  Saved on every change to `~/.space/settings.json`.
- **Agent errors**: if the agent can't start or exits, the thread shows the error (with the last
  lines of its stderr) and a Restart button. Plugins' stderr isn't shown in the window; run Space
  from a terminal with `RUST_LOG=debug` to see it.

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
