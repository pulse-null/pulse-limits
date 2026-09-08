<p align="center">
  <img src="docs/crt.png" width="640" alt="PulseLimits: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  <a href="https://github.com/pulse-null/pulse-limits/releases"><img src="https://img.shields.io/github/v/release/pulse-null/pulse-limits?label=release&color=2ea44f" alt="latest release"></a>
  <a href="https://github.com/pulse-null/pulse-limits/actions/workflows/ci.yml"><img src="https://github.com/pulse-null/pulse-limits/actions/workflows/ci.yml/badge.svg" alt="ci"></a>
  <a href="https://coveralls.io/github/pulse-null/pulse-limits?branch=main"><img src="https://coveralls.io/repos/github/pulse-null/pulse-limits/badge.svg?branch=main" alt="coverage"></a>
  <img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Fpulse-null%2Fpulse-limits%2Fmain%2FCargo.toml&query=%24.package.rust-version&label=rust&prefix=%E2%89%A5%20&color=dea584" alt="rust version">
  <img src="https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-informational" alt="platforms">
  <a href="https://github.com/pulse-null/homebrew-tap"><img src="https://img.shields.io/badge/brew-pulse--null%2Ftap-fbb040?logo=homebrew&logoColor=white" alt="homebrew tap"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/pulse-null/pulse-limits?color=blue" alt="license"></a>
</p>

<p align="center">
  Your AI plan limits, Grok, Claude and Codex, in your status bar and in your terminal,
  as a retro patient monitor.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Bar item: 16% and a ring">
</p>

One Rust binary, `pulse-limits`. It reuses the logins your CLIs already keep on the
machine and asks each provider's own usage endpoint once every five minutes. No account,
no server, nothing leaves the machine but those requests. The bar shows a ring with the
session percentage; a click opens the monitor; `pulse-limits tui` puts the same monitor
in a terminal tile.

## Install

### Linux

```sh
curl -fsSL https://raw.githubusercontent.com/pulse-null/pulse-limits/main/install.sh | bash
```

### macOS

```sh
brew install pulse-null/tap/pulse-limits && pulse-limits install
```

## Providers

There is no default. `install` enables the CLIs that have a login on the machine; toggle
them from the bar's provider menu or with `pulse-limits provider NAME`. The bar shows the
first enabled provider whose CLI is running right now, else the first enabled one; the
monitor and the TUI show all of them.

| Provider | Login it reuses | Where the numbers come from | Windows |
|---|---|---|---|
| `grok` | the Grok CLI's, `~/.grok/auth.json` | `cli-chat-proxy.grok.com/v1/billing`, the call the CLI makes for its credit pool; the plan name from `/v1/settings` | the weekly credit pool, on-demand spend when capped |
| `claude` | Claude Code's, `~/.claude/.credentials.json` on Linux, the Keychain on macOS | `api.anthropic.com/api/oauth/usage`, the call behind `/usage` in Claude Code | 5-hour session, week, per-model weekly caps |
| `codex` | the Codex CLI's, `~/.codex/auth.json` | `chatgpt.com/backend-api/wham/usage`, the call the CLI makes for its own rate limits | 5-hour, week, per-model extras |

## The monitor

- **The session number**, big, with its reset countdown. Green, amber from 60 %, red from 85 %.
- **The other windows** as rings, **a 12-hour trend** of the session window, and honest
  failure states: `NO SIGNAL` with the reason when there is no reading.
- **Five themes**: `crt`, `modern`, `cyber`, `synth`, `analog`, from the provider menu or
  `pulse-limits theme NAME`.

| CRT | Modern | Synth |
|:---:|:---:|:---:|
| <img src="docs/crt.png" width="280" alt="CRT theme"> | <img src="docs/modern.png" width="280" alt="Modern theme"> | <img src="docs/synth.png" width="280" alt="Synth theme"> |

| Cyber | Analog | |
|:---:|:---:|:---:|
| <img src="docs/cyber.png" width="280" alt="Cyber theme"> | <img src="docs/analog.png" width="280" alt="Analog theme"> | |

## TUI

```sh
pulse-limits tui                 # or: pulse-limits grok | claude | codex
pulse-limits tui --theme synth
```

The same monitor in a terminal, for a tmux pane or a tiling-WM tile: braille ECG, block
digits, bars, sparkline. Keys: `q` quit, `t` next theme, `r` reload, `?` help.

## Commands

```
pulse-limits install / uninstall   set up or remove the bar item
pulse-limits bar on|off|status     the bar item alone
pulse-limits provider NAME         enable or disable grok | claude | codex
pulse-limits theme NAME            crt | modern | cyber | synth | analog
pulse-limits tui [NAME]            the monitor in the terminal
pulse-limits open                  show or hide the monitor
pulse-limits status                the current reading as JSON, every provider
pulse-limits doctor                check every link of the chain, per provider
pulse-limits raw [NAME]            a provider's last raw usage reply
pulse-limits refresh               force a live fetch now
pulse-limits update / version
```

Plumbing, for scripts: `waybar`, `swiftbar`, `payload`, `activity`, `estimate PCT FETCHED`,
`reset`. Cache lives in `~/.cache/pulse-limits`, settings in `~/.config/pulse-limits`.

## Troubleshooting

`NO SIGNAL` means no reading at all; the header top-right says why (`? NO LOGIN`,
`? TOKEN EXPIRED`, `? NO PLAN ACCESS`, `? RATE LIMITED`, `? NETWORK`). Run
`pulse-limits doctor`: it checks every link per provider without printing tokens.

- **No login on this machine**: run `grok login`, `claude` or `codex login` once.
- **Token expired**: open that CLI once; it refreshes its own token.
- **Rate limited**: the usage endpoints have small per-account quotas, shared by every
  machine on the account; the binary backs off and keeps the last reading.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
