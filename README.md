# Space

A native macOS development environment built around projects, bots and knowledge. v0 is the
foundation: a Rust/[GPUI](https://gpui.rs) **harness** that hosts out-of-process **plugins** over the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP), plus a first agent plugin built on the
pi SDK. See [docs/architecture.md](docs/architecture.md) for the contract (manifest, discovery, ACP subset,
`_space/*` extensions).

```
crates/space-protocol   manifest types (space-plugin.toml), _space/* extension types, ACP re-exports
crates/space-core       discovery · process supervisor · ACP client · session model (no UI deps)
crates/space-app        GPUI desktop app, binary `space`
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
| `just run [DIR]` | open the app on a project directory (default: this repo) |
| `just demo "Reply with exactly: pong"` | open the app, auto-start the first agent and a session, send the prompt |
| `just test` | all Rust tests; they use the scripted `space-stub-agent`, so they make no model calls |
| `just lint` | clippy with `-D warnings` |
| `just smoke ["PROMPT"]` | headless end-to-end run against pi-agent, printing events (billed) |
| `just smoke-stub` | the same headless run against the stub agent (free) |

Without just, run `cargo run -p space-app -- [PROJECT_DIR]` or
`cargo run -p space-app -- --smoke "PROMPT" [--plugin ID] [--allow] [PROJECT_DIR]`.

Environment:

- `SPACE_PLUGIN_DIRS=dir1:dir2` replaces the bundled plugin roots (`<repo>/plugins`).
  `~/.space/plugins` and `<project>/.space/plugins` are still scanned.
- `SPACE_AUTOSTART=1` starts the first agent plugin and a session when the app launches.
  `SPACE_AUTOPROMPT="…"` also sends that prompt.
- `RUST_LOG=space_core=debug` turns on logging (stderr).

## Using the app

The left sidebar lists the discovered plugins, each with its kind and status
(stopped / starting / running / crashed). **Start** launches an agent and opens a session in the project
directory; **New session** opens another one. Sessions appear under their plugin. The header shows the
provider, model and thinking level the agent reports through `_space/status`. The transcript shows
streamed replies, collapsible "Thinking" blocks, tool-call cards and permission cards (Allow/Reject).
Enter sends a message, Shift+Enter adds a newline, and **Stop** sends `session/cancel`.
**Show plugin logs** opens the agent's stderr.

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
