# Space

A native macOS app for working with coding agents. A Rust/[GPUI](https://gpui.rs) harness runs
everything else as out-of-process **plugins**: the agent (over the
[Agent Client Protocol](https://agentclientprotocol.com)), guards, saved threads, titles, traces and the
terminal. How it fits together: [docs/architecture.md](docs/architecture.md).

## Install

```
brew install --cask wbrijesh/tools/space                      # install
brew update && brew upgrade --cask wbrijesh/tools/space       # update
```

Apple Silicon, macOS 13 or later. Space uses your Claude subscription through the Claude CLI: install it
(`brew install --cask claude-code`) and run `claude` once to sign in. The app isn't notarized yet; the
cask clears macOS's download mark so it opens normally.

## Using Space

| | |
|---|---|
| **Threads** | ⌘N new · ⌘W close · ⌘R rename · right-click a row to pin it · ⌘[ / ⌘] back / forward · ⌘⌥↑ / ⌘⌥↓ previous / next · hold ⌘ and type a row's number to jump · ⌘B sidebar |
| **Composer** | ⏎ send · ⇧⏎ newline · ↑ / ↓ your earlier messages · ⌘L focus · ⇧⌘R next permission mode · Esc stops the agent · a paste over 4,096 characters becomes a "Pasted text" attachment (click to read it) |
| **While the agent works** | ⏎ queues a message for when it's done · ⌘⏎ sends it into the running turn, after the current step |
| **Transcript** | ⌘F find · ⌘G / ⇧⌘G next / previous · ⌘↓ scroll to the bottom · ⇧⌘T Trajectory |
| **Terminal** | ⌘J (or ⌃\`) show / hide · ⌘T new tab · ⌘W close tab · ⌃Tab next tab · ⇧⌘⏎ maximize · ⌘K clear |
| **App** | ⌘, Settings · ⌘Q quit (asks while a terminal runs something) |

- **Permission modes** (the shield under the composer): Read Only, Workspace Write (default) or Full
  access. Outside Read Only, a guardrail model checks every tool call and asks you when it isn't sure;
  it needs an OpenAI key in Settings. See [plugins/permissions](plugins/permissions/README.md).
- **Closing** a thread keeps it: a new, empty thread in the same folder offers it under **Resume**.
  Closed threads are deleted after 30 days (Settings ▸ Threads).
- **Titles** come from the agent's model after your first message. A name you give (⌘R) stays.
- **The sidebar** groups threads under Pinned, Today, Yesterday, then one header per earlier day
  (by when each was created; writing in one doesn't move it). Pinned threads keep their pin while
  closed.
- **Folders.** Each thread works in one folder, picked under the composer before its first message.
  Opened from Spotlight or the Dock, Space starts where you worked last.
- **Terminals** belong to a thread and start in its folder; they keep running while hidden.
- **Trajectory** shows every model request and tool call of a thread
  ([plugins/trajectory](plugins/trajectory/README.md)).
- The agent sleeps after 30 minutes with nothing to do and wakes when you type.
- Threads, settings and traces are kept in `~/.space`.

## Developing

Needs Xcode with its Metal toolchain (`xcodebuild -downloadComponent MetalToolchain` if
`xcrun metal --version` fails), Rust stable, [Bun](https://bun.sh), [`just`](https://github.com/casey/just),
dx at GPUI's version (`cargo install dioxus-cli --version 0.7.10 --locked`), and the `claude` CLI signed in.

**`just run`** builds everything and starts Space with live reload: saving a Rust file patches the
running app (struct fields or `main` need `r` in the dx window), and a changed plugin is rebuilt and
restarted. Only one Space runs at a time: quit the installed one first, or `just run`'s exits at once.
Arguments go to the app: a project folder, `--gallery`, or `--scenario list`.

| | |
|---|---|
| `just run [ARGS]` | build everything and run Space |
| `just test` | Rust tests and the TypeScript plugins' tests; a scripted stub agent, so no model calls |
| `just lint` | clippy (with and without dev tooling) and the TypeScript typecheck |
| `just protocol` | regenerate the TypeScript protocol bindings from the Rust types |
| `just install` | release build into `~/Applications/Space.app` (Spotlight) |
| `just release-check` | release build, then check every plugin starts from a copy of the app |
| `just publish` | release-check, tag, GitHub release, update the cask; needs `packaging/release-notes/<version>.md` |
| `just smoke` / `just smoke-stub` | headless end-to-end run against the real agent (billed) / the stub |

Environment: `SPACE_AGENT=<id>` picks the agent plugin, `SPACE_PLUGIN_DIRS=a:b` replaces the bundled
plugin folders, `SPACE_SETTINGS=<file>` another settings file, `SPACE_NOTIFY=0` no notifications,
`SPACE_AUTOPROMPT="…"` sends a first prompt, `RUST_LOG=space_core=debug` logs. Dev-only ones:
[docs/ui-development.md](docs/ui-development.md).

```
crates/space-protocol   the protocol: manifest, ACP extensions, service and terminal methods
crates/space-core       discovery, supervisor, ACP client, tool pipeline, sessions (no UI)
crates/space-plugin     SDK for Rust service plugins
crates/space-terminal   a terminal's screen (alacritty's parser), keys and mouse to bytes
crates/space-ui         tokens, components, previews and gallery
crates/space-app        the app (binary `Space`)
plugins/                pi-agent (TypeScript) · permissions · alerts · session-store · titles ·
                        trajectory · terminal; each has a README
vendor/gpui-base        patched copy of a GPUI dependency (SPACE_PATCH.md)
packaging/, scripts/    release, cask, notices, app icon (scripts/make_app_icon.py)
```

More: [writing a plugin](docs/plugins.md) · [working on the UI](docs/ui-development.md).
