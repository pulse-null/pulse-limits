#!/usr/bin/env bash
# Builds bin/pulse-limits with cargo (https://rustup.rs) and, on macOS, the two AppKit helpers
# with swiftc (the Xcode Command Line Tools). Safe to re-run.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
# rustup puts cargo in ~/.cargo/bin, which a bar's PATH does not include
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null 2>&1 || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }
cargo build --release --quiet
cp -f target/release/pulse-limits bin/pulse-limits
echo "built bin/pulse-limits"
if [[ "$(uname -s)" == "Darwin" ]]; then
  command -v swiftc >/dev/null 2>&1 || { echo "swiftc not found: xcode-select --install" >&2; exit 1; }
  swiftc -O popover/PulsePopover.swift -o bin/pulse-popover
  swiftc -O menubar/MenuBarImage.swift -o bin/pulse-menubar
  echo "built bin/pulse-popover bin/pulse-menubar"
fi
