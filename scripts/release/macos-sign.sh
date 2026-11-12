#!/usr/bin/env bash
# Developer ID signing, notarization and stapling for the macOS release jobs.
# release-binaries.yml signs the CLI binaries with it; release-app.yml signs
# fno-app.app. One writer keeps the identity lookup and the `Accepted` check
# in one place.
#
#   macos-sign.sh sign <path>...      sign Mach-O files or .app bundles
#   macos-sign.sh notarize <path>...  submit the paths in one zip, require Accepted
#   macos-sign.sh staple <bundle>     staple the ticket and validate it
#   macos-sign.sh cleanup             delete the signing keychain
#
# Reads MACOS_CERT_P12_BASE64 -> CERT_P12, CERT_PASSWORD, NOTARY_KEY,
# NOTARY_KEY_ID and NOTARY_ISSUER from the environment. The identifiers match
# crates/fno/src/codesign.rs (`dev.fno.<stem>`), so permission grants keyed on
# them survive the move from local to CI signing.
set -euo pipefail

tmp="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
keychain="$tmp/fno-sign.keychain-db"

setup_keychain() {
  if [ -f "$keychain" ]; then
    return
  fi
  : "${CERT_P12:?CERT_P12 is not set}" "${CERT_PASSWORD:?CERT_PASSWORD is not set}"
  local kpass
  kpass="$(uuidgen)"
  security create-keychain -p "$kpass" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$kpass" "$keychain"
  printf '%s' "$CERT_P12" | base64 --decode > "$tmp/cert.p12"
  security import "$tmp/cert.p12" -P "$CERT_PASSWORD" -A -t cert -f pkcs12 -k "$keychain"
  rm -f "$tmp/cert.p12"
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kpass" "$keychain" >/dev/null
  security list-keychains -d user -s "$keychain" login.keychain-db
}

identity() {
  local id
  id="$(security find-identity -v -p codesigning "$keychain" | awk -F'"' '/Developer ID Application/ {print $2; exit}')"
  if [ -z "$id" ]; then
    echo "::error::no Developer ID Application identity in MACOS_CERT_P12_BASE64"
    exit 1
  fi
  printf '%s' "$id"
}

sign_file() {
  local id="$1" file="$2"
  codesign --force --timestamp --options runtime \
    --sign "$id" --identifier "dev.fno.$(basename "$file")" "$file"
  codesign --verify --strict "$file"
}

cmd_sign() {
  [ "$#" -gt 0 ] || { echo "macos-sign: sign needs at least one path" >&2; exit 2; }
  setup_keychain
  local id path inner
  id="$(identity)"
  for path in "$@"; do
    if [ -d "$path" ]; then
      # Inside out: nested code first, then the bundle, which takes its
      # identifier from its Info.plist.
      for inner in "$path"/Contents/MacOS/*; do
        [ -f "$inner" ] && sign_file "$id" "$inner"
      done
      codesign --force --timestamp --options runtime --sign "$id" "$path"
      codesign --verify --deep --strict "$path"
    elif [ -f "$path" ]; then
      sign_file "$id" "$path"
    else
      echo "macos-sign: not found: $path" >&2
      exit 2
    fi
  done
}

cmd_notarize() {
  [ "$#" -gt 0 ] || { echo "macos-sign: notarize needs at least one path" >&2; exit 2; }
  : "${NOTARY_KEY:?NOTARY_KEY is not set}" "${NOTARY_KEY_ID:?NOTARY_KEY_ID is not set}" "${NOTARY_ISSUER:?NOTARY_ISSUER is not set}"
  local stage="$tmp/notarize" path
  rm -rf "$stage" "$tmp/notarize.zip"
  mkdir -p "$stage"
  for path in "$@"; do
    cp -R "$path" "$stage/"
  done
  # notarytool takes a zip, not a tarball.
  (cd "$stage" && ditto -c -k . ../notarize.zip)
  printf '%s' "$NOTARY_KEY" > "$tmp/notary-key.p8"
  # notarytool can exit 0 on an Invalid verdict, so read the status.
  xcrun notarytool submit "$tmp/notarize.zip" --wait --timeout 30m --output-format json \
    --key "$tmp/notary-key.p8" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" \
    | tee "$tmp/notary.json"
  rm -f "$tmp/notary-key.p8"
  if [ "$(jq -r '.status' "$tmp/notary.json")" != "Accepted" ]; then
    echo "::error::notarization was not accepted; see the notarytool output above"
    exit 1
  fi
}

cmd_staple() {
  [ "$#" -eq 1 ] || { echo "macos-sign: staple takes one bundle" >&2; exit 2; }
  xcrun stapler staple "$1"
  xcrun stapler validate "$1"
}

cmd_cleanup() {
  if [ -f "$keychain" ]; then
    security delete-keychain "$keychain"
  fi
}

sub="${1:-}"
[ "$#" -gt 0 ] && shift
case "$sub" in
  sign) cmd_sign "$@" ;;
  notarize) cmd_notarize "$@" ;;
  staple) cmd_staple "$@" ;;
  cleanup) cmd_cleanup ;;
  *) echo "usage: macos-sign.sh sign|notarize|staple|cleanup [path...]" >&2; exit 2 ;;
esac
