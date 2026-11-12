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
Space harness (ACP client) ──stdin/stdout NDJSON──▶ space-ts pi-agent (src/main.ts)
                                                   ├─ @agentclientprotocol/sdk  agent() app, ACP v1
                                                   ├─ pi AgentSessionRuntime per ACP session (kept in the host's data folder)
                                                   │    tools: read, bash, edit, write (run in-process),
                                                   │    each put to the host's tool pipeline first
                                                   └─ claude-cli provider ──▶ `claude --print` (subscription)
```

| ACP | Behaviour |
|---|---|
| `initialize` | Returns `protocolVersion: 1`, `loadSession: true`, `agentInfo {name: "pi-agent", version}`, the Space protocol version in `_meta["space/protocolVersion"]`, and `_meta["space/steer"]: true` |
| `session/new {cwd}` | Creates an isolated pi session for `cwd`, kept in `_meta["space/dataDir"]` when the host gives a folder (else in memory), then sends the `_space/status` notification (`provider`, `model`, `thinkingLevel`, `availableModels`, `thinkingLevels`, context usage). No ACP modes: permission modes belong to the host. |
| `session/load {sessionId, cwd}` | Continues the pi session kept in `_meta["space/dataDir"]` (pi's own session file; never `~/.pi`) under the same id, then sends `_space/status`. Nothing is replayed: Space shows the conversation from its own saved thread. |
| `session/prompt` | Joins the text blocks (other block types are described inline) and runs `session.prompt()`. Hidden context sections in `_meta["space/context"]` go into the run's system prompt. It streams pi events as `session/update` notifications and resolves with `end_turn`, `cancelled` or `max_tokens`. Provider errors become a JSON-RPC error (-32603) carrying pi's message, for example the claude-cli usage-credit stop. |
| `session/cancel` | `session.abort()`; the pending prompt resolves with `cancelled` |
| `_space/complete {sessionId, prompt, provider?, model?}` | One `completeSimple` call on the session's model runtime (the session's model, or the one named; no tools, at most 256 tokens), outside the conversation; returns `{text}`. Space's titles plugin names threads with it. |
| `_space/session/steer {sessionId, id, text, images?, attachments?}` | `session.steer()`: pi delivers it after the current step. When it leaves pi's queue (`queue_update`), `_space/steered {sessionId, id}` goes out in order with the turn's updates. Rejected when no turn runs; still undelivered when the turn ends, it's dropped (Space sends it next). |
| `_space/session/unsteer {sessionId}` | `session.clearQueue()`; returns the ids it dropped |
| `_space/session/configure {sessionId, provider?, model?, thinkingLevel?}` | `session.setModel()` / `session.setThinkingLevel()` (session-only, pi clamps the level to the model), returns the new `_space/status` payload. Rejected while a prompt is running. |

`availableModels` lists the newest model of each family of the current provider (for claude-cli: Fable,
Opus, Sonnet, Haiku from pi's built-in Anthropic catalog, without dated snapshot ids), plus the
session's own model if it's an older one. `_space/status` is sent
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

`bun run test-host` checks all of this, and that a session kept in a folder loads again with its messages, on a real pi session with no model requests: pi-ai's faux provider plays
the model (including tool calls) and a fake host answers the requests.

### Traces (`src/trace.ts`)

For Space's Trajectory view, each session records what happened around every model request and sends
it as `_space/trace` notifications ([`TraceBatch`](src/generated/protocol.ts)). A hidden pi extension
reads pi's public hooks; nothing in pi or claude-cli is changed, and the request goes out exactly as pi
built it.

- `before_provider_request`: the payload as sent. A `header` (system prompt, tools, options) when it
  differs from the session's previous one, then a `request` with the turn, step and request numbers and
  the messages added since the previous request (all of them, `rewritten`, when the history changed).
  Tool results in messages become `{"toolResultRef": <call id>}` and images their sha256, type and path.
- `provider_stream_event`: `firstToken` and `chunks` (arrival times of text, thinking and tool-input
  chunks, as a first time and gaps).
- `message_end`: `response` with usage, duration, stop reason, response id, model, text and thinking.
- `tool_execution_start`/`_end`: the call's full input and output (cut at 256 KB, `truncated`).
- The prompt handler adds `turnStart`/`turnEnd` and `retry` (a retried request keeps its number).

Events go out every 100 ms while some are pending and at once at request, response, tool and turn
boundaries; the turn's answer waits until its trace was sent. Times are wall-clock ms measured with
`performance.now()`. Requests outside a turn (title completions) aren't recorded, and recording never
breaks a turn. `bun test` covers the helpers and the recorder fed hook events by hand.

Not visible from here: what the Claude CLI adds to the request (a billing header and identity line)
and the beta headers. A request's start includes starting the CLI when no spare process is waiting.

### A bad network (`src/resilient.ts`, `src/network.ts`)

Model requests wait out a dropped or flaky connection instead of failing the turn. The claude-cli
provider's stream function is wrapped where it's registered (pi and the plugin are unchanged):

- **Kinds of failure.** Sign-in, usage limit, invalid request and the like are shown at once.
  Connection errors (`ECONNRESET`, `fetch failed`, DNS, timeouts, a stream that ended early),
  Claude busy (429, 529, overloaded) and unknown errors are retried.
- **Offline or not** is decided by a TLS handshake with `api.anthropic.com` (the proxy, with
  `HTTPS_PROXY`), made only after a failure. Offline, the next request waits until it gets
  through (every 2 to 5 s); after a flap, until three in a row do. Online, it backs off 1 to 15 s
  (busy: 5 to 60 s).
- **Before content** (nothing streamed yet), a failed request is retried inside the wrapper. **After
  content**, pi's own retry drops the partial reply and asks again; `_space/retry {discard}` takes it
  out of the transcript.
- **A silent stream** is cut after 45 s if Claude can't be reached, and after 150 s regardless.
- **Giving up**: after 30 minutes of failing, or 6 unknown errors in a row. Stop ends any wait.
- While a request waits, `_space/retry {state}` tells Space: "Waiting for the network…",
  "Connection lost · reconnecting (attempt n)…" or "Claude is busy · retrying (attempt n)…".

pi's retry runs with a large budget and a 250 ms delay; the wrapper switches it on or off for each
error it hands back (`setRetryEnabled`), so pi retries exactly what the wrapper decided to.

### Isolation from your pi setup

- `PI_CODING_AGENT_DIR` is set to a private directory, `~/.space/pi-agent` by default. pi writes nothing to
  `~/.pi`, and in practice nothing at all: the only files are sessions, in the folder the host gives each
  one (`~/.space/threads/<thread>/agent/`, deleted with the thread).
- Settings are in memory (`SettingsManager.inMemory`) and credentials too (`InMemoryCredentialStore`). No
  `models.json` is read. Sessions without a host folder are in memory (`SessionManager.inMemory`).
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

Space runs it from one Bun executable shared by every bundled TypeScript plugin: `just build`
compiles `plugins/space-ts.ts` into `target/debug/space-ts`, and the manifest runs
`${target}/space-ts pi-agent` (inside the installed app, `Contents/Helpers/space-ts`). Bun runs the
TypeScript directly; there's no `dist/`.

```bash
bun install                  # from the repo root (a Bun workspace)
bun run typecheck            # tsc --noEmit over src + scripts (part of `just lint`)
bun src/main.ts              # speaks ACP on stdio
bun test src                 # the trace recorder (no model requests)
bun run test-host            # the host pipeline on a real pi session: preExecute, confine, context (no model requests)
bun run smoke                # the end-to-end ACP client test (bills a few tiny requests)
SMOKE_NO_PROMPTS=1 bun scripts/smoke.ts   # only handshake/status/configure checks, no model requests
```

Requires Bun and the Claude Code CLI, signed in to a Pro or Max plan (`claude auth login`).

The smoke test (`scripts/smoke.ts`) is an ACP client. It starts `bun src/main.ts` and runs these checks:
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
