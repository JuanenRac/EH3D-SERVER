#!/usr/bin/env bash
# Runs the EH3D server. Builds it first if needed. Extra arguments are passed on: ./run.sh [--config FILE]
set -u
cd "$(dirname "$0")"
pause() { [ -n "${NOPAUSE:-}" ] || { read -r -p "Press Enter to close..." _ || true; }; }
trap pause EXIT
if [ ! -x "target/release/eh3d-server" ]; then
    NOPAUSE=1 ./build.sh || exit 1
fi
"target/release/eh3d-server" "$@"
