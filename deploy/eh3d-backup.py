#!/usr/bin/env python3
# EH3D-SERVER - deploy/eh3d-backup.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D). GPL-3.0-or-later.
"""Copies the data folder of the EH3D server (users, tokens, machines, jobs) into one archive, and keeps only the newest ones.

    eh3d-backup.py [--config /etc/eh3d-server/backup.cfg] [--dry-run]

Started every night by `eh3d-backup.timer`. The settings are the lines `key = value` of the config file (`source`, `dest`, `keep`; see `deploy/backup.cfg.example`).
The archive is written under a temporary name, read back to check it is whole and only then given its name (`eh3d-data-YYYYmmdd-HHMMSS.tar.gz`), so the folder of the copies
never holds a half-written one; it is readable by its owner only, because the data holds the hashes of the passwords and the tokens of the nodes. Files that the server is
writing at this moment (`*.tmp`) are left out. Only the standard library is used.
"""
import argparse
import os
import re
import sys
import tarfile
import time
from pathlib import Path

DEFAULTS = {"source": "/var/lib/eh3d-server", "dest": "/var/backups/eh3d-server", "keep": "14"}
NAME = re.compile(r"^eh3d-data-\d{8}-\d{6}(-\d+)?\.tar\.gz$")


def read_config(path):
    """The settings: the defaults, over them the lines of the file. A mistake is an error, never a silent default."""
    cfg = dict(DEFAULTS)
    if path is None:
        return finish(cfg)
    for number, raw in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if "=" not in line:
            raise ValueError(f"line {number}: expected key = value")
        key, value = (part.strip() for part in line.split("=", 1))
        key = key.lower()
        if key not in DEFAULTS:
            raise ValueError(f"line {number}: unknown key '{key}'")
        if not value:
            raise ValueError(f"line {number}: {key} must not be empty")
        cfg[key] = value
    return finish(cfg)


def finish(cfg):
    try:
        keep = int(cfg["keep"])
    except ValueError:
        raise ValueError(f"keep must be a whole number, not '{cfg['keep']}'") from None
    if not 1 <= keep <= 3650:
        raise ValueError("keep must be from 1 to 3650")
    return {"source": Path(cfg["source"]), "dest": Path(cfg["dest"]), "keep": keep}


def make_backup(source, dest, now=None):
    """Writes the archive of `source` into `dest` and returns its path."""
    if not source.is_dir():
        raise FileNotFoundError(f"{source}: the data folder is not there")
    if not dest.is_dir():
        raise FileNotFoundError(f"{dest}: the folder of the copies is not there (make it and give it to the user of the backup)")
    stamp = time.strftime("%Y%m%d-%H%M%S", time.localtime(now))
    final = dest / f"eh3d-data-{stamp}.tar.gz"
    n = 1
    while final.exists():  # two runs in the same second
        final = dest / f"eh3d-data-{stamp}-{n}.tar.gz"
        n += 1
    temp = dest / (final.name + ".part")

    def left_out(info):
        return None if info.name.endswith(".tmp") else info

    try:
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as handle, tarfile.open(fileobj=handle, mode="w:gz") as tar:
            tar.add(source, arcname="data", filter=left_out)
            handle.flush()
        # read it back: every member must come out whole
        with tarfile.open(temp, "r:gz") as tar:
            for member in tar:
                if member.isfile():
                    stream = tar.extractfile(member)
                    while stream.read(1 << 20):
                        pass
        os.replace(temp, final)
    except BaseException:
        if temp.exists():
            temp.unlink()
        raise
    return final


def prune(dest, keep):
    """Removes the oldest copies beyond `keep`. Only files with the name of a copy are touched. Returns the names removed."""
    copies = sorted(p for p in dest.iterdir() if NAME.match(p.name))
    removed = []
    for old in copies[: max(0, len(copies) - keep)]:
        old.unlink()
        removed.append(old.name)
    return removed


def main(argv=None):
    parser = argparse.ArgumentParser(description="Backup of the data folder of the EH3D server")
    parser.add_argument("--config", help="the backup settings (default: the built-in ones)")
    parser.add_argument("--dry-run", action="store_true", help="say what would be done and do nothing")
    args = parser.parse_args(argv)
    try:
        cfg = read_config(args.config)
        if args.dry_run:
            print(f"would copy {cfg['source']} into {cfg['dest']} and keep the newest {cfg['keep']}")
            return 0
        made = make_backup(cfg["source"], cfg["dest"])
        removed = prune(cfg["dest"], cfg["keep"])
    except (OSError, ValueError, tarfile.TarError) as e:
        print(f"eh3d-backup: {e}", file=sys.stderr)
        return 1
    print(f"eh3d-backup: {made.name} ({made.stat().st_size} bytes); removed {len(removed)} old")
    return 0


if __name__ == "__main__":
    sys.exit(main())
