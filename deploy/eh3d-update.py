#!/usr/bin/env python3
# EH3D-SERVER - deploy/eh3d-update.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D). GPL-3.0-or-later.
"""The helper that updates the installed EH3D services from the releases of GitHub.

The server does not talk to GitHub and does not replace itself: this program does, started by systemd as root when the server leaves a request in the shared folder
(`eh3d-update.path` watches `request.json`). They talk through three files in that folder:

    request.json     what the server asks for: {"action": "check"} or {"action": "apply", "components": ["server", "studio"]}
    components.json  what the last check found: [{"id", "repo", "latest", "url", "published"}]
    status.json      what this helper is doing: {"state": "idle|checking|updating|done|error", "message", "log": [...]}

    eh3d-update.py --dir /var/lib/eh3d-updates [--config /etc/eh3d-update.json]     # does what request.json says, once

Every download is checked against the SHA-256 that the release publishes next to the file before anything is replaced, the files are written beside the old ones and
moved into place, and the old ones are kept as `.previous` so that a bad update can be put back by hand. Only the standard library is used.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

OWNER = "JuanenRac"
DEFAULT_CONFIG = {
    "server": {
        "repo": "EH3D-SERVER",
        # the file of the release that holds the program: {version} is the number of the version, {arch} the architecture of this machine
        "asset": "eh3d-server-{version}-linux-{arch}.tar.gz",
        "install_dir": "/opt/eh3d-server",
        "program": "eh3d-server",
        "restart": ["systemctl", "restart", "eh3d-server"],
    },
    "studio": {
        "repo": "EH3D-STUDIO",
        "asset": "eh3d-studio-{version}.tar.gz",
        "install_dir": "/opt/eh3d-studio",
        "program": None,
        "restart": None,
    },
}


def arch() -> str:
    m = os.uname().machine if hasattr(os, "uname") else "x86_64"
    return {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}.get(m, m)


class Folder:
    """The shared folder: the files that the server and this helper leave for each other."""

    def __init__(self, path: Path):
        self.path = path
        self.log: list[str] = []

    def read(self, name: str):
        try:
            return json.loads((self.path / name).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None

    def write(self, name: str, value) -> None:
        tmp = self.path / (name + ".tmp")
        tmp.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
        tmp.replace(self.path / name)

    def say(self, state: str, message: str) -> None:
        self.log = (self.log + [message])[-40:]
        self.write("status.json", {"state": state, "message": message, "log": self.log})


def http_get(url: str, timeout: int = 30) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "eh3d-update", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:  # noqa: S310 - the addresses are the ones of GitHub
        return r.read()


def latest_release(repo: str, get=http_get) -> dict:
    """The latest release of a repository: the version (without the v), its page, its date and its files."""
    data = json.loads(get(f"https://api.github.com/repos/{OWNER}/{repo}/releases/latest"))
    return {
        "latest": str(data["tag_name"]).lstrip("v"),
        "url": data.get("html_url", ""),
        "published": data.get("published_at", ""),
        "assets": {a["name"]: a["browser_download_url"] for a in data.get("assets", [])},
    }


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def expected_hash(text: str, name: str) -> str:
    """The hash that a `.sha256` file gives for `name` (`HASH  NAME`, or only the hash)."""
    for line in text.splitlines():
        parts = line.split()
        if parts and len(parts[0]) == 64 and (len(parts) == 1 or parts[-1].lstrip("*") == name):
            return parts[0].lower()
    return ""


def safe_members(tar: tarfile.TarFile, dest: Path):
    """The members of an archive that stay inside the folder they are extracted to (no absolute names, no `..`, no links out)."""
    root = dest.resolve()
    for m in tar.getmembers():
        target = (dest / m.name).resolve()
        if not str(target).startswith(str(root)) or m.islnk() or m.issym() or m.isdev():
            raise ValueError(f"the archive holds an unsafe entry: {m.name}")
        yield m


def install(folder: Folder, comp_id: str, comp: dict, release: dict, get=http_get, run=subprocess.run) -> None:
    version = release["latest"]
    name = comp["asset"].format(version=version, arch=arch())
    url = release["assets"].get(name)
    if not url:
        raise RuntimeError(f"the release {version} has no file called {name}")
    folder.say("updating", f"{comp_id}: downloading {name}")
    with tempfile.TemporaryDirectory(prefix="eh3d-update-") as tmp:
        tmpdir = Path(tmp)
        archive = tmpdir / name
        archive.write_bytes(get(url, 600))
        hash_url = release["assets"].get(name + ".sha256")
        if not hash_url:
            raise RuntimeError(f"the release has no {name}.sha256 to check the file with")
        want = expected_hash(get(hash_url).decode("utf-8", "replace"), name)
        if not want or sha256_of(archive) != want:
            raise RuntimeError(f"{name}: the hash does not match, nothing was installed")
        folder.say("updating", f"{comp_id}: the hash is right, unpacking")
        stage = tmpdir / "stage"
        stage.mkdir()
        with tarfile.open(archive) as tar:
            tar.extractall(stage, members=list(safe_members(tar, stage)))  # noqa: S202 - the members were checked
        entries = [p for p in stage.iterdir()]
        # an archive with one folder inside is taken from inside it
        src = entries[0] if len(entries) == 1 and entries[0].is_dir() else stage
        target = Path(comp["install_dir"])
        target.parent.mkdir(parents=True, exist_ok=True)
        new = target.parent / (target.name + ".new")
        if new.exists():
            shutil.rmtree(new)
        shutil.copytree(src, new)
        if comp.get("program"):
            os.chmod(new / comp["program"], 0o755)
        old = target.parent / (target.name + ".previous")
        if old.exists():
            shutil.rmtree(old)
        if target.exists():
            target.rename(old)
        new.rename(target)
    folder.say("updating", f"{comp_id}: installed {version}")
    if comp.get("restart"):
        folder.say("updating", f"{comp_id}: restarting")
        run(comp["restart"], check=True)


def check(folder: Folder, config: dict, get=http_get) -> list:
    found = []
    for comp_id, comp in config.items():
        folder.say("checking", f"{comp_id}: asking GitHub for the latest release of {comp['repo']}")
        try:
            r = latest_release(comp["repo"], get)
            found.append({"id": comp_id, "repo": comp["repo"], "latest": r["latest"], "url": r["url"], "published": r["published"]})
        except Exception as e:  # a repository without a release, or no network: said, and the rest goes on
            folder.say("checking", f"{comp_id}: {e}")
    folder.write("components.json", found)
    return found


def apply(folder: Folder, config: dict, ids: list, get=http_get, run=subprocess.run) -> None:
    done = []
    for comp_id in ids:
        comp = config.get(comp_id)
        if comp is None:
            raise RuntimeError(f"{comp_id} is not a component of this helper")
        release = latest_release(comp["repo"], get)
        install(folder, comp_id, comp, release, get, run)
        done.append(comp_id)
    folder.say("done", "updated: " + ", ".join(done))


def handle(folder: Folder, config: dict, get=http_get, run=subprocess.run) -> bool:
    """Does what request.json says (and removes it). False when there was no request."""
    request = folder.read("request.json")
    if request is None:
        return False
    try:
        (folder.path / "request.json").unlink()
    except OSError:
        pass
    folder.log = []
    try:
        if request.get("action") == "check":
            found = check(folder, config, get)
            folder.say("done", f"{len(found)} component(s) checked")
        elif request.get("action") == "apply":
            # the server may restart itself: the result is written before that, so the Studio can show it when it comes back
            apply(folder, config, [c for c in request.get("components", []) if c in config], get, run)
        else:
            raise RuntimeError("the request is neither check nor apply")
    except Exception as e:
        folder.say("error", str(e))
    return True


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Updates the EH3D services from the releases of GitHub, as the server asks.")
    p.add_argument("--dir", required=True, help="the shared folder (updates_dir of server.cfg)")
    p.add_argument("--config", help="a JSON file that replaces the components (id: repo, asset, install_dir, program, restart)")
    a = p.parse_args(argv)
    config = DEFAULT_CONFIG
    if a.config:
        config = json.loads(Path(a.config).read_text(encoding="utf-8"))
    folder = Folder(Path(a.dir))
    folder.path.mkdir(parents=True, exist_ok=True)
    handle(folder, config)
    return 0


if __name__ == "__main__":
    sys.exit(main())
