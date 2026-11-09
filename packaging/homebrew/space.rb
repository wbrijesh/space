# Template for Casks/space.rb in wbrijesh/homebrew-tools: `just publish` fills in the version and
# the zip's sha256 and pushes it. Install: brew install --cask wbrijesh/tools/space
cask "space" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/wbrijesh/space/releases/download/v#{version}/Space-#{version}-arm64.zip"
  name "Space"
  desc "Native macOS development environment built around projects, bots and knowledge"
  homepage "https://github.com/wbrijesh/space"

  depends_on macos: ">= :ventura"
  depends_on arch: :arm64

  app "Space.app"

  # Signed ad hoc, not notarized (no Developer ID yet): clear the download mark so macOS opens it.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Space.app"]
  end

  # `brew uninstall --zap space` also removes your threads, settings and traces.
  zap trash: "~/.space"

  caveats <<~EOS
    Space uses your Claude subscription through the Claude CLI. Install it and sign in once:
      brew install --cask claude-code
      claude
  EOS
end
