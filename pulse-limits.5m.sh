#!/usr/bin/env bash
# <swiftbar.title>PulseLimits</swiftbar.title>
# <swiftbar.version>v0.5.4</swiftbar.version>
# <swiftbar.author>Daniel Nacenta</swiftbar.author>
# <swiftbar.desc>Your Claude, Codex and Grok plan limits as a retro patient monitor: the heart rate is your usage.</swiftbar.desc>
# <swiftbar.dependencies>bash</swiftbar.dependencies>
# <swiftbar.runInBash>false</swiftbar.runInBash>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>
#
# Compatibility shim: older installs linked this name into SwiftBar. The plugin now runs at
# a one-minute cadence from pulse-limits.1m.sh; `pulse-limits install` relinks. Until then
# this keeps working at the old five-minute cadence.
exec "$(dirname "$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")")/bin/pulse-limits" swiftbar
