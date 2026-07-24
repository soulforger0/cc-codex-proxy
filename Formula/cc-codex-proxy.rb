class CcCodexProxy < Formula
  desc "Local Claude Code proxy for ChatGPT Codex, DeepSeek, and custom OpenAI endpoints"
  homepage "https://github.com/soulforger0/cc-codex-proxy"
  url "https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v1.1.1.tar.gz"
  sha256 "095854f54143ce3280b1d73f19d3a9dd3c4185a694bb6d10988e3dfbbe23bfde"
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
