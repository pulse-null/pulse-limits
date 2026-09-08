# Contributing

One maintainer, spare time. Issues and pull requests get read; replies can take a week.
Small, focused changes land. Anything larger needs an issue first. Be civil; that is the
whole code of conduct.

## Before you start

Open an issue before you start, even for small things. It prevents duplicate work and
lets me say no before you spend a weekend. Typos can skip this. Then branch from `main`
as `feat/<name>` or `fix/<name>` and open a pull request.

## Ground rules

1. Tokens come from the vendor CLI's own credential store. Never printed, never sent
   anywhere but the vendor's own endpoints. The one write is Grok's refresh, done the way
   its CLI does it, only while the CLI is closed, atomically, never deleting the file.
   `doctor` stays token-free.
2. The vendor CLIs are never spawned. Grok's self-updates and can delete its login.
   Read their files; do not run them.
3. Nothing leaves the machine except the GET requests to the usage endpoints. No
   telemetry, no update pings, no third-party hosts.
4. Quotas are tiny: Anthropic's endpoint refused a second call within a minute. Tests use
   fixtures and the local test server, never the live API. Probe live rarely and on
   purpose, and say in the pull request how many calls you made.
5. Fail loudly. `READER FAILED` on screen beats a plausible wrong number.

A pull request that crosses one of these gets a note asking for the change before review.

## Setup and checks

Rust 1.85 or newer. macOS also needs the Xcode Command Line Tools for the two Swift
files. Linux needs nothing else.

```sh
./build.sh                                   # release binary into bin/, swiftc on macOS
cargo test                                   # unit tests, local sockets, no network
cargo clippy --all-targets -- -D warnings
cargo fmt --check                            # rustfmt.toml keeps the house width
```

All four must pass; CI runs the same on Ubuntu and macOS. `panel.html` is checked by
rendering it in headless Chromium with a `pulse-limits payload` result in the URL
fragment, the TUI with a pty capture; if you touch either, attach a screenshot.

CI also measures line coverage with `cargo llvm-cov` and fails under 90 % on macOS, so
new code comes with tests: fixtures and the local server for HTTP, fake commands on a
private `PATH` for `security`, `open` and friends, ratatui's `TestBackend` for the TUI. A
test never reads the terminal, never calls a live API and never touches the real
`~/.config`.

## Adding a provider

Providers live in `src/providers/`. Copy the newest one, `grok.rs`, and change it. Do
not abstract three providers into a framework.

1. In the issue, name the vendor, the CLI whose login you will reuse, the endpoint that
   CLI already calls for its own limits, and a redacted sample reply.
2. Add `src/providers/<name>.rs` with exactly these two entry points:

   ```rust
   pub fn run(min_interval: i64) -> Doc
   pub fn doctor(pstatus: &str)
   ```

   `run` never fails: on any error it returns a `Doc` with `status` and `hint` set.
   Panics are caught upstream and shown as `READER FAILED`; that is the safety net, not
   the plan.
3. Use `Store` for all state: `due(min_interval)`, `get(url, headers)`, `accept(body)`,
   `set(status, hint)`, `set_backoff(BACKOFF_SECS)`, `cached()`, `emit(plan, windows,
   credits)`, `doctor_digests(digest)`. It gives you the cache, the throttle, the 180 s
   backoff after a 429, the history and the last-reply file. Do not write your own.
4. Register in `src/providers/mod.rs`: append the name to `KNOWN` (the name is also the
   CLI's process name), add one arm in `run` and one in `doctor`, and a login check in
   `has_login_for`.
5. Put real replies, with tokens and account ids redacted, under
   `tests/fixtures/<name>/`. Name invented ones `*.synthetic.json`. Load them with
   `include_str!`.
6. Tests go in the same file. Cover each HTTP branch (200, 401, 403, 404, 429, refused,
   bad shape) with `testing::serve(code, body)` and `testing::refused()`, show that no
   call is made without a usable token, and show the 429 backoff. Use
   `testing::Scratch::new("<name>")` so the real cache is never touched, and hold
   `testing::ENV` while you set environment variables.
7. `doctor` prints paths, ages and HTTP codes. Never the token, never the account.
8. Add a row to the README's provider table.

## Style

`cargo fmt` with the repo's `rustfmt.toml`, clippy clean, English identifiers. Comments
say why, not what, and are short. If the standard library or an existing dependency can
do it, use that.

## Commits and pull requests

Commits follow the Angular convention, `type: subject`, optionally `type(scope): subject`.
The subject is imperative, lower case, no period. Types:

| type | for |
|---|---|
| `feat` | a new capability (a provider, a command, a theme) |
| `fix` | a bug, with what was wrong |
| `docs` | README, this file, comments |
| `refactor` | code movement with no behaviour change |
| `test` | tests only |
| `style` | formatting only |
| `chore` | releases, formula, tooling, dependencies |

```
feat: Grok provider
fix(tui): read panel.url every 5 s, run --payload every 120 s
docs: shorter README, TUI
chore(formula): sha256 for v0.5.5
```

No issue key in commits and no trailers. The pull request title carries the issue as
scope, `type(PL-<n>): subject`, where `n` is the GitHub issue number and PL stands for
PulseLimits: `feat(PL-8): Grok provider`. One topic per pull request, rebased on `main`.
Say what you tested, on which OS, and how many live API calls you made.

## Reporting bugs

Open an issue with `pulse-limits version`, your OS and bar, and the output of
`pulse-limits doctor`. It prints no tokens; it does print file paths and, for some
providers, account ids, so look before pasting. It calls the API only when the last run
already failed. If a number is wrong, add `pulse-limits raw <provider>`, the last API
reply, with identifiers redacted; that is what I need to see.

## Security

A way to leak, write or misuse a credential is not an issue for the tracker. See
[SECURITY.md](SECURITY.md) for how to report it privately.

## Releases

The maintainer cuts releases, version bumps included. A pull request never changes the
version, a tag or the formula.

## License

AGPL-3.0-or-later. Your contribution is under the same license, inbound equals outbound.
No CLA, no copyright assignment.
