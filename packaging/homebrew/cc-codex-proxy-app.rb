cask "cc-codex-proxy-app" do
  version "0.4.2"
  sha256 "a67b31961a18b4c32364e8c50103ed201a7789da603e4cfa71748b67d8d3d079"

  url "https://github.com/soulforger0/cc-codex-proxy/releases/download/v#{version}/CCCodexProxy-#{version}-macOS.dmg"
  name "CC Codex Proxy"
  desc "Self-contained macOS menu bar app for Claude Code proxying to ChatGPT Codex, DeepSeek, or custom OpenAI endpoints"
  homepage "https://github.com/soulforger0/cc-codex-proxy"

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
