class PulseLimits < Formula
  desc "Claude, Codex and Grok plan limits, as a retro patient monitor (SwiftBar)"
  homepage "https://github.com/pulse-null/pulse-limits"
  url "https://github.com/pulse-null/pulse-limits/archive/refs/tags/v0.5.6.tar.gz"
  sha256 "0300cbdc1cd7c0d6717b744ea1fbfb83aaf0adf7d2290f2dff0795a83be3d3a6"
  license "AGPL-3.0-or-later"

  depends_on "rust" => :build
  depends_on :macos

  def install
    # Homebrew already requires the Command Line Tools, which ship swiftc.
    system "swiftc", "-O", "popover/PulsePopover.swift", "-o", "pulse-popover"
    system "swiftc", "-O", "menubar/MenuBarImage.swift", "-o", "pulse-menubar"
    # the binary finds panel.html and the shims one folder up from itself: libexec/bin -> libexec
    system "cargo", "install", *std_cargo_args(root: libexec)
    libexec.install "pulse-limits.1m.sh", "pulse-limits.5m.sh", "panel.html", "build.sh"
    (libexec/"bin").install "pulse-popover", "pulse-menubar"
    bin.install_symlink libexec/"bin/pulse-limits"
  end

  def caveats
    <<~EOS
      PulseLimits is a SwiftBar plugin. Install SwiftBar if you have not yet:
        brew install --cask swiftbar
      Then link the plugin into SwiftBar and start it:
        pulse-limits install
      It reads the login Claude Code keeps in your Keychain; run `claude` once first.
    EOS
  end

  test do
    output = shell_output("#{bin}/pulse-limits theme nope 2>&1", 64)
    assert_match "unknown theme", output
    assert_match "install", shell_output("#{bin}/pulse-limits help")
    assert_equal "v#{version}", shell_output("#{bin}/pulse-limits version").strip
  end
end
