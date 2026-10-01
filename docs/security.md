# Security

edamame is built to open Markdown documents you didn't write — a README in a cloned repository, a download, a file produced by an AI agent — without letting that document do anything you didn't ask for. This page explains what you're protected from, the settings that matter, and the one thing to stay careful about.

## What edamame trusts

**The documents you open are treated as untrusted**, along with anything they reference: images, diagrams, links, and remote URLs.

**Your own setup is trusted**: your configuration files, custom export commands, `$EDITOR`, and your terminal emulator. Anyone who can change those already controls your account, so edamame doesn't try to defend against them.

## What protects you

### Nothing in a document runs as a program

Document content never reaches a shell or runs as code. Mermaid diagrams and math formulas are drawn by renderers built into edamame that cannot execute code, read your files, or access the network, and the same is true of syntax highlighting in code blocks. SVG images can't read local files or reach the network either.

### Oversized or malformed content is bounded

A document can be crafted to exhaust memory or tie up the CPU — a tiny image file that expands to gigabytes when decoded, for example, or a code block designed to make highlighting crawl. edamame puts limits on images, diagrams, formulas, and code blocks, and anything over a limit is shown in a simpler form rather than processed in full: an image becomes an `[Image: alt text]` placeholder, a diagram or formula is shown as its source, and a code block is shown without colors. Your text is never hidden; you can always read and edit it.

### Remote images are fetched only with your permission

An image hosted on the web can be used to tell a document's author when and where you opened it, like a tracking pixel in an email. By default edamame asks before loading remote images from a document. You can change this with `remote_policy` in [configuration.md](configuration.md).

Even after you allow it, edamame refuses to fetch from your own machine or your local network (your router, other devices, cloud-provider metadata services), so a document can't use edamame to probe what's behind your firewall.

### Exported HTML is safe to share and open

An exported HTML file is usually something you send to other people, so edamame removes anything in it that could run in a browser. Harmless raw HTML in the document is kept: formatting such as `<details>`, `<kbd>` and `<sub>` survives, while scripts, event handlers like `onclick`, and styles are removed. Links that would run code (`javascript:`, `vbscript:`, `data:`) lose their target but keep their text. Diagrams and formulas are embedded as images, which a browser never runs code from.

### HTML export asks before embedding files from outside the document's folder

With **Inline images** turned on (it's off by default), the export embeds the document's images into the HTML file. A document could reference a private image elsewhere on your computer, such as `../../Pictures/passport.jpg`, hoping you'll send it back inside the export. So:

- Images in the document's own folder (or below it) are embedded normally.
- Images from anywhere else are listed by their full path, and edamame asks you first. **Embed** includes them; **Don't embed** leaves them as links.
- Only image files are ever embedded, so a document can't use this to pull in something like an SSH key.

### The update check sends nothing about you

Once a day at startup, edamame asks GitHub whether a newer version exists. The request URL is fixed — nothing in a document or your configuration can change it — and it carries only edamame's version number: no account, no usage data, nothing from your documents. Nothing is downloaded or installed. You can turn this off on the welcome screen or in Settings.

## What to be careful about

### Links open without asking

Following a link hands its target straight to your operating system. For web pages that means your browser, but a link can also point at another app (such as a `vscode://` link) or at a local file — and depending on your system, opening a file may *run* it rather than display it. A document that arrives bundled with other files could link to one of them.

Before following a link in a document you don't trust, check where it points.

## Reporting a problem

Please report privately rather than opening a public issue: [**Report a vulnerability**](https://github.com/mijowi/edamame/security/advisories/new).

The reporting policy — what's in scope, what to include, expected response time — lives in [`SECURITY.md`](../SECURITY.md) at the repository root.

---

*Contributors: how each of these protections works, and the invariants a change must not regress, are in [`dev/security-invariants.md`](dev/security-invariants.md).*
