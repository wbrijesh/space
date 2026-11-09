#!/usr/bin/env sh
# Check the release app as a user gets it: unzip target/release/Space-<version>-arm64.zip outside
# the repo (so it runs as an installed app, its plugins from inside the bundle) and run
# `Space --check-plugins`, which starts every bundled plugin and the agent (one session, no model
# call). Fails if any doesn't start. Nothing is written to ~/.space.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
zip="$root/target/release/Space-$version-arm64.zip"
[ -f "$zip" ] || { echo "missing $zip (run: just release)" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
ditto -x -k "$zip" "$work/Applications"
app="$work/Applications/Space.app"
mkdir -p "$work/project" "$work/pi-agent"

echo "== $("$app/Contents/MacOS/Space" --version)"
codesign --verify --strict "$app"
echo "== signature ok"
status=0
SPACE_SETTINGS="$work/settings.json" SPACE_PI_AGENT_DIR="$work/pi-agent" SPACE_NOTIFY=0 \
    "$app/Contents/MacOS/Space" --check-plugins "$work/project" > "$work/check.txt" || status=$?
cat "$work/check.txt"
[ "$status" -eq 0 ] || { echo "a bundled plugin didn't start" >&2; exit 1; }
# The plugins and their binaries must come from inside the app, not a repo checkout.
grep -q "^plugins from .*/Space.app/Contents/Resources/plugins$" "$work/check.txt" || { echo "plugins weren't loaded from inside the app" >&2; exit 1; }
grep -q "^helpers in .*/Space.app/Contents/Helpers$" "$work/check.txt" || { echo "plugin binaries weren't run from inside the app" >&2; exit 1; }
echo "== release check passed"
