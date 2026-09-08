#!/usr/bin/env bash
# PulseLimits installer: one command, no prerequisites. Safe to re-run; it updates in place.
#
#   curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
#   ./install.sh                # from a checkout: installs that checkout
#   ./install.sh --uninstall
#
# Linux: installs what is missing with your package manager (git, curl, a C linker, procps,
# xdg-utils; Waybar when you are on a Wayland session without one), Rust with rustup when
# there is no cargo, the Nerd Font symbols when no Nerd Font is installed, then clones (or
# updates) the repo into ~/.local/share/pulse-limits, builds, links the command into
# ~/.local/bin and writes the Waybar module (`pulse-limits bar on`). On a box with Nix it
# just installs the flake. macOS: Homebrew is the package (`brew install
# dnacenta/tap/pulse-limits && pulse-limits install`); this script is the git route there,
# and bootstraps rustup and SwiftBar the same way.
# Env: PULSE_LIMITS_DIR (where to clone, default ~/.local/share/pulse-limits),
#      PULSE_LIMITS_REPO (git URL, default https://github.com/dnacenta/pulse-limits.git),
#      PULSE_LIMITS_NO_SUDO=1 (never call sudo: report what is missing instead).
set -euo pipefail

REPO="${PULSE_LIMITS_REPO:-https://github.com/dnacenta/pulse-limits.git}"
PLUGIN="pulse-limits.1m.sh"
OLD_PLUGIN="pulse-limits.5m.sh"
PLUGIN_DIR_DEFAULT="$HOME/.config/swiftbar/plugins"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/pulse-limits"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pulse-limits"
OS=$(uname -s)
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"   # rustup's cargo, and where we link the command

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# --- where SwiftBar looks for plugins (macOS) --------------------------------------
plugin_dir() {
  local d; d=$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || true)
  printf '%s' "${d:-$PLUGIN_DIR_DEFAULT}"
}

# --- where the code lives: an explicit dir, the checkout this script sits in, or the default
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
  [[ -x "$d/bin/pulse-limits" ]] && "$d/bin/pulse-limits" bar off || true
  rm -rf "$CACHE_DIR" "$CONFIG_DIR"
  [[ "$OS" == "Darwin" ]] || rm -f "$HOME/.local/bin/pulse-limits"
  if [[ "$d" == "$HOME/.local/share/pulse-limits" && -d "$d" ]]; then rm -rf "$d"; say "Removed $d"; else say "Kept your checkout at $d"; fi
  say "Done. Rust, SwiftBar, Waybar and the fonts were left installed."
  exit 0
fi

# --- privileged package installs: sudo unless we are root or told not to ------------------
as_root() {
  if [[ "$(id -u)" == 0 ]]; then "$@"
  elif [[ -n "${PULSE_LIMITS_NO_SUDO:-}" ]]; then return 1
  else sudo "$@"; fi
}

# --- Rust: rustup's own installer, minimal profile, no prompts ---------------------------
ensure_rust() {
  have cargo && return
  say "Installing Rust with rustup (minimal profile, into ~/.cargo)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
  # shellcheck disable=SC1090
  [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
  have cargo || die "rustup finished but cargo is not on PATH; open a new shell and re-run"
}

# --- Linux: the distro's packages, then the font, then Waybar when it makes sense ---------
linux_packages() {
  local id like; id=$(. /etc/os-release 2>/dev/null && echo "${ID:-}"); like=$(. /etc/os-release 2>/dev/null && echo "${ID_LIKE:-}")
  local want_waybar=""
  if [[ "${XDG_SESSION_TYPE:-}" == "wayland" || -n "${WAYLAND_DISPLAY:-}" ]] && ! have waybar; then want_waybar=1; fi
  local missing=""
  for t in git curl cc pgrep xdg-open; do have "$t" || missing="$missing $t"; done
  [[ -n "$missing" || -n "$want_waybar" ]] || return 0
  say "Installing system packages:${missing}${want_waybar:+ waybar}"
  case " $id $like " in
    *" arch "*|*" archlinux "*|*" manjaro "*|*" cachyos "*)
      as_root pacman -S --needed --noconfirm git curl base-devel procps-ng xdg-utils ${want_waybar:+waybar ttf-nerd-fonts-symbols} ;;
    *" debian "*|*" ubuntu "*)
      as_root apt-get update -qq && as_root apt-get install -y -qq git curl build-essential procps xdg-utils ${want_waybar:+waybar} ;;
    *" fedora "*|*" rhel "*|*" centos "*)
      as_root dnf install -y git curl gcc procps-ng xdg-utils ${want_waybar:+waybar} ;;
    *" opensuse "*|*" suse "*)
      as_root zypper install -y git curl gcc procps xdg-utils ${want_waybar:+waybar} ;;
    *" alpine "*)
      as_root apk add git curl build-base procps xdg-utils ${want_waybar:+waybar} ;;
    *)
      die "unknown distro ($id); install these yourself, then re-run:${missing}${want_waybar:+ waybar}" ;;
  esac || die "package install failed (run with PULSE_LIMITS_NO_SUDO=1 to only be told what is missing)"
}

