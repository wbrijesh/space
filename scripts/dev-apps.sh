#!/usr/bin/env bash
# The two local apps, kept on the latest build:
#   Space      this repo's dev bundle (target/debug/Space.app), on AeroSpace workspace 3
#   SpaceTest  a copy in ~/.cache/space-test with the `changes` scenario and a fake `gh`, on 9
# Both run detached (nohup; logs and state in ~/.cache/space-dev), so closing a terminal or
# interrupting an agent doesn't stop them. Windows are moved by window id, without focusing them,
# and neither app takes focus: each start checks that it didn't (guard).
#
#   dev-apps.sh open     start whichever isn't running
#   dev-apps.sh update   build, then restart both on the new build (a failed build changes nothing)
#   dev-apps.sh status   what's running, on which build, and whether it's the latest
#   dev-apps.sh stop     quit both
#   dev-apps.sh log      follow both logs
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
state="$HOME/.cache/space-dev"
test_dir="$HOME/.cache/space-test"
fixture="$test_dir/changes-test"
mkdir -p "$state"

# Per app: the executable, how to find its process, and its workspace.
exe() { case "$1" in space) echo "$root/target/debug/Space.app/Contents/MacOS/Space" ;; test) echo "$test_dir/SpaceTest" ;; esac; }
pattern() { case "$1" in space) echo "target/debug/Space.app/Contents/MacOS/Space" ;; test) echo "SpaceTest --scenario changes" ;; esac; }
workspace() { case "$1" in space) echo 3 ;; test) echo 9 ;; esac; }
name() { case "$1" in space) echo Space ;; test) echo SpaceTest ;; esac; }

pid_of() { pgrep -f "$(pattern "$1")" | head -1 || true; }

start() {
    local app="$1" pid
    if [ -n "$(pid_of "$app")" ]; then echo "$(name "$app"): already running"; return; fi
    [ -x "$(exe "$app")" ] || { echo "$(name "$app"): not built yet (run: just apps-update)" >&2; return 1; }
    # The subshell execs into the app, so $! is the app's pid and nothing else holds this
    # script's output open.
    case "$app" in
        space)
            (cd "$root" && exec env SPACE_TARGET_DIR="$root/target/debug" SPACE_PROJECT="$root" SPACE_RELOAD_PLUGINS=1 \
                nohup "$(exe space)") < /dev/null > "$state/space.log" 2>&1 & ;;
        test)
            (cd "$test_dir" && exec env PATH="$fixture/bin:$PATH" FAKE_GH_STATE="$fixture/pr.json" \
                nohup "$(exe test)" --scenario changes "$fixture/w") < /dev/null > "$state/test.log" 2>&1 & ;;
    esac
    pid=$!
    echo "$pid" > "$state/$app.pid"
    # What it was started on, for status.
    cp "$(dirname "$(exe "$app")")/build.txt" "$state/$app.build" 2>/dev/null || rm -f "$state/$app.build"
    place "$app" "$pid"
}

# Move the app's window to its workspace once it shows (by its pid: the installed Space has the
# same name), without focusing it.
place() {
    local app="$1" pid="$2" id=""
    for _ in $(seq 1 120); do
        id="$(aerospace list-windows --all --format '%{window-id}|%{app-pid}' 2>/dev/null | awk -F'|' -v p="$pid" '$2 == p { print $1; exit }')"
        [ -n "$id" ] && break
        kill -0 "$pid" 2>/dev/null || { echo "$(name "$app"): exited at launch; see $state/$app.log" >&2; return 1; }
        sleep 0.5
    done
    if [ -z "$id" ]; then echo "$(name "$app"): started (pid $pid), but no window after 60 s" >&2; return 1; fi
    aerospace move-node-to-workspace --window-id "$id" "$(workspace "$app")" > /dev/null 2>&1 || true
    echo "$(name "$app"): running on workspace $(workspace "$app") (pid $pid)"
    guard "$app" "$pid"
}

# Space must never take focus when it starts: an app that makes itself active pulls you to its
# workspace (AeroSpace follows macOS focus), away from whatever you were doing. Check, a moment
# after its window shows, that the focused window isn't the app's; only reads AeroSpace's state.
guard() {
    local app="$1" pid="$2" focused
    sleep 1.5
    focused="$(aerospace list-windows --focused --format '%{app-pid}' 2>/dev/null || true)"
    if [ "$focused" = "$pid" ]; then
        echo "" >&2
        echo "!!! $(name "$app") took focus when it started, and pulled you to workspace $(workspace "$app")." >&2
        echo "!!! Space must never make itself active (see the launch in crates/space-app/src/app.rs)." >&2
        echo "" >&2
        return 1
    fi
}

stop() {
    local app="$1" pid
    pid="$(pid_of "$app")"
    [ -n "$pid" ] || return 0
    pkill -f "$(pattern "$app")" || true
    for _ in $(seq 1 25); do [ -z "$(pid_of "$app")" ] && return 0; sleep 0.2; done
    pkill -9 -f "$(pattern "$app")" || true
}

update() {
    (cd "$root" && just build) || { echo "build failed: the running apps are unchanged" >&2; exit 1; }
    "$root/scripts/dev-bundle.sh" > /dev/null
    stop space
    stop test
    # rm before cp: copying over the old file keeps its inode, and macOS kills the new binary at
    # launch for a stale code signature.
    rm -f "$test_dir/SpaceTest" && cp "$root/target/debug/Space" "$test_dir/SpaceTest"
    cp "$root/target/debug/Space.app/Contents/MacOS/build.txt" "$test_dir/build.txt"
    start space
    start test
}

field() { [ -f "$1" ] && sed -n "s/^$2=//p" "$1" || true; }

status() {
    local now_source built_binary
    now_source="$("$root/scripts/build-info.sh" --source)"
    built_binary="$(stat -f %m "$root/target/debug/Space" 2>/dev/null || echo none)"
    for app in space test; do
        local pid info since
        pid="$(pid_of "$app")"
        info="$state/$app.build"
        printf '%-11s' "$(name "$app")"
        if [ -z "$pid" ]; then echo "not running   → just apps"; continue; fi
        since="$(ps -o etime= -p "$pid" | tr -d ' ')"
        echo "running   workspace $(workspace "$app")   pid $pid   up $since"
        if [ "$pid" != "$(cat "$state/$app.pid" 2>/dev/null)" ] || [ ! -f "$info" ]; then
            printf '%11s%s\n' "" "started outside dev-apps: build unknown   → just apps-update"
            continue
        fi
        printf '%11s%s · built %s\n' "" "$(field "$info" version)" "$(field "$info" built)"
        if [ "$(field "$info" binary)" != "$built_binary" ]; then
            printf '%11s%s\n' "" "✗ stale: a newer build exists   → just apps-update"
        elif [ "$(field "$info" source)" != "$now_source" ]; then
            printf '%11s%s\n' "" "✗ the code changed since this build   → just apps-update"
        else
            printf '%11s%s\n' "" "✓ latest"
        fi
    done
}

case "${1:-status}" in
    open) start space; start test ;;
    update) update ;;
    status) status ;;
    stop) stop space; stop test; echo "stopped" ;;
    log) tail -n 40 -F "$state/space.log" "$state/test.log" ;;
    *) echo "usage: dev-apps.sh open|update|status|stop|log" >&2; exit 2 ;;
esac
