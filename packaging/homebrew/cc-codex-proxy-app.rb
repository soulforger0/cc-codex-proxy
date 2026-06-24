cask "cc-codex-proxy-app" do
  version "0.1.0"
  sha256 "PLACEHOLDER"

  url "https://github.com/soulforger0/cc-codex-proxy/releases/download/v#{version}/CCCodexProxy.zip"
  name "CC Codex Proxy"
  desc "Self-contained macOS menu bar app for Claude Code to ChatGPT Codex proxying"
  homepage "https://github.com/soulforger0/cc-codex-proxy"

  app "CCCodexProxy.app"

  zap trash: [
    "~/Library/Application Support/CCCodexProxy",
    "~/Library/Logs/CCCodexProxy",
  ]
end
