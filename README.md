<p align="center">
  <img src="docs/crt.png" width="660" alt="PulseLimits, CRT theme: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  Your Claude plan limits in the macOS menu bar, as a retro patient monitor.<br>
  The heartbeat is live: it races while Claude Code is streaming and slows when it idles.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Menu bar item: 16% and a ring">
</p>

```sh
brew install dnacenta/tap/pulse-limits && pulse-limits install
```

A [SwiftBar](https://github.com/swiftbar/SwiftBar) plugin. No account, no server, no
tracking. It reuses the login Claude Code already keeps in your Keychain, and the
only thing that ever leaves your Mac is the one request Claude Code itself makes
when you type `/usage`.

## What you get

**In the menu bar**: the 5-hour window as a number and a ring, battery-style.
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
  readings. The menu bar updates every minute, the panel every few seconds
  while open. While the number is dead-reckoned the caption says `· EST`; it
  snaps to the real value at each reading.
- **Honest when it cannot know.** Stale data turns amber and says how old it is.
  No data is a flat line with `NO SIGNAL`.

Right-click for a plain text menu and the theme switcher.

## Themes

Five looks, same data. Right-click the menu bar item, open **THEME**, pick one.

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

You need macOS, [Homebrew](https://brew.sh), the Xcode Command Line Tools, and a
Claude Code login (run `claude` once).

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
is safe to re-run. Update with `pulse-limits update`, remove with
`pulse-limits uninstall`.

## The `pulse-limits` command

```
pulse-limits install       link the plugin into SwiftBar and start it
pulse-limits uninstall     unlink it, drop cache and settings
pulse-limits theme NAME    crt | modern | cyber | synth | analog
pulse-limits refresh       force a live fetch now
pulse-limits open          show or hide the monitor
pulse-limits status        print the current reading as JSON
pulse-limits doctor        check every link of the chain
pulse-limits update        update to the latest release
pulse-limits keychain NAME pin the Keychain entry holding the login
```

## How it works

Everything is a Bash script, one HTML file, and two tiny Swift programs.

1. **Credentials.** `security find-generic-password -s "Claude Code-credentials"`
   reads the OAuth token Claude Code stores in your Keychain. The same item
   carries your plan tier, which is where the `MAX 20X` badge comes from.
2. **Usage.** One `GET https://api.anthropic.com/api/oauth/usage` with that token.
   It is the call behind `/usage` in Claude Code. It is undocumented, so it may
   change without notice; when it does, the monitor shows an error instead of a
   wrong number.
3. **Activity.** Claude Code writes every turn to `~/.claude/projects/*/*.jsonl`.
   The helper sums the output tokens of assistant lines stamped in the last
   minute, deduplicated by message id because a streaming reply is written
   several times, and notes when any transcript was last touched. About 30 ms.
   The same accounting drives the **dead reckoning**: each API reading is an
   anchor, the tokens produced between two anchors calibrate a rate in
   percent per output token (smoothed across readings, kept in
   `~/.cache/pulse-limits/calib.json`), and between readings the session
   number is anchor plus rate times tokens since. Uncalibrated until two
   readings with activity between them have been seen.
4. **The page.** The script packs the numbers as base64 JSON into the URL
   fragment of `panel.html`. The page reads it, draws everything on a canvas,
   and animates the trace. Countdowns tick in the page.
5. **The popover.** `bin/pulse-popover` is a borderless, non-activating panel
   with one WKWebView, shown under the mouse. It stays resident for ten idle
   minutes so the next click is instant, pushes fresh activity every two
   seconds and fresh usage every two minutes while visible, and exits on its own.
   SwiftBar's built-in webview popover would work too but paints a title bar
   that cannot be turned off; the script falls back to it if the helper is
   missing.
6. **The menu bar image.** `bin/pulse-menubar` renders the number and the ring
   as one 2x PNG in the system menu bar font, one per menu bar appearance.

The plugin declares `runInBash=false` so SwiftBar executes it directly. Its
default wraps every run and click in `zsh -l -c`, which loads your login
profile each time, half a second on a machine with nvm.

Claude Code refreshes the token roughly hourly while it runs. If it has not
run for a while, the API answers 401 and the monitor turns amber with
`TOKEN EXPIRED`. Open Claude Code once and it heals. Refreshing the token from
here is deliberately not done: rotating it behind Claude Code's back could log
Claude Code out.

## Privacy

- The token is read from the Keychain on each run and never written anywhere.
- The only network traffic is the usage request to `api.anthropic.com`.
- Transcripts are read locally for token counts and timestamps only; their
  content is never parsed beyond the `usage` field.
- Cache and settings live in `~/.cache/pulse-limits` and `~/.config/pulse-limits`.

## Troubleshooting

`NO SIGNAL` means the plugin has no reading at all, and the header top-right
says why: `? NO LOGIN`, `? TOKEN EXPIRED`, `? NO PLAN ACCESS`, `? RATE
LIMITED`, `? NETWORK`. Run the doctor and read it top to bottom; it checks
every link of the chain without printing your token:

```sh
pulse-limits doctor
```

Common causes:

- **No Claude Code login on this Mac.** Run `claude` once and log in with a
  claude.ai account. An API-key login gives a token the usage endpoint
  refuses (`NO PLAN ACCESS`).
- **The login is in a differently named Keychain entry.** Claude Code keys
  its entry to `CLAUDE_CONFIG_DIR`, so a Mac with a work config dir can have
  several `Claude Code-credentials…` entries. The plugin scans them and takes
  the one with a claude.ai login; the doctor lists them all. To force one:
  `pulse-limits keychain 'Claude Code-credentials-…'`.
- **Token expired.** Claude Code refreshes it while it runs; open it once.
- **Rate limited.** The usage endpoint has a small per-account quota, shared
  by every Mac on the account. The plugin backs off for three minutes after
  a 429 and keeps the last good reading; a fresh install with no reading yet
  shows `NO SIGNAL` until the quota frees up.
- **SwiftBar asked for Keychain access** and the prompt was dismissed. Run
  `security find-generic-password -s "Claude Code-credentials" -w >/dev/null`
  in a terminal and click *Always Allow*.
- **`readlink -f` unsupported** on macOS before 12.3: the plugin cannot find
  its files through the symlink. Copy the folder instead of linking.

## Tuning

Top of `pulse-limits.1m.sh`: the live-call throttle, trend depth, popover size.
In `panel.html`: each theme's palette, the tone thresholds (60 % amber, 85 % red),
the BPM mapping in `bpmNow()`, and the ECG shape (a sum of five gaussians: P, Q,
R, S, T).

Preview a theme without SwiftBar:

```sh
npx playwright screenshot --viewport-size=520,316 --wait-for-timeout=1500 \
  "file://$PWD/panel.html?theme=synth#$(printf '%s' '{"plan":"MAX 20X","source":"LIVE","status":"","hint":"","fetched":0,"history":[],"windows":[{"label":"SESSION","pct":63,"resets":null}],"credits":null,"activity":{"tok_per_min":1200,"idle_s":2,"sessions":1}}' | base64)" out.png
```

`attic/claude64.5m.sh` is where this started: a Commodore 64 boot screen in
plain text.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
See [LICENSE](LICENSE).
