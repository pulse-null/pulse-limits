#!/usr/bin/env bash
# <swiftbar.title>PulseLimits</swiftbar.title>
# <swiftbar.version>v0.5.1</swiftbar.version>
# <swiftbar.author>Daniel Nacenta</swiftbar.author>
# <swiftbar.desc>Your Claude, Codex and Grok plan limits as a retro patient monitor: the heart rate is your usage.</swiftbar.desc>
# <swiftbar.dependencies>bash</swiftbar.dependencies>
# <swiftbar.runInBash>false</swiftbar.runInBash>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>
#
# SwiftBar shim: the metadata above is all SwiftBar reads from this file. Everything else is
# the Rust binary next to it (bin/pulse-limits, built by ./build.sh): `swiftbar` prints the
# menu bar item and the menu, whose click actions run the binary directly (runInBash=false).
# SwiftBar calls the symlink, so the folder is found through it. Bash 3.2 as shipped by macOS.
exec "$(dirname "$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")")/bin/pulse-limits" swiftbar
