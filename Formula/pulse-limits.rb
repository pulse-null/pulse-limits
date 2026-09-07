class PulseLimits < Formula
  desc "Claude plan limits in the menu bar, as a retro patient monitor (SwiftBar)"
  homepage "https://github.com/dnacenta/pulse-limits"
  url "https://github.com/dnacenta/pulse-limits/archive/refs/tags/v0.4.0.tar.gz"
  sha256 "314f227528ae345e04f555c3f0489982667efad30ecc961080db3a5552365e69"
  license "AGPL-3.0-or-later"

  depends_on "jq"
  depends_on :macos

  def install
    # Homebrew already requires the Command Line Tools, which ship swiftc.
    system "swiftc", "-O", "popover/PulsePopover.swift", "-o", "pulse-popover"
    system "swiftc", "-O", "menubar/MenuBarImage.swift", "-o", "pulse-menubar"
    libexec.install "pulse-limits.1m.sh", "pulse-limits.5m.sh", "open-monitor.sh", "panel.html", "build.sh"
    (libexec/"bin").install "pulse-popover", "pulse-menubar"
    bin.install "pulse-limits"
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
    output = shell_output("#{libexec}/pulse-limits.1m.sh --theme nope 2>&1", 64)
    assert_match "unknown theme", output
    assert_match "install", shell_output("#{bin}/pulse-limits help")
  end
end