# The Waybar ring uses Nerd Font glyphs; the symbols-only font is 3 MB and lives in ~/.local/share/fonts.
ensure_nerd_font() {
  have fc-list && fc-list 2>/dev/null | grep -qi 'nerd' && return
  say "Installing the Nerd Font symbols (for the Waybar ring)"
  local dir="$HOME/.local/share/fonts/NerdFontsSymbolsOnly" url
  url=$(curl -fsSL https://api.github.com/repos/ryanoasis/nerd-fonts/releases/latest | grep -o '"browser_download_url": *"[^"]*NerdFontsSymbolsOnly\.tar\.xz"' | head -1 | sed 's/.*"\(http[^"]*\)"/\1/')
  [[ -n "$url" ]] || { warn "could not find the Nerd Font symbols release; the ring will show as boxes until a Nerd Font is installed"; return; }
  mkdir -p "$dir" && curl -fsSL "$url" | tar -xJ -C "$dir" && have fc-cache && fc-cache -f "$dir" >/dev/null 2>&1 || true
}

# --- macOS: the Command Line Tools and SwiftBar ---------------------------------------------
mac_prereqs() {
  if ! xcode-select -p >/dev/null 2>&1; then
    say "Installing the Xcode Command Line Tools (a dialog opens; this waits for it)"
    xcode-select --install 2>/dev/null || true
    local i=0; until xcode-select -p >/dev/null 2>&1; do sleep 10; i=$((i + 1)); (( i < 180 )) || die "the Command Line Tools did not finish installing; re-run when they have"; done
  fi
  [[ -d /Applications/SwiftBar.app ]] && return
  if have brew; then say "Installing SwiftBar with Homebrew"; brew install --cask swiftbar
  else
    say "Installing SwiftBar from its GitHub release (no Homebrew here)"
    local url; url=$(curl -fsSL https://api.github.com/repos/swiftbar/SwiftBar/releases/latest | grep -o '"browser_download_url": *"[^"]*SwiftBar[^"]*\.zip"' | head -1 | sed 's/.*"\(http[^"]*\)"/\1/')
    [[ -n "$url" ]] || die "could not find the SwiftBar release; install it from https://github.com/swiftbar/SwiftBar/releases and re-run"
    local tmp; tmp=$(mktemp -d); curl -fsSL -o "$tmp/SwiftBar.zip" "$url" && unzip -q -o "$tmp/SwiftBar.zip" -d "$tmp"
    local dest=/Applications; [[ -w /Applications ]] || { dest="$HOME/Applications"; mkdir -p "$dest"; }
    rm -rf "$dest/SwiftBar.app" && mv "$tmp/SwiftBar.app" "$dest/" && rm -rf "$tmp"
    say "SwiftBar in $dest (macOS will ask once before opening a downloaded app)"
  fi
}

# --- Nix: the flake is the package; nothing else is needed -------------------------------
if [[ "$OS" != "Darwin" ]] && have nix && ! have cargo; then
  say "Nix found: installing the flake"
  nix --extra-experimental-features 'nix-command flakes' profile install "github:dnacenta/pulse-limits"
  ensure_nerd_font
  pulse-limits bar on
  say "Installed. Add the module to your Waybar config as printed above, then: pulse-limits refresh"
  exit 0
fi

# --- prerequisites -------------------------------------------------------------------
if [[ "$OS" == "Darwin" ]]; then mac_prereqs; else linux_packages; fi
ensure_rust

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
say "Building (cargo, a minute the first time)"
(cd "$DIR" && ./build.sh)

# --- the command and the bar item ---------------------------------------------------------
if [[ "$OS" == "Darwin" ]]; then
  "$DIR/bin/pulse-limits" install
  say "Installed. Look for the ring at the right of your menu bar. Left-click opens the monitor, right-click picks a theme and the providers."
else
  mkdir -p "$HOME/.local/bin"
  ln -sfn "$DIR/bin/pulse-limits" "$HOME/.local/bin/pulse-limits"
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) warn "~/.local/bin is not in your PATH; add it so Waybar can run pulse-limits" ;; esac
  ensure_nerd_font
  "$DIR/bin/pulse-limits" bar on
  say "Installed. Add the module to your Waybar config as printed above, then: pulse-limits refresh"
fi
