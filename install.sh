#!/usr/bin/env bash
# PulseLimits installer. Safe to re-run; it updates in place.
#
#   curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
#   ./install.sh                # from a checkout: installs that checkout
#   ./install.sh --uninstall
#
# Both platforms need git and cargo (https://rustup.rs): the core is one Rust binary, built
# here. macOS also needs the Xcode Command Line Tools (swiftc, for the two AppKit helpers) and
# SwiftBar, installed with Homebrew when it is missing. Linux needs nothing else; Waybar shows
# the bar item when it is there.
# What it does: clones (or updates) the repo, runs ./build.sh, links the command into
# ~/.local/bin, and sets up the bar item (`pulse-limits install`).
# Env: PULSE_LIMITS_DIR (where to clone, default ~/.local/share/pulse-limits),
#      PULSE_LIMITS_REPO (git URL, default https://github.com/dnacenta/pulse-limits.git)
set -euo pipefail

REPO="${PULSE_LIMITS_REPO:-https://github.com/dnacenta/pulse-limits.git}"
PLUGIN="pulse-limits.1m.sh"
PLUGIN_DIR_DEFAULT="$HOME/.config/swiftbar/plugins"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
OS=$(uname -s)
export PATH="$HOME/.cargo/bin:$PATH"   # rustup's cargo

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
  if [[ -n "$here" && -f "$here/Cargo.toml" && -f "$here/$PLUGIN" ]]; then printf '%s' "$here"; return; fi
  if [[ "$OS" == "Darwin" ]]; then
    local link="$(plugin_dir)/$PLUGIN"
    if [[ -L "$link" ]]; then local t; t=$(readlink -f "$link" 2>/dev/null || true); [[ -n "$t" ]] && { printf '%s' "$(dirname "$t")"; return; }; fi
  fi
  printf '%s' "$HOME/.local/share/pulse-limits"
}

if [[ "${1:-}" == "--uninstall" ]]; then
  say "Uninstalling"
  d=$(resolve_dir)
  [[ -x "$d/bin/pulse-limits" ]] && "$d/bin/pulse-limits" bar off || true   # SwiftBar link or Waybar module file
  rm -rf "$CACHE_DIR" "$CONFIG_DIR"
  rm -f "$HOME/.local/bin/pulse-limits"
  if [[ "$d" == "$HOME/.local/share/pulse-limits" && -d "$d" ]]; then rm -rf "$d"; say "Removed $d"; else say "Kept your checkout at $d"; fi
  [[ "$OS" == "Darwin" ]] && say "Done. SwiftBar itself was left installed (brew uninstall --cask swiftbar to remove it)." || say "Done."
  exit 0
fi

# --- prerequisites -------------------------------------------------------------------
missing=""
for tool in git cargo; do command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"; done
[[ -z "$missing" ]] || die "needed first:$missing  (cargo comes with Rust: https://rustup.rs)"
if [[ "$OS" == "Darwin" ]]; then
  if ! xcode-select -p >/dev/null 2>&1; then
    warn "The Xcode Command Line Tools are needed to compile the two AppKit helpers."
    xcode-select --install 2>/dev/null || true
    die "Re-run this installer once the Command Line Tools have finished installing."
  fi
  if [[ ! -d /Applications/SwiftBar.app ]]; then
    command -v brew >/dev/null 2>&1 || die "SwiftBar is not installed and Homebrew is not here to install it: https://github.com/swiftbar/SwiftBar"
    say "Installing SwiftBar"; brew install --cask swiftbar
  fi
else
  command -v waybar >/dev/null 2>&1 || warn "waybar not found; the command still works, the bar item will not show until it is."
fi

# --- code ---------------------------------------------------------------------------------
DIR=$(resolve_dir)
if [[ -d "$DIR/.git" ]]; then
  say "Updating $DIR"
  git -C "$DIR" pull --ff-only -q || warn "could not fast-forward $DIR; using what is there"
elif [[ -f "$DIR/Cargo.toml" ]]; then
  say "Using $DIR"
else
  say "Cloning into $DIR"
  mkdir -p "$(dirname "$DIR")"
  git clone -q "$REPO" "$DIR"
fi
chmod +x "$DIR/$PLUGIN" "$DIR/pulse-limits.5m.sh" "$DIR/build.sh"

say "Building (cargo, a minute the first time)"
(cd "$DIR" && ./build.sh)

# --- the command in ~/.local/bin --------------------------------------------------------
mkdir -p "$HOME/.local/bin"
ln -sfn "$DIR/bin/pulse-limits" "$HOME/.local/bin/pulse-limits"
say "Linked ~/.local/bin/pulse-limits -> $DIR/bin/pulse-limits"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) warn "~/.local/bin is not in your PATH; add it (Waybar needs to find pulse-limits)" ;; esac

# --- the bar item -----------------------------------------------------------------------
"$DIR/bin/pulse-limits" install
if [[ "$OS" == "Darwin" ]]; then
  say "Installed. Look for the ring at the right of your menu bar. Left-click opens the monitor, right-click picks a theme and the providers."
else
  say "Installed. Add the module to your Waybar config as printed above, then: pulse-limits refresh"
fi
