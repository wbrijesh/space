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

# Launch the desktop app on a project directory
run dir=project: build
    cargo run -p space-app -- {{dir}}

# Launch and send a prompt in the first thread (billed when using pi-agent)
demo prompt="": build
    SPACE_AUTOPROMPT="{{prompt}}" cargo run -p space-app -- {{project}}

# Dev: open the app on the free stub agent with a few seeded threads (UI work, screenshots)
demo-ui:
    #!/usr/bin/env sh
    set -e
    cargo build -p space-core --bin space-stub-agent -p space-app
    dir=$(mktemp -d)/stub && mkdir -p "$dir"
    printf 'id="stub"\nname="Stub Agent"\nversion="0.1.0"\nkind="agent"\n[run]\ncommand="%s"\n' \
        "{{project}}/target/debug/space-stub-agent" > "$dir/space-plugin.toml"
    SPACE_PLUGIN_DIRS="$(dirname "$dir")" SPACE_AGENT=stub SPACE_DEMO=1 ./target/debug/Space {{project}}

# Run all Rust tests (uses the space-stub-agent, no model calls)
test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Headless end-to-end prompt against the real agent plugin (bills your Claude subscription)
smoke prompt="Reply with exactly: pong": build
    cargo run -p space-app -- --smoke "{{prompt}}" {{project}}

# Headless end-to-end run against the scripted stub agent (free)
smoke-stub prompt="hello, permission please":
    #!/usr/bin/env sh
    set -e
    cargo build -p space-core --bin space-stub-agent -p space-app
    dir=$(mktemp -d)/stub && mkdir -p "$dir"
    printf 'id="stub"\nname="Stub Agent"\nversion="0.1.0"\nkind="agent"\n[run]\ncommand="%s"\n' \
        "{{project}}/target/debug/space-stub-agent" > "$dir/space-plugin.toml"
    SPACE_PLUGIN_DIRS="$(dirname "$dir")" ./target/debug/Space --smoke "{{prompt}}" --allow {{project}}
