# Security

pulse-limits reads the login tokens your vendor CLIs keep on disk and sends them, over
TLS, only to the vendors' own usage endpoints. Anything that weakens that is a security
bug: a token in any output, a write to a credential store, a request to any other host,
a spawned vendor CLI.

## How to report

Privately, never as a public issue. Either way reaches only the maintainer:

1. On GitHub: open <https://github.com/pulse-null/pulse-limits/security/advisories/new>.
   That is the "Report a vulnerability" button on the repository's **Security and quality**
   tab (top row, beside Pull requests), under **Advisories** in its left sidebar.
2. By email: <security@pulse-null.com>, if you cannot use GitHub or prefer not to.

Expect a reply within a few days. A fix ships as a patch release and the report is
credited unless you ask otherwise.

Only the latest release is supported.
