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
# PulseLimits
# Reads the OAuth token Claude Code keeps in your Keychain, asks Anthropic's
# (undocumented) usage endpoint how much of your 5-hour and 7-day windows you
# have burned, and hands the numbers to panel.html: a CRT patient monitor
# whose ECG beats faster the more you have used.
#   left-click  -> the monitor (SwiftBar webview popover)
#   right-click -> plain text fallback menu
#   --waybar    -> one JSON line for a Waybar custom module (Linux)
# Runs every minute. The API is asked once per 5 min; in between, the session % is
# dead-reckoned from the tokens Claude Code produced since the last reading (see
# `pulse-popover --estimate`, or bin/pulse-activity.py where there is no Swift), so the
# menu bar moves with your usage.
# The only thing that leaves this machine is one GET to api.anthropic.com.
# Runs on the bash 3.2 that ships with macOS and on GNU userland: the BSD/GNU differences
# sit in epoch_of and mtime_of. runInBash=false makes SwiftBar exec this file and the click
# launcher directly instead of through `zsh -l -c`, which would pay your login profile
# (nvm, brew shellenv: ~0.5 s) on every run and every click.

set -u
OS=$(uname -s)
if [[ "$OS" == "Darwin" ]]; then
  export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"     # SwiftBar gives us launchd's PATH: pin our own
else
  export PATH="${PATH:+$PATH:}/usr/local/bin:/usr/bin:/bin"       # Waybar (or a Nix wrapper) hands us a PATH worth keeping
fi
export LC_ALL=C

USAGE_URL="https://api.anthropic.com/api/oauth/usage"
KEYCHAIN_SERVICE="Claude Code-credentials"
SELF=$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")   # SwiftBar calls the symlink
PANEL="$(dirname "$SELF")/panel.html"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CACHE="$CACHE_DIR/usage.json"
HISTORY="$CACHE_DIR/history.tsv"
MIN_INTERVAL=270       # seconds between live API calls (the plugin runs every minute, the API sees one call per 5)
THEMES="crt modern cyber synth analog"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
THEME_FILE="$CONFIG_DIR/theme"                     # chosen from the right-click menu; default crt
THEME=$(cat "$THEME_FILE" 2>/dev/null || echo crt)
MODE="${1:-}"          # --reset: drop the cache. --payload: fetch fresh, print JSON only. --waybar: Waybar JSON. --theme <name>: persist a theme. --activity: the helper's reading
[[ "$MODE" == "--payload" ]] && MIN_INTERVAL=45   # the popover asks every 2 min while open
BACKOFF="$CACHE_DIR/backoff"                       # written after a 429: no calls until this epoch
HISTORY_HOURS=12       # trend strip depth
POPOVER_W=520
POPOVER_H=316          # our own popover: 300 of screen + bezel. SwiftBar fallback adds its 32 px header
URLFILE="$CACHE_DIR/panel.url"           # read by open-monitor.sh on click
LAUNCHER="$(dirname "$SELF")/open-monitor.sh"
POPOVER_BIN="$(dirname "$SELF")/bin/pulse-popover"
MENUBAR_BIN="$(dirname "$SELF")/bin/pulse-menubar"
ACTIVITY_PY="$(dirname "$SELF")/bin/pulse-activity.py"   # --activity/--estimate in Python, for hosts without Swift

# --- palette: "light,dark" pairs for the text fallback menu --------------------
C_HEAD="#1c5f8a,#8fd3ff"
C_GREEN="#1E7F2A,#5FD75F"
C_AMBER="#A85E00,#FFB000"
C_RED="#B71C1C,#FF5C5C"
C_DIM="#707070,#8C8C8C"
MONO="font=Menlo size=12 trim=false"

# --- helpers ---------------------------------------------------------------------
line()  { printf '%s | %s\n' "$1" "$2"; }
sep()   { echo '---'; }
upper() { tr '[:lower:]' '[:upper:]'; }

bar() { # pct width -> "████░░░░"
  local pct=$1 width=$2 filled i out=""
  filled=$(( (pct * width + 50) / 100 ))
  (( filled > width )) && filled=$width
  (( filled < 0 )) && filled=0
  for (( i = 0; i < width; i++ )); do
    if (( i < filled )); then out="${out}█"; else out="${out}░"; fi
  done
  printf '%s' "$out"
}

