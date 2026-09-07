#!/usr/bin/env bash
# PulseLimits installer. Safe to re-run; it updates in place.
#
#   curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
#   ./install.sh                # from a checkout: installs that checkout
#   ./install.sh --uninstall
#
# What it does: checks macOS + Homebrew + Xcode Command Line Tools, installs jq and
# SwiftBar if missing, clones (or updates) the repo, builds the two Swift helpers,
# links the plugin into SwiftBar's plugin folder, and starts SwiftBar.
# Env: PULSE_LIMITS_DIR (where to clone, default ~/.local/share/pulse-limits),
#      PULSE_LIMITS_REPO (git URL, default https://github.com/dnacenta/pulse-limits.git)
set -euo pipefail

REPO="${PULSE_LIMITS_REPO:-https://github.com/dnacenta/pulse-limits.git}"
PLUGIN="pulse-limits.1m.sh"
OLD_PLUGIN="pulse-limits.5m.sh"
PLUGIN_DIR_DEFAULT="$HOME/.config/swiftbar/plugins"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- where SwiftBar looks for plugins ----------------------------------------------
plugin_dir() {
  local d; d=$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || true)
  printf '%s' "${d:-$PLUGIN_DIR_DEFAULT}"
}

# --- where the code lives -------------------------------------------------------------
# 1. an explicit PULSE_LIMITS_DIR; 2. the checkout this script sits in; 3. the checkout
# an existing SwiftBar link already points at; 4. the default clone location.
resolve_dir() {
  if [[ -n "${PULSE_LIMITS_DIR:-}" ]]; then printf '%s' "$PULSE_LIMITS_DIR"; return; fi
  local here; here=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd -P || true)
  if [[ -n "$here" && -f "$here/$PLUGIN" ]]; then printf '%s' "$here"; return; fi
  local link="$(plugin_dir)/$PLUGIN"
  if [[ -L "$link" ]]; then local t; t=$(readlink -f "$link" 2>/dev/null || true); [[ -n "$t" ]] && { printf '%s' "$(dirname "$t")"; return; }; fi
  printf '%s' "$HOME/.local/share/pulse-limits"
}

if [[ "${1:-}" == "--uninstall" ]]; then
  say "Uninstalling"
  pkill -f bin/pulse-popover 2>/dev/null || true
  rm -f "$(plugin_dir)/$PLUGIN" "$(plugin_dir)/$OLD_PLUGIN"
  rm -rf "$CACHE_DIR" "$CONFIG_DIR"
  d=$(resolve_dir)
  if [[ "$d" == "$HOME/.local/share/pulse-limits" && -d "$d" ]]; then rm -rf "$d"; say "Removed $d"; else say "Kept your checkout at $d"; fi
  open "swiftbar://refreshallplugins" 2>/dev/null || true
  say "Done. SwiftBar itself was left installed (brew uninstall --cask swiftbar to remove it)."
  exit 0
fi

# --- prerequisites -------------------------------------------------------------------
[[ "$(uname -s)" == "Darwin" ]] || die "PulseLimits is a macOS menu bar widget."
command -v brew >/dev/null 2>&1 || die "Homebrew is required: https://brew.sh"
if ! xcode-select -p >/dev/null 2>&1; then
  warn "The Xcode Command Line Tools are needed to compile the two helpers."
  xcode-select --install 2>/dev/null || true
  die "Re-run this installer once the Command Line Tools have finished installing."
fi
command -v jq >/dev/null 2>&1 || { say "Installing jq"; brew install jq; }
[[ -d /Applications/SwiftBar.app ]] || { say "Installing SwiftBar"; brew install --cask swiftbar; }

# --- code ---------------------------------------------------------------------------------
DIR=$(resolve_dir)
if [[ -d "$DIR/.git" ]]; then
  say "Updating $DIR"
  git -C "$DIR" pull --ff-only -q || warn "could not fast-forward $DIR; using what is there"
elif [[ -f "$DIR/$PLUGIN" ]]; then
  say "Using $DIR"
else
  say "Cloning into $DIR"
  mkdir -p "$(dirname "$DIR")"
  git clone -q "$REPO" "$DIR"
fi

say "Building the helpers"
(cd "$DIR" && ./build.sh)

# --- SwiftBar ------------------------------------------------------------------------
PD=$(plugin_dir)
mkdir -p "$PD"
defaults write com.ameba.SwiftBar PluginDirectory "$PD"
rm -f "$PD/$OLD_PLUGIN"
ln -sfn "$DIR/$PLUGIN" "$PD/$PLUGIN"
chmod +x "$DIR/$PLUGIN" "$DIR/open-monitor.sh"
say "Linked $PD/$PLUGIN"

if ! security find-generic-password -s "Claude Code-credentials" >/dev/null 2>&1; then
  warn "No Claude Code login found in the Keychain. Run 'claude' once and log in; the widget reads that token."
fi

pkill -f bin/pulse-popover 2>/dev/null || true
if pgrep -xq SwiftBar; then open "swiftbar://refreshallplugins"; else open -a SwiftBar; fi
say "Installed. Look for the ring at the right of your menu bar. Left-click opens the monitor, right-click picks a theme."
