#!/usr/bin/env bash
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
