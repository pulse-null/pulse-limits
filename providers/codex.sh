#!/usr/bin/env bash
# Codex: the ChatGPT login the OpenAI Codex CLI keeps in ~/.codex/auth.json (CODEX_HOME
# moves that folder) and the usage endpoint the CLI itself reads its rate limits from.
# Both are undocumented; the file and reply shapes follow what CodexBar (MIT) reads.
# The only thing that leaves this Mac is one GET to chatgpt.com. See _common.sh for
# the document this prints and the --doctor mode.
PROVIDER=codex
. "$(dirname "$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")")/_common.sh"
CODEX_DIR="${CODEX_HOME:-$HOME/.codex}"
AUTH_FILE="$CODEX_DIR/auth.json"
USAGE_URL="https://chatgpt.com/backend-api/wham/usage"
# `chatgpt_base_url` in config.toml sends the CLI through a proxy; follow it. A base without
# /backend-api serves the same document under /api/codex/usage.
base=$(grep -m1 '^[[:space:]]*chatgpt_base_url[[:space:]]*=' "$CODEX_DIR/config.toml" 2>/dev/null | sed 's/^[^=]*=//; s/#.*//; s/["'"'"'[:space:]]//g; s:/*$::')
if [[ -n "$base" ]]; then
  case "$base" in https://chatgpt.com|https://chat.openai.com) base="$base/backend-api" ;; esac
  case "$base" in */backend-api*) USAGE_URL="$base/wham/usage" ;; *) USAGE_URL="$base/api/codex/usage" ;; esac
fi

jwt_claims() {   # token -> its payload as JSON, nothing if it is not a JWT
  local p; p=$(printf '%s' "$1" | cut -d. -f2 | tr '_-' '/+')
  case $(( ${#p} % 4 )) in 2) p="$p==" ;; 3) p="$p=" ;; esac
  printf '%s' "$p" | base64 -D 2>/dev/null | jq -c 'select(type == "object")' 2>/dev/null
}

# --- 1. credentials: auth.json, written by `codex login` --------------------------------------
# An OAuth login has tokens.access_token (+ id_token with the plan, account_id); an API-key
# login has only OPENAI_API_KEY and cannot read plan limits. The access token is a JWT whose
# exp claim says when it dies; the CLI refreshes it while it runs, and rotating it from here
# could log the CLI out, so an expired one is reported, not refreshed.
token=""; account=""; plan="?"; expires=0
if [[ ! -f "$AUTH_FILE" ]]; then
  status="NO LOGIN"; hint="NO CODEX LOGIN ON THIS MAC. RUN: codex login"
elif ! auth=$(jq -c 'select(type == "object")' "$AUTH_FILE" 2>/dev/null) || [[ -z "$auth" ]]; then
  status="BAD AUTH FILE"; hint="$AUTH_FILE IS NOT JSON. RUN: codex login"
else
  token=$(printf '%s' "$auth" | jq -r '.tokens.access_token // .tokens.accessToken // empty')
  if [[ -z "$token" ]]; then
    if [[ -n $(printf '%s' "$auth" | jq -r '.OPENAI_API_KEY // empty') ]]; then
      status="NO PLAN ACCESS"; hint="CODEX IS LOGGED IN WITH AN API KEY, NOT A CHATGPT PLAN. RUN: codex login"
    else status="NO LOGIN"; hint="AUTH FILE HAS NO TOKENS. RUN: codex login"; fi
  else
    claims=$(jwt_claims "$(printf '%s' "$auth" | jq -r '.tokens.id_token // .tokens.idToken // empty')"); [[ -n "$claims" ]] || claims='{}'
    plan=$(printf '%s' "$claims" | jq -r '.["https://api.openai.com/auth"].chatgpt_plan_type // .chatgpt_plan_type // "?"')
    account=$(printf '%s' "$auth" | jq -r '.tokens.account_id // .tokens.accountId // empty')
    [[ -n "$account" ]] || account=$(printf '%s' "$claims" | jq -r '.["https://api.openai.com/auth"].chatgpt_account_id // .chatgpt_account_id // empty')
    expires=$(jwt_claims "$token" | jq -r '.exp // 0'); expires=${expires:-0}
    if (( expires > 0 && expires <= now )); then
      status="TOKEN EXPIRED"; hint="OPEN CODEX ONCE, IT REFRESHES THE TOKEN"; token=""
    fi
  fi
fi
[[ -f "$CACHE" ]] && plan=$(jq -r --arg claim "$plan" '.plan_type // $claim' "$CACHE")   # the reply knows better than the claim
plan_label=$(printf '%s' "$plan" | sed 's/_/ /g' | upper | sed 's/^?$//')

# --- doctor ---------------------------------------------------------------------------------------
if [[ "${1:-}" == "--doctor" ]]; then
  pstatus="${2:-}"   # what the plugin run just reported for this provider
  [[ -n "${CODEX_HOME:-}" ]] && ok "CODEX_HOME=$CODEX_HOME in this shell (SwiftBar does not see shell variables)"
  if [[ ! -f "$AUTH_FILE" ]]; then bad "no $AUTH_FILE: run 'codex login' (or set CODEX_HOME to where the CLI keeps it)"
  elif [[ -n "$token" ]]; then
    ok "$AUTH_FILE: ChatGPT login, plan ${plan}, account id $([[ -n "$account" ]] && echo present || echo missing), token expires $([[ "$expires" -gt 0 ]] && date -u -r "$expires" '+%Y-%m-%dT%H:%M:%SZ' || echo '?'), last refresh $(printf '%s' "$auth" | jq -r '.last_refresh // "?"')"
  else bad "$AUTH_FILE: $status ($hint)"; fi
  if command -v codex >/dev/null; then ok "codex CLI: $(command -v codex)"; else ok "codex CLI not on PATH (only needed to log in)"; fi
  ok "usage url: $USAGE_URL"
  echo "  usage api"
  if [[ -z "$token" ]]; then bad "skipped (no usable token)"
  elif [[ -z "$pstatus" ]]; then ok "reached through the plugin (not probed again: keep the calls rare)"
  elif pl_backing_off; then ok "not probed: backing off after a 429 until $(date -r "$(cat "$BACKOFF")" '+%H:%M:%S')"
  else
    hdr=(); [[ -n "$account" ]] && hdr=(-H "ChatGPT-Account-Id: $account")
    sleep 6; tmp=$(mktemp); code=$(curl -sS -m 15 -o "$tmp" -w '%{http_code}' -H "Authorization: Bearer $token" -H "Accept: application/json" ${hdr[@]+"${hdr[@]}"} "$USAGE_URL" 2>"$tmp.err" || echo 000)
    case "$code" in
      200) ok "HTTP 200: $(jq -c '{plan_type, primary: .rate_limit.primary_window.used_percent, secondary: .rate_limit.secondary_window.used_percent}' "$tmp" 2>/dev/null || echo 'unexpected JSON shape')" ;;
      401) bad "HTTP 401: the token is expired or revoked. Open the Codex CLI once, or run 'codex login'." ;;
      429) bad "HTTP 429 rate limited: too many usage calls recently. It recovers by itself; wait a few minutes." ;;
      000) bad "no answer from chatgpt.com: $(head -c 200 "$tmp.err")" ;;
      *)   bad "HTTP $code: $(head -c 240 "$tmp" | tr '\n' ' ')" ;;
    esac; rm -f "$tmp" "$tmp.err"
  fi
  echo "  last reply (shape digest)"
  pl_doctor_digests '{keys: (keys | join(",")), plan_type, primary: .rate_limit.primary_window, secondary: .rate_limit.secondary_window,
                      extra: [.additional_rate_limits[]? | .limit_name]}'
  exit 0
