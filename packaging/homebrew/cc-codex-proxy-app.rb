cask "cc-codex-proxy-app" do
  version "1.0.0"
  sha256 "ab80b695a1945f1f8e60ad679b6ac5b2303bd164a752992fe9d67a57179ff361"

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
