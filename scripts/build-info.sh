#!/usr/bin/env sh
# Describe a development build, as `build.txt` beside its executable (read by
# space_core::build_info, the app's About, and `scripts/dev-apps.sh status`). A release has none:
# it knows it's a release from its build (`just release` sets SPACE_RELEASE=1).
#   commit=<full commit>                   what it was built from (its version is the first 7)
#   checkout=<repo root>                   where, to follow that checkout for new commits
#                                          ($SPACE_CHECKOUT when built for another checkout)
#   edited=true|false                      whether the checkout had uncommitted edits
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
edited=false
[ -n "$(git status --porcelain)" ] && edited=true
echo "commit=$(git rev-parse HEAD)"
echo "checkout=${SPACE_CHECKOUT:-$root}"
echo "edited=$edited"
echo "built=$(date '+%-d %b %Y, %H:%M')"
echo "source=$(source_id)"
echo "binary=$(stat -f %m "$exe")"
