#!/usr/bin/env bash
# <swiftbar.title>PulseLimits</swiftbar.title>
# <swiftbar.version>v0.4.0</swiftbar.version>
# <swiftbar.author>Daniel Nacenta</swiftbar.author>
# <swiftbar.desc>Your Claude and Codex plan limits as a retro patient monitor: the heart rate is your usage.</swiftbar.desc>
# <swiftbar.dependencies>bash,jq,curl</swiftbar.dependencies>
# <swiftbar.runInBash>false</swiftbar.runInBash>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>
#
# PulseLimits
# Asks each enabled provider (providers/<name>.sh: Claude Code's login and Anthropic's
# usage endpoint, the Codex CLI's login and ChatGPT's) how much of its windows you have
# burned, and hands the numbers to panel.html: a CRT patient monitor whose ECG beats
# faster the more you have used.
#   left-click  -> the monitor (SwiftBar webview popover)
#   right-click -> plain text fallback menu, theme and provider switches
# Runs every minute. Each provider is asked once per 5 min; in between, Claude's session %
# is dead-reckoned from the tokens Claude Code produced since the last reading (see
# `pulse-popover --estimate`), so the menu bar moves with your usage.
# The menu bar shows one provider: the first enabled one whose CLI is running right now,
# else the first enabled one. The panel shows all of them.
# Nothing leaves this Mac but one GET per provider to its own usage endpoint.
# Runs on the bash 3.2 that ships with macOS. runInBash=false makes SwiftBar exec this file
# and the click launcher directly instead of through `zsh -l -c`, which would pay your
# login profile (nvm, brew shellenv: ~0.5 s) on every run and every click.

set -u
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export LC_ALL=C

SELF=$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")   # SwiftBar calls the symlink
HERE=$(dirname "$SELF")
PANEL="$HERE/panel.html"
PROVIDERS_DIR="$HERE/providers"
KNOWN_PROVIDERS="claude codex"   # one reader script each in providers/; the name is also the CLI's process name
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
MIN_INTERVAL=270       # seconds between live calls per provider (the plugin runs every minute, each API sees one call per 5)
THEMES="crt modern cyber synth analog"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
THEME_FILE="$CONFIG_DIR/theme"                     # chosen from the right-click menu; default crt
THEME=$(cat "$THEME_FILE" 2>/dev/null || echo crt)
PROVIDERS_FILE="$CONFIG_DIR/providers"             # enabled providers, one per line, first = menu bar default; default claude
if [[ -f "$PROVIDERS_FILE" ]]; then PROVIDERS=$(tr -s '\n\t' '  ' < "$PROVIDERS_FILE"); else PROVIDERS=claude; fi
MODE="${1:-}"          # --reset: drop the caches. --payload: fetch fresh, print JSON only. --theme <name>. --provider <name>: toggle it
[[ "$MODE" == "--payload" ]] && MIN_INTERVAL=45   # the popover asks every 2 min while open
POPOVER_W=520
POPOVER_H=316          # our own popover: 300 of screen + bezel. SwiftBar fallback adds its 32 px header
URLFILE="$CACHE_DIR/panel.url"           # read by open-monitor.sh on click
LAUNCHER="$HERE/open-monitor.sh"
POPOVER_BIN="$HERE/bin/pulse-popover"
MENUBAR_BIN="$HERE/bin/pulse-menubar"

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
known() { case " $KNOWN_PROVIDERS " in *" ${1:-} "*) return 0 ;; esac; return 1; }

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

epoch_of() { local s="${1%%.*}"; s="${s%%+*}"; s="${s%Z}"; date -u -j -f "%Y-%m-%dT%H:%M:%S" "$s" +%s 2>/dev/null; }

countdown() { # epoch -> "2H 14M" / "4D 07H" / "38M"
  local diff=$(( $1 - $(date +%s) )); (( diff < 0 )) && diff=0
  local d=$(( diff / 86400 )) h=$(( diff % 86400 / 3600 )) m=$(( diff % 3600 / 60 ))
  if (( d > 0 )); then printf '%dD %02dH' "$d" "$h"; elif (( h > 0 )); then printf '%dH %02dM' "$h" "$m"; else printf '%dM' "$m"; fi
}

