# Versioning

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).

This is the guideline for choosing a version number and for changing anything users depend on. It applies from 1.0 onward. Before 1.0, follow it where it's cheap to do so, but breaking changes are still allowed.

People assume an upgrade won't break their setup. The aim is to live up to that, not to hold every detail fixed forever, so use judgment and treat a rare, well-announced break as better than a permanent bad name.

## Version numbers

Version numbers describe the application, following [Semantic Versioning](https://semver.org). The library target is internal, so a change to it alone never affects the version.

- **Major:** something users rely on may need their attention.
- **Minor:** new features and other backward-compatible changes.
- **Patch:** bug fixes only.

## What to keep working

- `config.toml` keys and values, and the config and theme folder locations
- `keybindings.toml` action names and the key syntax
- Default keybindings
- Theme file keys
- Command-line flags, exit codes and `--diff`
- Custom export commands, including the `{html}` and `{out}` placeholders
- Vim `:` commands
- The `<!-- tui-columns: [...] -->` comment, which is written into users' documents
- How supported Markdown syntax renders, and the heading anchors it produces

## What can change freely

- Screen layout, wording, hints and palette labels
- The output of `--help` and `--doctor`
- The look of built-in themes
- `state.toml`, which is machine-written and migrated by edamame
- Log files
- The library target
- Rendering polish that doesn't change a document's meaning

## Making a change

**Renames:** keep the old name working for the rest of the major version.

- Use a `serde` alias plus `migrate_legacy_config_keys` for config keys, so the user's file is updated in place.
- Use an alias for theme keys and action names.
- An alias produces no warning. A rename that needs no action from the user can ship in a minor release.

**Removals:** save these for a major release. Unknown keys only warn, so removing a key is low risk, but a value that no longer parses resets the whole file to defaults. Keep accepting a removed value and fall back to a sensible one.

**Default keybindings:** keep existing defaults. A new default can only use a key that's currently unbound.

**Bug fixes:** a fix can change behavior that relied on the bug, and doesn't need a major release. Use judgment when many users are likely to depend on the old behavior.

**Security and data-safety fixes:** these can ship in any release, even if they change behavior.

**Announcing breaks:** every breaking change needs a clear [changelog](../../CHANGELOG.md) entry saying what to do. That entry also reaches users through the update and [post-upgrade](post-upgrade.md) notices.

## Experimental features

The experimental label lets a feature ship before its design has settled. The rest of this guideline doesn't apply to it until it graduates, except where noted below.

**Shipping one:**

- It's off by default, and users opt in through an ordinary config key or value. Don't add an `[experimental]` section, because graduating would then rename the key.
- Its user docs say it's experimental and may change or be removed.
- Its changelog entry is marked `(experimental)`.
- Any settings row or palette entry for it says "(experimental)".

**While it's experimental:**

- It can change or be removed in any minor release, with a changelog entry.
- It must not change behavior for anyone who hasn't opted in.
- Data safety and the [security invariants](security-invariants.md) apply in full.
- Removing it must follow the rule above: a removed value falls back rather than resetting the config.

**Ending the label:** in a minor release, either remove the label or remove the feature, and say so in the changelog. Decide within a few minor releases, so the label doesn't become a permanent excuse. After graduation, the feature follows the rest of this guideline.

## Minimum supported Rust version

`rust-version` can rise in a minor release, not a patch release, and the changelog should mention it. A patch release mustn't raise it through a dependency update either. Raising it is a deliberate change: re-run `cargo msrv find` and update the `msrv` job in `ci.yml`.

## Platforms

Linux and macOS are supported, and Windows is best-effort. See [windows.md](windows.md) for what that covers.
