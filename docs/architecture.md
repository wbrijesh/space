# Space architecture (v0)

Space is a native macOS development environment built around **projects**, **bots** and **knowledge**.
v0 delivers the foundation: a Rust/GPUI **harness** that hosts out-of-process **plugins**, and a first
**agent plugin** built on the pi SDK.

```
┌──────────────────────────── space (Rust) ────────────────────────────┐
│  space-app       GPUI UI: threads · transcript · composer · approvals│
│  space-core      plugin registry · process supervisor · sessions     │
│  space-protocol  manifest types · ACP re-exports · _space extensions │
└──────┬──────────────────────────────────────────────────────────────┘
       │ ACP: JSON-RPC 2.0, newline-delimited, over the plugin's stdio
┌──────▼─────────────────────────┐
│ plugins/pi-agent (Node, TS)    │
│ pi SDK session + claude-cli    │
└────────────────────────────────┘
```

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Plugin boundary | Out-of-process, one child process per plugin | Any language, crash isolation, sandboxable later |
| Wire protocol | [Agent Client Protocol](https://agentclientprotocol.com) (ACP) over stdio | Open standard with Rust + TS SDKs; Claude Code, Codex, Gemini already speak it |
| Space-specific calls | ACP extension methods/notifications prefixed `_space/` | Extends ACP without forking it |
| Core placement | `space-core` is a library used in-process by `space-app` | One binary for v0; everything crosses a protocol, so a daemon split later is packaging |
| Plugin runtime | Node (>= 24) | pi supports Node; dependencies pinned per plugin |
| Model access | pi + `@pi-plugins/claude-cli`, model `claude-cli/claude-opus-5-5` | Uses the user's signed-in `claude` CLI subscription; no API key |

## Plugin manifest

Every plugin is a directory containing `space-plugin.toml`:

```toml
id = "pi-agent"                 # unique, kebab-case
name = "Pi Agent"
version = "0.1.0"
kind = "agent"                  # agent | tool | source | knowledge (v0 implements agent)
description = "Coding agent on the pi SDK, using the Claude CLI subscription."

[run]
command = "node"                # resolved on PATH
args = ["dist/main.js"]         # relative paths resolve against the plugin directory
cwd = "plugin"                  # "plugin" (plugin dir) or "project" (session working dir); default "plugin"

[run.env]                       # optional extra environment
# FOO = "bar"
```

Discovery order (later wins on duplicate `id`):
1. `<repo>/plugins/*` (bundled, development)
2. `~/.space/plugins/*`
3. `<project>/.space/plugins/*`

## Protocol (v0 subset)

Framing: one JSON-RPC 2.0 message per line (LF) on the plugin's stdin/stdout. stderr is free-form logs,
captured by the harness. The harness is the ACP **client**; agent plugins are ACP **agents**.

Required ACP methods for an `agent` plugin:

| Direction | Method | Notes |
|---|---|---|
| client → agent | `initialize` | Negotiate protocol version and capabilities |
| client → agent | `session/new` | `cwd` = project directory; returns `sessionId` |
| client → agent | `session/prompt` | Text content blocks; resolves with `stopReason` when the turn settles |
| client → agent | `session/cancel` (notification) | Abort the running turn |
| agent → client | `session/update` (notification) | `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan` |
| agent → client | `session/request_permission` | Optional in v0; harness shows an approval card |

Space extensions (v0):

| Direction | Method | Payload |
|---|---|---|
| agent → client | `_space/status` (notification) | `SpaceStatus` (below): what the session runs on, what it could run on, context usage |
| client → agent | `_space/session/configure` (request) | `{ sessionId, provider?, model?, thinkingLevel? }` → the session's full `SpaceStatus` after the change |

`SpaceStatus` (every field but `sessionId` optional; the client merges partial updates):

```jsonc
{
  "sessionId": "…",
  "provider": "claude-cli",
  "model": "claude-opus-5-5",
  "thinkingLevel": "medium",
  "availableModels": [{ "provider": "claude-cli", "id": "claude-opus-5-5", "name": "Claude Opus 5.5" }, …],
  "thinkingLevels": ["low", "medium", "high", "xhigh", "max"],   // levels the current model supports
  "contextTokens": 18234,                                          // estimated tokens in context
  "contextWindow": 1000000
}
```

Agents send it after `session/new` and whenever it changes (pi-agent: after every turn). The app shows the
model and thinking level in the composer and switches them with `_space/session/configure`; omitted
fields are left unchanged, `provider` defaults to the current one, and an unsupported thinking level may
be clamped by the agent. Invalid requests fail with a JSON-RPC error (`-32602`). Prompts may carry
`resource_link` blocks (`file://…`) for files the user attached in the composer.

Tools run inside the agent (pi's built-in tools: read, bash, edit, write, …) in v0. The ACP `fs/*` and
`terminal/*` client capabilities are **not** advertised by the harness yet.

## Repository layout

```
space/
├─ Cargo.toml                 # Rust workspace
├─ .cargo/config.toml         # DEVELOPER_DIR → Xcode (Metal shaders for GPUI)
├─ crates/
│  ├─ space-protocol/         # manifest types, ACP re-exports, _space extension types
│  ├─ space-core/             # discovery, supervisor, ACP client connections, session state
│  └─ space-app/              # GPUI binary `Space`
├─ plugins/
│  └─ pi-agent/               # TypeScript ACP agent on the pi SDK
└─ docs/
```

## Harness implementation notes (v0)

- **GPUI version.** `space-app` depends on `gpui-kit = "0.9.0"`, renamed to `gpui` in Cargo.toml.
  gpui-kit re-exports `gpui-pre 0.3.11` (a crates.io snapshot of Zed's GPUI), its macOS platform and
  `gpui-component 0.9.0`, which provides the buttons, textarea and dark theme. `gpui-component` only builds
  against `gpui-pre`, not against a zed git rev or `gpui 0.2.2`. Using the kit gives us widgets plus a
  reproducible registry dependency instead of a full zed checkout. It builds cleanly on stable 1.98.
- **Runtime bridge.** `space-core` uses tokio, with `tokio::process` for children and
  `tokio-util` compat for ACP `ByteStreams`. The app owns a 2-worker tokio runtime and passes its
  handle to `Host::new`. Every async `space-core` method spawns its work onto that runtime, so GPUI tasks
  (`cx.spawn`) can await it directly. Host events go out on a tokio unbounded mpsc channel. A GPUI
  foreground task drains it in batches and folds each `SessionEvent` into a per-session `SessionState`
  (`SessionState::apply`).
- **ACP client.** This uses `agent-client-protocol 5.0.0`, the `Client.v1()` builder with typed handlers for
  `session/update`, `_space/status` (a `#[derive(JsonRpcNotification)]` struct in `space-protocol`)
  and `session/request_permission`. Permission responses are produced in a `cx.spawn`ed task, so the
  dispatch loop keeps running while the user decides. `session/cancel` answers any pending permission
  requests with `cancelled`.
- **Supervisor.** The process gets piped stdio and `kill_on_drop`. Each stderr line goes to a
  2000-line ring buffer and is also emitted as a `HostEvent::PluginLog`. On exit the harness reports
  `stopped` (killed or exit code 0) or `crashed` (exit code plus the last stderr line). Before spawning,
  any relative file argument in `run.args` must exist, which catches unbuilt plugins with a clear error.
- **App shell.** The window uses a transparent title bar (`appears_transparent`, traffic lights at
  18,17, `app_owns_titlebar_drag`) and draws its own 48px bar, which moves the window with
  `start_window_move`. The default agent plugin starts on launch. Each thread owns one ACP session;
  events that arrive before `session/new` returns are buffered and replayed. Colors, spacing and
  fonts are tokens in `space-ui` (`theme/`), which also maps them onto the gpui-component theme.
  Components live in `space-ui` and take props and callbacks only; `space-app/src/view.rs` wires
  them to app state. See [ui-development.md](ui-development.md).
- **Focus.** The root element tracks a focus handle, so clicking anything that is not focusable
  moves focus there and clears the composer's focused state. GPUI only moves focus on mouse-down
  into elements that track one, so without a sink the last focused control (or a keyboard focus
  ring) stayed active after clicking elsewhere. Clickable chrome is plain styled divs (no focus);
  dropdown triggers show a focus ring only after keyboard input, because popup menus hand focus back
  to their trigger when they close. Selected and hovered rows use clearly different fills.
- **Testing.** `space-core` ships a `space-stub-agent` binary, a scripted ACP agent with keywords
  `permission`, `slow` and `crash`. The integration tests in `crates/space-core/tests/stub_agent.rs` run against it.