# --- force a live call on the next run (Option-click the header, or --reset) ----
if [[ "$MODE" == "--reset" ]]; then rm -f "$CACHE_DIR"/usage*.json "$CACHE_DIR"/backoff*; exit 0; fi
# --- pick a theme (the right-click menu calls this, then SwiftBar refreshes us) -------
if [[ "$MODE" == "--theme" ]]; then
  case " $THEMES " in *" ${2:-} "*) mkdir -p "$CONFIG_DIR"; printf '%s\n' "$2" > "$THEME_FILE"; exit 0 ;; esac
  echo "unknown theme: ${2:-} (one of: $THEMES)" >&2; exit 64
fi
# --- enable or disable a provider (same menu); a newly enabled one goes last in priority ------
if [[ "$MODE" == "--provider" ]]; then
  known "${2:-}" || { echo "unknown provider: ${2:-} (one of: $KNOWN_PROVIDERS)" >&2; exit 64; }
  mkdir -p "$CONFIG_DIR"
  if [[ " $PROVIDERS " == *" $2 "* ]]; then new=$(printf '%s\n' $PROVIDERS | grep -vx "$2"); else new=$(printf '%s\n' $PROVIDERS "$2"); fi
  printf '%s\n' $new > "$PROVIDERS_FILE"; exit 0
fi

mkdir -p "$CACHE_DIR"

# --- 1. ask every enabled provider; a reader that prints no document is reported, not fatal ---
docs=""; enabled=""
for p in $PROVIDERS; do
  known "$p" || continue
  enabled="$enabled $p"
  doc=$(PL_MIN_INTERVAL=$MIN_INTERVAL "$PROVIDERS_DIR/$p.sh" 2>/dev/null)
  printf '%s' "$doc" | jq -e '.provider' >/dev/null 2>&1 \
    || doc=$(jq -cn --arg p "$p" '{ provider: $p, plan: "", source: "", fetched: 0, status: "READER FAILED",
                                    hint: "providers/\($p).sh PRINTED NO DOCUMENT. RUN: pulse-limits doctor", windows: [], credits: null, history: [] }')
  docs="$docs${docs:+,}$doc"
