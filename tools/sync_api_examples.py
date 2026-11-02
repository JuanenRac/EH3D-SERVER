#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/sync_api_examples.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Gives the clients the examples of the answers of the server.

The tests of the server (`tests/examples.rs`) make `tests/examples/*.json` from the real router and fail when a file is not what the server answers. The clients read a copy of
those files in their own tests, so a change in the answers that a client does not follow breaks the tests of that client. This copies the files to the repositories that sit
next to this one, and with `--check` only says whether the copies are the same (and changes nothing).

    python tools/sync_api_examples.py            # copy
    python tools/sync_api_examples.py --check    # compare; exit 1 when a copy differs or is missing
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "tests" / "examples"
#: The repository (next to this one), the folder of it that holds the copy and the examples it does not use (the phone apps have no page of updates).
CLIENTS = {
    "EH3D-STUDIO": ("src/examples", set()),
    "EH3D-ANDROID-CONTROL": ("app/src/test/resources/api-examples", {"updates"}),
    "EH3D-IOS-CONTROL": ("Tests/Eh3dCoreTests/Examples", {"updates"}),
}


def lines_of(path: Path) -> bytes:
    return path.read_bytes().replace(b"\r\n", b"\n")


def main(argv: list[str]) -> int:
    check = "--check" in argv
    files = sorted(SOURCE.glob("*.json"))
    if not files:
        print(f"there are no examples in {SOURCE}: run  EH3D_WRITE_EXAMPLES=1 cargo test --test examples")
        return 1
    differ = 0
    for repo, (folder, unused) in CLIENTS.items():
        base = ROOT.parent / repo
        if not base.is_dir():
            print(f"  skipped {repo}: not next to this repository")
            continue
        target = base / folder
        mine = [f for f in files if f.stem not in unused]
        wanted = {f.name for f in mine}
        for f in mine:
            copy = target / f.name
            same = copy.is_file() and lines_of(copy) == lines_of(f)
            if not same:
                differ += 1
                if check:
                    print(f"  DIFFERS {repo}/{folder}/{f.name}")
                else:
                    target.mkdir(parents=True, exist_ok=True)
                    copy.write_bytes(lines_of(f))
                    print(f"  copied  {repo}/{folder}/{f.name}")
        for extra in sorted(target.glob("*.json")) if target.is_dir() else []:
            if extra.name not in wanted:
                differ += 1
                print(f"  EXTRA   {repo}/{folder}/{extra.name} (the server no longer makes it)")
    if check:
        print(f"API_EXAMPLES={'FAIL' if differ else 'PASS'} differing={differ}")
        return 1 if differ else 0
    print(f"API_EXAMPLES=COPIED changed={differ}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
