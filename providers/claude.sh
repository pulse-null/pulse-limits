#!/usr/bin/env bash
# Claude: the claude.ai login Claude Code keeps in your Keychain, and Anthropic's
# (undocumented) usage endpoint, the call behind /usage in Claude Code. The only
# thing that leaves this Mac is one GET to api.anthropic.com. See _common.sh for
# the document this prints and the --doctor mode.
PROVIDER=claude
. "$(dirname "$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")")/_common.sh"
USAGE_URL="https://api.anthropic.com/api/oauth/usage"
KEYCHAIN_SERVICE="Claude Code-credentials"
CRED_PIN="$CONFIG_DIR/keychain"                                       # `pulse-limits keychain NAME` writes this
CREDS_FILE="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"   # Claude Code's file fallback

# --- 1. credentials ------------------------------------------------------------------
# Claude Code keys the Keychain entry to CLAUDE_CONFIG_DIR, so a Mac can hold several
# "Claude Code-credentials..." entries and only some carry a claude.ai login. SwiftBar does
# not see shell variables, so we scan for them. A pinned service name wins.
has_login() { printf '%s' "$1" | jq -e '(.claudeAiOauth.accessToken // "") != ""' >/dev/null 2>&1; }
keychain_dump() {   # "service<TAB>account<TAB>modified" for every item whose service or label mentions Claude
  security dump-keychain 2>/dev/null | awk -F'"' '
    /^keychain:/ { svc = ""; acct = ""; labl = ""; mdat = "" }
    /"acct"<blob>=/ { acct = $4 }
    /"labl"<blob>=/ { labl = $4 }
    /"mdat"<timedate>=/ { mdat = substr($4, 1, 15) }
    /"svce"<blob>=/ { svc = $4; if (tolower(svc) ~ /claude/ || tolower(labl) ~ /claude/) print svc "\t" acct "\t" mdat }'
}
keychain_items() {  # pinned first, then the default name (no dump needed when it works), then the scan
  local pin; pin=$(cat "$CRED_PIN" 2>/dev/null || true); [[ -n "$pin" ]] && printf '%s\t\t\n' "$pin"
  printf '%s\t\t\n' "$KEYCHAIN_SERVICE"
  keychain_dump
}
read_item() {       # service account -> the item's secret (account may be empty: first match)
  if [[ -n "${2:-}" ]]; then security find-generic-password -s "$1" -a "$2" -w 2>/dev/null
  else security find-generic-password -s "$1" -w 2>/dev/null; fi
}
find_creds() {
  local svc acct mdat json seen=" "
  while IFS=$'\t' read -r svc acct mdat; do
    [[ -n "$svc" && "$seen" != *"|$svc/$acct|"* ]] || continue; seen="$seen|$svc/$acct| "
    json=$(read_item "$svc" "$acct") || continue
    has_login "$json" && { printf '%s' "$json"; return 0; }
  done < <(keychain_items)
  local f
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

# --- doctor: list every candidate login and the last reply, never the token ----------------
if [[ "${1:-}" == "--doctor" ]]; then
  pstatus="${2:-}"   # what the plugin run just reported for this provider
  [[ -n "${CLAUDE_CONFIG_DIR:-}" ]] && ok "CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR in this shell (SwiftBar does not see shell variables)"
  pin=$(cat "$CRED_PIN" 2>/dev/null || true); [[ -n "$pin" ]] && ok "pinned Keychain service: $pin"
  tok=""; seen=" "; nitems=0
  while IFS=$'\t' read -r svc acct mdat; do
    [[ -n "$svc" && "$seen" != *"|$svc/$acct|"* ]] || continue; seen="$seen|$svc/$acct| "; nitems=$((nitems + 1))
    json=$(read_item "$svc" "$acct")
    if [[ -n "$json" ]]; then
      if has_login "$json"; then
        ok "Keychain '$svc' / account '$acct'${mdat:+ (modified $mdat)}: claude.ai login, $(printf '%s' "$json" | jq -r '.claudeAiOauth | "plan \(.subscriptionType // "?"), tier \(.rateLimitTier // "?"), expires \(if .expiresAt then (.expiresAt/1000|todate) else "?" end)"')"
        [[ -n "$tok" ]] || tok=$(printf '%s' "$json" | jq -r '.claudeAiOauth.accessToken')
      else bad "Keychain '$svc' / account '$acct'${mdat:+ (modified $mdat)}: no claude.ai login in it (keys: $(printf '%s' "$json" | jq -r 'if type == "object" then keys | join(",") else "not JSON, \(length) chars" end' 2>/dev/null))"; fi
    else bad "Keychain '$svc' / account '$acct': listed but not readable from here"; fi
  done < <({ [[ -n "$pin" ]] && printf '%s\t\t\n' "$pin"
             items=$(keychain_dump)
             if [[ -n "$items" ]]; then printf '%s\n' "$items"; else printf '%s\t\t\n' "$KEYCHAIN_SERVICE"; fi; })
  ok "$nitems Keychain item(s) mention Claude"
  for f in "$CREDS_FILE" "$HOME/.claude/.credentials.json"; do
    [[ -f "$f" ]] || continue
    if jq -e '(.claudeAiOauth.accessToken // "") != ""' "$f" >/dev/null 2>&1; then ok "file $f: claude.ai login"; [[ -n "$tok" ]] || tok=$(jq -r '.claudeAiOauth.accessToken' "$f")
    else bad "file $f: no claude.ai login in it"; fi
  done
  if [[ -z "$tok" ]]; then
    bad "no claude.ai login found anywhere on this Mac."
    echo "           In Claude Code run /status: it names the login method and, if set, the config dir."
    echo "           If Claude Code uses CLAUDE_CONFIG_DIR, its Keychain entry has a different name; list them with:"
    echo "             security dump-keychain | grep -o '\"Claude Code-credentials[^\"]*\"' | sort -u"
    echo "           and pin the right one:  pulse-limits keychain 'Claude Code-credentials-...'"
  fi
  echo "  usage api"
  if [[ -z "$tok" ]]; then bad "skipped (no token)"
  elif [[ -z "$pstatus" ]]; then ok "reached through the plugin (not probed again: the endpoint has a small per-account quota)"
  elif pl_backing_off; then ok "not probed: backing off after a 429 until $(date -r "$(cat "$BACKOFF")" '+%H:%M:%S')"
  else
    sleep 6; tmp=$(mktemp); code=$(curl -sS -m 15 -o "$tmp" -w '%{http_code}' -H "Authorization: Bearer $tok" -H "anthropic-beta: oauth-2025-04-20" "$USAGE_URL" 2>"$tmp.err" || echo 000)
    case "$code" in
      200) ok "HTTP 200: $(jq -c '{five_hour: .five_hour.utilization, seven_day: .seven_day.utilization}' "$tmp" 2>/dev/null || echo 'unexpected JSON shape')" ;;
      429) bad "HTTP 429 rate limited: the account made too many usage calls recently (another Mac with the panel open counts). It recovers by itself; wait a few minutes." ;;
      000) bad "no answer from api.anthropic.com: $(head -c 200 "$tmp.err")" ;;
      *)   bad "HTTP $code: $(head -c 240 "$tmp" | tr '\n' ' ')" ;;
    esac; rm -f "$tmp" "$tmp.err"
  fi
  echo "  last reply (shape digest)"
  pl_doctor_digests '{keys: (keys | join(",")), five_hour: .five_hour.utilization, seven_day: .seven_day.utilization,
                      limits: [.limits[]? | {kind, percent, model: .scope.model.display_name}]}'
  exit 0
