#!/usr/bin/env sh
# Stage this checkout's debug build as a whole app in <dest>/Space.app: the app, every bundled
# plugin, the server and the keeper, with what it was built from (build.txt) and what each part
# is (components.json). Space runs from staged builds, never from target/, so an agent rebuilding
# can't change what runs. `checkout` is the checkout the build follows for updates (the one this
# was built for, when building in another worktree). Build first (just build).
# Usage: stage.sh <dest> [checkout]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/debug"
dest="$1"
checkout="${2:-$root}"
tmp="$dest.staging"
app="$tmp/Space.app"

[ -x "$out/Space" ] || { echo "missing $out/Space (build first: just build)" >&2; exit 1; }
rm -rf "$tmp"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources/plugins"
cp "$out/Space" "$app/Contents/MacOS/Space"
cp "$root/crates/space-app/resources/Assets.car" "$app/Contents/Resources/Assets.car"

# Each bundled plugin: its manifest, and the executable it names (`command = "${target}/<name>"`).
for manifest in "$root"/plugins/*/space-plugin.toml; do
    dir="$(basename "$(dirname "$manifest")")"
    binary="$(sed -n 's/^command = "\${target}\/\(.*\)"/\1/p' "$manifest")"
    [ -x "$out/$binary" ] || { echo "missing $out/$binary for plugins/$dir" >&2; exit 1; }
    mkdir -p "$app/Contents/Resources/plugins/$dir"
    cp "$manifest" "$app/Contents/Resources/plugins/$dir/space-plugin.toml"
    cp "$out/$binary" "$app/Contents/Helpers/$binary"
done
for binary in space-server space-keeper; do
    cp "$out/$binary" "$app/Contents/Helpers/$binary"
done

# What it is: beside the app and beside the server (each reads the one next to it).
SPACE_CHECKOUT="$checkout" "$root/scripts/build-info.sh" "$out/Space" > "$app/Contents/MacOS/build.txt"
cp "$app/Contents/MacOS/build.txt" "$app/Contents/Helpers/build.txt"
"$root/scripts/components.sh" > "$app/Contents/Resources/components.json"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.brijesh.space</string>
  <key>CFBundleName</key><string>Space</string>
  <key>CFBundleDisplayName</key><string>Space</string>
  <key>CFBundleExecutable</key><string>Space</string>
  <key>CFBundleIconName</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$(date +%s)</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
rm -rf "$dest"
mv "$tmp" "$dest"
echo "$dest/Space.app"
