# Session store plugin

A Rust service plugin (`space-session-store`) that keeps your threads so they're still there after Space
restarts. It provides `session-store` and uses `read:transcript`.

```
~/.space/threads/<thread id>/
  meta.json      the summary the sidebar lists (title, project, mode, model, agent and session, times)
  events.jsonl   the thread's session events, appended as Space saves them
  agent/         the agent's own files for the session, so it can be loaded again (`session/load`)
```

- Space decides what to save and when (see [architecture.md](../../docs/architecture.md#service-protocol));
  the store keeps the events as they are.
- A line cut short by a quit mid-write is skipped when loading, and the next write starts on a new line.
- Each `store/write` says where its events start (`first`): events the store already has are skipped, so
  a write sent again isn't kept twice, and an older `meta.json` never replaces a newer one.
- Thread ids are checked so they can't leave the folder; folders without a `meta.json` are pruned after
  24 hours. Deleting a thread removes its folder.
- Without `spaceDir` in `plugin/initialize` (tests, scenarios) nothing is written.

Tests: `cargo test -p space-session-store` (`tests/store.rs`: save, read back, restart, continue the
session with the stub agent, and a store that can't write).
