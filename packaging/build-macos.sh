#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-target/dist}"
target_dir="${CARGO_TARGET_DIR:-target}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/baudet-macos.XXXXXX")"
trap 'rm -rf "$work"' EXIT

version="$(cargo pkgid --locked -p baudet-ui)"
version="${version##*#}"
version="${version##*@}"
export MACOSX_DEPLOYMENT_TARGET=11.0

rustup target add aarch64-apple-darwin x86_64-apple-darwin
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --locked --release -p baudet-ui --target-dir "$target_dir" --target "$target"
done

app="$work/Baudet.app"
binary="$app/Contents/MacOS/baudet-ui"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$binary" \
    "$target_dir/aarch64-apple-darwin/release/baudet-ui" \
    "$target_dir/x86_64-apple-darwin/release/baudet-ui"
lipo "$binary" -verify_arch arm64 x86_64
cp LICENSE packaging/FONT-LICENSES.txt "$app/Contents/Resources/"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>baudet-ui</string>
  <key>CFBundleIdentifier</key><string>dev.baudet.desktop</string>
  <key>CFBundleName</key><string>Baudet</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist"
otool -arch all -L "$binary" | awk '
    $0 ~ /^[[:space:]]/ && $1 !~ /^\/System\/Library\// && $1 !~ /^\/usr\/lib\// {
        print "Unbundled library: " $1 > "/dev/stderr"; failed = 1
    }
    END { exit failed }
'
codesign --force --sign - "$app"
codesign --verify --strict "$app"

status=0
"$binary" --help > "$work/help.txt" 2>&1 || status=$?
test "$status" -eq 2
grep -Fq 'usage: baudet-ui' "$work/help.txt"

mkdir "$work/image"
ditto "$app" "$work/image/Baudet.app"
ln -s /Applications "$work/image/Applications"
hdiutil create -quiet -ov -volname Baudet -format UDZO -srcfolder "$work/image" "$out/Baudet-macOS-universal.dmg"
hdiutil verify -quiet "$out/Baudet-macOS-universal.dmg"
ditto "$app" "$out/Baudet.app"
codesign --verify --strict "$out/Baudet.app"
printf '%s\n' "$out/Baudet-macOS-universal.dmg"
