cask "cc-codex-proxy-app" do
  version "1.1.0"
  sha256 "3537b77bbd0d0daec7fd160d88af3146fe73cd571cc335cf116a4492fe6531cc"

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