done
docs="[$docs]"
enabled=${enabled# }

# --- 2. the active provider: the first enabled one whose CLI runs right now, else the first ----
active=""
for p in $enabled; do pgrep -xq "$p" 2>/dev/null && { active=$p; break; }; done
[[ -n "$active" ]] || active=${enabled%% *}

# --- 3. payload for the monitor: the active provider on top, every provider in providers[] ----
activity_json=null
[[ -x "$POPOVER_BIN" ]] && activity_json=$("$POPOVER_BIN" --activity 2>/dev/null || echo null)
payload=$(jq -cn --argjson providers "$docs" --arg active "$active" --arg theme "$THEME" --argjson activity "$activity_json" '
  (first($providers[] | select(.provider == $active))
   // { provider: "", plan: "", source: "", fetched: 0, status: "NO PROVIDER",
        hint: "ENABLE ONE: RIGHT-CLICK THE MENU BAR ITEM, PROVIDERS", windows: [], credits: null, history: [] }) as $a
  | { provider: $a.provider, plan: $a.plan, source: $a.source, theme: $theme, fetched: $a.fetched, history: $a.history, activity: $activity,
      status: $a.status, hint: $a.hint, windows: $a.windows, credits: $a.credits,
      providers: [ $providers[] | { name: .provider, plan, status, hint, fetched, windows } ] }')
eval "$(printf '%s' "$payload" | jq -r '@sh "status=\(.status) hint=\(.hint) plan_label=\(.plan)"')"
have_data=1; [[ $(printf '%s' "$payload" | jq '.windows | length') -gt 0 ]] || have_data=0
read -r s_api fetched < <(printf '%s' "$payload" | jq -r '[ ((first(.windows[] | select(.label == "SESSION")) // { pct: 0 }).pct), .fetched ] | @tsv')
# dead reckoning between readings, Claude only: the tokens are counted from Claude Code's transcripts
estimate_json=null
if (( have_data )) && [[ "$active" == claude && -x "$POPOVER_BIN" ]]; then
  estimate_json=$("$POPOVER_BIN" --estimate "$s_api" "$fetched" 2>/dev/null || echo null)
  payload=$(printf '%s' "$payload" | jq -c --argjson e "$estimate_json" '. + { estimate: $e }')
fi
s_pct=$(printf '%s' "$estimate_json" | jq -r --argjson api "$s_api" 'if . != null and .calibrated then .pct_est else $api end | . + 0.5 | floor')
b64=$(printf '%s' "$payload" | base64 | tr -d '\n')
printf 'file://%s#%s' "$PANEL" "$b64" > "$URLFILE"
if [[ "$MODE" == "--payload" ]]; then printf '%s\n' "$payload"; exit 0; fi

# --- 4. menu bar line: "10%" then a ring that fills with it, like the battery item ---
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

# --- 5. text fallback (right-click) -------------------------------------------------
n_enabled=$(printf '%s\n' $enabled | grep -c .)
hdr="PULSE LIMITS"
[[ -n "$active" ]] && (( n_enabled > 1 )) && hdr="$hdr  ·  $(printf '%s' "$active" | upper)"
[[ -n "$plan_label" ]] && hdr="$hdr  ·  $plan_label"
line "$hdr" "$MONO color=$C_HEAD"
line 'FORCE REFRESH' "$MONO color=$C_GREEN alternate=true bash=$SELF param1=--reset terminal=false refresh=true"
line "THEME  ·  $(printf '%s' "$THEME" | upper)" "$MONO color=$C_DIM"
for th in $THEMES; do
  checked=false; [[ "$th" == "$THEME" ]] && checked=true
  line "--$(printf '%s' "$th" | upper)" "$MONO color=$C_GREEN checked=$checked bash=$SELF param1=--theme param2=$th terminal=false refresh=true"
done
line "PROVIDERS" "$MONO color=$C_DIM"
for p in $KNOWN_PROVIDERS; do
  checked=false; [[ " $enabled " == *" $p "* ]] && checked=true
  line "--$(printf '%s' "$p" | upper)" "$MONO color=$C_GREEN checked=$checked bash=$SELF param1=--provider param2=$p terminal=false refresh=true"
done
if (( n_enabled == 0 )); then
  line "?$status  ERROR" "$MONO color=$C_RED"
  line "$hint" "$MONO color=$C_DIM"
fi
row() { # label pct iso
  local e; e=$(epoch_of "$3"); local when="?"; [[ -n "$e" ]] && when=$(countdown "$e")
  line "$(printf '%-8s %s %3d%%   RESETS IN %s' "$1" "$(bar "$2" 20)" "$2" "$when")" "$MONO color=$(tone "$2")"
}
# one block per provider: its plan, its error if any, its windows
while IFS= read -r decl; do
  eval "$decl"
  sep
  line "$(printf '%s' "$p" | upper)${plan:+  ·  $plan}" "$MONO color=$C_HEAD"
  if [[ -n "$st" ]]; then
    line "?$st  ERROR" "$MONO color=$C_RED"
    [[ -n "$hi" ]] && line "$hi" "$MONO color=$C_DIM"
  fi
  while IFS=$'\t' read -r name pct iso; do
    [[ -n "${name:-}" ]] && row "$(printf '%.8s' "$name")" "$pct" "$iso"
  done < <(printf '%s' "$docs" | jq -r --arg p "$p" '.[] | select(.provider == $p) | .windows[] | [ .label, (.pct + 0.5 | floor), (.resets // "-") ] | @tsv')
done < <(printf '%s' "$docs" | jq -r '.[] | @sh "p=\(.provider) plan=\(.plan) st=\(.status) hi=\(.hint)"')
