# Security policy

## Reporting a vulnerability

**Please report privately, not as a public issue.**

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/mijowi/edamame/security/advisories/new).

Please include what you'd put in a bug report — version, OS, terminal — plus
a proof-of-concept document if the issue is triggered by file content.

You should get an acknowledgement within a week. edamame is maintained by one
person, so please allow reasonable time for a fix before disclosing publicly.

## Supported versions

edamame is pre-1.0. Only the latest release receives fixes.

## What counts as a vulnerability

edamame is built on the assumption that **the documents it opens are
untrusted** — you might open a `.md` file from a repository, a download, or
an AI agent. Anything that lets document content escape that boundary is in
scope, in particular:

- Image or SVG decoding that crashes, hangs, or exhausts memory
- A document causing a network request you didn't consent to, or reaching a
  private/internal address
- Reading files outside the document's own directory tree, especially where
  the content ends up in an HTML export
- Executable content surviving into exported HTML (scripts, unsafe link
  schemes, inline SVG)
- Anything reaching a shell, or a subprocess argument built from document
  content

**Out of scope:** a malicious local config (`config.toml`,
`keybindings.toml`, `$EDITOR`) is trusted — an attacker who can write those
files already controls your account. The terminal emulator is trusted too.

The full threat model, and the hardening currently in place, is documented in
[`docs/security.md`](docs/security.md). Contributors changing any
content-handling path should also read
[`docs/dev/security-invariants.md`](docs/dev/security-invariants.md).
