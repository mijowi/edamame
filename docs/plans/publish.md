# Publish checklist

- [ ] Add a license
- [ ] Update cargo.toml with license and other fields needed for crates.io publish
- [ ] Set up CI with these, at minimum:
    - `cargo fmt --check`
    - `cargo clippy --all-targets -- -D warnings`
    - `cargo test`
- [ ] Move everything in `issues.md` to GitHub issue tracking
- [ ] Prune docs, especially plans
- [ ] Tag v0.1.0
- [ ] Make a nice README with screenshots and videos
- [ ] Add user-facing documentation
- [ ] Add a documentation page for the terminal upgrade notice and a link to the page from within edamame
- [ ] Compile binaries and add to GitHub Releases. Targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu (build on the oldest Ubuntu runner you can, for glibc compat), x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu
- [ ] Set GitHub repo to public
- [ ] Publish to crates.io
- [ ] Make a homebrew tap

Suggested order: README + LICENSE + Cargo metadata → CI green → repo public → tag v0.1.0 → release workflow produces binaries → cargo publish.
