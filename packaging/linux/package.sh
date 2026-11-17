#!/usr/bin/env bash
# Package the Linux build as an AppImage and a .tar.gz (the .deb comes from cargo-deb).
#   packaging/linux/package.sh <only3d binary> <version> <output dir>
# Needs appimagetool on PATH (CI downloads it); without it only the .tar.gz is made.
set -euo pipefail
bin="$1"; version="$2"; out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../.."
work="$(mktemp -d)"
mkdir -p "$out"
arch="$(uname -m)"

# a plain archive: the program, its desktop entry and icon
t="$work/only3d-$version-linux-$arch"
mkdir -p "$t"
cp "$bin" "$t/only3d"
cp "$here/only3d.desktop" "$here/../icons/only3d.png" "$root/LICENSE" "$t/"
tar -C "$work" -czf "$out/only3d-$version-linux-$arch.tar.gz" "$(basename "$t")"
echo "$out/only3d-$version-linux-$arch.tar.gz"

# the AppImage: one file to download, mark executable and run
if command -v appimagetool >/dev/null; then
  a="$work/only3d.AppDir"
  mkdir -p "$a/usr/bin"
  cp "$bin" "$a/usr/bin/only3d"
  cp "$here/only3d.desktop" "$a/only3d.desktop"
  cp "$here/../icons/only3d.png" "$a/only3d.png"
  cp "$here/../icons/only3d.png" "$a/.DirIcon"
  cat > "$a/AppRun" <<'RUN'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/only3d" "$@"
RUN
  chmod +x "$a/AppRun" "$a/usr/bin/only3d"
  ARCH="$arch" appimagetool --no-appstream "$a" "$out/only3d-$version-linux-$arch.AppImage"
  echo "$out/only3d-$version-linux-$arch.AppImage"
fi
rm -rf "$work"
