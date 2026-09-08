---
name: New provider
about: Another CLI whose plan limits should show up
labels: enhancement
---

**Vendor and CLI**: name, where the CLI is installed from.

**The login it keeps**: the file (or Keychain entry) and which fields hold the token and the plan. Redact values.

**The endpoint the CLI itself calls for its limits**: URL, method, headers. How you found it (the CLI's source, its logs, an open-source tool that reads it).

**A redacted sample reply** and what each field means (windows, percentages, reset times, plan name).

**Token lifetime and refresh**: how long it lives, whether the CLI rotates the refresh token, anything that means the reader must never refresh it.

**Which plans you can test on.**
