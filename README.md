<p align="center">
  <img src="docs/crt.png" width="660" alt="PulseLimits, CRT theme: a phosphor-green patient monitor showing a heartbeat, the session percentage, week and model rings, and a 12-hour trend">
</p>

<h1 align="center">PulseLimits</h1>

<p align="center">
  Your Claude and Codex plan limits in the macOS menu bar, as a retro patient monitor.<br>
  The heartbeat is live: it races while Claude Code is streaming and slows when it idles.
</p>

<p align="center">
  <img src="docs/menubar.png" width="140" alt="Menu bar item: 16% and a ring">
</p>

```sh
brew install dnacenta/tap/pulse-limits && pulse-limits install
```

A [SwiftBar](https://github.com/swiftbar/SwiftBar) plugin. No account, no server, no
tracking. It reuses the logins your CLIs already keep on this Mac (Claude Code's
in the Keychain, the Codex CLI's in `~/.codex`), and the only thing that ever
leaves your Mac is the one request each CLI itself makes to show its own usage.

## What you get

**In the menu bar**: the 5-hour window as a number and a ring, battery-style.
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
  readings. The menu bar updates every minute, the panel every few seconds
  while open. While the number is dead-reckoned the caption says `· EST`; it
  snaps to the real value at each reading.
- **Honest when it cannot know.** Stale data turns amber and says how old it is.
  No data is a flat line with `NO SIGNAL`.

Right-click for a plain text menu, the theme switcher and the provider switches.

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

## Providers

One reader per CLI, in `providers/<name>.sh`. Each is asked once every five
minutes, backs off for three minutes after a 429, keeps its own cache, trend and
last reply, and fails on its own: a provider that cannot read anything shows a
dead ring that says why (`CODEX ? NO LOGIN`) and never takes the others down.

| Provider | Reads | Asks | Enabled |
|---|---|---|---|
| `claude` | the claude.ai login Claude Code keeps in your Keychain (or `~/.claude/.credentials.json`) | `GET api.anthropic.com/api/oauth/usage`, the call behind `/usage` in Claude Code | by default |
| `codex` | the ChatGPT login the [Codex CLI](https://github.com/openai/codex) keeps in `~/.codex/auth.json` (`CODEX_HOME` moves it) | `GET chatgpt.com/backend-api/wham/usage`, the call the CLI itself makes for its rate limits; a `chatgpt_base_url` in `config.toml` is followed | right-click → PROVIDERS |

Enable or disable one from the right-click menu, under **PROVIDERS**, or with
`pulse-limits provider codex`. The list lives in `~/.config/pulse-limits/providers`,
one name per line, in priority order.

**The menu bar shows one provider**: the first enabled one whose CLI process is
running right now (`pgrep -x claude`, `pgrep -x codex`), else the first enabled
one. **The panel shows all of them**: the session block and the trend belong to
the one in the menu bar, and the other providers' windows join the rings as
`CODEX 5H`, `CODEX 7D` (up to four rings; the Analog theme keeps its two gauges).
The plan badge names the provider when more than one is on.

Codex specifics: the plan comes from the `chatgpt_plan_type` claim in the id
token until the first reply, which carries `plan_type`; the access token's `exp`
claim is checked before calling, so a stale login shows `TOKEN EXPIRED` without
a request (open the Codex CLI once, it refreshes its own token; nothing is
refreshed from here); an API-key login (`OPENAI_API_KEY` in `auth.json`) has no
plan limits and shows `NO PLAN ACCESS`; per-model caps in
`additional_rate_limits` become rings named after the last word of the limit
(`SPARK`); the prepaid credit balance is not shown. The heartbeat and the dead
reckoning stay Claude-only, since they are measured from Claude Code's
transcripts: with Codex in the menu bar the session number moves only at each
reading.

**Untested against a live account.** The Codex reader was written from
[CodexBar](https://github.com/steipete/CodexBar)'s (MIT) source and tested with
fixture files and a local HTTP server replaying the documented reply shape
(200, 401, 429, a changed shape, no network), not against `chatgpt.com`. Both
interfaces are undocumented and may change; when they do, the monitor shows an
error rather than a wrong number. If yours misbehaves, run `pulse-limits doctor`
and open an issue with its output (there are no tokens in it).

## Install

You need macOS, [Homebrew](https://brew.sh), the Xcode Command Line Tools, and a
login in at least one CLI: Claude Code (run `claude` once) or the Codex CLI
(`codex login`).

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
pulse-limits provider NAME enable or disable a provider: claude | codex
pulse-limits refresh       force a live fetch now
pulse-limits open          show or hide the monitor
pulse-limits status        print the current reading as JSON, every enabled provider
pulse-limits doctor        check every link of the chain, per provider
pulse-limits raw [NAME]    print a provider's last raw usage reply
pulse-limits update        update to the latest release
pulse-limits keychain NAME pin the Keychain entry holding the Claude login
```

## How it works

Everything is a Bash script, one HTML file, and two tiny Swift programs.

1. **Providers.** The plugin runs `providers/<name>.sh` for each enabled
   provider. A reader finds the CLI's login, makes one request to the CLI's own
   usage endpoint, and prints one JSON document: the plan, `status`/`hint` when
   something is wrong, and `windows` (`SESSION` is the short one the menu bar
   shows, `WEEK` the seven-day one, anything else a per-model cap), each with a
   percentage and a reset time. `providers/_common.sh` holds what they share:
   the five-minute throttle, the 429 backoff, the per-provider cache
   (`usage-<name>.json`), last reply and trend history.
2. **Claude.** `security find-generic-password -s "Claude Code-credentials"`
   reads the OAuth token Claude Code stores in your Keychain; the same item
   carries the plan tier behind the `MAX 20X` badge. Then
   `GET https://api.anthropic.com/api/oauth/usage`, the call behind `/usage` in
   Claude Code. **Codex.** `~/.codex/auth.json`, then
   `GET https://chatgpt.com/backend-api/wham/usage` with the account id header
   the CLI sends. Both endpoints are undocumented and may change without
   notice; when they do, the monitor shows an error instead of a wrong number.
3. **Activity.** Claude Code writes every turn to `~/.claude/projects/*/*.jsonl`.
   The helper sums the output tokens of assistant lines stamped in the last
   minute, deduplicated by message id because a streaming reply is written
   several times, and notes when any transcript was last touched. About 30 ms.
   The same accounting drives the **dead reckoning** of Claude's session
   number: each API reading is an
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

Each CLI refreshes its own token while it runs. If it has not run for a while,
its API answers 401 (the Codex reader also reads the token's `exp` claim and
does not even ask) and the monitor turns amber with `TOKEN EXPIRED`. Open the
CLI once and it heals. Refreshing a token from here is deliberately not done:
rotating it behind the CLI's back could log the CLI out.

## Privacy

- Tokens are read on each run (the Keychain for Claude, `~/.codex/auth.json`
  for Codex) and never written anywhere; `pulse-limits doctor` and `raw` never
  print them.
- The only network traffic is one usage request per enabled provider, to
  `api.anthropic.com` and `chatgpt.com` respectively.
- Transcripts are read locally for token counts and timestamps only; their
  content is never parsed beyond the `usage` field.
- Cache and settings live in `~/.cache/pulse-limits` and `~/.config/pulse-limits`.

## Troubleshooting

`NO SIGNAL` means the provider in the menu bar has no reading at all, and the
header top-right says why: `? NO LOGIN`, `? TOKEN EXPIRED`, `? NO PLAN ACCESS`,
`? RATE LIMITED`, `? NETWORK`. Another enabled provider with a problem shows it
under its dead ring (`CODEX ? NO LOGIN`) and in the right-click menu. Run the
doctor and read it top to bottom; it checks every link of the chain, per
provider, without printing your tokens:

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
- **Claude token expired.** Claude Code refreshes it while it runs; open it once.
- **No Codex login on this Mac.** Run `codex login`; the reader wants
  `~/.codex/auth.json` (or `$CODEX_HOME/auth.json`, but SwiftBar does not see
  your shell's `CODEX_HOME`, so the default folder is what counts). An API-key
  login has no plan limits (`NO PLAN ACCESS`).
- **Codex token expired.** The Codex CLI refreshes it while it runs; open it once.
- **Rate limited.** Each usage endpoint has a small per-account quota, shared
  by every Mac on the account. The plugin backs off for three minutes after
  a 429 and keeps that provider's last good reading; a fresh install with no
  reading yet shows `NO SIGNAL` until the quota frees up.
- **SwiftBar asked for Keychain access** and the prompt was dismissed. Run
  `security find-generic-password -s "Claude Code-credentials" -w >/dev/null`
  in a terminal and click *Always Allow*.
- **`readlink -f` unsupported** on macOS before 12.3: the plugin cannot find
  its files through the symlink. Copy the folder instead of linking.

## Tuning

Top of `pulse-limits.1m.sh`: the live-call throttle, popover size, the known
providers. `providers/_common.sh`: the trend depth and the shared reader
plumbing; a new provider is one more `providers/<name>.sh` printing the same
document, added to `KNOWN_PROVIDERS`. In `panel.html`: each theme's palette,
the tone thresholds (60 % amber, 85 % red), the BPM mapping in `bpmNow()`, and
the ECG shape (a sum of five gaussians: P, Q, R, S, T).

Preview a theme without SwiftBar (or feed it the output of `pulse-limits status`):

```sh
npx playwright screenshot --viewport-size=520,316 --wait-for-timeout=1500 \
  "file://$PWD/panel.html?theme=synth#$(printf '%s' '{"plan":"MAX 20X","provider":"claude","source":"LIVE","status":"","hint":"","fetched":0,"history":[],"windows":[{"label":"SESSION","pct":63,"resets":null},{"label":"WEEK","pct":21,"resets":null}],"credits":null,"activity":{"tok_per_min":1200,"idle_s":2,"sessions":1},"providers":[{"name":"claude","plan":"MAX 20X","status":"","hint":"","windows":[]},{"name":"codex","plan":"PLUS","status":"","hint":"","windows":[{"label":"SESSION","pct":17,"resets":null},{"label":"WEEK","pct":42,"resets":null}]}]}' | base64)" out.png
```

`attic/claude64.5m.sh` is where this started: a Commodore 64 boot screen in
plain text.

## License

GNU Affero General Public License v3.0 or later. Copyright (C) 2026 Daniel Nacenta.
See [LICENSE](LICENSE).
