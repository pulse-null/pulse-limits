#!/usr/bin/env bash
# Shows or hides the PulseLimits monitor. SwiftBar runs this on a left-click of the menu bar
# item, Waybar on a click of the module (`pulse-limits open` from a terminal on either).
# macOS: the helper (bin/pulse-popover) stays resident for a while; we only launch it if it is not.
# Linux: no popover yet; the page opens in the browser through a one-line redirect file,
# because xdg-open drops the #fragment of a file:// URL, and the data travels in the fragment.
set -u
OS=$(uname -s)
if [[ "$OS" == "Darwin" ]]; then export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
else export PATH="${PATH:+$PATH:}/usr/local/bin:/usr/bin:/bin"; fi
SELF=$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")
HERE=$(dirname "$SELF")
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
BIN="$HERE/bin/pulse-popover"
URLFILE="$CACHE_DIR/panel.url"      # written by pulse-limits.1m.sh on every run
PIDFILE="$CACHE_DIR/popover.pid"    # written by the helper itself
STAMP="$CACHE_DIR/popover.closed"   # written by the helper when it hides
OPENER="$CACHE_DIR/open.html"       # Linux: redirects the browser to the panel URL, fragment intact
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
[[ -f "$URLFILE" ]] || { echo "no reading yet: run pulse-limits status first" >&2; exit 0; }
if [[ "$OS" == "Darwin" && -x "$BIN" ]]; then
  "$BIN" "$W" "$H" >/dev/null 2>&1 &  # first launch shows itself
  disown
elif [[ "$OS" == "Darwin" ]]; then
  open "$(cat "$URLFILE")"           # helper not built: at least show the page
else
  url=$(cat "$URLFILE")
  printf '<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0;url=%s"><title>PulseLimits</title><a href="%s">PulseLimits</a>\n' "$url" "$url" > "$OPENER"
  xdg-open "$OPENER" >/dev/null 2>&1 &
  disown
fi
