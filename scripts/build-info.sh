#!/usr/bin/env sh
# Describe a local build, as `build.txt` beside its executable (read by the app's About and by
# `scripts/dev-apps.sh status`):
#   version=0.1.6 +2 · a95ab81 · edited   the last release, commits since it, the commit, edits
#   built=24 Nov 2026, 00:41               when
#   source=<commit>-<hash of the edits>    exactly what was built, to tell whether it's current
#   binary=<mtime of the executable>       which build of the binary this describes
# `build-info.sh --source` prints only the current source line (what a build now would be).
# Usage: build-info.sh <executable> | build-info.sh --source
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

source_id() {
    edits="$( { git diff HEAD --binary; git ls-files -o --exclude-standard -z | xargs -0 shasum 2>/dev/null; } | shasum | cut -c1-12)"
    echo "$(git rev-parse --short=7 HEAD)-$edits"
}
if [ "${1:-}" = "--source" ]; then source_id; exit 0; fi

exe="$1"
cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
desc="$(git describe --tags --match 'v[0-9]*' --abbrev=7 --dirty=-edited 2>/dev/null || true)"
edited=""
case "$desc" in *-edited) edited=" · edited"; desc="${desc%-edited}" ;; esac
case "$desc" in
    "") version="$cargo_version · $(git rev-parse --short=7 HEAD)$edited" ;;
    *-*-g*)
        sha="${desc##*-g}"; rest="${desc%-g*}"; n="${rest##*-}"; tag="${rest%-*}"
        version="${tag#v} +$n · $sha$edited" ;;
    *) version="${desc#v}$edited" ;;
esac
echo "version=$version"
echo "built=$(date '+%-d %b %Y, %H:%M')"
echo "source=$(source_id)"
echo "binary=$(stat -f %m "$exe")"