fi

# --- 2. usage: fresh cache, else one live call -------------------------------------------
if [[ -n "$token" ]] && pl_due; then
  pl_get "$USAGE_URL" -H "Authorization: Bearer $token" -H "anthropic-beta: oauth-2025-04-20"
  case "$PL_CODE" in
    200) if jq -e '(.five_hour != null) or ((.limits // []) | length > 0)' "$PL_BODY" >/dev/null 2>&1; then pl_accept
         else status="BAD RESPONSE"; hint="THE USAGE ENDPOINT CHANGED SHAPE"; fi ;;
    401) status="TOKEN EXPIRED"; hint="OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN" ;;
    403) status="NO PLAN ACCESS"; hint="LOG IN TO CLAUDE CODE WITH A CLAUDE.AI PLAN, NOT AN API KEY" ;;
    404) status="NO USAGE DATA";  hint="THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW" ;;
    429) pl_backoff 180                                # the account quota is small and shared across Macs: pause 3 min
         if (( cache_age > 300 )); then status="RATE LIMITED"; hint="TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN"; fi ;;
    000) status="NETWORK";       hint="COULD NOT REACH API.ANTHROPIC.COM" ;;
    *)   status="HTTP $PL_CODE"; hint="UNEXPECTED ANSWER FROM API.ANTHROPIC.COM" ;;
  esac
  rm -f "$PL_BODY"
fi

# --- 3. the windows, from the last good reply ---------------------------------------------
windows="[]"; credits=null
if [[ -f "$CACHE" ]]; then
  # Newer replies carry a limits[] array (kind: session / weekly_all / weekly_scoped); older
  # ones the five_hour / seven_day blocks. Read whichever is there, limits[] first.
  windows=$(jq -c '
    def lim(k): (first(.limits[]? | select(.kind == k)) // null);
    def win(name; k; legacy):
      (if lim(k) != null then { label: name, pct: (lim(k).percent // legacy.utilization // 0), resets: (lim(k).resets_at // legacy.resets_at) }
       elif legacy != null then { label: name, pct: (legacy.utilization // 0), resets: legacy.resets_at }
       else empty end);
    [ win("SESSION"; "session"; .five_hour), win("WEEK"; "weekly_all"; .seven_day) ]
    + [ .limits[]? | select(.kind == "weekly_scoped")
        | { label: ((.scope.model.display_name // .scope.surface // "SCOPED") | ascii_upcase),
            pct: (.percent // 0), resets: .resets_at } ]' "$CACHE")
  credits=$(jq -c '.extra_usage | if (.is_enabled == true) and ((.used_credits // 0) > 0)
                                   then { used: .used_credits, currency: (.currency // "") } else null end' "$CACHE")
fi
pl_emit "$plan_label" "$windows" "$credits"