fi

# --- 2. usage: fresh cache, else one live call ------------------------------------------------
if [[ -n "$token" ]] && pl_due; then
  hdr=(); [[ -n "$account" ]] && hdr=(-H "ChatGPT-Account-Id: $account")
  pl_get "$USAGE_URL" -H "Authorization: Bearer $token" -H "Accept: application/json" ${hdr[@]+"${hdr[@]}"}
  case "$PL_CODE" in
    200) if jq -e '.rate_limit | (.primary_window // .secondary_window) != null' "$PL_BODY" >/dev/null 2>&1; then pl_accept
         else status="BAD RESPONSE"; hint="THE USAGE ENDPOINT CHANGED SHAPE"; fi ;;
    401) status="TOKEN EXPIRED"; hint="OPEN CODEX ONCE, IT REFRESHES THE TOKEN" ;;
    403) status="NO PLAN ACCESS"; hint="THIS LOGIN CANNOT READ PLAN LIMITS. LOG IN TO CODEX WITH A CHATGPT PLAN" ;;
    404) status="NO USAGE DATA";  hint="THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW" ;;
    429) pl_backoff 180                                # same courtesy as the Claude reader: pause 3 min
         if (( cache_age > 300 )); then status="RATE LIMITED"; hint="TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN"; fi ;;
    000) status="NETWORK";       hint="COULD NOT REACH CHATGPT.COM" ;;
    *)   status="HTTP $PL_CODE"; hint="UNEXPECTED ANSWER FROM CHATGPT.COM" ;;
  esac
  rm -f "$PL_BODY"
fi

# --- 3. the windows, from the last good reply -------------------------------------------------
# primary_window is normally the 5-hour window and secondary_window the week, but the CLI tells
# them apart by limit_window_seconds, so do we. additional_rate_limits[] are per-model caps
# (e.g. GPT-5.3-Codex-Spark); their primary window carries the utilisation; the last word
# of the name is the ring label, so it fits next to the others.
# credits is a prepaid balance, not spend, so it is not shown as CREDITS.
windows="[]"
if [[ -f "$CACHE" ]]; then
  windows=$(jq -c '
    def name(s): if s == 18000 then "SESSION" elif s == 604800 then "WEEK" elif s >= 86400 then "\(s / 86400 | floor)D" else "\(s / 3600 | floor)H" end;
    def win(w; lbl): (w // null) as $w
      | if $w == null then empty
        else { label: lbl, pct: ($w.used_percent // 0), resets: (if ($w.reset_at // 0) > 0 then ($w.reset_at | todate) else null end) } end;
    [ (.rate_limit // {}) | win(.primary_window; name(.primary_window.limit_window_seconds // 18000)),
                            win(.secondary_window; name(.secondary_window.limit_window_seconds // 604800)) ]
    + [ .additional_rate_limits[]? | select(type == "object")
        | win((.rate_limit.primary_window // .rate_limit.secondary_window); ((.limit_name // .metered_feature // "EXTRA") | split("-") | last | split(" ") | last | ascii_upcase | .[0:8])) ]' "$CACHE" 2>/dev/null || echo '[]')
fi
pl_emit "$plan_label" "$windows" null
