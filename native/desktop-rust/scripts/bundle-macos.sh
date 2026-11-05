#!/usr/bin/env bash
# Builds `T3 Code.app` (and a .dmg) from a compiled t3-desktop binary.
#
#   scripts/bundle-macos.sh <binary> <out-dir>
#
# Signs when MACOS_SIGNING_IDENTITY is set; notarizes when APPLE_ID, APPLE_TEAM_ID and
# APPLE_APP_PASSWORD are set too. Otherwise the bundle is ad-hoc signed.
set -euo pipefail

binary=${1:?usage: bundle-macos.sh <binary> <out-dir>}
out=${2:?usage: bundle-macos.sh <binary> <out-dir>}
root=$(cd "$(dirname "$0")/.." && pwd)

name="T3 Code"
identifier="com.t3tools.t3code.native"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)

app="$out/$name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/t3-desktop"

iconset=$(mktemp -d)/icon.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size "$root/../../assets/prod/black-macos-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double "$root/../../assets/prod/black-macos-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/icon.icns"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundleIdentifier</key><string>$identifier</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>t3-desktop</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ -n "${MACOS_SIGNING_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$MACOS_SIGNING_IDENTITY" "$app"
else
  codesign --force --sign - "$app"
fi
codesign --verify --strict "$app"

dmg="$out/T3-Code-$version-$(uname -m).dmg"
if [ -n "${DMG_ARCH:-}" ]; then
  dmg="$out/T3-Code-$version-$DMG_ARCH.dmg"
fi
rm -f "$dmg"
hdiutil create -volname "$name" -srcfolder "$app" -ov -format UDZO "$dmg" >/dev/null

if [ -n "${MACOS_SIGNING_IDENTITY:-}" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  codesign --force --timestamp --sign "$MACOS_SIGNING_IDENTITY" "$dmg"
  xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait
  xcrun stapler staple "$dmg"
fi

echo "$dmg"
