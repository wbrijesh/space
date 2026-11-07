# Writing a plugin

A plugin is a directory with a `space-plugin.toml` and a program Space starts as a child process,
talking JSON-RPC 2.0 over its stdin/stdout (one message per line; stderr is your log, shown in the logs
panel). There are two kinds:

- **Agents** (`provides = ["agent"]`) speak ACP. See [architecture.md](architecture.md#agent-protocol-acp-subset)
  and `plugins/pi-agent` for a full one.
- **Service plugins** provide anything else: guards, sandboxes, catalogs, settings, hidden context.
  This guide is about them.

Drop the directory into `<repo>/plugins`, `~/.space/plugins` or `<project>/.space/plugins`. It starts with
the app and is restarted on next use if it exits.

## 1. The manifest

```toml
id = "audit"
name = "Audit log"
version = "0.1.0"
description = "Writes every tool call to a file."
provides = ["settings"]                                # what you implement (at least one capability)
hooks = [{ event = "tools/pre-execute", order = 50 }]  # chains you join; lower order runs first

[run]
command = "${target}/space-audit"   # or any executable; relative args resolve against the plugin dir
```

| `provides` | You answer | The host |
|---|---|---|
| `settings` | `settings/schema`, `settings/validate` (optional) | shows the fields; stores text in the settings file and secrets in the Keychain; sends `settings/changed` |
| `catalog:<name>` | `catalog/get` | caches the entries; refetches on your `catalog/changed`; the UI renders them (`permission-modes`: the composer's mode menu) |
| `context` | `context/get` | adds your sections to every prompt, hidden from the transcript |
| `sandbox` | `sandbox/confine` | gives agents your argv to spawn (one provider at a time) |

Hook `tools/pre-execute`: answer `hooks/tools.preExecute` for every tool call of every agent.

`required = true` marks a guard Space mustn't run tools without: it can't be turned off, and while it
isn't running, tool calls wait for it.

## 2. In Rust, with `space-plugin`

```rust
use space_plugin::{Plugin, RpcError, protocol::service::*};

struct Audit;

impl Plugin for Audit {
    async fn pre_execute(&self, call: HookCall) -> Result<HookDecision, RpcError> {
        eprintln!("{} {} in {}", call.call.tool, call.call.args, call.mode);
        Ok(HookDecision::next()) // pass it on
    }
}

fn main() {
    space_plugin::run(Audit);
}
```

Every [`Plugin`] method has a default, so implement only what your manifest declares. Keep the
[`Host`] from `initialize` to notify the host (`catalog_changed`, `hooks_ready`).
`space_plugin::testing::connect` runs a plugin in-process for unit tests.

## 3. Answering a tool call

`hooks/tools.preExecute` gets the pending call (`tool`, `kind`, `args`, `cwd`), the thread's permission
`mode`, the `projectRoot`, and `history`: the user's messages and earlier calls, from the host's own
records (never tool output). Answer:

| Decision | Effect |
|---|---|
| `next` / `allow` | pass the call on to the next listener |
| `deny` + reason | the call fails with the reason; nothing after you can change that |
| `ask` + reason | the user is asked after the last listener (unless a later one denies) |
| `unavailable` + reason | you can't decide right now: the call is **held** (the agent waits, the UI shows Paused with your reason) until you send `hooks/ready`, the mode changes, the user stops the turn, or the host's periodic recheck |

Errors and timeouts (15 s) count as `unavailable`: Space fails closed, so a broken guard pauses work
instead of letting it through. When you're `unavailable` because of something the user must fix, put it
in a catalog entry's **blocker** too (text, links, `lastAttemptMs`/`nextAttemptMs`), so the composer
says what's wrong and when you'll try again; send `catalog/changed` and `hooks/ready` whenever that
changes. "Retry now" on your blocker (`retry:<your id>`) arrives as `plugin/retry`.

## 4. Settings

Declare fields in `settings/schema` (`kind: "secret"` for keys). Before storing a new value the host
calls `settings/validate`: answer `ok`, `invalid` (with why; nothing is stored) or `unverifiable` (you
couldn't check; the user can save anyway). New values arrive as `settings/changed`; secrets the Keychain
wouldn't hand over are listed in `unreadable`.

## 5. Testing

`crates/space-core/src/bin/stub_guard.rs` is a scripted service plugin used by the host's tests: a
good template. `plugins/permissions/tests/guardrail.rs` shows an end-to-end test of a real plugin with
the stub agent and a fake upstream server. Use your own Keychain service name in tests
(`Secrets::new("…test…")`), never `dev.brijesh.space`.
