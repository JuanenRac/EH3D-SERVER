#!/usr/bin/env bash
# PRETTY_RUN_BEGIN
if [ -z "${PRETTY_RUN:-}" ] && [ -f "$(dirname "$0")/tools/pretty_run.py" ] && command -v python3 >/dev/null 2>&1; then
    NOPAUSE=1 PRETTY_RUN=1 exec python3 "$(dirname "$0")/tools/pretty_run.py" --wrap --mode "${PRETTY_MODE:-build}" -- bash "$0" "$@"
fi
# PRETTY_RUN_END
# Builds the EH3D server (release). It needs EH3D-DRILLPRO and EH3D-ROUTEPRO next to this folder.
set -u
cd "$(dirname "$0")"
pause() { [ -n "${NOPAUSE:-}" ] || { read -r -p "Press Enter to close..." _ || true; }; }
trap pause EXIT
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed. Get it from https://rustup.rs"
    exit 1
fi
for sibling in EH3D-DRILLPRO EH3D-ROUTEPRO; do
    [ -d "../$sibling" ] || { echo "$sibling must be next to this folder"; exit 1; }
done
cargo build --release
