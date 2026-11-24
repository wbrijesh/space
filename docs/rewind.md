# Going back to an earlier message

✎ **Edit** one of your messages in place, or ↺ **Rewind** to just before it. The message and
everything after it leave the thread, and the files Claude's edits changed since go back as they
were. **⌘Z** brings it all back. The behaviour was checked against the real `claude` CLI (2.1.331,
Agent SDK 0.3.328) on 2026-11-24.

## What you see

- **Hover one of your messages:** ✎ ↺ ⧉ next to the time.
- **✎ Edit:** the bubble becomes a text field and everything after it dims, which shows what will
  go. ⏎ sends the edited message in the original's place; Esc leaves it as it was.
- **↺ Rewind:** happens at once, with no confirmation. The thread ends before the message, and
  the message (its text, pictures, pasted texts, files and quote) goes back into the composer.
- **Esc Esc** in an empty composer, with nothing running, picks your last message from the
  keyboard: ↑/↓ choose, ⏎ edits, ⌘⏎ rewinds, Esc or typing leaves.
- **A notice** in the window's bottom-right corner, over the composer:
  - It says what happened: "Rewound." or "Edited.", and "Claude's file changes after this point
    were undone." when there were some.
  - It has Undo ⌘Z and goes by itself after 5 s.
  - It stays until closed only when there's something to look at:
    - files kept as they are, and why (`you changed it since`, `written by a subagent`);
    - or a problem (the thread couldn't go back, and nothing changed).
- **⌘Z** undoes the last rewind: the messages, the files as they were just before, and the old
  conversation.
  - It works from anywhere no text field has focus, and from the composer while it holds only
    what the rewind put there.
  - It keeps working through the edited message's turn (undoing stops that turn), until you
    send another message.
  - Several rewinds undo one at a time.

## Decisions

These were chosen by the user (2026-11-24); the reasons are kept here so they aren't reopened by
accident.

- **Code and conversation go back together.** There's no choice to make and no file list to pick
  from. Undo is the safety net, not a confirmation dialog.
- **Your work is never overwritten.** A file you changed after Claude last wrote it keeps your
  version, and the notice names it.
- **Claude Code's own checkpoints, not a snapshot of the folder.** A shadow git repository was
  researched and tested, then rejected:
  - Restoring a snapshot of the whole folder would also undo other threads' work and your own
    edits in the same folder.
  - Telling them apart needed a snapshot around every command.
  - Claude Code's checkpoints track exactly what this session's Edit and Write calls changed.
- **Later messages are discarded**, not kept as versions to switch between.

## How it works

**Message ids.**
- Space gives every message it sends a UUID (`space/messageId` on `session/prompt`, and
  `SteerSession.id` for a steered message).
- claude-agent sends it as the SDK message's `uuid`. It is then the transcript entry's uuid, and
  Claude Code's file checkpoint id.
- Threads saved before this keep no ids, so their old messages can't be gone back to.

**Files (`_space/session/rewind`).**
- The agent answers which files its edits changed since the message (`rewindFiles` dry run), and
  with `restoreFiles` puts them back.
- Before that, Space:
  - reads those files as they are, for Undo;
  - compares each with a hash of what Claude's edits last wrote to it (`SessionEvent::Written`,
    recorded at each turn's end from the edit calls' `locations`);
  - after the restore, writes back the ones you changed since.
- Claude Code's checkpoints cover:
  - **Restored:** Edit and Write, also when a command changed the file later, and files outside
    the folder. A file Claude created is deleted.
  - **Not restored:** files only a command touched, a subagent's edits, and symlinks
    (`skipped`).
- A message that went in while Claude was working has no checkpoint of its own. Going back to it
  moves only the conversation; the notice says the files are as they were.

**The conversation.**
- `SessionEvent::Rewound { message_id, session_id }` cuts the transcript and keeps what it took,
  for `RewindUndone`. The thread's log stays append-only.
- Nothing happens to the agent's conversation until the next message
  (`SessionState::fork`). Then `session/new` with `space/forkOf` makes a new session from a copy
  of the conversation up to just before the message:
  - claude-agent reads the transcript (`rewind.ts`) and resumes it with `resumeSessionAt` set to
    the entry before the message, as a new session id (`forkSession`).
  - A message from before a compaction, which a resume can't reach, is copied with the SDK's
    `forkSession()` instead. That gives entries new uuids and keeps the old ones in
    `forkedFrom`.
  - The first message gets a fresh session.
- The old session's transcript stays byte-identical, so Undo goes back to it.
- Claude Code's own `resumeDropsTurn` check isn't used: it refuses anything but dropping the last
  turn.

**Waiting.**
- A running turn is stopped first; its queued messages go back into the composer, as Stop does.
- A thread whose agent is asleep is loaded first: files can only be put back by a running
  session.
- Messages before a `NewSession` divider (the saved session couldn't be continued) can't be gone
  back to.

## Where it lives

| Part | Where |
|---|---|
| Protocol | `REWIND_*`, `MESSAGE_ID_META_KEY`, `FORK_OF_META_KEY`, `SUBAGENT_META_KEY`, `RewindSession` in `crates/space-protocol/src/ext.rs` |
| Events and reducer | `crates/space-core/src/session.rs` (`Rewound`, `RewindUndone`, `Written`, `NewSession`, `fork`, `rewinds`) |
| Host | `Session::rewind`, `AgentHandle::fork_session`, message ids in `crates/space-core/src/host/agents.rs` |
| Agent | `plugins/claude-agent/src/rewind.ts` (cut point), `session.ts` (`rewind`, forks, ids), `claude.ts` (`enableFileCheckpointing`) |
| App | `crates/space-app/src/rewind.rs` (flows, Undo, notice, Esc Esc) |
| UI | `message_actions.rs` (✎ ↺), `transcript.rs` (`editing_message`, `RewindMarks`), `notice.rs` |
| Stub | `write` steps, `space/forkOf` and `_space/session/rewind` in `space-stub-agent` |
| Scenario | `--scenario rewind <scratch folder>`; dev script `rewind`, `edit N`, `rewindto N`, `undo` |
