# Changelog

All notable changes to edamame are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each released version's section is also what ships as the GitHub release notes: `dist` reads the section matching the tag and puts it at the top of the release body, above the generated install and download tables. edamame's update check reads that same body back and shows the part above the `## Install` heading, so **an entry written here is what users see in the "Update available" modal**. Keep entries short and user-facing for that reason — a release cut without a matching section here still notifies, just with no summary.

## [Unreleased]

### Added

- Startup update check. edamame now checks GitHub for a newer release at most once a day and shows a one-time notice, with the release notes, when a new version first appears. It says nothing when you are up to date. Opt out with `editor.check_for_updates`, the "Check for updates" row in the settings overlay, or the toggle on the welcome screen.
- "Check for updates" in the command palette, and a matching button on the About page, for checking on demand at any time.

### Changed

- The About page no longer contacts GitHub when it opens, and no longer shows a "Current release" row.

## [0.1.0] - 2026-08-17

First public release.
