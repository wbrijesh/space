#!/usr/bin/env sh
# Wrap the debug build in target/debug/Space.app for `just run`: macOS gives notifications only to
# an app with a bundle id. The executable is a hard link to target/debug/Space (no copy), so each
# build is picked up without taking more space. Same bundle id as the release app, so only one of
# them runs at a time.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/debug"
app="$out/Space.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

[ -x "$out/Space" ] || { echo "missing $out/Space (build first: just build)" >&2; exit 1; }
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
ln -f "$out/Space" "$app/Contents/MacOS/Space"
cp -f "$root/crates/space-app/resources/Assets.car" "$app/Contents/Resources/Assets.car"
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
  <key>CFBundleVersion</key><string>0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
echo "$app"
