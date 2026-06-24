cask "cc-codex-proxy-app" do
  version "0.1.0"
  sha256 "PLACEHOLDER"

  url "https://github.com/soulforger0/cc-codex-proxy/releases/download/v#{version}/CCCodexProxy.zip"
  name "CC Codex Proxy"
  desc "macOS menu bar controller for cc-codex-proxy"
  homepage "https://github.com/soulforger0/cc-codex-proxy"

  app "CCCodexProxy.app"

  zap trash: [
    "~/Library/Application Support/CCCodexProxy",
    "~/Library/Logs/CCCodexProxy",
  ]
end

