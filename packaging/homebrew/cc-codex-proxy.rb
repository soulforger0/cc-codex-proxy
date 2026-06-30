class CcCodexProxy < Formula
  desc "Local Claude Code proxy for ChatGPT Codex, DeepSeek, and custom OpenAI endpoints"
  homepage "https://github.com/soulforger0/cc-codex-proxy"
  url "https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v0.2.2.tar.gz"
  sha256 "PLACEHOLDER"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", "crates/cc-codex-proxy", "--root", prefix
  end

  service do
    run [opt_bin/"cc-codex-proxy", "serve"]
    keep_alive true
    log_path var/"log/cc-codex-proxy.log"
    error_log_path var/"log/cc-codex-proxy.err.log"
  end

  test do
    assert_match "cc-codex-proxy", shell_output("#{bin}/cc-codex-proxy --help")
  end
end
