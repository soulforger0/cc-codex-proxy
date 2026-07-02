cask "cc-codex-proxy-app" do
  version "0.3.0"
  sha256 "07a1de3df5372119f31a658c8bc0b3c95f97ec38f3f7bc032576db65fa88cb87"

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
