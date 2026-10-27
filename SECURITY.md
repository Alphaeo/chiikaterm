# Security policy

## Supported versions

Security fixes go into the latest release. chiika is young (0.x): update to the latest version
before reporting.

## Reporting a vulnerability

**Do not open a public issue.** Report it privately on GitHub:
[Security › Report a vulnerability](https://github.com/Alphaeo/chiikaterm/security/advisories/new).

Say what an attacker can do, on which version and OS, and how to reproduce it. You will get an
answer within a week; once fixed, the advisory is published with credit to you, unless you prefer
otherwise.

## What chiika trusts

Useful to know what counts as a vulnerability:

- The daemon of a project listens on `127.0.0.1` only, and every connection must give the token
  of `.chiika/daemon.toml`. chiika does not restrict who can read that file beyond the
  project folder's own permissions: whoever can read the project folder is trusted.
- Agents run Claude Code with the permission rules of the project's policy; Claude Code enforces
  them. A way for an agent to get past a `deny` rule through chiika (its tools, hooks or
  settings) is a vulnerability.
- The project folder (`.chiika/`, `TEAM.md`, `policy.toml`) is trusted like the rest of the
  repository: open chiika only in projects you trust.
