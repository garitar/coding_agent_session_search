class CodingAgentSearch < Formula
  desc "Unified TUI search over local coding agent histories"
  homepage "https://github.com/coding-agent-search/coding-agent-search"
  version "0.1.27"
  url "https://github.com/Dicklesworthstone/coding_agent_session_search/releases/download/v0.1.27/coding-agent-search-v0.1.27-linux-x86_64.tar.gz"
  sha256 "2a442a1fbff0f25841b2768e2578a66f5e1ea901a774443cf44742048e57d564"
  license "MIT"

  def verify_checksum_placeholder!
    if stable.checksum.to_s.include?("REPLACE_WITH_REAL_SHA256")
      odie "Formula checksum placeholder detected; update to real SHA256 before publishing."
    end
  end

  def install
    verify_checksum_placeholder!
    bin.install "coding-agent-search"
    generate_completions_from_executable(bin/"coding-agent-search", "completions", shells: [:bash, :zsh, :fish])
    man1.install buildpath/"coding-agent-search.1" if File.exist?("coding-agent-search.1")
  end

  test do
    system "#{bin}/coding-agent-search", "--help"
  end
end
