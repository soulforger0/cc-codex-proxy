class CcCodexProxy < Formula
  desc "Local Claude Code proxy for ChatGPT Codex, DeepSeek, and custom OpenAI endpoints"
  homepage "https://github.com/soulforger0/cc-codex-proxy"
  url "https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v0.5.1.tar.gz"
  sha256 "239a6f5892900ac76c846a73d8e8c71e7732fcdb342e85577048e5d170800290"
  license "MIT"

  depends_on "rust" => :build
  depends_on macos: :ventura

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/cc-codex-proxy")
  end

  service do
    run [opt_bin/"cc-codex-proxy", "serve"]
    keep_alive true
    log_path var/"log/cc-codex-proxy.log"
    error_log_path var/"log/cc-codex-proxy.err.log"
  end

  test do
    assert_match "Local Claude Code to ChatGPT Codex proxy", shell_output("#{bin}/cc-codex-proxy --help")
  end
end
