#!/usr/bin/env bash
# <swiftbar.title>PulseLimits</swiftbar.title>
# <swiftbar.version>v0.4.0</swiftbar.version>
# <swiftbar.author>Daniel Nacenta</swiftbar.author>
# <swiftbar.desc>Your Claude plan limits as a retro patient monitor: the heart rate is your usage.</swiftbar.desc>
# <swiftbar.dependencies>bash,jq,curl</swiftbar.dependencies>
# <swiftbar.runInBash>false</swiftbar.runInBash>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>
#
# Compatibility shim: older installs linked this name into SwiftBar. The plugin now
# lives in pulse-limits.1m.sh (one-minute cadence). Run `pulse-limits install` to
# relink; until then this keeps working at the old five-minute cadence.
exec "$(dirname "$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")")/pulse-limits.1m.sh" "$@"
