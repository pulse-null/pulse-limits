<p align="center">
  <img src="docs/crt.png" width="640" alt="PulseLimits: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  Your Claude, Codex and Grok plan limits in the menu bar (macOS) or in Waybar (Linux),
  as a retro patient monitor. The heartbeat is live: it races while your CLI is streaming and flatlines when it idles.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Menu bar item: 16% and a ring">
</p>

One Rust binary, `pulse-limits`. It reuses the logins your CLIs already keep on the
machine and asks each provider's own usage endpoint once every five minutes. No account,
no server, nothing leaves the machine but those requests. Click the bar item for the
monitor, or run it in a terminal tile with `pulse-limits tui`.

## Install

Both platforms get the same two things: the `pulse-limits` command, with the terminal
monitor, and the bar item. The bar item is optional: `pulse-limits bar off` removes it,
`bar on` brings it back.

### macOS

Needs SwiftBar and a login in Claude Code, the Codex CLI or the Grok CLI.

```sh
brew install --cask swiftbar
brew install dnacenta/tap/pulse-limits    # builds the binary with cargo
pulse-limits install
```

Or the one-line installer (needs [cargo](https://rustup.rs) and installs SwiftBar if missing):

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

Or by hand: `git clone https://github.com/dnacenta/pulse-limits.git && cd pulse-limits && ./build.sh && ./bin/pulse-limits install`.

### Linux

Needs a login in a CLI as above; for the bar item, Waybar and a Nerd Font.

```sh
nix profile install github:dnacenta/pulse-limits
pulse-limits bar on
```

Or the one-line installer (needs git and cargo; clones into `~/.local/share/pulse-limits`,
links the command into `~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

Or by hand: `git clone … && cd pulse-limits && ./build.sh && ./bin/pulse-limits bar on`.

`bar on` writes `~/.config/waybar/pulse-limits.jsonc` and prints the two lines to add to
your own Waybar config; it never edits `config.jsonc` or `style.css`:

```jsonc
"include": ["~/.config/waybar/pulse-limits.jsonc"],
"modules-right": [..., "custom/pulse-limits"],
```

The ring is Waybar's own `format-icons` picked by percentage, using the Nerd Font circle
slices, and the tone travels as a CSS class (`warn`, `crit`, `stale`, `dead`) you can
colour in `style.css`. Click opens the monitor in the browser. Update with
`pulse-limits update` (git and Homebrew installs) or by rebuilding the flake.

**Any distro.** The binary, the Waybar module and the paths are the same everywhere;
only how you get the binary differs. At runtime it needs `pgrep` (procps) and
`xdg-open` (xdg-utils), which every desktop has. Building needs Rust 1.85 or newer:

| Distro | Get the binary |
|---|---|
| NixOS, or any box with Nix | `nix profile install github:dnacenta/pulse-limits`; nothing else to install |
| Arch, Omarchy, CachyOS | `pacman -S rust git waybar ttf-nerd-fonts-symbols`, then the installer or `./build.sh` (Arch's `rust` is current) |
| Debian, Ubuntu | their packaged `rustc` is too old; install Rust from [rustup.rs](https://rustup.rs), `apt install git curl xdg-utils procps`, then the installer or `./build.sh`; Waybar and a Nerd Font from their sites |

## Providers

Enable them from the right-click menu (PROVIDERS) or with `pulse-limits provider NAME`.
The bar shows the first enabled provider whose CLI is running right now, else the first
enabled one; the panel and the TUI show all of them.

| Provider | Login it reuses | Where the numbers come from | Windows |
|---|---|---|---|
| `claude` (default) | Claude Code's, from the Keychain on macOS or `~/.claude/.credentials.json` on Linux | `api.anthropic.com/api/oauth/usage`, the call behind `/usage` in Claude Code | 5-hour session, week, per-model weekly caps |
| `codex` | the Codex CLI's, `~/.codex/auth.json` | `chatgpt.com/backend-api/wham/usage`, the call the CLI makes for its own rate limits | 5-hour, week, per-model extras |
| `grok` | the Grok CLI's, `~/.grok/auth.json` | `cli-chat-proxy.grok.com/v1/billing`, the call the CLI makes for its credit pool; the plan name from `/v1/settings` | the weekly credit pool, on-demand spend when capped |

Each provider is asked once every five minutes, backs off three minutes after a 429,
and fails on its own: one that cannot read anything shows a dead ring saying why and
never takes the others down. Tokens are read, never refreshed and never written; when
one expires the monitor says `TOKEN EXPIRED` until you open that CLI once. The `grok`
binary is never run from here: a run may self-update or rewrite the login.

All three endpoints are undocumented and may change; when they do, the monitor shows an
error rather than a wrong number. Claude and Grok are tested against real accounts (Max
and X Premium+); Codex was written from [CodexBar](https://github.com/steipete/CodexBar)'s
source and tested on fixtures only. If yours misbehaves, run `pulse-limits doctor` and
open an issue with its output; there are no tokens in it.

## What you see

- **The session number**, big, with its reset countdown. Green, amber from 60 %, red from 85 %.
- **The heartbeat**: its rate follows what Claude Code is doing right now, measured from
  the transcripts it writes locally, `2.4K TOK/MIN · 3 SESSIONS`, or `IDLE 45M` and a flat line.
- **Live between readings**: the API is asked every five minutes; in between the session
  number is dead-reckoned from the tokens produced since, calibrated on earlier readings,
  and snaps to the real value at each reading (`· EST` while estimated).
- **The other windows** as rings, **a 12-hour trend**, and honest failure states.
- **Five themes**, right-click → THEME: `crt`, `modern`, `cyber`, `synth`, `analog`.

| CRT | Modern | Synth |
|:---:|:---:|:---:|
| <img src="docs/crt.png" width="280" alt="CRT theme"> | <img src="docs/modern.png" width="280" alt="Modern theme"> | <img src="docs/synth.png" width="280" alt="Synth theme"> |

## Terminal UI

```sh
pulse-limits tui                 # or: pulse-limits claude | codex | grok
pulse-limits tui --theme synth
```

The same monitor in a terminal, for a tmux pane or a tiling-WM tile: braille ECG, block
digits, bars, sparkline. Keys: `q` quit, `t` next theme, `r` reload, `?` help. It reads
the payload the bar last wrote every five seconds and builds one itself every two
minutes, so it works with no bar at all and never bypasses the API throttle.

## Commands

```
pulse-limits install / uninstall   set up or remove the bar item (SwiftBar or Waybar)
pulse-limits bar on|off|status     the bar item alone
pulse-limits provider NAME         enable or disable claude | codex | grok
pulse-limits theme NAME            crt | modern | cyber | synth | analog
pulse-limits tui [NAME]            the monitor in the terminal
pulse-limits open                  show or hide the monitor
pulse-limits status                the current reading as JSON, every provider
pulse-limits doctor                check every link of the chain, per provider
pulse-limits raw [NAME]            a provider's last raw usage reply
pulse-limits refresh               force a live fetch now
pulse-limits update / version
pulse-limits keychain NAME         pin the Keychain entry holding the Claude login (macOS)
```

Plumbing, for scripts: `waybar`, `swiftbar`, `payload`, `activity`, `estimate PCT FETCHED`, `reset`.

## How it works

- **Rust for everything that computes**: providers, cache, backoff, dead reckoning,
  activity, the bar outputs, the TUI. Dependencies: ratatui, crossterm, serde, serde_json,
  ureq with rustls, base64.
- **Swift only where AppKit is unavoidable**: the popover window and the menu bar image
  (`popover/`, `menubar/`), both just asking the binary for data.
- **A 17-line SwiftBar shim**, `pulse-limits.1m.sh`: SwiftBar reads plugin metadata from
  comments in a script file, so that file execs `pulse-limits swiftbar`.
- **Data flow**: the binary packs the numbers as base64 JSON into the URL fragment of
  `panel.html`; the popover, the browser on Linux and the TUI read it from there.
  Cache in `~/.cache/pulse-limits`, settings in `~/.config/pulse-limits`.

## Troubleshooting

`NO SIGNAL` means no reading at all; the header top-right says why (`? NO LOGIN`,
`? TOKEN EXPIRED`, `? NO PLAN ACCESS`, `? RATE LIMITED`, `? NETWORK`). Run
`pulse-limits doctor`: it checks every link per provider without printing tokens.

- **No login on this machine**: run `claude`, `codex login` or `grok login` once.
- **Token expired**: open that CLI once; it refreshes its own token.
- **Rate limited**: the usage endpoints have small per-account quotas, shared by every
  machine on the account; the plugin backs off and keeps the last reading.
- **Claude login in another Keychain entry** (`CLAUDE_CONFIG_DIR` setups): the doctor
  lists every entry; pin one with `pulse-limits keychain NAME`.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