tone() { local p=$1; if (( p >= 85 )); then echo "$C_RED"; elif (( p >= 60 )); then echo "$C_AMBER"; else echo "$C_GREEN"; fi; }

# BSD (macOS) and GNU (Linux) userlands differ in exactly these two places
epoch_of() { # ISO-8601 -> epoch. The API stamps resets_at in UTC as Z or +00:00; either is dropped and read as UTC
  local s="${1%%.*}"; s="${s%%+*}"; s="${s%Z}"
  if [[ "$OS" == "Darwin" ]]; then date -u -j -f "%Y-%m-%dT%H:%M:%S" "$s" +%s 2>/dev/null
  else date -u -d "$s" +%s 2>/dev/null; fi
}
mtime_of() { if [[ "$OS" == "Darwin" ]]; then stat -f %m "$1"; else stat -c %Y "$1"; fi; }

countdown() { # epoch -> "2H 14M" / "4D 07H" / "38M"
  local diff=$(( $1 - $(date +%s) )); (( diff < 0 )) && diff=0
  local d=$(( diff / 86400 )) h=$(( diff % 86400 / 3600 )) m=$(( diff % 3600 / 60 ))
  if (( d > 0 )); then printf '%dD %02dH' "$d" "$h"; elif (( h > 0 )); then printf '%dH %02dM' "$h" "$m"; else printf '%dM' "$m"; fi
}
short() { # seconds -> "45S" / "12M" / "3H" / "2D", as the panel writes ages
  local s=$1
  if (( s < 60 )); then printf '%dS' "$s"; elif (( s < 3600 )); then printf '%dM' "$(( s / 60 ))"
  elif (( s < 86400 )); then printf '%dH' "$(( s / 3600 ))"; else printf '%dD' "$(( s / 86400 ))"; fi
}
fmt_k() { # tokens -> "850" / "3.4K" / "12K", as the panel writes rates
  local n=$1
  if (( n >= 10000 )); then printf '%dK' "$(( (n + 500) / 1000 ))"
  elif (( n >= 1000 )); then printf '%d.%dK' "$(( (n + 50) / 1000 ))" "$(( (n + 50) % 1000 / 100 ))"
  else printf '%d' "$n"; fi
}
win_text() { # label pct iso -> "SESSION  ████░░░░░░░░░░░░░░░░  17%   RESETS IN 2H 14M"
  local e when="?"; e=$(epoch_of "$3"); [[ -n "$e" ]] && when=$(countdown "$e")
  printf '%-8s %s %3d%%   RESETS IN %s' "$1" "$(bar "$2" 20)" "$2" "$when"
}

# activity and dead reckoning: the Swift helper where it was built (a Mach-O, so macOS only,
# even in a checkout shared with a Linux box), else the Python port
have_helper() { [[ ( "$OS" == "Darwin" && -x "$POPOVER_BIN" ) || -f "$ACTIVITY_PY" ]]; }
helper() { # --activity | --estimate PCT FETCHED
  if [[ "$OS" == "Darwin" && -x "$POPOVER_BIN" ]]; then "$POPOVER_BIN" "$@"
  elif [[ -x "$ACTIVITY_PY" ]]; then "$ACTIVITY_PY" "$@"
  elif [[ -f "$ACTIVITY_PY" ]]; then python3 "$ACTIVITY_PY" "$@"
  else return 1; fi
}

# --- force a live call on the next run (Option-click the header, or --reset) ----
if [[ "$MODE" == "--reset" ]]; then rm -f "$CACHE" "$CACHE_DIR/backoff"; exit 0; fi
# --- pick a theme (the right-click menu calls this, then SwiftBar refreshes us) -------
if [[ "$MODE" == "--theme" ]]; then
  case " $THEMES " in *" ${2:-} "*) mkdir -p "$CONFIG_DIR"; printf '%s\n' "$2" > "$THEME_FILE"; exit 0 ;; esac
  echo "unknown theme: ${2:-} (one of: $THEMES)" >&2; exit 64
