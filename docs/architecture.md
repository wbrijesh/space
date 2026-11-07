# Space architecture (v0)

Space is a native macOS development environment built around **projects**, **bots** and **knowledge**.
v0 delivers the foundation: a Rust/GPUI **harness** that hosts out-of-process **plugins**, a first
**agent plugin** built on the pi SDK, and a **permissions plugin** that guards every tool call.

```
┌──────────────────────────── space (Rust) ─────────────────────────────┐
│  space-app       GPUI UI: threads · transcript · composer · settings  │
│  space-core      registry · supervisor · sessions · tool pipeline     │
│  space-protocol  manifest · ACP re-exports · _space/* · service proto │
│  space-plugin    SDK for service plugins in Rust                      │
└──────┬──────────────────────────────────────────┬────────────────────┘
       │ ACP (JSON-RPC over stdio)                │ service protocol (JSON-RPC over stdio)
┌──────▼───────────────────────┐   ┌──────────────▼──────────────────────────┐
│ plugins/pi-agent (Node, TS)  │   │ plugins/permissions (Rust)               │
│ pi SDK + claude-cli          │   │ modes · macOS sandbox · decision model   │
└──────────────────────────────┘   └──────────────────────────────────────────┘
```

Everything a plugin adds goes through the host: agents ask the host before every tool call
(`_space/tools/preExecute`) and before spawning a command (`_space/sandbox/confine`); the host runs the
hook chain and routes seams to whichever plugin provides them. Agents and guards never talk directly.

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Plugin boundary | Out-of-process, one child process per plugin | Any language, crash isolation |
| Agent protocol | [Agent Client Protocol](https://agentclientprotocol.com) (ACP) over stdio | Open standard with Rust + TS SDKs |
| Service protocol | Plain JSON-RPC 2.0 over stdio, same framing ([`space_protocol::service`]) | Guards, catalogs and settings aren't agents |
| Space-specific calls | ACP extension methods prefixed `_space/` | Extends ACP without forking it |
| Composition | Plugins declare **capabilities** (`provides`) and **hooks**; the host routes by capability | Swap a provider without touching consumers (learned from DeepSeek Harness's seams) |
| Tool pipeline | Owned by the host, run for every agent | Every agent gets the same guards for free |
| Failing guards | Fail closed: the call is **held** until the guard can decide | A broken guard never lets a call through or turns into a prompt |
| Core placement | `space-core` is a library used in-process by `space-app` | One binary; everything crosses a protocol |
| Model access | pi + `@pi-plugins/claude-cli` | Uses the user's `claude` CLI subscription; no API key |

## Plugin manifest

Every plugin is a directory containing `space-plugin.toml`:

```toml
id = "permissions"              # unique, kebab-case
name = "Permissions"
version = "0.1.0"
description = "…"
provides = ["sandbox", "context", "settings", "catalog:permission-modes"]   # or ["agent"]
hooks = [{ event = "tools/pre-execute", order = 10 }]                      # lower order runs first
required = true                 # a guard Space can't run tools without (can't be turned off)

[run]
command = "${target}/space-permissions"   # ${target}: the cargo profile dir Space runs from
args = []                       # relative paths resolve against the plugin directory
cwd = "plugin"                  # "plugin" or "project"

[run.env]                       # optional extra environment
```

Capabilities: `agent` (an ACP agent; provides nothing else), `sandbox` (`sandbox/confine`), `context`
(hidden prompt sections), `settings` (fields on the Settings page), `catalog:<name>` (a list the UI renders,
e.g. `catalog:permission-modes` for the composer's mode menu). Hook events: `tools/pre-execute`.

Discovery order (later wins on duplicate `id`): `<repo>/plugins/*`, `~/.space/plugins/*`,
`<project>/.space/plugins/*`. Writing one: [plugins.md](plugins.md).

## Agent protocol (ACP subset)

The harness is the ACP **client**; agent plugins are ACP **agents**.

| Direction | Method | Notes |
|---|---|---|
| client → agent | `initialize` | Protocol version and capabilities |
| client → agent | `session/new` | `cwd` = the thread's folder |
| client → agent | `session/prompt` | Text and `resource_link` blocks; `_meta["space/context"]` carries hidden context sections |
| client → agent | `session/cancel` (notification) | Abort the running turn |
| agent → client | `session/update` (notification) | message/thought chunks, `tool_call`, `tool_call_update`, `plan` |
| agent → client | `session/request_permission` | The agent's own approval card |

Space extensions:

| Direction | Method | Payload |
|---|---|---|
| agent → client | `_space/status` (notification) | `SpaceStatus`: model, thinking level, what's available, context usage |
| client → agent | `_space/session/configure` | `{ sessionId, provider?, model?, thinkingLevel? }` → the full `SpaceStatus` |
| agent → client | `_space/tools/preExecute` | `{ sessionId, toolCallId, tool, kind, args, cwd? }` → `{ decision: allow \| deny, reason? }`, after the host's pipeline (may take as long as the call is held) |
| agent → client | `_space/sandbox/confine` | `{ sessionId, argv, cwd }` → `{ argv }` to spawn instead (unchanged without a `sandbox` provider) |

`SpaceStatus` (every field but `sessionId` optional; the client merges partial updates):

```jsonc
{ "sessionId": "…", "provider": "claude-cli", "model": "claude-opus-5-5", "thinkingLevel": "medium",
  "availableModels": [{ "provider": "claude-cli", "id": "claude-opus-5-5", "name": "Claude Opus 5.5" }],
  "thinkingLevels": ["low", "medium", "high", "xhigh", "max"], "contextTokens": 18234, "contextWindow": 1000000 }
```

## Service protocol

Service plugins speak [`space_protocol::service`]; the Rust SDK is `crates/space-plugin`.

The Rust types in `space-protocol` are the only definition of Space's protocol. TypeScript
plugins import bindings generated from them (`plugins/pi-agent/src/generated/protocol.ts`):
`just protocol` rewrites the file, and `cargo test` fails while it is stale. Both kinds of
plugin report `PROTOCOL_VERSION` when they start (agents in `initialize`'s
`_meta["space/protocolVersion"]`; a plain ACP agent leaves it out), and a plugin built for
another version is not started.

| Method | Direction | When |
|---|---|---|
| `plugin/initialize` | host → plugin | first, with every setting's value (and any unreadable secrets); the plugin answers with the `protocolVersion` it was built for (the SDK fills it in) and is refused on a mismatch |
| `settings/schema` | host → plugin | `provides settings`: the fields |
| `settings/validate` | host → plugin | before a new value is stored: `ok`, `invalid` (not stored) or `unverifiable` (stored only if the user saves anyway) |
| `settings/changed` | host → plugin | the user changed a setting |
| `catalog/get`, `catalog/changed` | both | a catalog's entries; the plugin says when they changed |
| `context/get` | host → plugin | hidden sections for the next prompt |
| `sandbox/confine` | host → plugin | the argv to spawn |
| `hooks/tools.preExecute` | host → plugin | before every tool call: `allow`, `deny`, `ask`, `next` or `unavailable` |
| `hooks/ready` | plugin → host | the plugin's answers may have changed: held calls run the chain again |
| `plugin/retry` | host → plugin | the user pressed a blocker's "Retry now" |

Catalog entries can carry a **blocker** (`ready: false`): text, links (`open-settings:<plugin>`,
`open-url:<url>`, `retry:<plugin>`, `use-mode:<entry>`) and the last/next attempt times. The composer
shows it as a pill above the input (times tick live) and won't send while the thread's mode has one.

## The tool pipeline

`_space/tools/preExecute` runs the `tools/pre-execute` chain (lowest `order` first) with the call, the
thread's permission mode, project root and history (the user's messages and earlier calls, from the
host's own records; never tool output).

- `deny` wins: the call fails with the reason (shown as **Blocked** on the tool row).
- `ask` is remembered; after the last listener the user gets an approval card with the reasons.
- `allow` / `next` pass the call on; with no listeners the call is allowed.
- `unavailable`, an error, a timeout, or a listener that isn't running **holds** the call (fail closed).
  The agent's request stays open; the tool row shows **Paused** with the reason, the sidebar row a pause
  icon, and a notification goes out if you aren't watching. The whole chain runs again (with fresh
  facts, so a mode change applies) when a listener sends `hooks/ready`, a service (re)starts, the
  thread's mode changes, the user presses Retry now, or every 10 s (`HostOptions::held_recheck`).
  Stopping the turn ends the wait. A listener that crashed 3 times in 5 minutes is reported as "keeps
  crashing".
- `required` plugins can't be turned off.

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
- **Follow scrolling.** The transcript and the logs panel use `space-ui`'s `FollowScrollArea`
  (`components/follow_scroll.rs`). It measures its content after layout and before the scroll
  container's prepaint, so it decides with this frame's max offset. Only input changes the offset
  between frames (it's measured from the top, so content arriving below leaves it alone), so any
  change is a user scroll: upward scrolling past 24px stops following, scrolling back within 24px
  of the bottom resumes it, and content growth never stops it. While following, it re-pins with
  `ScrollHandle::scroll_to_bottom`, which GPUI applies after layout, so streaming text never shows a
  frame off the bottom. The app only calls `jump_to_bottom` (thread opened or switched, message sent)
  and `glide_to_bottom` (⌘↓; the glide runs on the animation clock). ⌘↓ is bound at the root and in
  the composer's textarea (`Input && !ComposerMultiline`); a multi-line composer keeps it for
  moving the caret to the end.
- **Settings.** `space-app/src/settings.rs` loads `~/.space/settings.json` (`SPACE_SETTINGS` overrides;
  serde defaults, so partial or older files load; a corrupt file loads as defaults and is only replaced
  once the user changes something) and saves atomically on every change. The page (⌘,) is an in-window
  view, not a second window, so tiling window managers don't have to place it. Model and thinking
  choices come from the latest `_space/status` that listed them; when a new thread's `session/new`
  returns, its defaults go out as one `_space/session/configure` (on failure the agent's default stays,
  logged). The macOS permission status comes from `getNotificationSettingsWithCompletionHandler`,
  whose callback runs off the main thread and answers through a oneshot channel; it's refreshed when
  the page opens and whenever the window becomes active.
- **Plugin settings.** Secrets live in the macOS Keychain (`Secrets`, service `dev.brijesh.space`; tests
  use their own services), text values in the settings file. A save goes through `settings/validate`
  first; the field shows "Checking…", then the outcome, and "Save anyway" when the plugin couldn't check.
- **Testing.** `space-core` ships `space-stub-agent` (a scripted ACP agent) and `space-stub-guard` (a
  scripted service plugin: decisions, holds, crashes, catalogs, validation). `crates/space-core/tests/`
  run the host and pipeline against them; `plugins/permissions/tests/guardrail.rs` runs the real
  permissions plugin against a scripted local stand-in for OpenAI.

## Repository layout

```
space/
├─ crates/
│  ├─ space-protocol/   # manifest, ACP re-exports, _space extensions, service protocol
│  ├─ space-plugin/     # SDK for Rust service plugins
│  ├─ space-core/       # discovery, supervisor, ACP + service connections, pipeline, sessions
│  ├─ space-ui/         # components, tokens, gallery
│  └─ space-app/        # GPUI binary `Space`
├─ plugins/
│  ├─ pi-agent/         # TypeScript ACP agent on the pi SDK
│  └─ permissions/      # Rust service plugin: modes, sandbox, decision model
└─ docs/
```
