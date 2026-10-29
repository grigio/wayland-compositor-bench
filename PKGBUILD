# Maintainer: grigio <https://github.com/grigio>
# Arch Linux local package for wayland-compositor-bench (not published to the AUR).
#
# Run from the repository root:
#   makepkg -si
#   makepkg && sudo pacman -U wayland-compositor-bench-*.pkg.tar.zst
#
# This PKGBUILD builds from the checkout in $startdir. Do not pass
# --cleanbuild: makepkg's default $srcdir is ./src, which is the Rust source
# directory.

pkgname=wayland-compositor-bench
pkgver=0.3.1
pkgrel=1
pkgdesc="Measure the Wayland session you are logged into: memory, cost per frame, idle power"
arch=('x86_64')
url="https://github.com/grigio/wayland-compositor-bench"
license=('GPL-3.0-only')
depends=('gtk4' 'wayland')
makedepends=('cargo' 'pkgconf')
source=()
sha256sums=()

prepare() {
  cd "$startdir"
  export CARGO_HOME="$startdir/.cargo-home"
  cargo fetch --locked
}

build() {
  cd "$startdir"
  export CARGO_HOME="$startdir/.cargo-home"
  cargo build --release --locked
}

check() {
  cd "$startdir"
  export CARGO_HOME="$startdir/.cargo-home"
  cargo test --locked
}

package() {
  cd "$startdir"
  install -Dm755 target/release/wayland-compositor-bench "$pkgdir/usr/bin/wayland-compositor-bench"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 CHANGELOG.md "$pkgdir/usr/share/doc/$pkgname/CHANGELOG.md"
}
