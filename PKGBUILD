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
pkgver=0.5.0
pkgrel=1
pkgdesc="Measure the Wayland session you are logged into: memory, cost per frame, idle power"
arch=('x86_64')
url="https://github.com/grigio/wayland-compositor-bench"
license=('GPL-3.0-only')
depends=('gtk4' 'wayland' 'desktop-file-utils' 'gtk-update-icon-cache' 'hicolor-icon-theme')
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
  install -Dm644 DESIGN.md "$pkgdir/usr/share/doc/$pkgname/DESIGN.md"
  install -Dm644 CHANGELOG.md "$pkgdir/usr/share/doc/$pkgname/CHANGELOG.md"
  install -Dm644 packaging/wayland-compositor-bench.desktop \
    "$pkgdir/usr/share/applications/wayland-compositor-bench.desktop"
  install -Dm644 packaging/wayland-compositor-bench.svg \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/wayland-compositor-bench.svg"
}
