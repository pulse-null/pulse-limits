# Security

pulse-limits reads the login tokens your vendor CLIs keep on disk and sends them, over
TLS, only to the vendors' own usage endpoints. Anything that weakens that is a security
bug: a token in any output, a write to a credential store, a request to any other host,
a spawned vendor CLI.

Report it privately at
<https://github.com/pulse-null/pulse-limits/security/advisories/new> ("Report a vulnerability"
under the repository's Security tab, the one beside Issues and Pull requests).
Do not open a public issue. Expect a reply within a few days; a fix ships as a patch
release and the report is credited unless you ask otherwise.

Only the latest release is supported.
