# claude-agent

Space's agent: an [ACP](https://agentclientprotocol.com) agent in TypeScript on the
[Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview), which drives the user's own,
signed-in `claude` CLI. Claude Code runs the agent loop, compaction and commands; requests draw on the
Claude subscription it's signed in to. There's no API key.

The `_space/*` types and method names come from `src/generated/protocol.ts`, generated from the Rust
types (`just protocol`; never edit it by hand). The contract with the harness is in
[`docs/architecture.md`](../../docs/architecture.md).

```
Space (ACP client) ──stdin/stdout NDJSON──▶ space-ts claude-agent (src/main.ts)
                                           ├─ session.ts   a session: turns, steering, hooks, status
                                           ├─ claude.ts    the only module importing the SDK
                                           │    one Claude Code process per session (streaming input)
                                           └─ trace.ts     `_space/trace` for the Trajectory view
```

## Claude Code as Space runs it

- **Isolated.** Nothing of the user's or the project's Claude Code setup is loaded: no `CLAUDE.md`
  or `AGENTS.md`, skills, hooks, MCP servers (claude.ai connectors included), plugins, permission
  rules or auto memory (`settingSources: []`, `skills: []`, `strictMcpConfig`,
  `autoMemoryEnabled: false`, `ENABLE_CLAUDEAI_MCP_SERVERS=false`).
- **Tools:** Bash, Read, Edit, Write, Task (subagents), WebFetch and WebSearch. Claude Code's other
  tools (cron jobs, notifications, worktrees, …) aren't offered.
- **On the subscription only.** A session starts only when Claude Code is signed in to a Claude
  subscription (`accountInfo()`), and a turn stops if Claude Code says it would use an API key
  (`apiKeySource`). Credential variables are removed from its environment.
- **The CLI:** the user's `claude` (on `PATH` or where its installers put it), at least
  `MIN_CLAUDE_VERSION`, the release the pinned SDK was built against (a test keeps them equal).
- **Nothing in the background** (`CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1`): a turn's work is done
  when it ends, and Claude never starts a turn on its own.
- **A dropped connection** is retried for about half an hour (`CLAUDE_CODE_MAX_RETRIES=60`); while it
  waits, `_space/retry` says so ("Connection lost · reconnecting…", "Claude is busy · retrying…").

## The contract

| Space | Claude Code |
|---|---|
| `session/new`, `session/load` | One process per session, with the ACP session id as Claude Code's session id; loading resumes the transcript Claude Code kept. |
| `session/prompt` | One user message (images first, then the text). The turn ends when Claude Code reports the session **idle** after running it (`session_state_changed`), which covers every request, a `/goal`'s continuations and a steered message that arrived as it ended. |
| Streaming | Text and thinking (summarized) deltas become message and thought chunks; tool_use and tool_result blocks become tool calls and their results (subagents' too). |
| `_space/tools/preExecute` | A `PreToolUse` hook on every call puts it to the host first; a deny, an error or a stopped turn blocks it. Claude Code's own permission prompt allows only calls the host allowed. |
| `_space/sandbox/confine` | The same hook rewrites a Bash command to the argv the host returns. |
| Hidden context | A `UserPromptSubmit` hook adds the prompt's `space/context` sections beside it. |
| `session/cancel` | Withdraw what's steered and unread, then `interrupt()`. A stopped turn also stops a `/goal` (a quiet `/goal clear`), or the next prompt would carry on with it. |
| `_space/session/steer` / `unsteer` | A user message with `priority: "next"`: Claude reads it once the running tool calls finish. `_space/steered` goes out when the request carrying it does. Taking one back (or stopping the turn) withdraws what Claude hasn't read. |
| `_space/session/configure` | `setModel`, and the effort level (Space's thinking level). |
| `_space/status` | The newest model of each family, the model's effort levels, context use, "Continue in Claude Code", and the commands. |
| `_space/complete` | A one-turn, tool-free query outside the session (thread titles). |
| `_space/session/btw` | A one-turn, tool-free query on a throwaway fork of the session. |

## Slash commands

`/compact`, `/context` and `/goal` are Claude Code's own: a prompt that is exactly the command (with
arguments) runs it; their output appears as the agent's reply (`/compact` says how far the context
shrank). Any other text starting with `/` is sent as written. `/btw` is answered on the side
(`_space/session/btw`); Claude Code doesn't run it headless.

## Develop

```bash
bun install             # from the repo root (a Bun workspace)
bun run typecheck       # tsc over src + scripts (part of `just lint`)
bun test src            # recorded Claude Code streams (src/fixtures) through the session and trace; no model calls
bun run smoke           # end to end through the real CLI (uses a little of the subscription)
SMOKE_NO_PROMPTS=1 bun scripts/smoke.ts   # only what makes no model request
```

The fixtures were recorded with `SPACE_CLAUDE_AGENT_RECORD=<dir> bun scripts/smoke.ts`, which writes
every message Claude Code sends to `<dir>/<session>.jsonl`. stdout carries only protocol records; logs
go to stderr with a `[claude-agent]` prefix. On stdin EOF or SIGTERM every session's Claude Code is
closed (its transcript stays, to resume).
