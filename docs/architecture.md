# Space architecture (v0)

Space is a native macOS development environment built around **projects**, **bots** and **knowledge**.
v0 delivers the foundation: a Rust/GPUI **harness** that hosts out-of-process **plugins**, and a first
**agent plugin** built on the pi SDK.

```
┌──────────────────────────── space (Rust) ────────────────────────────┐
│  space-app       GPUI UI: sidebar · chat · tool cards · approvals    │
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
| agent → client | `_space/status` (notification) | `{ sessionId, model, provider, thinkingLevel }` — shown in the UI header |

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
│  └─ space-app/              # GPUI binary `space`
├─ plugins/
│  └─ pi-agent/               # TypeScript ACP agent on the pi SDK
└─ docs/
```
