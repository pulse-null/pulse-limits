#!/usr/bin/env bash
# Shared plumbing for the provider readers in this folder. Each providers/<name>.sh
# sets PROVIDER and sources this file; it is never run on its own.
#
# A provider prints ONE JSON document on stdout and always exits 0:
#   { "provider": "claude", "plan": "MAX 20X", "status": "", "hint": "",
#     "source": "LIVE" | "CACHE" | "", "fetched": <epoch of the reading, 0 if none>,
#     "windows": [ { "label": "SESSION", "pct": 17, "resets": "<iso8601>" | null }, ... ],
#     "credits": { "used": 1.5, "currency": "EUR" } | null,
#     "history": [ [epoch, session_pct], ... ] }
# The window labelled SESSION is the short one the menu bar shows, WEEK the 7-day
# one; anything else is a per-model cap. Errors go in status (a short name) and
# hint (what to do about it), never in the exit code, so one broken provider
# cannot take the others down. `<name>.sh --doctor [STATUS]` prints ok/PROBLEM
# lines about the credentials and the last reply instead, without the token.
#
# Per provider, in the cache dir: usage-<name>.json (last good reply),
# last-reply-<name>.json (last reply of any kind), backoff-<name> (no calls
# until this epoch, written after a 429), history-<name>.tsv (trend rows).
set -u
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export LC_ALL=C
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
MIN_INTERVAL="${PL_MIN_INTERVAL:-270}"   # seconds between live calls; the plugin lowers it while the panel is open
HISTORY_HOURS=12                          # trend strip depth
mkdir -p "$CACHE_DIR"
now=$(date +%s)
CACHE="$CACHE_DIR/usage-$PROVIDER.json"
LAST_REPLY="$CACHE_DIR/last-reply-$PROVIDER.json"
BACKOFF="$CACHE_DIR/backoff-$PROVIDER"
HISTORY="$CACHE_DIR/history-$PROVIDER.tsv"
# Installs from before the providers split kept Claude's files without a suffix: adopt them once.
if [[ "$PROVIDER" == claude ]]; then
  for pair in usage.json:usage-claude.json last-reply.json:last-reply-claude.json backoff:backoff-claude history.tsv:history-claude.tsv; do
    [[ -f "$CACHE_DIR/${pair%%:*}" && ! -e "$CACHE_DIR/${pair##*:}" ]] && mv "$CACHE_DIR/${pair%%:*}" "$CACHE_DIR/${pair##*:}"
  done
fi
status=""; hint=""; source="CACHE"
cache_age=999999
[[ -f "$CACHE" ]] && cache_age=$(( now - $(stat -f %m "$CACHE") ))

upper() { tr '[:lower:]' '[:upper:]'; }
ok()  { printf '  ok       %s\n' "$*"; }   # doctor lines, same shape as the pulse-limits CLI prints
bad() { printf '  PROBLEM  %s\n' "$*"; }

# A live call is due when the cache is older than MIN_INTERVAL and no backoff is running.
pl_due() { (( cache_age > MIN_INTERVAL )) && (( now >= $(cat "$BACKOFF" 2>/dev/null || echo 0) )); }
pl_backing_off() { (( now < $(cat "$BACKOFF" 2>/dev/null || echo 0) )); }

# pl_get URL [curl args...]: one GET. Leaves the body in $PL_BODY (a temp file the
# caller removes) and the HTTP code in $PL_CODE (000 = no answer). Keeps a copy in
# last-reply-<name>.json so `pulse-limits raw` can show a failure, not only a success.
pl_get() {
  local url=$1; shift
  PL_BODY=$(mktemp "$CACHE_DIR/$PROVIDER.XXXXXX")
  PL_CODE=$(curl -sS -m 15 -o "$PL_BODY" -w '%{http_code}' -H "User-Agent: pulse-limits" "$@" "$url" 2>/dev/null) || PL_CODE=000
  { printf '{"http": %s, "at": %s, "body": ' "${PL_CODE:-0}" "$now"
    jq -c . "$PL_BODY" 2>/dev/null || jq -Rs . "$PL_BODY" 2>/dev/null || printf '""'
    printf '}\n'; } > "$LAST_REPLY" 2>/dev/null
}
pl_accept()  { mv -f "$PL_BODY" "$CACHE"; source="LIVE"; cache_age=0; }
pl_backoff() { echo $(( now + $1 )) > "$BACKOFF"; }   # seconds to wait

# Trend: one row per live reading (epoch, session %, week %), pruned to HISTORY_HOURS.
pl_history_add() {   # windows json
  printf '%s' "$1" | jq -r --arg now "$now" '
    def w(l): (first(.[] | select(.label == l)) // { pct: 0 });
    [$now, (w("SESSION").pct + 0.5 | floor), (w("WEEK").pct + 0.5 | floor)] | @tsv' >> "$HISTORY"
  awk -F'\t' -v cut="$(( now - HISTORY_HOURS * 3600 ))" '$1 >= cut' "$HISTORY" > "$HISTORY.tmp" && mv -f "$HISTORY.tmp" "$HISTORY"
}
pl_history_json() {
  if [[ -s "$HISTORY" ]]; then awk -F'\t' 'BEGIN{printf "["} {printf "%s[%s,%s]", (NR>1?",":""), $1, $2} END{printf "]"}' "$HISTORY"
  else printf '[]'; fi
}

# pl_emit PLAN WINDOWS_JSON CREDITS_JSON: print the document from $status/$hint/$source.
pl_emit() {
  local fetched=0
  if [[ -f "$CACHE" ]]; then fetched=$(( now - cache_age )); else source=""; fi
  # nothing cached and a backoff running: say so instead of a bare NO DATA
  [[ -z "$status" && ! -f "$CACHE" ]] && pl_backing_off && { status="RATE LIMITED"; hint="BACKING OFF AFTER A 429, RETRYING IN A FEW MINUTES"; }
  [[ "$source" == LIVE ]] && pl_history_add "$2"
  jq -cn --arg provider "$PROVIDER" --arg plan "$1" --arg status "$status" --arg hint "$hint" --arg source "$source" \
         --argjson fetched "$fetched" --argjson windows "$2" --argjson credits "$3" --argjson history "$(pl_history_json)" '
    ($windows | length == 0) as $empty
    | { provider: $provider, plan: $plan, source: $source, fetched: $fetched,
        status: (if $empty and $status == "" then (if $source == "" then "NO DATA" else "NO LIMITS IN REPLY" end) else $status end),
        hint:   (if $empty and $hint == "" and $source != "" then "THE USAGE REPLY HAD NO WINDOWS. RUN: pulse-limits doctor" else $hint end),
        windows: $windows, credits: $credits, history: $history }'
}

# Doctor helpers shared by the readers.
pl_doctor_digests() {
  [[ -f "$LAST_REPLY" ]] && ok "last attempt: $(jq -c '{http, at: (.at|todate), body: (.body | if type == "object" then {keys: (keys|join(",")), error: .error} else (tostring | .[0:200]) end)}' "$LAST_REPLY" 2>/dev/null)"
  if [[ -f "$CACHE" ]]; then ok "cached reply ($(( cache_age / 60 )) min old): $(jq -c "$1" "$CACHE" 2>/dev/null || echo 'not JSON')"
  else bad "no reply cached yet"; fi
}
