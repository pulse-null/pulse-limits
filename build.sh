#!/usr/bin/env bash
# Compiles the two native helpers (needs the Xcode Command Line Tools: swiftc) and the
# terminal UI (needs cargo; skipped with a note when it is missing).
set -eu
cd "$(dirname "$0")"
mkdir -p bin
swiftc -O popover/PulsePopover.swift -o bin/pulse-popover
swiftc -O menubar/MenuBarImage.swift -o bin/pulse-menubar
echo "built bin/pulse-popover bin/pulse-menubar"
# rustup puts cargo in ~/.cargo/bin, which the plugin's fixed PATH does not include
export PATH="$HOME/.cargo/bin:$PATH"
if command -v cargo >/dev/null 2>&1; then
  cargo build --release --quiet --manifest-path tui/Cargo.toml
  cp -f tui/target/release/pulse-tui bin/pulse-tui
  echo "built bin/pulse-tui"
else
  echo "cargo not found: the TUI is skipped, install Rust from https://rustup.rs"
fi
