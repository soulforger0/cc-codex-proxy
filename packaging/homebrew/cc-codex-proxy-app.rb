cask "cc-codex-proxy-app" do
  version "0.2.1"
  sha256 "PLACEHOLDER"

  url "https://github.com/soulforger0/cc-codex-proxy/releases/download/v#{version}/CCCodexProxy-#{version}-macOS.dmg"
  name "CC Codex Proxy"
  desc "Self-contained macOS menu bar app for Claude Code proxying to ChatGPT Codex or DeepSeek"
  homepage "https://github.com/soulforger0/cc-codex-proxy"

  app "CCCodexProxy.app"

  zap trash: [
    "~/Library/Application Support/CCCodexProxy",
    "~/Library/Logs/CCCodexProxy",
  ]
end
