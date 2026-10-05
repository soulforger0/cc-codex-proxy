cask "cc-codex-proxy-app" do
  version "1.1.3"
  sha256 "8c3be7cd1f857b6a0917d8c8c5f5b59f8b8fb6edb57865a2be14ad2f64fe9817"

  url "https://github.com/soulforger0/cc-codex-proxy/releases/download/v#{version}/CCCodexProxy-#{version}-macOS.dmg"
  name "CC Codex Proxy"
  desc "Self-contained macOS menu bar app for Claude Code proxying to ChatGPT Codex, DeepSeek, or custom OpenAI endpoints"
  homepage "https://github.com/soulforger0/cc-codex-proxy"

  depends_on arch: :arm64
  depends_on macos: :ventura

  app "CCCodexProxy.app"

  caveats do
    <<~EOS
      Releases are currently ad-hoc signed and not notarized. If macOS blocks
      first launch, right-click CCCodexProxy.app and choose Open.
    EOS
  end

  zap trash: [
    "~/Library/Application Support/CCCodexProxy",
    "~/Library/Logs/CCCodexProxy",
  ]
end
