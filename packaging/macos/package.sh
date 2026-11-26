#!/usr/bin/env bash
# Build only3d.app and a .dmg around it.
#   packaging/macos/package.sh <only3d binary> <version> <output dir>
# The binary can be universal (lipo).
#
# Signing: with MACOS_SIGN_IDENTITY set (a "Developer ID Application: …" identity in the
# keychain), the app and the disk image are signed with it and the hardened runtime. With
# NOTARY_APPLE_ID, NOTARY_TEAM_ID and NOTARY_PASSWORD (an app-specific password) set as well,
# the disk image is notarized and the ticket stapled, so macOS opens it without asking.
# Without them the app is signed ad hoc, and macOS asks people to allow it once:
# System Settings > Privacy & Security > Open Anyway.
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
identity="${MACOS_SIGN_IDENTITY:-}"
if [ -n "$identity" ]; then
  # one executable, no nested code: sign it, then the bundle around it
  codesign --force --options runtime --timestamp --sign "$identity" "$app/Contents/MacOS/only3d"
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
  codesign --verify --strict --verbose=2 "$app"
else
  echo "MACOS_SIGN_IDENTITY not set: signing ad hoc" >&2
  codesign --force --deep --sign - "$app"
  codesign --verify --deep "$app"
fi
# the disk image: the app and a link to Applications, to drag it across
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/only3d-$version-macos-universal.dmg"
rm -f "$dmg"
hdiutil create -volname "only3d $version" -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  if [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ]; then
    xcrun notarytool submit "$dmg" --apple-id "$NOTARY_APPLE_ID" --team-id "$NOTARY_TEAM_ID" \
      --password "$NOTARY_PASSWORD" --wait --timeout 30m
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
  else
    echo "NOTARY_* not set: signed but not notarized" >&2
  fi
fi
rm -rf "$work"
echo "$dmg"
