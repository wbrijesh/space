# pi-agent

The first Space plugin. It is an [ACP](https://agentclientprotocol.com) agent written in TypeScript on the
[pi SDK](https://www.npmjs.com/package/@earendil-works/pi-coding-agent). Model requests go through
[`@pi-plugins/claude-cli`](https://www.npmjs.com/package/@pi-plugins/claude-cli), which runs each request
via the signed-in Claude Code CLI (`claude --print`), so it uses your Claude subscription. You don't need
an API key.

The `_space/*` types and method names come from `src/generated/protocol.ts`, generated from the
Rust types (`just protocol`; never edit it by hand). The contract with the harness (manifest, ACP subset, `_space/status`) is in
[`docs/architecture.md`](../../docs/architecture.md).

## How it works

```
Space harness (ACP client) ──stdin/stdout NDJSON──▶ node dist/main.js
                                                   ├─ @agentclientprotocol/sdk  agent() app, ACP v1
                                                   ├─ pi AgentSessionRuntime per ACP session (in memory)
                                                   │    tools: read, bash, edit, write (run in-process),
                                                   │    each put to the host's tool pipeline first
                                                   └─ claude-cli provider ──▶ `claude --print` (subscription)
```

| ACP | Behaviour |
|---|---|
| `initialize` | Returns `protocolVersion: 1`, `loadSession: false`, `agentInfo {name: "pi-agent", version}`, and the Space protocol version in `_meta["space/protocolVersion"]` |
| `session/new {cwd}` | Creates an isolated pi session for `cwd`, then sends the `_space/status` notification (`provider`, `model`, `thinkingLevel`, `availableModels`, `thinkingLevels`, context usage). No ACP modes: permission modes belong to the host. |
| `session/prompt` | Joins the text blocks (other block types are described inline) and runs `session.prompt()`. Hidden context sections in `_meta["space/context"]` go into the run's system prompt. It streams pi events as `session/update` notifications and resolves with `end_turn`, `cancelled` or `max_tokens`. Provider errors become a JSON-RPC error (-32603) carrying pi's message, for example the claude-cli usage-credit stop. |
| `session/cancel` | `session.abort()`; the pending prompt resolves with `cancelled` |
| `_space/session/configure {sessionId, provider?, model?, thinkingLevel?}` | `session.setModel()` / `session.setThinkingLevel()` (session-only, pi clamps the level to the model), returns the new `_space/status` payload. Rejected while a prompt is running. |

`availableModels` lists the current provider's models (for claude-cli: pi's built-in Anthropic catalog),
without dated snapshot ids, grouped Opus / Sonnet / Haiku / Fable and newest first. `_space/status` is sent
again after every turn with the updated `contextTokens` / `contextWindow` from `session.getContextUsage()`.

How pi events map to `session/update`:

- `text_delta` → `agent_message_chunk`
- `thinking_delta` → `agent_thought_chunk`
- `tool_execution_start` → `tool_call`. It carries the title (`bash: ls -la`, `read: src/x.ts`), `status: in_progress`, `rawInput`, and a kind: read→`read`, edit/write→`edit`, bash→`execute`, grep/find/ls→`search`, anything else→`other`.
- `tool_execution_end` → `tool_call_update` with status `completed` or `failed`, plus the result text truncated to about 8k characters.

### The host's tool pipeline (`src/host.ts`)

pi-agent doesn't decide what it may do; Space does (see [`docs/architecture.md`](../../docs/architecture.md)).
An inline pi extension and a replacement bash tool connect pi to it:

- **Before every tool call** (`tool_call` handler): `_space/tools/preExecute {sessionId, toolCallId, tool,
  kind, args}`. The host runs its guard plugins and may ask the user; the answer is `allow` or `deny`. A
  denied call is blocked with the host's reason (the model sees it as the tool's error, e.g. "Blocked by
  guardrail: …"). If the request itself fails, the call is blocked too: nothing runs unchecked.
- **Before bash spawns** (custom `BashOperations` on a bash tool that replaces pi's): `_space/sandbox/confine
  {sessionId, argv: ["/bin/bash", "-c", command], cwd}`. An unchanged argv runs as pi normally would; a
  different one (e.g. wrapped in `sandbox-exec` for Read Only) runs exactly as returned. A failed request
  means the command doesn't run. pi's `spawnHook` can't be used: it's synchronous.
- **Hidden context**: sections from the prompt's `_meta["space/context"]` become pi system-prompt sections
  (`space_<id>`) for that run, through `before_agent_start`. They're never part of the user's message and
  never sent back over ACP; the previous run's sections are removed first.

`pnpm test-host` checks all of this on a real pi session with no model requests: pi-ai's faux provider plays
the model (including tool calls) and a fake host answers the requests.

### Isolation from your pi setup

- `PI_CODING_AGENT_DIR` is set to a private directory, `~/.space/pi-agent` by default. pi writes nothing to
  `~/.pi`, and in practice nothing at all, because every store is in memory.
- Settings are in memory (`SettingsManager.inMemory`) and credentials too (`InMemoryCredentialStore`). No
  `models.json` is read, and sessions are in memory (`SessionManager.inMemory`).
- Your configured pi packages, extensions, skills, prompt templates and themes are **not** loaded. This
  matters because your `~/.pi/agent/settings.json` may install `@pi-plugins/claude-cli` too. The provider
  is registered once, as an inline extension factory. Project context files such as `AGENTS.md` and
  `CLAUDE.md`, found by walking up from `cwd`, are still loaded.
- If `claude` cannot be found on `PATH`, for example when a GUI app starts the plugin with a minimal
  environment, the agent prepends `~/.local/bin` (or `~/.claude/local`, `/opt/homebrew/bin`,
  `/usr/local/bin`) to `PATH`.

stdout carries only protocol records. Logs go to stderr with a `[pi-agent]` prefix. When stdin reaches EOF,
or on SIGINT/SIGTERM, the agent disposes all sessions (stopping claude-cli's spare CLI processes) and exits 0.

## Run

```bash
pnpm install
pnpm build        # tsc → dist/
pnpm typecheck    # src + scripts
pnpm start        # node dist/main.js (speaks ACP on stdio)
pnpm smoke        # build, then run the end-to-end ACP client test (bills a few tiny requests)
pnpm test-host    # the host pipeline on a real pi session: preExecute, confine, context (no model requests)
SMOKE_NO_PROMPTS=1 node scripts/smoke.ts   # only handshake/status/configure checks, no model requests
```

Requires Node ≥ 24 and the Claude Code CLI, signed in to a Pro or Max plan (`claude auth login`).

The smoke test (`scripts/smoke.ts`) is an ACP client. It starts `node dist/main.js` and runs these checks:
initialize, `_space/status`, the "pong" prompt, a bash tool turn (`_space/tools/preExecute`, which it
allows, then `tool_call` + `tool_call_update` + `end_turn`), cancelling a turn after its first chunk (`cancelled`), and a clean exit on EOF. Set
`SMOKE_SKIP_CANCEL=1` to skip the cancel check.

## Environment variables

| Variable | Default | Meaning |
|---|---|---|
| `SPACE_PI_MODEL` | `claude-cli/claude-opus-5-5` | `provider/model-id`, e.g. `claude-cli/claude-sonnet-5-5` |
| `SPACE_PI_THINKING` | `medium` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max` (clamped to what the model supports) |
| `SPACE_PI_AGENT_DIR` | `~/.space/pi-agent` | Private pi config directory (never `~/.pi/agent`) |

The agent never sets `PI_CLAUDE_CLI_ALLOW_USAGE_CREDITS`. With it unset, claude-cli stops with an error
instead of charging usage credits, and that error reaches the client as a JSON-RPC error.
