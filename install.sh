#!/usr/bin/env bash
# PulseLimits installer. Safe to re-run; it updates in place.
#
#   curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
#   ./install.sh                # from a checkout: installs that checkout
#   ./install.sh --uninstall
#
# macOS: checks Homebrew + Xcode Command Line Tools, installs jq and SwiftBar if missing,
# clones (or updates) the repo, builds the two Swift helpers, and links the plugin into
# SwiftBar (`pulse-limits bar on`).
# Linux: checks jq, curl and python3, clones (or updates) the repo, links the command into
# ~/.local/bin, and writes the Waybar module file (`pulse-limits bar on`). No Swift, no build.
# Env: PULSE_LIMITS_DIR (where to clone, default ~/.local/share/pulse-limits),
#      PULSE_LIMITS_REPO (git URL, default https://github.com/dnacenta/pulse-limits.git)
set -euo pipefail

REPO="${PULSE_LIMITS_REPO:-https://github.com/dnacenta/pulse-limits.git}"
PLUGIN="pulse-limits.1m.sh"
OLD_PLUGIN="pulse-limits.5m.sh"
PLUGIN_DIR_DEFAULT="$HOME/.config/swiftbar/plugins"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
OS=$(uname -s)

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- where SwiftBar looks for plugins (macOS) --------------------------------------
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
  if [[ "$OS" == "Darwin" ]]; then
    local link="$(plugin_dir)/$PLUGIN"
    if [[ -L "$link" ]]; then local t; t=$(readlink -f "$link" 2>/dev/null || true); [[ -n "$t" ]] && { printf '%s' "$(dirname "$t")"; return; }; fi
  fi
  printf '%s' "$HOME/.local/share/pulse-limits"
}

if [[ "${1:-}" == "--uninstall" ]]; then
  say "Uninstalling"
  d=$(resolve_dir)
  [[ -x "$d/pulse-limits" ]] && "$d/pulse-limits" bar off || true   # SwiftBar link or Waybar module file
  rm -rf "$CACHE_DIR" "$CONFIG_DIR"
  [[ "$OS" == "Darwin" ]] || rm -f "$HOME/.local/bin/pulse-limits"
  if [[ "$d" == "$HOME/.local/share/pulse-limits" && -d "$d" ]]; then rm -rf "$d"; say "Removed $d"; else say "Kept your checkout at $d"; fi
  [[ "$OS" == "Darwin" ]] && say "Done. SwiftBar itself was left installed (brew uninstall --cask swiftbar to remove it)." || say "Done."
  exit 0
fi

# --- prerequisites -------------------------------------------------------------------
if [[ "$OS" == "Darwin" ]]; then
  command -v brew >/dev/null 2>&1 || die "Homebrew is required: https://brew.sh"
  if ! xcode-select -p >/dev/null 2>&1; then
    warn "The Xcode Command Line Tools are needed to compile the two helpers."
    xcode-select --install 2>/dev/null || true
    die "Re-run this installer once the Command Line Tools have finished installing."
  fi
  command -v jq >/dev/null 2>&1 || { say "Installing jq"; brew install jq; }
  [[ -d /Applications/SwiftBar.app ]] || { say "Installing SwiftBar"; brew install --cask swiftbar; }
else
  # --- Linux: nothing is installed for you; say what is missing ------------------------
  missing=""
  for tool in git jq curl python3; do command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"; done
  [[ -z "$missing" ]] || die "install these with your package manager first:$missing"
  command -v waybar >/dev/null 2>&1 || warn "waybar not found; the command still works, the bar item will not show until it is."
fi

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
chmod +x "$DIR/$PLUGIN" "$DIR/open-monitor.sh" "$DIR/pulse-limits"
[[ -f "$DIR/bin/pulse-activity.py" ]] && chmod +x "$DIR/bin/pulse-activity.py"   # absent in a copied pre-Linux folder

if [[ "$OS" == "Darwin" ]]; then
  # --- macOS: build the helpers, link into SwiftBar ----------------------------------------
  say "Building the helpers"
  (cd "$DIR" && ./build.sh)
  if ! security find-generic-password -s "Claude Code-credentials" >/dev/null 2>&1; then
    warn "No Claude Code login found in the Keychain. Run 'claude' once and log in; the widget reads that token."
  fi
  "$DIR/pulse-limits" bar on
  say "Installed. Look for the ring at the right of your menu bar. Left-click opens the monitor, right-click picks a theme."
else
  # --- Linux: the command in ~/.local/bin, the module file for Waybar ---------------------
  mkdir -p "$HOME/.local/bin"
  ln -sfn "$DIR/pulse-limits" "$HOME/.local/bin/pulse-limits"
  say "Linked ~/.local/bin/pulse-limits -> $DIR/pulse-limits"
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) warn "~/.local/bin is not in your PATH; add it, Waybar needs to find pulse-limits" ;; esac
  [[ -f "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json" ]] \
    || warn "No Claude Code login found (${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json). Run 'claude' once and log in; the widget reads that token."
  "$DIR/pulse-limits" bar on
  say "Installed. Add the module to your Waybar config as printed above, then: pulse-limits refresh"
fi
