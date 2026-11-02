# Security Policy

## Supported versions

Only the latest release gets security fixes.

## Reporting a vulnerability

Please don't open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** on
[njoneja/shredlab](https://github.com/njoneja/shredlab/security/advisories/new).

Include what you found, how to reproduce it, and which version and OS you
used. You should hear back within a week. Once a fix is released, the
advisory is published with credit to you (unless you'd rather not).

## Scope

ShredLab is a desktop app that runs entirely on your machine; it has no
server or accounts. Things we'd treat as vulnerabilities include:

- An audio file that crashes the app in an exploitable way or runs code when
  opened.
- The app reading, writing or deleting files outside the song library and
  the files you chose.
- Bundled code or model files that are not what the release says they are.

Known issues in dependencies are tracked automatically (Dependabot and
`cargo deny`), but reports are still welcome if you notice one first.
