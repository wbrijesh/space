# Working on Space

## Commits

- **One commit per task.** When a task the user gave you is done (built, `just lint` and
  `just test` pass, checked in the app when it's visible), commit it yourself. Don't ask first.
  Commit only that task's changes: if the tree holds other work, stage just this task's files
  and hunks.
- **Changes to a task's work amend its commit.** When the user asks to change how a committed
  task was done (a fix, a tweak, a different look), `git commit --amend` that task's commit
  instead of adding a new one, and update its message if what it says changed. If other commits
  came after it, fold the change into the right commit (`git commit --fixup <sha>`, then
  `git -c sequence.editor=: rebase -i --autosquash <sha>~1`). A new, separate task gets a new
  commit.
- **Never push** unless the user asks.
- **The pre-commit hook** (`.githooks/pre-commit`, enabled by `just hooks`) runs `just lint`
  and `just test`. Never skip it with `--no-verify`: fix what fails.
- **Message style** (follow `git log`): a short subject, `Area: what changed` or a plain
  sentence (`Right pane: Files and Terminal modes`, `Ride out a bad network: wait, retry,
  resume`), no trailing period; then a body in plain prose saying what changed and why, wrapped
  at ~72 columns; then the `Co-Authored-By:` line. Release commits are just `Space 0.1.x`.

## Local apps

`just apps` starts Space (workspace 2) and SpaceTest (workspace 9), detached. After each committed
change, run `just apps-update` (build, then restart SpaceTest on it) and check `just apps-status`.

Space builds itself: once it runs from a staged build (`just self-host`, done once), its server
builds each new commit on the checkout's branch in `~/.space/builds/worktree` and offers it (a
notice and the menu bar item: Relaunch, Restart, or Restart now / when idle). Don't restart it
yourself: the user chooses when. Never kill its server or keeper (`~/.space/*.sock`), and test
servers only in scratch folders (`SPACE_SERVER_DIR=<dir>`).

## Disk use

Build output grows fast here (GPUI is a large dependency graph), so:

- **One way to build:** `just build`, `just test`, `just lint` (or `just check` while iterating).
  No ad-hoc `cargo build -p … --features …` or `--target`: each new combination compiles another
  full copy of the affected crates next to the old ones.
- **Worktrees only when parallel work needs them.** Each has its own `target/` (several GB).
  Remove it (`git worktree remove`, then `git branch -d` once merged) as soon as its work lands.
  Don't run subagents in isolated worktrees by default.
- **Test copies of the app:** reuse one copy (it's ~190 MB); clean scratch folders when a task
  ends.
- **Release builds only from the main checkout**; `just install` removes the built bundle.
- **When `just build` warns** that `target/` is past 40 GB, run `just clean`.
