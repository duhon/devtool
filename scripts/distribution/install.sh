#!/bin/bash
set -euo pipefail
umask 077

REPOSITORY='@REPOSITORY@'
VERSION='@VERSION@'
ARCHIVE=''
EXPECTED_SHA=''
ARCHIVE_NAME='services-macos-arm64.tar.gz'

fail() { printf 'Installation failed: %s\n' "$*" >&2; exit 1; }
usage() {
  printf '%s\n' 'Usage: bash install.sh [--archive /path/services-macos-arm64.tar.gz] [--sha256 HASH]'
  printf '%s\n' 'Automatically uses the package beside this script, or downloads it from its GitHub release.'
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --archive) [ "$#" -ge 2 ] || fail '--archive needs a path'; ARCHIVE=$2; shift 2 ;;
    --sha256) [ "$#" -ge 2 ] || fail '--sha256 needs a hash'; EXPECTED_SHA=$2; shift 2 ;;
    --no-open) shift ;;
    --help) usage; exit 0 ;;
    *) usage >&2; fail "Unknown argument: $1" ;;
  esac
done
[ "$(/usr/bin/uname -s)" = Darwin ] || fail 'Only macOS is supported'
[ "$(/usr/bin/uname -m)" = arm64 ] || fail 'Only Apple Silicon is supported; run from a native arm64 terminal, not Rosetta'
OS_VERSION=$(/usr/bin/sw_vers -productVersion)
/usr/bin/awk -v version="$OS_VERSION" 'BEGIN { split(version, v, "[.]"); exit !(v[1] > 13 || (v[1] == 13 && v[2] >= 5)) }' \
  || fail 'macOS 13.5 or newer is required by the bundled Node.js runtime'
[ -n "${HOME:-}" ] && [ "$HOME" != / ] && [ -d "$HOME" ] || fail 'A valid user HOME is required'
[ "$(/usr/bin/id -u)" -ne 0 ] || fail 'Run this as your own user, without sudo'

WORK=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/services-install.XXXXXXXX")
cleanup() { /bin/rm -r -- "$WORK"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
download() {
  /usr/bin/curl --fail --silent --show-error --location --proto '=https' \
    --retry 2 --connect-timeout 15 --max-time 600 "$1" --output "$2"
}
if [ -z "$ARCHIVE" ] && [ -f "${BASH_SOURCE[0]:-}" ]; then
  INSTALLER_DIR=$(CDPATH= cd -- "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P)
  if [ -f "$INSTALLER_DIR/$ARCHIVE_NAME" ]; then
    ARCHIVE="$INSTALLER_DIR/$ARCHIVE_NAME"
  fi
fi
if [ -n "$ARCHIVE" ]; then
  [ -f "$ARCHIVE" ] || fail "Archive not found: $ARCHIVE"
  /bin/cp "$ARCHIVE" "$WORK/$ARCHIVE_NAME"
  if [ -z "$EXPECTED_SHA" ]; then
    SUMS="$(/usr/bin/dirname "$ARCHIVE")/checksums.txt"
    [ -f "$SUMS" ] || fail 'Supply --sha256 or place checksums.txt beside the archive'
    /bin/cp "$SUMS" "$WORK/checksums.txt"
  fi
else
  [[ "$REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail 'Remote source is not configured; build with --repository OWNER/REPO'
  [[ "$VERSION" =~ ^[0-9]+[.][0-9]+[.][0-9]+$ ]] || fail 'Invalid release version'
  # This script is a release asset; the checksum and package come from that same release.
  BASE="https://github.com/$REPOSITORY/releases/download/devtool-v$VERSION"
  printf 'Downloading Services DevTools %s...\n' "$VERSION"
  download "$BASE/checksums.txt" "$WORK/checksums.txt"
  download "$BASE/$ARCHIVE_NAME" "$WORK/$ARCHIVE_NAME"
fi
if [ -z "$EXPECTED_SHA" ]; then
  EXPECTED_SHA=$(/usr/bin/awk -v name="$ARCHIVE_NAME" '$2 == name { count++; hash=$1 } END { if (count != 1) exit 1; print hash }' "$WORK/checksums.txt") \
    || fail 'Missing or ambiguous package checksum'
fi
[[ "$EXPECTED_SHA" =~ ^[a-f0-9]{64}$ ]] || fail 'Invalid SHA-256 checksum'
ACTUAL_SHA=$(/usr/bin/shasum -a 256 "$WORK/$ARCHIVE_NAME")
ACTUAL_SHA=${ACTUAL_SHA%% *}
[ "$ACTUAL_SHA" = "$EXPECTED_SHA" ] || fail 'Package checksum mismatch; nothing was installed'
/usr/bin/tar -tzf "$WORK/$ARCHIVE_NAME" > "$WORK/files.txt"
/usr/bin/awk '/^\// || /(^|\/)\.\.(\/|$)/ || $0 !~ /^ServicesDevTools(\/|$)/ { bad=1 } END { exit bad }' "$WORK/files.txt" \
  || fail 'Unsafe archive paths'
/usr/bin/tar -xzf "$WORK/$ARCHIVE_NAME" -C "$WORK" --no-same-owner
PAYLOAD="$WORK/ServicesDevTools"
[ -x "$PAYLOAD/native/bin/services-devtools" ] && [ -x "$PAYLOAD/native/bin/services-install" ] || fail 'Incomplete distribution'
"$PAYLOAD/native/bin/services-install" install "$PAYLOAD"
