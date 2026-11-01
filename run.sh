#!/usr/bin/env bash
# Runs the EH3D server. Builds it first if needed. Extra arguments are passed on: ./run.sh [--config FILE]
set -u
cd "$(dirname "$0")"
pause() { [ -n "${NOPAUSE:-}" ] || { read -r -p "Press Enter to close..." _ || true; }; }
trap pause EXIT
BIN="${CARGO_TARGET_DIR:-target}/release/eh3d-server"
if [ ! -x "$BIN" ]; then
    NOPAUSE=1 ./build.sh || exit 1
fi
"$BIN" "$@"
