<p align="center">
  <img src="docs/crt.png" width="640" alt="PulseLimits: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

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

No prerequisites. The installer adds what is missing with your package manager (git, a
C linker, procps, xdg-utils; Waybar when you are on a Wayland session without one),
Rust with rustup when there is no cargo, the Nerd Font symbols when no Nerd Font is
installed, then clones into `~/.local/share/pulse-limits`, builds, links the command into
`~/.local/bin` and writes the Waybar module. Debian, Ubuntu, Arch and derivatives,
Fedora, openSUSE and Alpine are known to it. On a box with Nix it installs the flake
instead, which `nix profile install github:pulse-null/pulse-limits` does by hand.
`PULSE_LIMITS_NO_SUDO=1` makes it only tell you what to install.

The bar item is a Waybar custom module. `pulse-limits bar on` writes
`~/.config/waybar/pulse-limits.jsonc` and prints the two lines to add to your own
config; it never edits `config.jsonc` or `style.css` (Omarchy manages those):

```jsonc
"include": ["~/.config/waybar/pulse-limits.jsonc"],
"modules-right": [..., "custom/pulse-limits"],
```

The ring is Waybar's own `format-icons` picked by percentage, using the Nerd Font circle
slices; the tone travels as a CSS class (`warn`, `crit`, `stale`, `dead`) for `style.css`.
A click opens the monitor in the browser. The same binary, module and paths serve every
distro. Omarchy 4 replaced Waybar with its own bar, so the module targets Omarchy 3,
Hyprland and Sway setups on Waybar. `pulse-limits bar off` removes the module file and
keeps the command; `pulse-limits update` pulls and rebuilds.

### macOS

```sh
brew install pulse-null/tap/pulse-limits && pulse-limits install
```

Homebrew builds the binary with its Rust toolchain; `pulse-limits install` adds SwiftBar
if you do not have it, links the plugin and starts it. Left-click opens the monitor in a
popover, right-click gives a text menu with the theme and provider switches. Two things
here are Swift because they need AppKit: the popover window and the menu bar image
(`popover/`, `menubar/`), both just asking the binary for data; and SwiftBar reads plugin
metadata from comments in a script file, so a 17-line shim, `pulse-limits.1m.sh`, execs
`pulse-limits swiftbar`. `pulse-limits bar off` unlinks the plugin and keeps the command;
`pulse-limits update` upgrades through Homebrew.

Claude Code on macOS keeps its login in the Keychain, keyed per config directory, so a
Mac can hold several `Claude Code-credentials` entries; the binary scans them and takes
the one with a claude.ai login, the doctor lists them, and `pulse-limits keychain NAME`
pins one.

By hand, on either platform: `git clone https://github.com/pulse-null/pulse-limits.git && cd pulse-limits && ./build.sh && ./bin/pulse-limits install` (needs cargo).

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

Each provider is asked once every five minutes, backs off three minutes after a 429,
and fails on its own: one that cannot read anything shows a dead ring saying why and
never takes the others down. Tokens are read, never refreshed and never written; when
one expires the monitor says `TOKEN EXPIRED` until you open that CLI once.

- **Grok** has one limit, a weekly credit pool, shown as `WEEK`; the token lives six
  hours. The `grok` binary is never run from here: a run may self-update or rewrite the
  login. Tested on an X Premium+ account; SuperGrok, Free and Team replies are unseen.
- **Claude** is the only provider with live activity: the heartbeat's rate follows the
  output tokens Claude Code writes to its local transcripts (`2.4K TOK/MIN · 3 SESSIONS`,
  or `IDLE 45M` and a flat line), and between readings the session number is
  dead-reckoned from those tokens, calibrated on earlier readings, snapping to the real
  value at each reading (`· EST` while estimated). Tested on a Max account.
- **Codex** was written from [CodexBar](https://github.com/steipete/CodexBar)'s source and
  tested on fixtures only; per-model caps become rings named after the limit.

All three endpoints are undocumented and may change; when they do, the monitor shows an
error rather than a wrong number. If yours misbehaves, run `pulse-limits doctor` and open
an issue with its output; there are no tokens in it.

## The monitor

- **The session number**, big, with its reset countdown. Green, amber from 60 %, red from 85 %.
- **The other windows** as rings, **a 12-hour trend** of the session window, and honest
  failure states: `NO SIGNAL` with the reason when there is no reading.
- **Five themes**: `crt`, `modern`, `cyber`, `synth`, `analog`, from the provider menu or
  `pulse-limits theme NAME`.

| CRT | Modern | Synth |
|:---:|:---:|:---:|
| <img src="docs/crt.png" width="280" alt="CRT theme"> | <img src="docs/modern.png" width="280" alt="Modern theme"> | <img src="docs/synth.png" width="280" alt="Synth theme"> |

## Terminal UI

```sh
pulse-limits tui                 # or: pulse-limits grok | claude | codex
pulse-limits tui --theme synth
```

The same monitor in a terminal, for a tmux pane or a tiling-WM tile: braille ECG, block
digits, bars, sparkline. Keys: `q` quit, `t` next theme, `r` reload, `?` help. It reads
the payload the bar last wrote every five seconds and builds one itself every two
minutes, so it works with no bar at all and never bypasses the API throttle.

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
Dependencies: ratatui, crossterm, serde, serde_json, ureq with rustls, base64.

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
