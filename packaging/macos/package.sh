#!/usr/bin/env bash
# Build only3d.app and a .dmg around it.
#   packaging/macos/package.sh <only3d binary> <version> <output dir>
# The binary can be universal (lipo). The app is signed ad hoc (no Apple Developer ID yet), so
# macOS asks people to allow it once: System Settings > Privacy & Security > Open Anyway.
set -euo pipefail
bin="$1"; version="$2"; out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
app="$work/only3d.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$out"
cp "$bin" "$app/Contents/MacOS/only3d"
chmod +x "$app/Contents/MacOS/only3d"
cp "$here/../icons/only3d.icns" "$app/Contents/Resources/only3d.icns"
# the licences: only3d's own, and its third-party notices (packaging/third_party_notices.py)
cp "$here/../../LICENSE" "$app/Contents/Resources/LICENSE"
notices="$here/../../target/THIRD-PARTY-NOTICES.txt"
if [ -f "$notices" ]; then cp "$notices" "$app/Contents/Resources/"; fi
sed "s/__VERSION__/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"
codesign --verify --deep "$app"
# the disk image: the app and a link to Applications, to drag it across
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/only3d-$version-macos-universal.dmg"
rm -f "$dmg"
hdiutil create -volname "only3d $version" -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$work"
echo "$dmg"
