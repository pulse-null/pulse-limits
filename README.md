<p align="center">
  <img src="docs/crt.png" width="660" alt="PulseLimits, CRT theme: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  Your Claude plan limits in the menu bar (macOS) or in Waybar (Linux), as a retro patient monitor.<br>
  The heartbeat is live: it races while Claude Code is streaming and slows when it idles.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Menu bar item: 16% and a ring">
</p>

```sh
brew install dnacenta/tap/pulse-limits && pulse-limits install     # macOS
nix profile install github:dnacenta/pulse-limits && pulse-limits bar on   # Linux
```

A [SwiftBar](https://github.com/swiftbar/SwiftBar) plugin on macOS, a Waybar `custom`
module on Linux. No account, no server, no tracking. It reuses the login Claude Code
already keeps on your machine, and the only thing that ever leaves it is the one request
Claude Code itself makes when you type `/usage`.

## What you get

**In the bar**: the 5-hour window as a number and a ring, battery-style.
Green, amber from 60 %, red from 85 %.

**On click**, a monitor:

- **A heartbeat that means something.** Its rate follows what Claude Code is doing
  right now, measured from the transcripts it writes locally: output tokens per
  minute across every open session, and how long since anything happened.
  `3.4K TOK/MIN · 7 SESSIONS` with a racing trace; `IDLE 45M` and a flat line
  when nothing is running.
- **The session window** as a big number, its reset countdown, and a bar.
- **The other windows** as rings: the week, plus any per-model weekly cap your
  plan carries.
- **A 12-hour trend** of the session window, so you can see when you burned it.
- **Live between readings.** The API is asked once every five minutes; in
  between, the session number is dead-reckoned from the tokens Claude Code
  produced since the last reading, using a rate calibrated from the previous
  readings. The bar updates every minute, the panel every few seconds
  while open. While the number is dead-reckoned the caption says `· EST`; it
  snaps to the real value at each reading.
- **Honest when it cannot know.** Stale data turns amber and says how old it is.
  No data is a flat line with `NO SIGNAL`.

On macOS, right-click for a plain text menu and the theme switcher. On Linux the
same summary is the module's tooltip, and `pulse-limits theme NAME` switches.

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

## Install

Both installs give you the same two things: the `pulse-limits` command (with the
terminal monitor, `pulse-limits tui [provider]`) and the bar item, SwiftBar on macOS,
Waybar on Linux. The bar item is optional: `pulse-limits bar off` removes it and keeps
the command, `pulse-limits bar on` brings it back.

|          | macOS | Linux |
|----------|-------|-------|
| Needs    | Homebrew, the Xcode Command Line Tools, a Claude Code login (run `claude` once) | `jq`, `curl`, `python3`, a Claude Code login; for the bar item Waybar and a Nerd Font |
| Package  | `brew install dnacenta/tap/pulse-limits` | `nix profile install github:dnacenta/pulse-limits`, or a git checkout |
| Bar item | `pulse-limits install` (links the SwiftBar plugin, starts SwiftBar) | `pulse-limits bar on`, then two lines in your Waybar config (below) |
| Update   | `pulse-limits update` | `nix flake update` and rebuild, or `git pull` |
| Remove   | `pulse-limits uninstall` | `pulse-limits uninstall` |

### macOS

**Homebrew** (recommended):

```sh
brew install --cask swiftbar          # if you do not have SwiftBar yet
brew install dnacenta/tap/pulse-limits
pulse-limits install
```

**One-line installer**, which also installs jq and SwiftBar if they are missing:

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

**By hand:**

```sh
git clone https://github.com/dnacenta/pulse-limits.git
cd pulse-limits && ./build.sh && ./pulse-limits install
```

`./build.sh` compiles the two small Swift helpers (about ten seconds). Every path
is safe to re-run.

### Linux

**Nix** (the flake builds the scripts and the Python helper; no Swift, no build step):

```sh
nix profile install github:dnacenta/pulse-limits
pulse-limits bar on
```

or, in a NixOS / Home Manager flake, add the input and put
`inputs.pulse-limits.packages.${pkgs.system}.default` in your packages.

**One-line installer** (clones into `~/.local/share/pulse-limits`, links the command
into `~/.local/bin`, writes the Waybar module file):

```sh
curl -fsSL https://raw.githubusercontent.com/dnacenta/pulse-limits/main/install.sh | bash
```

**By hand:**

```sh
git clone https://github.com/dnacenta/pulse-limits.git
cd pulse-limits && ./pulse-limits bar on
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
{"text": "17%", "tooltip": "PULSE LIMITS  ·  MAX 20X\nSESSION  ███░░░░░░░░░░░░░░░░░  17%   RESETS IN 2H 14M\n...", "class": "ok", "percentage": 17}
```

`class` is one of `ok`, `warn`, `crit`, `stale`, `dead`. `percentage` is what Waybar
uses to pick the ring from `format-icons`: the eight glyphs are the Material Design
circle slices from the Nerd Fonts (`md-circle-slice-1` to `-8`, U+F0A9E to U+F0AA5), so
the bar font must be a Nerd Font (Omarchy's `JetBrainsMono Nerd Font` is one). Waybar
indexes the array by `percentage / (100 / 8)` in integer arithmetic, so each step is
12 % wide and the full circle shows from 84 %; that is Waybar's rounding, not the data.
The tooltip is the same summary as the macOS right-click menu, plus the activity line
and the age of the reading.

**What works on Linux and what does not yet:**

- Works: the reading, the dead reckoning and the heartbeat measurement (`bin/pulse-activity.py`,
  the Swift helper ported to Python, same numbers), the Waybar module, `pulse-limits`
  end to end, the themes, the doctor.
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
pulse-limits refresh       force a live fetch now
pulse-limits open          show or hide the monitor
pulse-limits status        print the current reading as JSON
pulse-limits waybar        print one Waybar JSON line (what the Waybar module runs)
pulse-limits doctor        check every link of the chain
pulse-limits update        update to the latest release
pulse-limits keychain NAME pin the Keychain entry holding the login (macOS)
```

## How it works

Everything is a Bash script, one HTML file, a small Python script, and (on macOS)
two tiny Swift programs. The BSD/GNU differences sit behind two shell helpers,
`epoch_of` and `mtime_of`, chosen by `uname`.

1. **Credentials.** On macOS, `security find-generic-password -s "Claude Code-credentials"`
   reads the OAuth token Claude Code stores in your Keychain. On Linux, Claude Code
   writes the same JSON to `~/.claude/.credentials.json` (mode 0600; `CLAUDE_CONFIG_DIR`
   is honoured), and the plugin reads that. The same item carries your plan tier, which
   is where the `MAX 20X` badge comes from.
2. **Usage.** One `GET https://api.anthropic.com/api/oauth/usage` with that token.
   It is the call behind `/usage` in Claude Code. It is undocumented, so it may
   change without notice; when it does, the monitor shows an error instead of a
   wrong number.
3. **Activity.** Claude Code writes every turn to `~/.claude/projects/*/*.jsonl`.
   The helper sums the output tokens of assistant lines stamped in the last
   minute, deduplicated by message id because a streaming reply is written
   several times, and notes when any transcript was last touched. About 30 ms.
   The Swift helper does it on macOS; `bin/pulse-activity.py` (Python 3, standard
   library only) does the same on Linux, or on a Mac without the Swift build.
   The same accounting drives the **dead reckoning**: each API reading is an
   anchor, the tokens produced between two anchors calibrate a rate in
   percent per output token (smoothed across readings, kept in
   `~/.cache/pulse-limits/calib.json`), and between readings the session
   number is anchor plus rate times tokens since. Uncalibrated until two
   readings with activity between them have been seen.
4. **The page.** The script packs the numbers as base64 JSON into the URL
   fragment of `panel.html`. The page reads it, draws everything on a canvas,
   and animates the trace. Countdowns tick in the page.
5. **The popover (macOS).** `bin/pulse-popover` is a borderless, non-activating panel
   with one WKWebView, shown under the mouse. It stays resident for ten idle
   minutes so the next click is instant, pushes fresh activity every two
   seconds and fresh usage every two minutes while visible, and exits on its own.
   SwiftBar's built-in webview popover would work too but paints a title bar
   that cannot be turned off; the script falls back to it if the helper is
   missing. On Linux the page opens in the browser instead (see above).
6. **The bar item.** On macOS `bin/pulse-menubar` renders the number and the ring
   as one 2x PNG in the system menu bar font, one per menu bar appearance. On Linux
   Waybar renders the number as text and picks the ring glyph from `format-icons`.

The plugin declares `runInBash=false` so SwiftBar executes it directly. Its
default wraps every run and click in `zsh -l -c`, which loads your login
profile each time, half a second on a machine with nvm.

Claude Code refreshes the token roughly hourly while it runs. If it has not
run for a while, the API answers 401 and the monitor turns amber with
`TOKEN EXPIRED`. Open Claude Code once and it heals. Refreshing the token from
here is deliberately not done: rotating it behind Claude Code's back could log
Claude Code out.

## Privacy

- The token is read from the Keychain (macOS) or the credentials file (Linux) on
  each run and never written anywhere.
- The only network traffic is the usage request to `api.anthropic.com`.
- Transcripts are read locally for token counts and timestamps only; their
  content is never parsed beyond the `usage` field.
- Cache and settings live in `~/.cache/pulse-limits` and `~/.config/pulse-limits`
  (`XDG_CACHE_HOME` / `XDG_CONFIG_HOME` are honoured).

## Troubleshooting

`NO SIGNAL` means the plugin has no reading at all, and the header top-right
says why: `? NO LOGIN`, `? TOKEN EXPIRED`, `? NO PLAN ACCESS`, `? RATE
LIMITED`, `? NETWORK`. Run the doctor and read it top to bottom; it checks
every link of the chain without printing your token:

```sh
pulse-limits doctor
```

Common causes:

- **No Claude Code login on this machine.** Run `claude` once and log in with a
  claude.ai account. An API-key login gives a token the usage endpoint
  refuses (`NO PLAN ACCESS`); on Linux it writes no usable token at all (`NO LOGIN`).
- **The login is in a differently named Keychain entry** (macOS). Claude Code keys
  its entry to `CLAUDE_CONFIG_DIR`, so a Mac with a work config dir can have
  several `Claude Code-credentials…` entries. The plugin scans them and takes
  the one with a claude.ai login; the doctor lists them all. To force one:
  `pulse-limits keychain 'Claude Code-credentials-…'`.
- **Token expired.** Claude Code refreshes it while it runs; open it once.
- **Rate limited.** The usage endpoint has a small per-account quota, shared
  by every machine on the account. The plugin backs off for three minutes after
  a 429 and keeps the last good reading; a fresh install with no reading yet
  shows `NO SIGNAL` until the quota frees up.
- **SwiftBar asked for Keychain access** and the prompt was dismissed. Run
  `security find-generic-password -s "Claude Code-credentials" -w >/dev/null`
  in a terminal and click *Always Allow*.
- **`readlink -f` unsupported** on macOS before 12.3: the plugin cannot find
  its files through the symlink. Copy the folder instead of linking.
- **Waybar shows nothing** (Linux). `pulse-limits bar status`, then check that the
  `include` line and `"custom/pulse-limits"` are in the config Waybar actually
  loads, that `pulse-limits` is on the PATH Waybar was started with (the module
  file names it by absolute path when it is not), and that the bar font is a Nerd
  Font. `pulse-limits waybar` in a terminal shows the line the module gets.

## Tuning

Top of `pulse-limits.1m.sh`: the live-call throttle, trend depth, popover size.
In `panel.html`: each theme's palette, the tone thresholds (60 % amber, 85 % red),
the BPM mapping in `bpmNow()`, and the ECG shape (a sum of five gaussians: P, Q,
R, S, T).

Preview a theme without a bar:

```sh
npx playwright screenshot --viewport-size=520,316 --wait-for-timeout=1500 \
  "file://$PWD/panel.html?theme=synth#$(printf '%s' '{"plan":"MAX 20X","source":"LIVE","status":"","hint":"","fetched":0,"history":[],"windows":[{"label":"SESSION","pct":63,"resets":null}],"credits":null,"activity":{"tok_per_min":1200,"idle_s":2,"sessions":1}}' | base64)" out.png
```

`attic/claude64.5m.sh` is where this started: a Commodore 64 boot screen in
plain text.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
See [LICENSE](LICENSE).
