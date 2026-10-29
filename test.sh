#!/usr/bin/env bash
# EH3D-SERVER test launcher: the tests, shown in a live panel; the version is never changed. GPL-3.0-or-later.
# PRETTY_RUN_BEGIN
if [ -z "${PRETTY_RUN:-}" ] && [ -f "$(dirname "$0")/tools/pretty_run.py" ] && command -v python3 >/dev/null 2>&1; then
    NOPAUSE=1 PRETTY_RUN=1 exec python3 "$(dirname "$0")/tools/pretty_run.py" --wrap --mode "${PRETTY_MODE:-test}" -- bash "$0" "$@"
fi
# PRETTY_RUN_END
cd "$(dirname "$0")" || exit 1
cargo test
