#!/usr/bin/env bash
# Shows or hides the PulseLimits monitor. SwiftBar runs this on a left-click of the menu bar item.
# The helper (bin/pulse-popover) stays resident for a while; we only launch it if it is not.
set -u
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
SELF=$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")
HERE=$(dirname "$SELF")
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
BIN="$HERE/bin/pulse-popover"
URLFILE="$CACHE_DIR/panel.url"      # written by pulse-limits.1m.sh on every run
PIDFILE="$CACHE_DIR/popover.pid"    # written by the helper itself
STAMP="$CACHE_DIR/popover.closed"   # written by the helper when it hides
W=520; H=316

# The click that ran us may have just hidden the monitor (its outside-click
# monitor fires before SwiftBar runs us). Do not bounce it straight back open.
if [[ -f "$STAMP" ]]; then
  closed=$(cut -d. -f1 "$STAMP"); rm -f "$STAMP"
  (( $(date +%s) - closed <= 1 )) && exit 0
fi
if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  kill -USR1 "$(cat "$PIDFILE")"      # resident: toggle
  exit 0
fi
[[ -f "$URLFILE" ]] || exit 0
if [[ -x "$BIN" ]]; then
  "$BIN" "$W" "$H" >/dev/null 2>&1 &  # first launch shows itself
  disown
else
  open "$(cat "$URLFILE")"           # helper not built: at least show the page
fi
