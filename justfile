# Space task runner (https://github.com/casey/just)

plugin := "plugins/pi-agent"
project := justfile_directory()

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

# Wrap the built binary in target/<profile>/Space.app (bundle id, icon; needed for notifications)
bundle profile="debug":
    scripts/bundle.sh {{profile}}

# Launch the desktop app on a project directory (from the .app bundle, so notifications work)
run dir=project: build (bundle "debug")
    ./target/debug/Space.app/Contents/MacOS/Space {{dir}}

# Launch and send a prompt in the first thread (billed when using pi-agent)
demo prompt="": build
    SPACE_AUTOPROMPT="{{prompt}}" cargo run -p space-app -- {{project}}

# Dev: component gallery (tokens, every component state, animation clock controls)
gallery:
    cargo build --workspace
    scripts/bundle.sh debug >/dev/null
    ./target/debug/Space.app/Contents/MacOS/Space --gallery

# Dev: boot the real app into a scripted state (stub agent); `just scenario name=list`
scenario name="many-threads":
    cargo build --workspace
    scripts/bundle.sh debug >/dev/null
    ./target/debug/Space.app/Contents/MacOS/Space --scenario {{name}} {{project}}

# Release build without dev tooling (no gallery, scenarios or clock controls)
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