fi
# --- one activity reading (the doctor asks for it) ----------------------------------------
if [[ "$MODE" == "--activity" ]]; then helper --activity; exit; fi

mkdir -p "$CACHE_DIR"
now=$(date +%s)
status=""; hint=""       # status = short error name, hint = what to do about it

# --- 1. credentials: the claude.ai login Claude Code keeps ------------------------------
# macOS: in the Keychain. Claude Code keys the entry to CLAUDE_CONFIG_DIR, so a Mac can hold
# several "Claude Code-credentials…" entries and only some carry a claude.ai login. SwiftBar
# does not see shell variables, so we scan for them. A pinned service name wins.
# Linux: in ${CLAUDE_CONFIG_DIR:-~/.claude}/.credentials.json (mode 0600), the file that is
# the fallback on macOS.
CRED_PIN="$CONFIG_DIR/keychain"
CREDS_FILE="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"   # Claude Code's file fallback
has_login() { printf '%s' "$1" | jq -e '(.claudeAiOauth.accessToken // "") != ""' >/dev/null 2>&1; }
keychain_items() {   # "service<TAB>account" for every item whose service or label mentions Claude; pinned first
  local pin; pin=$(cat "$CRED_PIN" 2>/dev/null || true); [[ -n "$pin" ]] && printf '%s\t\n' "$pin"
  printf '%s\t\n' "$KEYCHAIN_SERVICE"
  security dump-keychain 2>/dev/null | awk -F'"' '
    /^keychain:/ { svc = ""; acct = ""; labl = "" }
    /"acct"<blob>=/ { acct = $4 }
    /"labl"<blob>=/ { labl = $4 }
    /"svce"<blob>=/ { svc = $4; if (tolower(svc) ~ /claude/ || tolower(labl) ~ /claude/) print svc "\t" acct }'
}
read_item() {        # service account -> the item's secret (account may be empty: first match)
  if [[ -n "${2:-}" ]]; then security find-generic-password -s "$1" -a "$2" -w 2>/dev/null
  else security find-generic-password -s "$1" -w 2>/dev/null; fi
}
find_creds() {
  local svc acct json seen=" " f
  if [[ "$OS" == "Darwin" ]]; then
    while IFS=$'\t' read -r svc acct; do
      [[ -n "$svc" && "$seen" != *"|$svc/$acct|"* ]] || continue; seen="$seen|$svc/$acct| "
      json=$(read_item "$svc" "$acct") || continue
      has_login "$json" && { printf '%s' "$json"; return 0; }
    done < <(keychain_items)
  fi
  for f in "$CREDS_FILE" "$HOME/.claude/.credentials.json"; do
    [[ -f "$f" ]] || continue
    json=$(cat "$f"); has_login "$json" && { printf '%s' "$json"; return 0; }
  done
  return 1
}
token=""; plan="?"; tier="?"
if creds=$(find_creds); then
  token=$(printf '%s' "$creds" | jq -r '.claudeAiOauth.accessToken // empty')
  plan=$(printf '%s' "$creds" | jq -r '.claudeAiOauth.subscriptionType // "?"')
  tier=$(printf '%s' "$creds" | jq -r '.claudeAiOauth.rateLimitTier // "?"')
fi
if [[ -z "$token" ]]; then
  status="NO LOGIN"; hint="NO CLAUDE.AI LOGIN FOUND. RUN: pulse-limits doctor"
fi
plan_label=$(printf '%s' "$tier" | sed 's/^default_claude_//; s/_/ /g' | upper)
[[ "$tier" == "?" ]] && plan_label=$(printf '%s' "$plan" | upper | sed 's/^?$//')

# --- 2. usage: fresh cache, else one live call ------------------------------------
source="CACHE"
cache_age=999999
[[ -f "$CACHE" ]] && cache_age=$(( now - $(mtime_of "$CACHE") ))

