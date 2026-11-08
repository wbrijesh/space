# Space task runner (https://github.com/casey/just)

plugin := "plugins/pi-agent"
project := justfile_directory()

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

# Build the Rust workspace and the bundled pi-agent plugin
build: build-rust build-plugin

build-rust:
    cargo build --workspace

build-plugin:
    #!/usr/bin/env sh
    if [ -f {{plugin}}/package.json ]; then
        pnpm -C {{plugin}} install && pnpm -C {{plugin}} build
    else
        echo "skipping {{plugin}}: no package.json"
    fi

# Without dev tooling (no gallery, scenarios, clock controls or hot reload): for shipping and for
# judging real performance, not for running daily
# Release build: target/release/Space.app
release:
    cargo build --release -p space-app --no-default-features
    scripts/bundle.sh release

# Run all Rust tests (uses the space-stub-agent, no model calls)
test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p space-app --no-default-features --all-targets -- -D warnings

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
