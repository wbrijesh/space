# Titles plugin

A Rust service plugin (`space-titles`) that names each thread after its first message.

- Provides `session-title`: Space calls `title/generate` once the first message is sent, and shows the
  first line of the message until a title arrives (and keeps it if none does within 15 s).
- Asks the thread's agent for one short completion (`agent/complete`, which the host forwards to the
  agent as `_space/complete`), with a prompt of the first 500 characters of the message, and tidies the
  reply: first line, no quotes, "Title:" label or trailing punctuation, at most 8 words.
- **Title model** (Settings ▸ Titles): "Same as agent" by default, so a thread is named with its own model
  and no other key or account is needed. Picking a model stores it as `provider/model`.
- Manifest: `uses = ["read:transcript", "agent/complete"]`.

Tests: `cargo test -p space-titles` (unit tests, and `tests/titles.rs` end to end with the stub agent:
own model, the setting, a failing and a hanging model, and no `agent/complete` in `uses`).