backoff_until=$(cat "$BACKOFF" 2>/dev/null || echo 0)
if [[ -n "$token" ]] && (( cache_age > MIN_INTERVAL )) && (( now >= backoff_until )); then
  tmp=$(mktemp "$CACHE_DIR/usage.XXXXXX")
  code=$(curl -sS -m 15 -o "$tmp" -w '%{http_code}' \
           -H "Authorization: Bearer $token" \
           -H "anthropic-beta: oauth-2025-04-20" \
           -H "User-Agent: pulse-limits/0.2" \
           "$USAGE_URL" 2>/dev/null) || code=000
  # keep whatever came back so `pulse-limits raw` can show a failure, not just a success
  { printf '{"http": %s, "at": %s, "body": ' "${code:-0}" "$now"; jq -c . "$tmp" 2>/dev/null || jq -Rs . "$tmp" 2>/dev/null || printf '""'; printf '}\n'; } > "$CACHE_DIR/last-reply.json" 2>/dev/null
  case "$code" in
    200) if jq -e '(.five_hour != null) or ((.limits // []) | length > 0)' "$tmp" >/dev/null 2>&1; then
           mv -f "$tmp" "$CACHE"; source="LIVE"; cache_age=0
           # trend history: one row per live reading, pruned to HISTORY_HOURS
           jq -r --arg now "$now" '
             def lim(k): (first(.limits[]? | select(.kind == k)) // null);
             def pct(k; legacy): (if lim(k) != null then (lim(k).percent // legacy.utilization // 0) elif legacy != null then (legacy.utilization // 0) else 0 end);
             [$now, (pct("session"; .five_hour) + 0.5 | floor), (pct("weekly_all"; .seven_day) + 0.5 | floor)] | @tsv' \
              "$CACHE" >> "$HISTORY"
           awk -F'\t' -v cut="$(( now - HISTORY_HOURS * 3600 ))" '$1 >= cut' "$HISTORY" > "$HISTORY.tmp" && mv -f "$HISTORY.tmp" "$HISTORY"
         else status="BAD RESPONSE"; hint="THE USAGE ENDPOINT CHANGED SHAPE"; fi ;;
    401)     status="TOKEN EXPIRED"; hint="OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN" ;;
    403)     status="NO PLAN ACCESS"; hint="LOG IN TO CLAUDE CODE WITH A CLAUDE.AI PLAN, NOT AN API KEY" ;;
    404)     status="NO USAGE DATA";  hint="THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW" ;;
    429)     echo $(( now + 180 )) > "$BACKOFF"                 # the account quota is small and shared across Macs: pause 3 min
             if (( cache_age > 300 )); then status="RATE LIMITED"; hint="TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN"; fi ;;
    000)     status="NETWORK";       hint="COULD NOT REACH API.ANTHROPIC.COM" ;;
    *)       status="HTTP $code";    hint="UNEXPECTED ANSWER FROM API.ANTHROPIC.COM" ;;
  esac
  rm -f "$tmp"
fi

have_data=0; [[ -f "$CACHE" ]] && have_data=1

