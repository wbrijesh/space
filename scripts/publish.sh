#!/usr/bin/env sh
# Publish release v<version> (run by `just publish` after `just release-check` passed):
#   1. tag v<version> and push the tag,
#   2. create the GitHub release on wbrijesh/space with the zip,
#   3. update Casks/space.rb in wbrijesh/homebrew-tools (from packaging/homebrew/space.rb, with
#      this version and the zip's sha256), commit and push.
# Needs `gh` signed in with push access to both repos. Refuses to run twice for one version.
set -eu
version="$1"
root="$(cd "$(dirname "$0")/.." && pwd)"
zip="$root/target/release/Space-$version-arm64.zip"
tag="v$version"
[ -f "$zip" ] || { echo "missing $zip (run: just release-check)" >&2; exit 1; }
cd "$root"
[ -z "$(git status --porcelain)" ] || { echo "the working tree has uncommitted changes; commit them first" >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1; then
    echo "$tag already exists: bump the version in Cargo.toml first" >&2
    exit 1
fi
sha="$(shasum -a 256 "$zip" | cut -d' ' -f1)"

git tag -a "$tag" -m "Space $version"
git push origin "$tag"
gh release create "$tag" "$zip" --repo wbrijesh/space --title "Space $version" \
    --notes "Install: \`brew install --cask wbrijesh/tools/space\` (Apple Silicon). Needs the Claude CLI, signed in."

tap="$(mktemp -d)"
trap 'rm -rf "$tap"' EXIT
gh repo clone wbrijesh/homebrew-tools "$tap" -- --quiet
mkdir -p "$tap/Casks"
sed -e "s/@VERSION@/$version/" -e "s/@SHA256@/$sha/" "$root/packaging/homebrew/space.rb" > "$tap/Casks/space.rb"
git -C "$tap" add Casks/space.rb
git -C "$tap" commit -q -m "bump space to $tag"
git -C "$tap" push -q
echo "published $tag: https://github.com/wbrijesh/space/releases/tag/$tag"
echo "install: brew install --cask wbrijesh/tools/space"
