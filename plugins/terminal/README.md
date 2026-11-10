# Terminal plugin

A Rust service plugin (`space-terminal-plugin`) that runs the shells of the terminal pane. It provides
`terminal`; the app draws the screens (`crates/space-terminal`).

- Each terminal is your login shell (`$SHELL`) on its own PTY and in its own session, started in the
  thread's folder, with `TERM=xterm-256color`, `COLORTERM=truecolor` and `TERM_PROGRAM=Space`.
- Output goes to the host in numbered batches; the newest 2 MB are kept, so the app can draw a
  terminal from scratch (`terminal/attach`) and pick up the live stream where the replay ends.
- A tab's title is the program in front of the shell.
- Closing a terminal hangs up its session and kills what's left two seconds later. The same happens
  to a thread's terminals on `thread/deleted`, and to every terminal when Space quits.
- `SPACE_TERMINAL_SHELL="prog args"` runs that instead of the login shell (tests, scenarios).

Tests: `cargo test -p space-terminal-plugin` (`tests/end_to_end.rs`: real shells through the host).
