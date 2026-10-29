#!/usr/bin/env bash
# Build a Debian/Ubuntu package from this checkout.
#
# No debhelper and no dh-cargo: Debian 13 ships a rustc older than the
# rust-version declared in Cargo.toml, so the binary is compiled with whatever
# cargo is on PATH and the package tree is assembled by hand with dpkg-deb.
#
# Usage: packaging/build-deb.sh [version]
# Output: dist/wayland-compositor-bench_<version>_<arch>.deb
#
# The package is built against the GTK / Wayland development packages installed
# on the machine, so run it in a container matching the oldest supported distro
# (Debian 13 / trixie) to keep the dependencies honest.

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

version=${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}
[ -n "$version" ] || { echo "build-deb: cannot read version from Cargo.toml" >&2; exit 1; }
pkg=wayland-compositor-bench
bin=wayland-compositor-bench
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

for cmd in cargo dpkg-deb readelf strip; do
  command -v "$cmd" >/dev/null || {
    echo "build-deb: '$cmd' not found; run this on Debian/Ubuntu with cargo and binutils installed" >&2
    exit 1
  }
done

arch=${DEB_ARCH:-$(dpkg --print-architecture)}
out=$root/dist/${pkg}_${version}_${arch}.deb

echo "build-deb: building $pkg $version for $arch"
cargo build --release --locked

install -Dm755 "target/release/$bin" "$stage/usr/bin/$bin"
# Debian requires stripped binaries; lintian flags them otherwise.
strip --remove-section=.comment --remove-section=.note --strip-unneeded \
  "$stage/usr/bin/$bin"

install -Dm644 README.md "$stage/usr/share/doc/$pkg/README.md"
gzip -9n -c CHANGELOG.md > "$stage/usr/share/doc/$pkg/changelog.gz"
chmod 644 "$stage/usr/share/doc/$pkg/changelog.gz"

# A Debian copyright file, not a copy of the GPL: lintian rejects shipping
# the full licence text when /usr/share/common-licenses already has it.
cat > "$stage/usr/share/doc/$pkg/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: $pkg
Source: https://github.com/grigio/$pkg

Files: *
Copyright: $(date +%Y) grigio
License: GPL-3.0-only
 On Debian systems, the complete text of the GNU General Public License
 version 3 can be found in /usr/share/common-licenses/GPL-3.
EOF


# The binary is built against the build host's glibc, so declare the oldest
# symbol version it actually references instead of hardcoding a guess.
libc_version=$(readelf --version-info "$stage/usr/bin/$bin" \
  | grep -o 'GLIBC_[0-9.]*' | sed 's/^GLIBC_//' | sort -Vu | tail -n1)
libc_version=${libc_version:-2.39}

# Same for GTK: the CSS in the UI uses `backdrop-filter`, which needs 4.16.
# The floor is read from the build host so it never lies about what was tested.
gtk_version=$(pkg-config --modversion gtk4 2>/dev/null || true)
gtk_floor=${GTK_FLOOR:-4.16}

{
  echo "$pkg ($version-1) unstable; urgency=medium"
  echo
  echo "  * Upstream release $version."
  echo
  echo " -- grigio <grigio@users.noreply.github.com>  $(date -R)"
} | gzip -9n > "$stage/usr/share/doc/$pkg/changelog.Debian.gz"

installed_size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)

mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: $pkg
Version: $version-1
Section: utils
Priority: optional
Architecture: $arch
Maintainer: grigio <grigio@users.noreply.github.com>
Installed-Size: $installed_size
Depends: libc6 (>= $libc_version), libgtk-4-1 (>= $gtk_floor), libwayland-client0
Homepage: https://github.com/grigio/$pkg
Description: measure the memory, CPU and power cost of a Wayland session
 A single Rust binary that measures the Wayland session you are logged into:
 how much memory and CPU the compositor costs at rest, what it costs per
 presented frame, and how much power the machine draws while it sits there.
 .
 Everything is read from /proc, the session's cgroup v2 and sysfs: CPU time
 from schedstat, frame intervals from presentation_time, GPU time from DRM
 client counters in fdinfo, and memory as the anon + shmem working set.
 Built against GTK $gtk_version on Debian.
EOF

mkdir -p "$root/dist"
dpkg-deb --root-owner-group --build "$stage" "$out"
echo "build-deb: wrote $out"
