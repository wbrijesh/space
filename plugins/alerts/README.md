# Alerts plugin

A Rust service plugin (`space-alerts`) that tells you when an agent finishes, pauses or needs your
approval.

- Observes `turn/started`, `turn/ended`, `tool/held` and `permission/requested`; uses `host/notify`,
  so Space posts the notification (clicking it opens the thread) and plays the sound.
- Not looking at the thread: a notification, with the sound. Looking at it: the sound alone. A stopped
  turn says nothing. A tool call waiting for a guard alerts once per turn ("Paused: …"). Every
  approval card alerts ("Needs your approval"): the agent asks, or the guardrail leaves it to you.
- Settings ▸ Alerts: **Notify when an agent finishes or needs you** and **Play a sound**, both on by
  default.
- `SPACE_NOTIFY=0` silences Space's notifications and sounds altogether.

Tests: `cargo test -p space-alerts`.
