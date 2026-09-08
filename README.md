<p align="center">
  <img src="docs/crt.png" width="660" alt="PulseLimits, CRT theme: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  Your Claude and Codex plan limits in the menu bar (macOS) or in Waybar (Linux), as a retro patient monitor.<br>
  The heartbeat is live: it races while Claude Code is streaming and slows when it idles.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Menu bar item: 16% and a ring">
</p>

```sh
brew install dnacenta/tap/pulse-limits && pulse-limits install                # macOS
nix profile install github:dnacenta/pulse-limits && pulse-limits bar on      # Linux
```

One Rust binary, `pulse-limits`. A [SwiftBar](https://github.com/swiftbar/SwiftBar)
plugin on macOS, a Waybar `custom` module on Linux, a terminal UI on both. No account,
no server, no tracking. It reuses the logins your CLIs already keep on this machine
(Claude Code's in the Keychain or its credentials file, the Codex CLI's in `~/.codex`),
and the only thing that ever leaves it is the one request each CLI itself makes to
show its own usage.

## What you get

**In the bar**: the 5-hour window as a number and a ring, battery-style.
Green, amber from 60 %, red from 85 %. With more than one provider enabled it
shows the one whose CLI is running right now (see [Providers](#providers)).

**On click**, a monitor:

- **A heartbeat that means something.** Its rate follows what Claude Code is doing
  right now, measured from the transcripts it writes locally: output tokens per
  minute across every open session, and how long since anything happened.
  `3.4K TOK/MIN · 7 SESSIONS` with a racing trace; `IDLE 45M` and a flat line
  when nothing is running.
- **The session window** as a big number, its reset countdown, and a bar.
- **The other windows** as rings: the week, plus any per-model cap your plan
  carries, plus the other enabled providers' windows (`CODEX 5H`, `CODEX 7D`).
- **A 12-hour trend** of the session window, so you can see when you burned it.
- **Live between readings.** The API is asked once every five minutes; in
  between, the session number is dead-reckoned from the tokens Claude Code
  produced since the last reading, using a rate calibrated from the previous
  readings. The bar updates every minute, the panel every few seconds
  while open. While the number is dead-reckoned the caption says `· EST`; it
  snaps to the real value at each reading.
- **Honest when it cannot know.** Stale data turns amber and says how old it is.
  No data is a flat line with `NO SIGNAL`.

On macOS, right-click for a plain text menu, the theme switcher and the provider
switches. On Linux the same summary is the module's tooltip, and `pulse-limits theme`
and `pulse-limits provider` switch from the terminal.

## Themes

Five looks, same data. Right-click the menu bar item, open **THEME**, pick one
(or `pulse-limits theme NAME`).

| CRT | Modern |
|:---:|:---:|
| <img src="docs/crt.png" width="420" alt="CRT theme"> | <img src="docs/modern.png" width="420" alt="Modern theme"> |
| Phosphor monitor, scanlines, 5x7 pixel font | A macOS widget, follows light and dark mode |

| Cyber | Synth |
|:---:|:---:|
| <img src="docs/cyber.png" width="420" alt="Cyber theme"> | <img src="docs/synth.png" width="420" alt="Synth theme"> |
| Neon HUD, chromatic-aberration digits | Sunset, perspective grid, the trend as a skyline |

| Analog | |
|:---:|:---:|
| <img src="docs/analog.png" width="420" alt="Analog theme"> | |
| VU meter, round gauges, chart-recorder strip | |

## Providers

One reader per CLI, in `src/providers/`. Each is asked once every five minutes,
backs off for three minutes after a 429, keeps its own cache, trend and last reply,
and fails on its own: a provider that cannot read anything shows a dead ring that says
why (`CODEX ? NO LOGIN`) and never takes the others down.

| Provider | Reads | Asks | Enabled |
|---|---|---|---|
| `claude` | the claude.ai login Claude Code keeps in your Keychain (macOS) or in `~/.claude/.credentials.json` | `GET api.anthropic.com/api/oauth/usage`, the call behind `/usage` in Claude Code | by default |
| `codex` | the ChatGPT login the [Codex CLI](https://github.com/openai/codex) keeps in `~/.codex/auth.json` (`CODEX_HOME` moves it) | `GET chatgpt.com/backend-api/wham/usage`, the call the CLI itself makes for its rate limits; a `chatgpt_base_url` in `config.toml` is followed | right-click → PROVIDERS, or `pulse-limits provider codex` |

The list lives in `~/.config/pulse-limits/providers`, one name per line, in priority
order. **The bar shows one provider**: the first enabled one whose CLI process is
running right now (`pgrep -x claude`, `pgrep -x codex`), else the first enabled one.
**The panel shows all of them**: the session block and the trend belong to the one in
the bar, and the other providers' windows join the rings as `CODEX 5H`, `CODEX 7D` (up
to four rings; the Analog theme keeps its two gauges). The plan badge names the
provider when more than one is on. `pulse-limits tui codex` (or `pulse-limits codex`)
puts Codex in the terminal whatever the bar shows.

Codex specifics: the plan comes from the `chatgpt_plan_type` claim in the id token
until the first reply, which carries `plan_type`; the access token's `exp` claim is
checked before calling, so a stale login shows `TOKEN EXPIRED` without a request (open
the Codex CLI once, it refreshes its own token; nothing is refreshed from here); an
API-key login (`OPENAI_API_KEY` in `auth.json`) has no plan limits and shows `NO PLAN
ACCESS`; per-model caps in `additional_rate_limits` become rings named after the last
word of the limit (`SPARK`); the prepaid credit balance is not shown. The heartbeat and
the dead reckoning stay Claude-only, since they are measured from Claude Code's
transcripts: with Codex in the bar the session number moves only at each reading.

**Untested against a live account.** The Codex reader was written from
[CodexBar](https://github.com/steipete/CodexBar)'s (MIT) source and tested with
fixture files and a local HTTP server replaying the documented reply shape (200, 401,
429, a changed shape, no network), not against `chatgpt.com`. Both interfaces are
undocumented and may change; when they do, the monitor shows an error rather than a
wrong number. If yours misbehaves, run `pulse-limits doctor` and open an issue with
its output (there are no tokens in it).

## Install

Every install gives you the same two things: the `pulse-limits` command (with the
terminal monitor, `pulse-limits tui [provider]`) and the bar item, SwiftBar on macOS,
Waybar on Linux. The bar item is optional: `pulse-limits bar off` removes it and keeps
the command, `pulse-limits bar on` brings it back.

|          | macOS | Linux |
|----------|-------|-------|
| Needs    | [Rust](https://rustup.rs) (cargo), the Xcode Command Line Tools, SwiftBar, a login in Claude Code (run `claude` once) or the Codex CLI (`codex login`) | Rust (cargo), a login as on macOS; for the bar item Waybar and a Nerd Font |
| Package  | `brew install dnacenta/tap/pulse-limits` (builds with the `rust` formula) | `nix profile install github:dnacenta/pulse-limits`, or a git checkout |
| Bar item | `pulse-limits install` (links the SwiftBar plugin, starts SwiftBar) | `pulse-limits bar on`, then two lines in your Waybar config (below) |
| Update   | `pulse-limits update` | `nix flake update` and rebuild, or `git pull && ./build.sh` |
| Remove   | `pulse-limits uninstall` | `pulse-limits uninstall` |

### macOS

**Homebrew** (recommended; the formula builds the binary with cargo, no Rust toolchain
of your own needed):

```sh
brew install --cask swiftbar          # if you do not have SwiftBar yet
brew install dnacenta/tap/pulse-limits
pulse-limits install
```

**One-line installer**, which needs cargo and installs SwiftBar if it is missing:

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

**By hand:**

```sh
git clone https://github.com/dnacenta/pulse-limits.git
cd pulse-limits && ./build.sh && ./bin/pulse-limits install
```

`./build.sh` runs `cargo build --release` (a minute the first time) and compiles the two
small Swift helpers (about ten seconds), all into `bin/`. Every path is safe to re-run.

### Linux

**Nix** (the flake builds the binary with `rustPlatform.buildRustPackage`):

```sh
nix profile install github:dnacenta/pulse-limits
pulse-limits bar on
```

or, in a NixOS / Home Manager flake, add the input and put
`inputs.pulse-limits.packages.${pkgs.system}.default` in your packages.

**One-line installer** (needs git and cargo; clones into `~/.local/share/pulse-limits`,
builds, links the command into `~/.local/bin`, writes the Waybar module file):

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

**By hand:**

```sh
git clone https://github.com/dnacenta/pulse-limits.git
cd pulse-limits && ./build.sh && ./bin/pulse-limits bar on
```

`pulse-limits bar on` writes `~/.config/waybar/pulse-limits.jsonc` with the module
definition and prints the two lines you add to your own config; it never edits your
`config.jsonc` or `style.css` (Omarchy manages those, and they carry comments). The
lines are:

```jsonc
"include": ["~/.config/waybar/pulse-limits.jsonc"],
"modules-right": [..., "custom/pulse-limits"],
```

The module file it writes:

```jsonc
{
  "custom/pulse-limits": {
    "exec": "pulse-limits waybar",
    "return-type": "json",
    "interval": 60,
    "format": "{text} {icon}",
    "format-icons": ["󰪞", "󰪟", "󰪠", "󰪡", "󰪢", "󰪣", "󰪤", "󰪥"],
    "on-click": "pulse-limits open",
    "signal": 8,
    "tooltip": true
  }
}
```

And the tones, for `style.css`. The class follows the session window, like the colour
of the macOS item:

```css
#custom-pulse-limits { min-width: 12px; margin: 0 7.5px; }
#custom-pulse-limits.warn  { color: #ffb000; }   /* 60 % and up */
#custom-pulse-limits.crit  { color: #ff5c5c; }   /* 85 % and up */
#custom-pulse-limits.stale { color: #ffb000; }   /* a reading, but the API could not be asked: TOKEN EXPIRED, NETWORK, ... */
#custom-pulse-limits.dead  { color: #8c8c8c; }   /* no reading at all */
```

Then reload Waybar (`pkill -SIGUSR2 waybar`; on Omarchy, `omarchy-restart-waybar`).
`pulse-limits refresh` forces a live reading and pokes the module with `pkill -RTMIN+8
waybar`; `pulse-limits bar status` says whether the file is there and Waybar is running;
`pulse-limits doctor` checks the rest.

**What `pulse-limits waybar` prints**, once a minute, for the module:

```json
{"text":"17%","tooltip":"PULSE LIMITS  ·  MAX 20X\nSESSION  ███░░░░░░░░░░░░░░░░░  17%   RESETS IN 2H 14M\n...","class":"ok","percentage":17}
```

`class` is one of `ok`, `warn`, `crit`, `stale`, `dead`. `percentage` is what Waybar
uses to pick the ring from `format-icons`: the eight glyphs are the Material Design
circle slices from the Nerd Fonts (`md-circle-slice-1` to `-8`, U+F0A9E to U+F0AA5), so
the bar font must be a Nerd Font (Omarchy's `JetBrainsMono Nerd Font` is one). Waybar
indexes the array by `percentage / (100 / 8)` in integer arithmetic, so each step is
12 % wide and the full circle shows from 84 %; that is Waybar's rounding, not the data.
The tooltip is the same summary as the macOS right-click menu (every enabled provider),
plus the activity line and the age of the reading.

**What works on Linux and what does not yet:**

- Works: the readings, both providers, the dead reckoning and the heartbeat
  measurement (same binary, same numbers), the Waybar module, the terminal UI,
  `pulse-limits` end to end, the themes, the doctor.
- Not yet: the popover. A click opens the monitor page in your browser (through a
  one-line redirect file, because `xdg-open` drops the `#fragment` that carries the
  data). A resident `webkit2gtk` panel with the same toggle behaviour as the macOS one
  is the natural next step. The page does not refresh itself in a browser tab; close
  and click again.
- Not yet: Polybar and i3blocks variants, an AUR package, Homebrew on Linux. Omarchy 4
  moved from Waybar to its own bar; the module is for Waybar setups (Omarchy 3, Hyprland,
  Sway, river...).

## The `pulse-limits` command

```
pulse-limits install       set up the bar item (SwiftBar on macOS, Waybar on Linux) and start it
pulse-limits uninstall     remove the bar item, drop cache and settings
pulse-limits bar on|off|status  the bar item alone: link it, unlink it, or say whether it is
pulse-limits theme NAME    crt | modern | cyber | synth | analog
pulse-limits provider NAME enable or disable a provider: claude | codex
pulse-limits refresh       force a live fetch now
pulse-limits open          show or hide the monitor
pulse-limits tui [NAME]    the monitor in the terminal (pulse-limits claude / codex are the same)
pulse-limits status        print the current reading as JSON, every enabled provider
pulse-limits doctor        check every link of the chain, per provider
pulse-limits raw [NAME]    print a provider's last raw usage reply
pulse-limits update        update to the latest release
pulse-limits keychain NAME pin the Keychain entry holding the Claude login (macOS)
pulse-limits version       print the installed version
```

And the plumbing the bars and the popover run, useful for scripting:
`pulse-limits waybar` (one Waybar JSON line), `pulse-limits swiftbar` (the SwiftBar
menu), `pulse-limits payload` (the document the panel reads, refreshing it),
`pulse-limits activity` (what Claude Code is doing now, as JSON), `pulse-limits
estimate PCT FETCHED` (the dead-reckoned session % from a reading), `pulse-limits reset`
(drop the cached readings so the next run asks live).

## Terminal UI

The same monitor in a terminal, for a tmux pane or a tile in a tiling window manager.
Every five seconds it reads the payload the bar last packed for the panel
(`~/.cache/pulse-limits/panel.url`, refreshed by the bar every minute), every two
minutes it builds the payload itself as the popover does, which is what feeds a box
with no bar, and every two seconds it measures the activity. So it shows what the
panel shows: the heartbeat, the session number and its reset, the other windows as
bars, twelve hours of trend. The providers keep their own API throttle.

```sh
pulse-limits tui                 # or: pulse-limits claude
pulse-limits codex               # Codex, whatever the bar shows
pulse-limits tui --theme synth
```

```
  PULSE LIMITS                                        MAX 20X  ● LIVE
  ───────────────────────────────────────────────────────────────────
  ● 2.4K TOK/MIN · 3 SESSIONS                                 SESSION
      ⢠⡄         ⣀          ⣤                            ███ ███ █ █
      ⢸⡇         ⣿         ⢀⣿                            █     █   █
    ⣀⣀⡼⡇⣰⠲⣄    ⣀⡀⡿⣄⡴⢲⡀   ⢀⣀⣸⢸⢠⠖⢦                         ███ ███  █
  ⠉⠉⠉⠁⠈⠁⠿⠁ ⠈⠉⠉⠉⠉⠁⠉⠁⠉  ⠉⠉⠉⠉⠉ ⠉⠘⠋ ⠈⠉⠉  ⠈⠉⠉⠉⠉⠉                █ █   █ █
                                                         ███ ███ █ █
  ██████████████████████████████░░░░░░░░░░░░░░░   RESET 2H 13M · EST

  WEEK   ███████████████░░░░░░░░░░░░░░░░░░░░░░   42%  RESET 4D 07H
  FABLE  ████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   12%  RESET 4D 07H

  SESSION · 12H
  ····················▁▁▁▂▂▂▃▃▃▄▄▄▅▅▆▆▆▇▇█████·····▁▁▂▂▃▃▄▄▅▅▆▆
  -12H                                                          NOW

  UPDATED 27S AGO · NEXT RESET 18:40    q quit  t theme  r reload  ? help
```

Keys: `q` quit, `t` next theme (crt, modern, cyber, synth, analog; it starts
on the panel's), `r` re-read now (it rebuilds the payload only when the panel's file
is older than two minutes), `?` help. Colour follows the percentage as in
the panel: green below 60 %, amber below 85 %, red above. Truecolor terminals get
the panel's palettes, others the 16 ANSI colours. It degrades down to about
40x12 and looks best from 90x28 up; the trace is braille, so the terminal font
needs those glyphs (most do).

## How it works

Everything that computes is one Rust program (`src/`, built on ratatui for the
terminal, ureq for the two HTTP calls, serde for the JSON). Around it: one HTML
file, and on macOS two tiny Swift programs and a five-line shim.

1. **Providers.** `pulse-limits` runs one reader per enabled provider
   (`src/providers/claude.rs`, `codex.rs`). A reader finds the CLI's login, makes one
   request to the CLI's own usage endpoint, and produces one document: the plan,
   `status`/`hint` when something is wrong, and `windows` (`SESSION` is the short one
   the bar shows, `WEEK` the seven-day one, anything else a per-model cap), each with
   a percentage and a reset time. `src/providers/mod.rs` holds what they share: the
   five-minute throttle, the 429 backoff, the per-provider cache
   (`~/.cache/pulse-limits/usage-<name>.json`), last reply and trend history.
2. **Claude.** `security find-generic-password -s "Claude Code-credentials"` reads the
   OAuth token Claude Code stores in your Keychain (on Linux, the same JSON from
   `~/.claude/.credentials.json`, mode 0600; `CLAUDE_CONFIG_DIR` is honoured); the
   same item carries the plan tier behind the `MAX 20X` badge. Then
   `GET https://api.anthropic.com/api/oauth/usage`, the call behind `/usage` in
   Claude Code. **Codex.** `~/.codex/auth.json`, then
   `GET https://chatgpt.com/backend-api/wham/usage` with the account id header
   the CLI sends. Both endpoints are undocumented and may change without
   notice; when they do, the monitor shows an error instead of a wrong number.
3. **Activity.** Claude Code writes every turn to `~/.claude/projects/*/*.jsonl`.
   `src/activity.rs` sums the output tokens of assistant lines stamped in the last
   minute, deduplicated by message id because a streaming reply is written
   several times, and notes when any transcript was last touched. A few
   milliseconds. The same accounting drives the **dead reckoning** of Claude's session
   number (`src/estimate.rs`): each API reading is an anchor, the tokens produced
   between two anchors calibrate a rate in percent per output token (smoothed across
   readings, kept in `~/.cache/pulse-limits/calib.json`), and between readings the
   session number is anchor plus rate times tokens since. Uncalibrated until two
   readings with activity between them have been seen.
4. **The page.** `pulse-limits payload` packs the numbers as base64 JSON into the URL
   fragment of `panel.html` (`~/.cache/pulse-limits/panel.url`). The page reads it,
   draws everything on a canvas, and animates the trace. Countdowns tick in the page.
5. **The bar.** SwiftBar runs `pulse-limits.1m.sh` once a minute; that file is
   nothing but SwiftBar's metadata comments and `exec bin/pulse-limits swiftbar`, which
   prints the menu bar item and the right-click menu. Every click in that menu runs
   the binary directly (`runInBash=false`: no `zsh -l -c`, no login profile, no half
   a second on a machine with nvm). On Linux, Waybar runs `pulse-limits waybar`.
6. **The popover (macOS).** `bin/pulse-popover` is a borderless, non-activating panel
   with one WKWebView, shown under the mouse by `pulse-limits open`. It stays resident
   for ten idle minutes so the next click is instant, runs `pulse-limits activity`
   every two seconds and `pulse-limits payload` every two minutes while visible, and
   exits on its own. It is AppKit, so it is Swift: nothing else in it computes. SwiftBar's
   built-in webview popover would work too but paints a title bar that cannot be turned
   off; the menu falls back to it if the helper is missing. On Linux the page opens in
   the browser instead (see above).
7. **The menu bar image.** `bin/pulse-menubar` renders the number and the ring as one
   2x PNG in the system menu bar font, one per menu bar appearance. Also AppKit, also
   Swift, also nothing but drawing. On Linux Waybar renders the number as text and
   picks the ring glyph from `format-icons`.

Each CLI refreshes its own token while it runs. If it has not run for a while,
its API answers 401 (the Codex reader also reads the token's `exp` claim and
does not even ask) and the monitor turns amber with `TOKEN EXPIRED`. Open the
CLI once and it heals. Refreshing a token from here is deliberately not done:
rotating it behind the CLI's back could log the CLI out.

**Grok.** `pulse-limits provider grok` adds the Grok CLI. Its login lives in
`~/.grok/auth.json` (`GROK_HOME` moves it), and the numbers come from the call the CLI
itself makes for its quota, `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`,
plus `/settings` about once an hour for the plan name (`X PREMIUM+`, `SUPERGROK`,
`SUPERGROK HEAVY`). Grok has one limit, a weekly credit pool: it is shown as `WEEK` and,
having no session window, takes the menu bar slot; an on-demand cap appears as
`ONDEMAND`. The CLI's token lives six hours and only the CLI refreshes it, so after a
quiet evening the monitor says `TOKEN EXPIRED` until you open `grok` once. The token is
never refreshed from here and the `grok` binary is never run: a run may self-update,
sync its config, or rewrite the login.

## Privacy

- Tokens are read on each run (the Keychain or the credentials file for Claude,
  `~/.codex/auth.json` for Codex) and never written anywhere; `pulse-limits doctor`
  and `raw` never print them.
- The only network traffic is one usage request per enabled provider, to
  `api.anthropic.com` and `chatgpt.com` respectively.
- Transcripts are read locally for token counts and timestamps only; their
  content is never parsed beyond the `usage` field.
- Cache and settings live in `~/.cache/pulse-limits` and `~/.config/pulse-limits`
  (`XDG_CACHE_HOME` / `XDG_CONFIG_HOME` are honoured).

## Troubleshooting

`NO SIGNAL` means the provider in the bar has no reading at all, and the
header top-right says why: `? NO LOGIN`, `? TOKEN EXPIRED`, `? NO PLAN ACCESS`,
`? RATE LIMITED`, `? NETWORK`. Another enabled provider with a problem shows it
under its dead ring (`CODEX ? NO LOGIN`) and in the right-click menu. Run the
doctor and read it top to bottom; it checks every link of the chain, per
provider, without printing your tokens:

```sh
pulse-limits doctor
```

Common causes:

- **No Claude Code login on this machine.** Run `claude` once and log in with a
  claude.ai account. An API-key login gives a token the usage endpoint
  refuses (`NO PLAN ACCESS`); on Linux it writes no usable token at all (`NO LOGIN`).
- **The login is in a differently named Keychain entry** (macOS). Claude Code keys
  its entry to `CLAUDE_CONFIG_DIR`, so a Mac with a work config dir can have
  several `Claude Code-credentials…` entries. The reader scans them and takes
  the one with a claude.ai login; the doctor lists them all. To force one:
  `pulse-limits keychain 'Claude Code-credentials-…'`.
- **Claude token expired.** Claude Code refreshes it while it runs; open it once.
- **No Codex login on this machine.** Run `codex login`; the reader wants
  `~/.codex/auth.json` (or `$CODEX_HOME/auth.json`, but the bar does not see
  your shell's `CODEX_HOME`, so the default folder is what counts). An API-key
  login has no plan limits (`NO PLAN ACCESS`).
- **Codex token expired.** The Codex CLI refreshes it while it runs; open it once.
- **Rate limited.** Each usage endpoint has a small per-account quota, shared
  by every machine on the account. The reader backs off for three minutes after
  a 429 and keeps that provider's last good reading; a fresh install with no
  reading yet shows `NO SIGNAL` until the quota frees up.
- **SwiftBar asked for Keychain access** and the prompt was dismissed. Run
  `security find-generic-password -s "Claude Code-credentials" -w >/dev/null`
  in a terminal and click *Always Allow*.
- **`readlink -f` unsupported** on macOS before 12.3: the shim cannot find the
  binary through the SwiftBar symlink. Copy the folder instead of linking.
- **Waybar shows nothing** (Linux). `pulse-limits bar status`, then check that the
  `include` line and `"custom/pulse-limits"` are in the config Waybar actually
  loads, that `pulse-limits` is on the PATH Waybar was started with (the module
  file names it by absolute path when it is not), and that the bar font is a Nerd
  Font. `pulse-limits waybar` in a terminal shows the line the module gets.

## Tuning

`src/payload.rs`: the live-call throttles (270 s from the bar, 45 s from the popover
and the TUI). `src/providers/mod.rs`: the trend depth, the backoff, the known
providers; a new provider is one more module producing the same document, added to
`KNOWN`. `src/swiftbar.rs`: the popover size and the menu palette. In `panel.html`:
each theme's palette, the tone thresholds (60 % amber, 85 % red), the BPM mapping in
`bpmNow()`, and the ECG shape (a sum of five gaussians: P, Q, R, S, T).

Preview a theme without a bar (or feed it the output of `pulse-limits payload`):

```sh
npx playwright screenshot --viewport-size=520,316 --wait-for-timeout=1500 \
  "file://$PWD/panel.html?theme=synth#$(pulse-limits payload | base64)" out.png
```

`attic/claude64.5m.sh` is where this started: a Commodore 64 boot screen in
plain text.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
See [LICENSE](LICENSE).
