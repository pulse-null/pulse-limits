<!-- Title: type(PL-<issue>): subject   e.g. feat(PL-8): Grok provider. Commits: type: subject, no issue key. -->

Closes #

## What

## Tested

- OS and bar:
- `./build.sh`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`: all pass
- Live API calls made while developing (count, and why):
- Screenshot attached if `panel.html` or the TUI changed:

## Checklist

- [ ] Tokens are read from the CLI's own store, never refreshed, written or printed
- [ ] No vendor CLI is spawned; nothing leaves the machine but the usage GETs
- [ ] Tests use fixtures and the local test server, not the live API
- [ ] Fixtures are redacted (tokens, account ids)
- [ ] For a provider: registered in `KNOWN`, `run`, `doctor`, `has_login_for`; README row added
