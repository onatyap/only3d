#!/usr/bin/env bash
# Put a signing certificate in a temporary keychain for packaging/macos/package.sh.
#   MACOS_CERT_P12_BASE64=… MACOS_CERT_PASSWORD=… packaging/macos/keychain.sh
# MACOS_CERT_P12_BASE64 is a "Developer ID Application" certificate with its private key,
# exported from Keychain Access as .p12 and base64-encoded. Prints the signing identity's name;
# in GitHub Actions it is also written to $GITHUB_ENV as MACOS_SIGN_IDENTITY. The keychain is
# left in the search list for the rest of the job (CI runners are thrown away afterwards).
set -euo pipefail
: "${MACOS_CERT_P12_BASE64:?the .p12 certificate, base64-encoded}"
: "${MACOS_CERT_PASSWORD:?the .p12 password}"
dir="${RUNNER_TEMP:-$(mktemp -d)}"
keychain="$dir/only3d-signing.keychain-db"
pass="$(openssl rand -hex 16)"
p12="$dir/only3d-signing.p12"
printf '%s' "$MACOS_CERT_P12_BASE64" | base64 --decode > "$p12"
security create-keychain -p "$pass" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$pass" "$keychain"
security import "$p12" -k "$keychain" -P "$MACOS_CERT_PASSWORD" -T /usr/bin/codesign
rm -f "$p12"
security set-key-partition-list -S apple-tool:,apple: -s -k "$pass" "$keychain" >/dev/null
# search this keychain as well as the ones already listed
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
identity="$(security find-identity -p codesigning "$keychain" | sed -n 's/^ *[0-9][0-9]*) [0-9A-F]* "\(.*\)".*$/\1/p' | head -1)"
if [ -z "$identity" ]; then
  echo "no signing identity in the certificate" >&2
  exit 1
fi
if [ -n "${GITHUB_ENV:-}" ]; then echo "MACOS_SIGN_IDENTITY=$identity" >> "$GITHUB_ENV"; fi
echo "$identity"
