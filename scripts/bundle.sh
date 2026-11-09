#!/usr/bin/env sh
# Assemble the release app, target/release/Space.app, with its plugins inside, and zip it for the
# cask. Run by `just release` after the release build (`cargo build --release --workspace` and the
# Bun build of target/release/space-ts).
#
#   Contents/MacOS/Space                    the app
#   Contents/Helpers/<binary>               every bundled plugin's executable (`${target}/…` in
#                                           its manifest; Space expands ${target} to Helpers)
#   Contents/Resources/plugins/<dir>/       the bundled plugins' manifests
#   Contents/Resources/Assets.car           the icon
#   Contents/Resources/THIRD_PARTY_NOTICES.md
#
# Signed ad hoc (no Developer ID yet): each helper, then the app. Prints the zip and its sha256.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/release"
app="$out/Space.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
zip="$out/Space-$version-arm64.zip"
# A fresh build number each time: macOS caches icons by bundle id + version.
build="$(date +%s)"

[ -x "$out/Space" ] || { echo "missing $out/Space (build first: just release)" >&2; exit 1; }
rm -rf "$app" "$zip"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources/plugins"
cp "$out/Space" "$app/Contents/MacOS/Space"
cp "$root/crates/space-app/resources/Assets.car" "$app/Contents/Resources/Assets.car"
cp "$root/THIRD_PARTY_NOTICES.md" "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"

# Each bundled plugin: its manifest, and the executable it names (`command = "${target}/<name>"`).
for manifest in "$root"/plugins/*/space-plugin.toml; do
    dir="$(basename "$(dirname "$manifest")")"
    binary="$(sed -n 's/^command = "\${target}\/\(.*\)"/\1/p' "$manifest")"
    [ -n "$binary" ] || { echo "plugins/$dir: its command must be \${target}/<binary> to ship" >&2; exit 1; }
    [ -x "$out/$binary" ] || { echo "missing $out/$binary for plugins/$dir" >&2; exit 1; }
    mkdir -p "$app/Contents/Resources/plugins/$dir"
    cp "$manifest" "$app/Contents/Resources/plugins/$dir/space-plugin.toml"
    cp "$out/$binary" "$app/Contents/Helpers/$binary"
done

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
  <key>CFBundleVersion</key><string>$build</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

# Ad-hoc signatures: the helpers first, then the app (which seals them).
for helper in "$app"/Contents/Helpers/*; do
    codesign --force --sign - "$helper"
done
codesign --force --sign - "$app"
codesign --verify --strict "$app"

ditto -c -k --keepParent "$app" "$zip"
echo "$app"
echo "$zip ($(du -h "$zip" | cut -f1))"
echo "sha256 $(shasum -a 256 "$zip" | cut -d' ' -f1)"
