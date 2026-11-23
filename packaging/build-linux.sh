#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-target/dist}"
target_dir="${CARGO_TARGET_DIR:-target}"
target=x86_64-unknown-linux-gnu
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/baudet-linux.XXXXXX")"
trap 'rm -rf "$work"' EXIT

version="$(cargo pkgid --locked -p baudet-ui)"
version="${version##*#}"
version="${version##*@}"
rustup target add "$target"
cargo build --locked --release -p baudet-ui --target-dir "$target_dir" --target "$target"

stage="$work/package"
binary="$stage/usr/bin/baudet-ui"
install -Dm755 "$target_dir/$target/release/baudet-ui" "$binary"
install -Dm644 LICENSE "$stage/usr/share/doc/baudet/copyright"
install -Dm644 packaging/FONT-LICENSES.txt "$stage/usr/share/doc/baudet/FONT-LICENSES.txt"
mkdir -p "$stage/DEBIAN" "$stage/usr/share/applications" "$work/debian"
cat > "$stage/usr/share/applications/dev.baudet.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Baudet
Comment=Desktop client for local and self-hosted model APIs
Exec=baudet-ui
Terminal=false
Categories=Development;
DESKTOP

cat > "$work/debian/control" <<'CONTROL'
Source: baudet
Section: devel
Priority: optional
Maintainer: Justus Brugman

Package: baudet
Architecture: amd64
Description: Baudet desktop client
CONTROL

ldd "$binary" | tee "$work/libraries.txt"
if grep -Fq 'not found' "$work/libraries.txt"; then
    exit 1
fi
linked="$(cd "$work" && dpkg-shlibdeps -O -e"$binary")"
linked="${linked#shlibs:Depends=}"

# The window, rendering and file-picker backends also load libraries at runtime.
runtime='libegl1, libgl1, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxrandr2, libxinerama1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libdbus-1-3, xdg-desktop-portal'
cat > "$stage/DEBIAN/control" <<CONTROL
Package: baudet
Version: $version
Architecture: amd64
Section: devel
Priority: optional
Maintainer: Justus Brugman
Depends: ${linked:+$linked, }$runtime
Recommends: xdg-desktop-portal-gtk | xdg-desktop-portal-kde
Description: Desktop client for local and self-hosted model APIs
CONTROL

package="$out/baudet_${version}_amd64.deb"
dpkg-deb --root-owner-group --build "$stage" "$package"
dpkg-deb --field "$package" Depends
dpkg-deb --extract "$package" "$work/extracted"
status=0
"$work/extracted/usr/bin/baudet-ui" --help > "$work/help.txt" 2>&1 || status=$?
test "$status" -eq 2
grep -Fq 'usage: baudet-ui' "$work/help.txt"
printf '%s\n' "$package"
