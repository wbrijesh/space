# Alerts plugin

A Rust service plugin (`space-alerts`) that tells you when an agent finishes or pauses.

- Observes `turn/started`, `turn/ended` and `tool/held`; uses `host/notify`, so Space posts the
  notification (clicking it opens the thread) and plays the sound.
- Not looking at the thread: a notification, with the sound. Looking at it: the sound alone. A stopped
  turn says nothing. A tool call waiting for a guard alerts once per turn ("Paused: …").
- Settings ▸ Alerts: **Notify when an agent finishes** and **Play a sound**, both on by default.
- `SPACE_NOTIFY=0` silences Space's notifications and sounds altogether.

Tests: `cargo test -p space-alerts`.
