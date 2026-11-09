# Space task runner (https://github.com/casey/just)

project := justfile_directory()
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1`

# Builds everything, then launches through dx with live Rust edits: saving a file patches the running
# app in ~1 s (render code, layout, styles; struct fields or `main` need a rebuild: press `r` in
# the dx window). Plugins are other processes: when their sources change, the app rebuilds and
# restarts them (the agent once it's idle). Arguments go to the app:
#   just run                      this repo as the project
#   just run ~/code/other         another project
#   just run --gallery            the component gallery
#   just run --scenario list      scripted states (stub agent); `--scenario <name>` opens one
# Needs dx at GPUI's subsecond version: cargo install dioxus-cli --version 0.7.10 --locked
# Run Space: the one way to run it while developing (plain `just` does this too)
run *args: build
    SPACE_TARGET_DIR={{project}}/target/debug SPACE_PROJECT={{project}} SPACE_RELOAD_PLUGINS=1 \
        dx serve --hot-patch -p space-app --bin Space --features hot --macos --args="{{args}}"

# Build the Rust workspace (app and Rust plugins) and the TypeScript plugins
build: build-rust build-ts

build-rust:
    cargo build --workspace

# Every bundled TypeScript plugin runs from one Bun executable, next to the Rust plugins
build-ts:
    bun install
    bun build --compile plugins/space-ts.ts --outfile target/debug/space-ts

# Without dev tooling (no gallery, scenarios, clock controls or hot reload); plugins inside the
# app, ad-hoc signed, zipped for the cask. For shipping and judging real performance, not daily use
# Release build: target/release/Space.app and Space-<version>-arm64.zip
release:
    cargo build --release --workspace --no-default-features
    bun install
    # (not --bytecode: Bun can't precompile the pi SDK's modules to bytecode)
    bun build --compile --minify plugins/space-ts.ts --outfile target/release/space-ts
    scripts/bundle.sh release

# Copy the release app to ~/Applications, where Spotlight and Launchpad find it. It has the same
# bundle id as `just run`'s app, so opening it while that one runs just brings that one forward
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
    cd plugins/pi-agent && bun test src && bun scripts/host-test.ts

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p space-app --no-default-features --all-targets -- -D warnings
    cd plugins/pi-agent && bun run typecheck

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

# Regenerate the TypeScript protocol bindings (plugins/pi-agent/src/generated) from the Rust types
protocol:
    SPACE_WRITE_PROTOCOL=1 cargo test -p space-protocol typescript_bindings_are_current
