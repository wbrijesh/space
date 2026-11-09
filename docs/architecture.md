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
│ plugins/pi-agent (TS, Bun)   │   │ plugins/permissions (Rust)               │
│ pi SDK + claude-cli          │   │ modes · macOS sandbox · decision model   │
└──────────────────────────────┘   │ plugins/alerts · session-store · titles  │
                                   └──────────────────────────────────────────┘
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
requires = []                   # capabilities another plugin must provide (pi-agent: ["sandbox"])
observes = []                   # events a service plugin is told about (e.g. "turn/ended")
uses = ["read:transcript"]      # host services it may call, and read:transcript
env = []                        # variables of Space's environment it gets (pi-agent: ["SSH_AUTH_SOCK", …])

[run]
command = "${target}/space-permissions"   # ${target}: the cargo profile dir Space runs from
args = []                       # relative paths resolve against the plugin directory
cwd = "plugin"                  # "plugin" or "project"

[run.env]                       # optional variables to set
```

Capabilities: `agent` (an ACP agent; provides nothing else), `sandbox` (`sandbox/confine`), `context`
(hidden prompt sections), `settings` (fields on the Settings page), `session-store` (saved threads; must
also use `read:transcript`), `session-title` (thread titles; must also use `read:transcript`),
`trajectory` (the Trajectory tab; must observe `trace`), `catalog:<name>` (a list the UI renders, e.g.
`catalog:permission-modes` for the composer's mode menu). Hook events: `tools/pre-execute`.

`requires` names capabilities (`sandbox`, `context`, `catalog:<name>`), not plugin ids, so any provider
satisfies it. A plugin starts once a provider of each runs. With no provider, or with requirements that
lead back to itself, it isn't started and its status says why ("Needs a sandbox provider."). When a
provider is turned off, the plugins that need it stop first ("Waiting for …") and start again once it runs
(the app restarts its agent). A provider that crashes or is restarted is only gone for a moment: what needs
it keeps running (a turn isn't lost), tool calls its hooks would decide are held, and commands it would
confine fail closed; a crashed provider is started again after the restart backoff. The exit of a process
that was already replaced changes nothing. When Space quits, agents stop first, then services, each after
its dependents.

Discovery order: the bundled plugins (`<repo>/plugins/*` when developing; inside an installed
`Space.app`, `Contents/Resources/plugins/*`), then `~/.space/plugins/*`, `<project>/.space/plugins/*`.
A bundled plugin's manifest runs `${target}/<binary>`: `target/<profile>` when developing,
`Contents/Helpers` in an installed app, so one manifest works in both. Rust plugins are their own
binaries; every bundled TypeScript plugin runs from one Bun executable, `space-ts <plugin id>`
(`plugins/space-ts.ts`, compiled by `just build`). An app is "installed" when it runs from a
`….app/Contents/MacOS/` outside any `target/` folder. A project
plugin replaces a user plugin with the same `id`; neither may use the `id` of a bundled plugin (it's
refused, so it can't inherit that plugin's settings, secrets or trust). Writing one: [plugins.md](plugins.md).

**Permissions.** A plugin gets only what its manifest declares:
- the hooks it lists and the events it `observes`;
- the host services it `uses` (`host/notify`, `host/open-url`, `agent/complete`, `sessions/read`). Any other
  call is answered "not permitted" (`-32003`).
- with `read:transcript`, what the user and the agent wrote. Without it, payloads keep ids and metadata only:
  a hook listener's history keeps each user message as an item, with its text emptied.
- with `read:trace` (required to observe `trace`), everything sent to and from the model, file contents
  included.
- its own settings and secrets;
- a clean environment: `PATH`, `HOME`, `USER`, `TMPDIR`, `LANG` and `SHELL`, the variables it names in `env`,
  `[run.env]`, and `SPACE_PLUGIN_DIR`/`SPACE_PROJECT_DIR`.

Bundled plugins (`<repo>/plugins`, the app's own, or `SPACE_PLUGIN_DIRS`) are trusted. Plugins from `~/.space/plugins`
and `<project>/.space/plugins` don't start until you approve them in Settings → Plugins, which lists their
permissions in plain words. Until then they count as not installed, and the composer shows "… needs your
approval" and won't send.

Answers are saved in `plugin-grants.json` in Space's folder (the settings file's, `~/.space`), by plugin directory, with its
`id` and the permissions you answered for (a changed `id` asks again). A plugin that later asks for more asks again; one that asks for less keeps
your answer.

This governs only what a plugin can do through Space's channels. Plugins are unsandboxed programs running
as you, with your access to files and the network: one could read `~/.space` (saved threads, approvals)
directly. Approve only plugins you trust.

## Agent protocol (ACP subset)

The harness is the ACP **client**; agent plugins are ACP **agents**.

| Direction | Method | Notes |
|---|---|---|
| client → agent | `initialize` | Protocol version and capabilities |
| client → agent | `session/new` | `cwd` = the thread's folder; `_meta["space/dataDir"]` = a folder for the session's own files, when a session store is installed |
| client → agent | `session/load` | Continue a saved thread's session (agents with `loadSession`), same `_meta`; what the agent replays meanwhile isn't shown (the thread already is) |
| client → agent | `session/prompt` | Image blocks (base64 bytes plus the saved file's `file://` uri; only to agents with `promptCapabilities.image`), then text and `resource_link` blocks; `_meta["space/context"]` carries hidden context sections |
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
| client → agent | `_space/complete` | `{ sessionId, prompt, provider?, model? }` → `{ text }`: one short, tool-free completion by the session's model (or the one named), outside the conversation. The host forwards plugins' `agent/complete` here |
| agent → client | `_space/trace` (notification) | `{ sessionId, events: TraceEvent[] }`: what happened around each model request (headers on change, requests with their new messages, first token, chunk times, responses with usage, retries, tool start/end); see Traces below |

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
| `events/observe` | host → plugin | an event the plugin `observes`, one at a time per plugin; see below |
| `host/notify`, `host/open-url` | plugin → host | host services the plugin `uses`; the app carries them out |
| `agent/complete` | plugin → host | a host service: `_space/complete` on the agent of the given session |
| `title/generate` | host → plugin | `provides session-title`: `{ sessionId, firstMessage, projectRoot }` → `{ title }`; see below |
| `store/list`, `store/load`, `store/write`, `store/delete`, `store/data-dir` | host → plugin | `provides session-store`: saved threads; see below |
| `trace/record` | host → plugin | `observes trace` (with `read:trace`): every trace batch, `{ threadId, sessionId, events }`, in order; none is dropped |
| `trajectory/read`, `trajectory/detail`, `trajectory/stats` | host → plugin | `provides trajectory`: a page of a thread's rows (from the end, or matching a query), one row's full content, the thread's totals |
| `trajectory/changed` | plugin → host | `provides trajectory`: a thread's trajectory changed (`{ threadId, version }`); the app reads it again |

**Observed events** (`session/created`, `turn/started`, `turn/ended`, `tool/held`, `tool/result`,
`thread/titled`, `thread/deleted`) come from the host's own session events. Each observing plugin has a bounded queue
(256) drained by its own task, which sends one event and waits for the answer (10 s at most) before the
next, so events arrive in order and nothing else waits: a slow plugin only drops its own newest events,
with a warning. `turn/ended` and `tool/held` say whether the user is `watching` the thread (the app tells
the host which session is on screen while Space is frontmost).

**Saved threads** go to the `session-store` provider (bundled: `plugins/session-store`, which keeps
`~/.space/threads/<thread>/` with `meta.json`, an append-only `events.jsonl`, `agent/`, the session's
own files, and `attachments/`, the images sent in the thread; `store/data-dir` names both folders).
Images are normalized before they're sent (oriented, 8-bit sRGB, at most 1568 px on the long side,
metadata stripped; JPEG, or PNG when they have transparency), written there as `<sha256>.<ext>` before
the message that refers to them, and the message event keeps only their path, type and size. `plugin/initialize` tells every plugin Space's folder (`spaceDir`; absent in tests and
scenarios, when nothing is kept). A thread is its summary (`ThreadMeta`: title, project, mode, model,
agent and session, times) and its session events, which the store keeps without reading them. The app
saves the user's messages, finished agent messages and thoughts (chunks merged), tool calls and
results, permission and guardrail decisions, status changes and turn starts and ends, not held calls:
when a turn ends, when the thread's mode changes, and on quit (up to 2 s). One save at a time per
thread; one asked for meanwhile follows it. Each write says where its events start (`first`), so the store
skips events it already has (a failed save that had written them, a quit save repeating one in flight),
and an older summary never replaces a newer one. A thread deleted during a save is deleted after it. A
failed save keeps the events for the next one and says so in the banner; the conversation goes on. A
thread that fails to open is read again when next opened, and nothing is sent into it meanwhile. At launch the sidebar lists
saved threads; opening one reads its events and replays them through `SessionState::apply` (a turn cut
short ends as cancelled, so no card waits). Writing in it loads its session again (`session/load`) when
the agent can; otherwise the composer offers "Continue in new session". Right-click → Delete Thread
removes the folder. Without a store, threads live in memory. A reloaded session's guards start with what
the thread shows (`Session::seed_history`: its messages, and its tool calls by title), so their first
judgements have the same context as later ones.

**Thread titles.** A thread shows the first line of its first message at once. When that message is sent,
the host asks the `session-title` provider (bundled: `plugins/titles`) for a title; it answers through
`agent/complete`, with the thread's own model unless its "Title model" setting picks one, and the reply
is tidied to a few words. A title replaces the first line, is saved (`ThreadMeta.titled`) and told to
observers (`thread/titled`); a failure, or no title within 15 s, keeps the first line.

**Traces.** Agents send `_space/trace` batches as they work. The host adds what only it knows: the
user's message (`prompt`) and the hidden context it sent (`context`, with each section's plugin) before
the prompt goes out, and the tool pipeline's verdict on each call (`guard`: mode, deciding plugin,
reason, how long it held the call). Sessions are created with their thread id, so each batch reaches
plugins as `trace/record { threadId, sessionId, events }`. Every plugin that observes `trace` has its own
unbounded queue, drained one call at a time (30 s at most each), so batches arrive in order and none is
dropped. The `trajectory` provider (bundled: `plugins/trajectory`, keeping
`~/.space/trajectory/<thread>/<session>.jsonl`) turns them into rows (SYSTEM, USER, CONTEXT, ASSISTANT,
TOOL), numbered requests and totals, answers the app's Trajectory tab (`trajectory/read` pages from the
end, `trajectory/detail`, `trajectory/stats`), says when a thread changed (`trajectory/changed`, at most
every 200 ms; the app gets `HostEvent::TrajectoryChanged`), and drops a thread's trace on
`thread/deleted`.

Catalog entries can carry a **blocker** (`ready: false`): text, links (`open-settings:<plugin>`,
`open-url:<url>`, `retry:<plugin>`, `use-mode:<entry>`, `new-session:<thread>`) and the last/next attempt times. The composer
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
  icon, and the alerts plugin (`tool/held`) posts a notification if you aren't watching. The whole chain runs again (with fresh
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
  `start_window_move`. The default agent plugin starts on launch. Each thread owns one ACP session
  (a saved thread's is loaded when you write in it); events that arrive before `session/new` returns
  are buffered and replayed. Saving and reopening threads is `space-app/src/persist.rs`. Colors, spacing and
  fonts are tokens in `space-ui` (`theme/`), which also maps them onto the gpui-component theme.
  Components live in `space-ui` and take props and callbacks only; `space-app/src/view.rs` wires
  them to app state. See [ui-development.md](ui-development.md).
- **Focus.** The root element tracks a focus handle, so clicking anything that is not focusable
  moves focus there and clears the composer's focused state. GPUI only moves focus on mouse-down
  into elements that track one, so without a sink the last focused control (or a keyboard focus
  ring) stayed active after clicking elsewhere. Clickable chrome is plain styled divs (no focus);
  dropdown triggers show a focus ring only after keyboard input, because popup menus hand focus back
  to their trigger when they close. Selected and hovered rows use clearly different fills.
- **Follow scrolling.** The transcript uses `space-ui`'s `FollowScrollArea`
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
  use their own services), text, switch and model values (`provider/model`, picked from the agent's
models with "Same as agent" first) in the settings file (older files' `notifications`
  switches move to the alerts plugin's settings on load). A save goes through `settings/validate`
  first; the field shows "Checking…", then the outcome, and "Save anyway" when the plugin couldn't check.
- **Testing.** `space-core` ships `space-stub-agent` (a scripted ACP agent) and `space-stub-guard` (a
  scripted service plugin: decisions, holds, crashes, catalogs, validation). `crates/space-core/tests/`
  run the host and pipeline against them; `plugins/permissions/tests/guardrail.rs` runs the real
  permissions plugin against a scripted local stand-in for OpenAI, and `plugins/session-store/tests/`
  saves, restarts and continues a thread with the real store and the stub agent, and
  `plugins/titles/tests/` names threads through the stub agent's `_space/complete`.

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
│  ├─ permissions/      # Rust service plugin: modes, sandbox, decision model
│  ├─ alerts/           # Rust service plugin: a notification or sound when a turn ends or pauses
│  ├─ session-store/    # Rust service plugin: saved threads in ~/.space/threads
│  ├─ titles/           # Rust service plugin: thread titles from the thread's agent model
│  └─ trajectory/       # Rust service plugin: every thread's trace, for the Trajectory tab
└─ docs/
```
