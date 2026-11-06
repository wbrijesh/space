#!/usr/bin/env sh
# Wrap a built `Space` binary in a macOS .app bundle: target/<profile>/Space.app.
# macOS needs a bundle (with a bundle id) for notifications, and uses it for the Dock
# icon and app name. Usage: scripts/bundle.sh [debug|release]
set -eu
profile="${1:-debug}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/target/$profile/Space"
app="$root/target/$profile/Space.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

[ -x "$bin" ] || { echo "missing $bin (build first)" >&2; exit 1; }
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Space"
cp "$root/crates/space-app/resources/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.brijesh.space</string>
  <key>CFBundleName</key><string>Space</string>
  <key>CFBundleDisplayName</key><string>Space</string>
  <key>CFBundleExecutable</key><string>Space</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
# Ad-hoc signature over the whole bundle (required for notification permission).
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
