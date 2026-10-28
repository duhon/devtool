#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P)
INSTALLER="$ROOT/dist/distribution/install.sh"
if [ ! -f "$INSTALLER" ]; then
  printf 'Installer not built. Run "cargo run -p xtask -- build" in %s first.\n' "$ROOT" >&2
  exit 1
fi
exec /bin/bash "$INSTALLER" "$@"
