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

Apple Silicon, macOS 13 or later. Space runs Claude Code on your Claude subscription: install the
Claude CLI (`brew install --cask claude-code`) and run `claude` once to sign in. The app isn't notarized yet; the
cask clears macOS's download mark so it opens normally.

## Using Space

| | |
|---|---|
| **Threads** | ⌘N new · ⌘W close · ⌘R rename · ⇧⌘P pin / unpin (or right-click a row) · ⌘O open its folder in the app used last · ⌘[ / ⌘] back / forward · ⌘⌥↑ / ⌘⌥↓ previous / next · hold ⌘ and type a row's number to jump (the numbers show after 0.3 s of holding) · ⌘B sidebar |
| **Composer** | ⏎ send · ⇧⏎ newline · ↑ / ↓ your earlier messages · `/` the agent's commands (↑ / ↓ pick, Tab completes, ⏎ runs, Esc closes) · ⌘L focus · Esc stops the agent · a paste over 4,096 characters becomes a "Pasted text" attachment (click to read it) · a quote from the transcript sits above the input (× removes it) |
| **While the agent works** | ⏎ queues a message for when it's done · ⌘⏎ sends it into the running turn, after the current step |
| **Transcript** | ⌘F find · ⌘G / ⇧⌘G next / previous · ⌘↓ scroll to the bottom · ⇧⌘C Chat · ⇧⌘T Trajectory · select text in a message, then **Quote** to answer it (the agent gets it as a blockquote) |
| **Right pane** | ⌘⌥B (or the title bar's pane button) show / hide, on the tab used last · ⌘J (or ⌃\`) Terminal, from it hide · ⌘P Files with its tree, from the tree hide · ⇧⌘L Side chat, from it hide · ⌃⇧G Changes, from it hide · ⌘W closes the shown file or terminal tab (in Changes, the open diff); with no file open, one terminal, or in the side chat, it hides the pane · ⇧⌘⏎ (or the pane's expand button) maximize / restore it · in a terminal, ⌘T new tab · ⌃Tab next tab · ⌘K clear |
| **Trackpad** | two-finger swipe left shows the right pane, right hides it, wherever the pointer is (Settings ▸ Trackpad: the sidebar instead, the other way round, or off) |
| **App** | ⌘, Settings · ⌘Q quit (always asks, and says if threads are working or terminals run something) |

- **No checks:** the agent runs every tool call it decides on (shell commands, edits, fetches)
  without asking. Space's guard hooks stay open to a plugin that wants to add checks
  ([writing a plugin](docs/plugins.md)).
- **Closing** a thread keeps it: a new, empty thread in the same folder offers it under **Resume**.
  Closed threads are deleted after 30 days (Settings ▸ Threads).
- **Titles** come from the agent's model after your first message. A name you give (⌘R) stays.
- **The sidebar** groups threads under Pinned, Today, Yesterday, then one header per earlier day
  (by when each was created; writing in one doesn't move it). Pinned threads keep their pin while
  closed.
- **Folders.** Each thread works in one folder, picked under the composer before its first message
  and shown there after (hover for the full path). In a git repo the menu's branch row opens its
  branches: type to filter, or a new name to create one; picking switches the folder (git refuses
  if uncommitted changes would be lost, and says why).
  Opened from Spotlight or the Dock, Space starts where you worked last.
- **Open in** (title bar, right): opens the thread's folder in the app you used last; the chevron
  lists the editors, Finder and terminals installed, and Copy Path.
- **The right pane** has four modes, **Files**, **Terminal**, **Side chat** and **Changes** (tabs
  at its top; a narrow pane names only the shown one), each with its own bar under them: the
  open files (with a toggle for the tree beside them), the terminals, the side chat's, or the
  branch and what to do next. A thread's first opening offers the four; then it reopens on the
  tab used last.
- **Files** is the thread's folder as a tree: `.gitignore`d files are left out, git changes are
  colored (M, A, D, R, U) and folders with changes get a dot, a folder holding only a folder
  shows as one row, and it updates as files change. Type in the filter to search every path.
  A file opens in a tab beside the tree: code with line numbers and syntax colors (read-only;
  select, copy, ⌘F), Markdown rendered or as source, images fitted to the pane, and "Open in"
  for the rest. A file with uncommitted changes opens on **Changes** (its diff against the last
  commit: old and new line numbers, added lines green, removed red, changed words brighter);
  **Full file** shows the whole file with the changes in place. Open files reload when they change. Arrows move in the tree; right-click for Open in, Reveal in Finder and Copy Path.
- **Terminals** belong to a thread and start in its folder; they keep running while hidden.
- **Side chat** (⇧⌘L, or `/btw` and a space in the composer) is a conversation about the thread
  beside it: it starts from a copy of the thread, is told what the thread adds as it goes on
  (each message you send carries what's new; "Caught up with the main thread" shows it), and can
  only read (files and the web), never change anything. `/btw` opens it focused at once, and
  what you type after goes there, however fast. ↺ starts it over; it's saved with the thread.
- **Changes** (⌃⇧G) lists the folder's uncommitted files with their line counts; click one for
  its diff under it (one at a time). The bar shows the branch, one status line and only the
  next step as its button, the others that make sense under ▾: **Commit** (everything; Claude
  writes the message in the repository's style, Undo takes it back until it's pushed), **Push**,
  **Pull** (fast-forward only), **Create PR** (Claude writes the title and description; the
  GitHub CLI opens it), **Commit to new branch** on the default branch, and **Switch to main**
  after a merge. The first pull request in a repository asks which branch it goes into; that's
  then the default (kept in the repository's git config) and shows as "→ main" in the bar. An
  open pull request shows as a chip (it opens in the browser) and ▾ **Edit pull request**
  changes its title, description and base. The branch menu switches branches at any point of a
  conversation, or creates one from the current branch or the default one; threads in the
  folder are told with their next message. Nothing changes branches while a thread is working
  in the folder, and a merge or rebase in progress offers no steps. It reads the folder again as
  files or `.git` change, and asks the remote when shown and every few minutes after.
- **Slash commands**: `/compact` (optionally with what to keep), `/context`, `/goal <condition>`
  (Claude keeps working until it holds) and `/btw`, which opens the side chat.
- **Trajectory** shows every model request and tool call of a thread
  ([plugins/trajectory](plugins/trajectory/README.md)). **Continue in Claude Code** (left of its
  search) copies a command that continues the thread in the Claude CLI as a fork
  (`claude --resume <id> --fork-session`), so the thread in Space is never touched.
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
| `just check [crates]` | while iterating: clippy and tests for those crates only (default `space-app`) |
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
plugins/                claude-agent (TypeScript) · alerts · session-store · titles ·
                        trajectory · terminal; each has a README
vendor/gpui-base        patched copy of a GPUI dependency (SPACE_PATCH.md)
packaging/, scripts/    release, cask, notices, app icon (scripts/make_app_icon.py)
```

More: [writing a plugin](docs/plugins.md) · [working on the UI](docs/ui-development.md).
