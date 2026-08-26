# Publishing to the AUR

## Account setup (once)

1. Register at https://aur.archlinux.org, add your SSH public key under My Account.
2. Clone the (empty) package repo — the name you pick is the repo:
```bash
git clone ssh://aur@aur.archlinux.org/edamame.git
cd edamame
```

The source package (recommended as primary)

Build from the GitHub tag tarball. `Cargo.lock` is committed, so `--frozen` works and the build is reproducible. Create PKGBUILD:

```
# Maintainer: mijowi <mijowi@mijowi.com>
pkgname=edamame
pkgver=0.1.2
pkgrel=1
pkgdesc="A fast TUI Markdown editor and viewer"
arch=('x86_64' 'aarch64')
url="https://github.com/mijowi/edamame"
license=('Apache-2.0')
depends=('gcc-libs')
makedepends=('cargo')
source=("$pkgname-$pkgver.tar.gz::https://github.com/mijowi/edamame/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('SKIP')   # replace with real hash, see below

prepare() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR=target
  cargo build --frozen --release
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm0755 -t "$pkgdir/usr/bin/" target/release/edamame
  install -Dm0644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
```

Notes specific to edamame:
- I deliberately omitted a check(). The watcher tests need live FSEvents/inotify and the clean-chroot build sandbox usually withholds them (same failure you see under the agent sandbox) — a cargo test check phase would hang. If you want a check phase, scope it: cargo test --frozen --release -- --skip watcher.
- depends=('gcc-libs') covers the glibc/libgcc link. The default clipboard feature pulls arboard, which on Linux is pure-Rust (x11rb + wayland protocol over a socket), so no libxcb/X system dep is needed. If a future arboard bump reintroduces a libxcb link, add libxcb to depends.
- aarch64 is listed for Arch Linux ARM users; the official AUR only builds/tests x86_64.

Get the real hash and finish:
```bash
updpkgsums                       # rewrites sha256sums from the source
makepkg --printsrcinfo > .SRCINFO
makepkg -si                      # local build+install smoke test
namcap PKGBUILD *.pkg.tar.zst    # lint
git add PKGBUILD .SRCINFO && git commit -m "Initial import: edamame 0.1.2" && git push
```
.SRCINFO is mandatory and must be regenerated on every version bump — the AUR web frontend reads only that file, not the PKGBUILD.

Optional: edamame-bin

Since cargo-dist already publishes edamame-x86_64-unknown-linux-gnu.tar.xz + a SHA256SUMS on each GitHub release, a -bin package that just unpacks the prebuilt binary is a low-effort second package (fast installs, no Rust toolchain for users). Same structure, but source=(...release/download/v$pkgver/edamame-x86_64-unknown-linux-gnu.tar.xz), arch=('x86_64') only (dist builds no other Linux arch except musl), no build(), and provides/conflicts=('edamame'). Worth it, but the source package is the one to lead with.

Updates

Bump pkgver, reset pkgrel=1, updpkgsums, regenerate .SRCINFO, commit, push. That's the whole loop.