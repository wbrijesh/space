# Space task runner (https://github.com/casey/just)

project := justfile_directory()
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1`

# Builds everything, then runs Space as target/debug/Space.app (a bundle, for notifications). When a
# plugin's sources change, the app rebuilds and restarts it (the agent once it's idle); a change to
# the app itself needs a restart (⌃C, then `just run` again). Arguments go to the app:
#   just run                      this repo as the project
#   just run ~/code/other         another project
#   just run --gallery            the component gallery
#   just run --scenario list      scripted states (stub agent); `--scenario <name>` opens one
# Run Space: the one way to run it while developing (plain `just` does this too)
run *args: build
    scripts/dev-bundle.sh > /dev/null
    SPACE_TARGET_DIR={{project}}/target/debug SPACE_PROJECT={{project}} SPACE_RELOAD_PLUGINS=1 \
        target/debug/Space.app/Contents/MacOS/Space {{args}}

# The two local apps, detached and kept on the latest build: Space (this repo) on workspace 2,
# SpaceTest (the `changes` scenario, fake gh) on 9. See scripts/dev-apps.sh
# Start whichever of Space and SpaceTest isn't running
apps:
    @scripts/dev-apps.sh open

# Build, then restart Space and SpaceTest on the new build (a failed build leaves them running)
apps-update:
    @scripts/dev-apps.sh update

# Show whether Space and SpaceTest run, on which build, and whether it's the latest
apps-status:
    @scripts/dev-apps.sh status

# Quit Space and SpaceTest
apps-stop:
    @scripts/dev-apps.sh stop

# Follow Space's and SpaceTest's logs
apps-log:
    @scripts/dev-apps.sh log

# Build the Rust workspace (app and Rust plugins) and the TypeScript plugins
build: size-check build-rust build-ts

build-rust:
    cargo build --workspace

# Every bundled TypeScript plugin runs from one Bun executable, next to the Rust plugins. Bun writes
# a temporary executable to the current folder (and leaves it there if the build fails), so it runs
# from target/, which `just clean` empties.
build-ts:
    bun install
    mkdir -p target/debug
    cd target && bun build --compile ../plugins/space-ts.ts --outfile debug/space-ts
    rm -f .*.bun-build

# Free disk space: build output, Bun's leftover temporary files, and records of removed worktrees
clean:
    cargo clean
    rm -f .*.bun-build
    git worktree prune

# Warn when the build output has grown past 40 GB (old copies pile up as settings change)
[private]
size-check:
    #!/usr/bin/env sh
    kb=$(du -sk target 2>/dev/null | cut -f1)
    if [ "${kb:-0}" -gt 41943040 ]; then echo "warning: target/ is $((kb / 1048576)) GB; run \`just clean\`" >&2; fi

# Without dev tooling (no gallery, scenarios, clock controls or scripts); plugins inside the
# app, ad-hoc signed, zipped for the cask. For shipping and judging real performance, not daily use
# Release build: target/release/Space.app and Space-<version>-arm64.zip
release:
    cargo build --release --workspace --no-default-features
    bun install
    mkdir -p target/release
    cd target && bun build --compile --minify ../plugins/space-ts.ts --outfile release/space-ts
    rm -f .*.bun-build
    scripts/bundle.sh release

# Copy the release app to ~/Applications, where Spotlight and Launchpad find it. It has the same
# bundle id as `just run`'s app: only one of them runs at a time.
# Install the current code as ~/Applications/Space.app
install: release
    mkdir -p ~/Applications
    rm -rf ~/Applications/Space.app
    ditto target/release/Space.app ~/Applications/Space.app
    rm -rf target/release/Space.app

# Run the release app from a copy outside the repo and check every bundled plugin starts from it
release-check: release
    scripts/release-check.sh

# Publish a release: build and check it, tag it, upload the zip to a GitHub release, and update the
# cask in wbrijesh/homebrew-tools. Only when a release is meant to go out
# Publish v<version> (the workspace version) to GitHub and the Homebrew tap
publish: release-check
    scripts/publish.sh {{version}}

# Run all tests: Rust (with the space-stub-agent) and the TypeScript plugins; no model calls
test: build-ts
    cargo test --workspace
    cd plugins/claude-agent && bun test src

# While iterating: clippy and tests for the given crates only (e.g. `just check space-ui space-app`);
# run `just lint` and `just test` before committing
check +crates="space-app":
    cargo clippy {{prepend("-p ", crates)}} --all-targets -- -D warnings
    cargo test {{prepend("-p ", crates)}}

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p space-app --no-default-features --all-targets -- -D warnings
    cd plugins/claude-agent && bun run typecheck

# Headless end-to-end prompt against the real agent plugin (bills your Claude subscription)
smoke prompt="Reply with exactly: pong": build
    cargo run -p space-app -- --smoke "{{prompt}}" {{project}}

# Headless end-to-end run against the scripted stub agent (free)
smoke-stub prompt="hello, permission please":
    #!/usr/bin/env sh
    set -e
    cargo build -p space-core --bin space-stub-agent -p space-app
    dir=$(mktemp -d)/stub && mkdir -p "$dir"
    printf 'id="stub"\nname="Stub Agent"\nversion="0.1.0"\nprovides=["agent"]\n[run]\ncommand="%s"\n' \
        "{{project}}/target/debug/space-stub-agent" > "$dir/space-plugin.toml"
    SPACE_PLUGIN_DIRS="$(dirname "$dir")" ./target/debug/Space --smoke "{{prompt}}" --allow {{project}}

# Regenerate the TypeScript protocol bindings (plugins/claude-agent/src/generated) from the Rust types
protocol:
    SPACE_WRITE_PROTOCOL=1 cargo test -p space-protocol typescript_bindings_are_current
