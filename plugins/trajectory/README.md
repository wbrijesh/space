# Trajectory

Keeps a trace of every model request and tool call in each thread, and answers the app's
Trajectory tab. Bundled and on by default; Settings ▸ Plugins turns it off.

```
~/.space/trajectory/<thread id>/<session id>.jsonl   one trace batch per line, as received
```

- **Input**: `trace/record` batches from the host, in order (it observes `trace`, which needs
  `read:trace`: everything sent to and from the model, file contents included). Nothing leaves
  this Mac. Without a Space folder (tests, scenarios) traces stay in memory.
- **Rows** (`src/project.rs`, pure): SYSTEM (one per header: the system prompt, tools and
  options, with what changed since the previous one), USER (the message, image and file counts),
  CONTEXT (one per hidden section, labelled with its plugin), ASSISTANT (one per model request:
  reply, first token, duration, output tokens; running until its response; failed or cancelled
  from the stop reason, or when the turn ends without one), TOOL (input → output, with the
  guard's verdict and how long it held the call). A thread's sessions follow one another; turns
  and requests are numbered across the thread; row ids are `<session>:<event index>`.
- **Services**: `trajectory/read` (a page from the end, `older` cursor, or the rows matching
  every word of `query` in any part of their content), `trajectory/detail` (a row's id or
  `request:<n>`), `trajectory/stats`. It sends `trajectory/changed` at most every 200 ms per
  thread while batches arrive.
- **Settings**: "Keep traces for (days)" (empty: forever; swept at start and daily) and "Record
  streamed chunk timing" (on; off drops chunk times before they're kept).
- `thread/deleted` removes the thread's folder.

Tests: `cargo test -p space-trajectory`.