# --- 4. payload for the monitor: JSON, base64, in the URL fragment ------------------
hist_json="[]"
[[ -s "$HISTORY" ]] && hist_json=$(awk -F'\t' 'BEGIN{printf "["} {printf "%s[%s,%s]", (NR>1?",":""), $1, $2} END{printf "]"}' "$HISTORY")
activity_json=$(helper --activity 2>/dev/null) || activity_json=null
[[ -n "$activity_json" ]] || activity_json=null
if (( have_data )); then
  payload=$(jq -c --arg plan "$plan_label" --arg source "$source" --arg status "$status" --arg hint "$hint" --arg theme "$THEME" \
               --argjson fetched "$(( now - cache_age ))" --argjson history "$hist_json" --argjson activity "$activity_json" '
    # Newer replies carry a limits[] array (kind: session / weekly_all / weekly_scoped); older
    # ones the five_hour / seven_day blocks. Read whichever is there, limits[] first.
    def lim(k): (first(.limits[]? | select(.kind == k)) // null);
    def win(name; k; legacy):
      (if lim(k) != null then { label: name, pct: (lim(k).percent // legacy.utilization // 0), resets: (lim(k).resets_at // legacy.resets_at) }
       elif legacy != null then { label: name, pct: (legacy.utilization // 0), resets: legacy.resets_at }
       else empty end);
    ([ win("SESSION"; "session"; .five_hour), win("WEEK"; "weekly_all"; .seven_day) ]
     + [ .limits[]? | select(.kind == "weekly_scoped")
         | { label: ((.scope.model.display_name // .scope.surface // "SCOPED") | ascii_upcase),
             pct: (.percent // 0), resets: .resets_at } ]) as $w
    | { plan: $plan, source: $source, theme: $theme, fetched: $fetched, history: $history, activity: $activity,
        status: (if ($w | length) == 0 and $status == "" then "NO LIMITS IN REPLY" else $status end),
        hint:   (if ($w | length) == 0 and $hint == "" then "THE USAGE REPLY HAD NO WINDOWS. RUN: pulse-limits doctor" else $hint end),
        windows: $w,
        credits: (.extra_usage | if (.is_enabled == true) and ((.used_credits // 0) > 0)
                                 then { used: .used_credits, currency: (.currency // "") } else null end) }' "$CACHE")
else
  payload=$(jq -cn --arg plan "$plan_label" --arg status "${status:-NO DATA}" --arg hint "$hint" --arg theme "$THEME" \
               '{ plan: $plan, source: "", status: $status, hint: $hint, theme: $theme, fetched: 0, history: [], windows: [], credits: null }')
fi
s_pct=0; s_reset="-"; w_pct=0; w_reset="-"; s_api=0
read -r s_api s_reset w_pct w_reset < <(printf '%s' "$payload" | jq -r '
  def w(l): (first(.windows[] | select(.label == l)) // { pct: 0, resets: null });
  [ (w("SESSION").pct), (w("SESSION").resets // "-"), (w("WEEK").pct + 0.5 | floor), (w("WEEK").resets // "-") ] | @tsv')
[[ $(printf '%s' "$payload" | jq '.windows | length') -gt 0 ]] || have_data=0
# dead reckoning between API readings: anchor + k * tokens produced since, calibrated by the helper
estimate_json=null
if (( have_data )) && have_helper; then
  estimate_json=$(helper --estimate "$s_api" "$(( now - cache_age ))" 2>/dev/null) || estimate_json=null
  [[ -n "$estimate_json" ]] || estimate_json=null
  payload=$(printf '%s' "$payload" | jq -c --argjson e "$estimate_json" '. + { estimate: $e }')
fi
s_pct=$(printf '%s' "$estimate_json" | jq -r --argjson api "$s_api" 'if . != null and .calibrated then .pct_est else $api end | . + 0.5 | floor')
b64=$(printf '%s' "$payload" | base64 | tr -d '\n')
printf 'file://%s#%s' "$PANEL" "$b64" > "$URLFILE"
if [[ "$MODE" == "--payload" ]]; then printf '%s\n' "$payload"; exit 0; fi

# --- Waybar custom module (Linux): one JSON line. Waybar picks the ring glyph from its own
# format-icons by `percentage`; the tone travels as a CSS class, as the panel names them.
if [[ "$MODE" == "--waybar" ]]; then
  if (( have_data )); then
    label="${s_pct}%"; [[ -n "$status" ]] && label="${label}!"
    class=ok; (( s_pct >= 60 )) && class=warn; (( s_pct >= 85 )) && class=crit; [[ -n "$status" ]] && class=stale
  else
    label="--"; class=dead
  fi
  tip="PULSE LIMITS"; [[ -n "$plan_label" ]] && tip="$tip  ·  $plan_label"
  while IFS=$'\t' read -r name pct iso; do
    [[ -n "${name:-}" ]] && tip="$tip"$'\n'"$(win_text "$(printf '%.8s' "$name")" "$pct" "$iso")"
  done < <(printf '%s' "$payload" | jq -r '.windows[] | [ .label, (.pct + 0.5 | floor), (.resets // "-") ] | @tsv')
  credits=$(printf '%s' "$payload" | jq -r '.credits | if . then "EXTRA    \(.used) \(.currency)" else empty end')
  [[ -n "$credits" ]] && tip="$tip"$'\n'"$credits"
  a_tok=""; a_idle=0; a_sess=0
  read -r a_tok a_idle a_sess < <(printf '%s' "$activity_json" | jq -r 'if type == "object" and (.tok_per_min|type) == "number" then [.tok_per_min, .idle_s, (.sessions // 0)] | @tsv else empty end')
  if [[ -n "$a_tok" ]]; then
    if (( a_tok > 0 )); then act="$(fmt_k "$a_tok") TOK/MIN"; (( a_sess > 1 )) && act="$act · $a_sess SESSIONS"; else act="IDLE $(short "$a_idle")"; fi
    tip="$tip"$'\n'"$act"
  fi
  if (( have_data )); then
    s_api_r=$(printf '%s' "$s_api" | jq '. + 0.5 | floor')
    (( s_pct != s_api_r )) && tip="$tip"$'\n'"SESSION NOW ${s_pct}% · EST"
    tip="$tip"$'\n'"UPDATED $(short "$cache_age") AGO · $source"
  fi
  while IFS= read -r l; do [[ -n "$l" ]] && tip="$tip"$'\n'"$l"; done < <(printf '%s' "$payload" | jq -r '(if .status != "" then "? \(.status)" else empty end), (.hint // empty)')
  jq -cn --arg text "$label" --arg tip "$tip" --arg class "$class" --argjson pct "$s_pct" \
     '{ text: $text, tooltip: $tip, class: $class, percentage: $pct }
      | .tooltip |= (gsub("&"; "&amp;") | gsub("<"; "&lt;") | gsub(">"; "&gt;"))'   # Waybar reads it as Pango markup
  exit 0
fi

# --- 5. menu bar line: "10%" then a ring that fills with it, like the battery item ---
if (( have_data )); then
  label="${s_pct}%"; tcolor=$(tone "$s_pct"); ring_pct=$s_pct
  [[ -n "$status" ]] && { label="${label}!"; tcolor=$C_RED; }
else
  label="--"; tcolor=$C_RED; ring_pct=0
fi
title="● $label"; img=""
if [[ -x "$MENUBAR_BIN" ]]; then
  # one PNG per menu bar appearance: text and arc in the tone colour, a neutral track
  read -r iw ih img_light < <("$MENUBAR_BIN" "$ring_pct" "$label" "${tcolor%%,*}" "${tcolor%%,*}" "#c9ced2")
  read -r _ _ img_dark < <("$MENUBAR_BIN" "$ring_pct" "$label" "${tcolor##*,}" "${tcolor##*,}" "#3a4044")
  if [[ -n "${img_light:-}" && -n "${img_dark:-}" ]]; then
    title=""; img="image=$img_light,$img_dark width=$iw height=$ih"
  fi
fi
if [[ -x "$POPOVER_BIN" ]]; then
  action="bash=$LAUNCHER terminal=false"
else
  action="webview=true webvieww=$POPOVER_W webviewh=$(( POPOVER_H + 32 )) href=file://$PANEL#$b64"
fi
line "$title" "$MONO color=$tcolor $img $action"
sep

# --- 6. text fallback (right-click) -------------------------------------------------
hdr="PULSE LIMITS"
[[ -n "$plan_label" ]] && hdr="$hdr  ·  $plan_label"
line "$hdr" "$MONO color=$C_HEAD"
line 'FORCE REFRESH' "$MONO color=$C_GREEN alternate=true bash=$SELF param1=--reset terminal=false refresh=true"
line "THEME  ·  $(printf '%s' "$THEME" | upper)" "$MONO color=$C_DIM"
for th in $THEMES; do
  checked=false; [[ "$th" == "$THEME" ]] && checked=true
  line "--$(printf '%s' "$th" | upper)" "$MONO color=$C_GREEN checked=$checked bash=$SELF param1=--theme param2=$th terminal=false refresh=true"
done
if [[ -n "$status" ]]; then
  line "?$status  ERROR" "$MONO color=$C_RED"
  [[ -n "$hint" ]] && line "$hint" "$MONO color=$C_DIM"
fi
if (( have_data )); then
  sep
  while IFS=$'\t' read -r name pct iso; do
    [[ -n "${name:-}" ]] && line "$(win_text "$(printf '%.8s' "$name")" "$pct" "$iso")" "$MONO color=$(tone "$pct")"
  done < <(printf '%s' "$payload" | jq -r '.windows[] | [ .label, (.pct + 0.5 | floor), (.resets // "-") ] | @tsv')
fi
