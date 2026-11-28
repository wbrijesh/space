#!/usr/bin/env sh
# Fingerprint each part of Space at the commit checked out here, as JSON: comparing two builds'
# fingerprints says what an update between them changes, and so what applying it costs.
#   app      the window (space-app and what it's built from)           → the app relaunches
#   server   the host, its service plugins, the app↔server protocol    → the server restarts
#   agent    the agent plugin                                          → agents restart
#   keeper   the keeper                                                → everything restarts
#   protocol the server↔agent protocol version: a change restarts agents too
# Each is a hash of the git trees of its folders (committed content only).
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
tree() { git rev-parse "HEAD:$1" 2>/dev/null || echo none; }
hash_of() { for p in "$@"; do echo "$p $(tree "$p")"; done | shasum | cut -c1-12; }
shared="crates/space-core crates/space-protocol crates/space-plugin Cargo.lock"
app=$(hash_of crates/space-app crates/space-ui crates/space-terminal crates/space-client vendor $shared)
server=$(hash_of crates/space-server plugins/alerts plugins/session-store plugins/titles plugins/trajectory plugins/terminal $shared)
agent=$(hash_of plugins/claude-agent plugins/space-ts.ts package.json bun.lock)
keeper=$(hash_of crates/space-keeper)
protocol=$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' crates/space-protocol/src/lib.rs)
printf '{"app": "%s", "server": "%s", "agent": "%s", "keeper": "%s", "protocol": %s}\n' "$app" "$server" "$agent" "$keeper" "${protocol:-0}"
