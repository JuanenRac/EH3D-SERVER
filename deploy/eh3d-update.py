#!/usr/bin/env python3
# EH3D-SERVER - deploy/eh3d-update.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D). GPL-3.0-or-later.
"""The helper that updates the installed EH3D services from the releases of GitHub.

The server does not talk to GitHub and does not replace itself: this program does, started by systemd as root when the server leaves a request in the shared folder
(`eh3d-update.path` watches `request.json`). They talk through three files in that folder:

    request.json     what the server asks for: {"action": "check"} or {"action": "apply", "components": ["server", "studio"]}
    components.json  what the last check found: [{"id", "repo", "latest", "url", "published"}]
    status.json      what this helper is doing: {"state": "idle|checking|updating|done|error", "message", "log": [...], "percent", "steps": [{"id", "state", "percent"}]}
    history.json     the last jobs: [{"id", "started", "kind", "label", "state"}]
    previous.json    the version that each update left behind: {"<id>": {"available": true, "version": "0.0.7" | null}}

A request may also be {"action": "undo", "components": [...]}, which puts back the version an update left behind (the one installed becomes the one left behind),
or {"action": "clear_history"}, which empties history.json.

    eh3d-update.py --dir /var/lib/eh3d-updates [--config /etc/eh3d-update.json]     # does what request.json says, once

Every download is checked against the SHA-256 that the release publishes next to the file before anything is replaced. That proves the file arrived whole, not who made it
(the hash is published in the same place as the file). When the machine has a file of allowed signers (`--signers`, or `/etc/eh3d-update.signers` if it exists) the release must
also publish `<file>.sig`, an OpenSSH signature of the file made with `ssh-keygen -Y sign -n eh3d-release`, and it must verify with `ssh-keygen -Y verify`
against one of those keys: then a stolen account of GitHub, which can replace the file and its hash, still cannot make this machine install what the owner did not sign.
Without that file the helper works as before, and says in its log that the files are not checked for who made them.
 the files are written beside the old ones and
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
import time
import urllib.request
from pathlib import Path

OWNER = "JuanenRac"
#: The file of allowed signers (the format of `ssh-keygen -Y verify`): one line per public key, `eh3d-release <key type> <key>`. Used when it exists.
DEFAULT_SIGNERS = "/etc/eh3d-update.signers"
SIGNATURE_NAMESPACE = "eh3d-release"
SIGNATURE_IDENTITY = "eh3d-release"
SIGNERS = DEFAULT_SIGNERS if os.path.exists(DEFAULT_SIGNERS) else None
#: The file the helper leaves in what it installs, so that the version left behind by the next update can be told.
VERSION_MARK = ".eh3d-version"
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
        self.state = "idle"
        self.message = ""
        self.steps: list[dict] = []
        self.percent = 0

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
        self.state, self.message = state, message
        self.flush()

    def flush(self) -> None:
        self.write("status.json", {"state": self.state, "message": self.message, "log": self.log, "percent": self.percent, "steps": self.steps})

    def plan(self, ids: list) -> None:
        """The steps that a job will go through, all waiting: the Studio draws a bar for each."""
        self.steps = [{"id": i, "state": "pending", "percent": 0} for i in ids]
        self.percent = 0
        self.flush()

    def step(self, step_id: str, state: str) -> None:
        """Marks a step running, done or failed (a step that is not in the plan is ignored) and works out the percent of the whole job."""
        for st in self.steps:
            if st["id"] == step_id:
                st["state"] = state
                st["percent"] = 100 if state in ("done", "skipped") else 50 if state == "running" else st["percent"]
        done = sum(1 for st in self.steps if st["state"] in ("done", "skipped"))
        self.percent = round(100 * done / len(self.steps)) if self.steps else 0
        self.flush()

    def fail_running(self) -> None:
        """The step that was running when the job stopped is the one that failed."""
        for st in self.steps:
            if st["state"] == "running":
                st["state"] = "failed"
        self.flush()

    def remember(self, kind: str, label: str, state: str, started: int) -> None:
        """Keeps the job in the history (the last fifteen)."""
        history = self.read("history.json")
        history = history if isinstance(history, list) else []
        history.append({"id": str(started), "started": started, "kind": kind, "label": label, "state": state})
        self.write("history.json", history[-15:])


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
        "notes": str(data.get("body") or "")[:8000],
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
        if not target.is_relative_to(root) or m.islnk() or m.issym() or m.isdev():
            raise ValueError(f"the archive holds an unsafe entry: {m.name}")
        yield m


def verify_signature(archive: Path, signature: bytes, signers: str, run=subprocess.run) -> bool:
    """True when `signature` (the text of an OpenSSH signature) is a valid signature of `archive` by one of the keys in `signers`."""
    with tempfile.TemporaryDirectory(prefix="eh3d-sig-") as tmp:
        sig_file = Path(tmp) / "file.sig"
        sig_file.write_bytes(signature)
        with archive.open("rb") as data:
            done = run(
                ["ssh-keygen", "-Y", "verify", "-f", signers, "-I", SIGNATURE_IDENTITY, "-n", SIGNATURE_NAMESPACE, "-s", str(sig_file)],
                stdin=data,
                capture_output=True,
            )
    return done.returncode == 0


def install(folder: Folder, comp_id: str, comp: dict, release: dict, get=http_get, run=subprocess.run) -> None:
    version = release["latest"]
    name = comp["asset"].format(version=version, arch=arch())
    url = release["assets"].get(name)
    if not url:
        raise RuntimeError(f"the release {version} has no file called {name}")
    folder.step(f"{comp_id}.download", "running")
    folder.say("updating", f"{comp_id}: downloading {name}")
    with tempfile.TemporaryDirectory(prefix="eh3d-update-") as tmp:
        tmpdir = Path(tmp)
        archive = tmpdir / name
        archive.write_bytes(get(url, 600))
        folder.step(f"{comp_id}.download", "done")
        folder.step(f"{comp_id}.verify", "running")
        hash_url = release["assets"].get(name + ".sha256")
        if not hash_url:
            raise RuntimeError(f"the release has no {name}.sha256 to check the file with")
        want = expected_hash(get(hash_url).decode("utf-8", "replace"), name)
        if not want or sha256_of(archive) != want:
            raise RuntimeError(f"{name}: the hash does not match, nothing was installed")
        if SIGNERS:
            sig_url = release["assets"].get(name + ".sig")
            if not sig_url:
                raise RuntimeError(f"the release has no {name}.sig and this machine only installs what is signed ({SIGNERS})")
            if not verify_signature(archive, get(sig_url), SIGNERS):
                raise RuntimeError(f"{name}: the signature is not one of the allowed signers, nothing was installed")
            folder.say("updating", f"{comp_id}: the signature is right")
        else:
            folder.say("updating", f"{comp_id}: no file of allowed signers: the hash proves the file arrived whole, not who made it")
        folder.say("updating", f"{comp_id}: the hash is right, unpacking")
        folder.step(f"{comp_id}.verify", "done")
        folder.step(f"{comp_id}.install", "running")
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
        (new / VERSION_MARK).write_text(version + chr(10), encoding="utf-8")
        old = target.parent / (target.name + ".previous")
        if old.exists():
            shutil.rmtree(old)
        if target.exists():
            target.rename(old)
        new.rename(target)
    folder.step(f"{comp_id}.install", "done")
    folder.say("updating", f"{comp_id}: installed {version}")
    if comp.get("restart"):
        folder.step(f"{comp_id}.restart", "running")
        folder.say("updating", f"{comp_id}: restarting")
        run(comp["restart"], check=True)
        folder.step(f"{comp_id}.restart", "done")


def version_in(path: Path):
    """The version of an installed folder: what the helper left in it, or the `version` of the `version.json` that the pages bring. None when it cannot be told."""
    try:
        text = (path / VERSION_MARK).read_text(encoding="utf-8").strip()
        if text:
            return text
    except OSError:
        pass
    try:
        value = json.loads((path / "version.json").read_text(encoding="utf-8")).get("version")
        return str(value) if value else None
    except (OSError, ValueError, AttributeError):
        return None


def previous_of(comp: dict) -> Path:
    target = Path(comp["install_dir"])
    return target.parent / (target.name + ".previous")


def write_previous(folder: Folder, config: dict) -> None:
    """Says which components have a version left behind that can be put back, and which version it is."""
    found = {}
    for comp_id, comp in config.items():
        old = previous_of(comp)
        found[comp_id] = {"available": old.is_dir(), "version": version_in(old) if old.is_dir() else None}
    folder.write("previous.json", found)


def undo_steps(config: dict, ids: list) -> list:
    out = []
    for comp_id in ids:
        comp = config.get(comp_id)
        if comp is None:
            continue
        out.append(f"{comp_id}.restore")
        if comp.get("restart"):
            out.append(f"{comp_id}.restart")
    return out


def undo(folder: Folder, config: dict, ids: list, run=subprocess.run) -> None:
    """Puts back the version that an update left behind. The one installed takes its place as the one left behind, so doing it twice goes forward again."""
    if not ids:
        raise RuntimeError("say which components to put back")
    for comp_id in ids:
        if not previous_of(config[comp_id]).is_dir():
            raise RuntimeError(f"{comp_id}: there is no earlier version kept to put back")
    folder.plan(undo_steps(config, ids))
    for comp_id in ids:
        comp = config[comp_id]
        target, old = Path(comp["install_dir"]), previous_of(comp)
        folder.step(f"{comp_id}.restore", "running")
        before, after = version_in(target), version_in(old)
        folder.say("updating", f"{comp_id}: putting back {after or 'the earlier version'}")
        aside = target.parent / (target.name + ".undone")
        if aside.exists():
            shutil.rmtree(aside)
        if target.exists():
            target.rename(aside)
        old.rename(target)
        if aside.exists():
            aside.rename(old)
        folder.step(f"{comp_id}.restore", "done")
        folder.say("updating", f"{comp_id}: back to {after or 'the earlier version'} (was {before or 'unknown'})")
        if comp.get("restart"):
            folder.step(f"{comp_id}.restart", "running")
            folder.say("updating", f"{comp_id}: restarting")
            run(comp["restart"], check=True)
            folder.step(f"{comp_id}.restart", "done")
    folder.say("done", "put back: " + ", ".join(ids))


def check(folder: Folder, config: dict, get=http_get) -> list:
    found = []
    for comp_id, comp in config.items():
        folder.say("checking", f"{comp_id}: asking GitHub for the latest release of {comp['repo']}")
        try:
            r = latest_release(comp["repo"], get)
            found.append({"id": comp_id, "repo": comp["repo"], "latest": r["latest"], "url": r["url"], "published": r["published"], "notes": r["notes"]})
        except Exception as e:  # a repository without a release, or no network: said, and the rest goes on
            folder.say("checking", f"{comp_id}: {e}")
    folder.write("components.json", found)
    return found


def steps_of(config: dict, ids: list) -> list:
    """The ids of the steps of an update: download, verify and install of each component, and its restart when it has one."""
    out = []
    for comp_id in ids:
        comp = config.get(comp_id)
        if comp is None:
            continue
        out += [f"{comp_id}.download", f"{comp_id}.verify", f"{comp_id}.install"]
        if comp.get("restart"):
            out.append(f"{comp_id}.restart")
    return out


def apply(folder: Folder, config: dict, ids: list, get=http_get, run=subprocess.run) -> None:
    folder.plan(steps_of(config, ids))
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
    folder.steps, folder.percent = [], 0
    started = int(time.time())
    kind = str(request.get("action", ""))
    if kind == "clear_history":
        # emptying the list is not a job to put in it
        folder.write("history.json", [])
        folder.say("done", "the history is empty")
        write_previous(folder, config)
        return True
    label = ", ".join(c for c in request.get("components", []) if c in config) if kind in ("apply", "undo") else kind
    state = "done"
    try:
        if kind == "check":
            found = check(folder, config, get)
            folder.say("done", f"{len(found)} component(s) checked")
        elif kind == "apply":
            # the server may restart itself: the result is written before that, so the Studio can show it when it comes back
            apply(folder, config, [c for c in request.get("components", []) if c in config], get, run)
        elif kind == "undo":
            undo(folder, config, [c for c in request.get("components", []) if c in config], run)
        else:
            raise RuntimeError("the request is neither check, apply, undo nor clear_history")
    except Exception as e:
        state = "failed"
        folder.fail_running()
        folder.say("error", str(e))
    folder.remember(kind or "unknown", label, state, started)
    try:
        write_previous(folder, config)
    except OSError:
        pass
    return True


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Updates the EH3D services from the releases of GitHub, as the server asks.")
    p.add_argument("--dir", required=True, help="the shared folder (updates_dir of server.cfg)")
    p.add_argument("--signers", help="the file of allowed signers (ssh-keygen format): when given or present at /etc/eh3d-update.signers, only signed releases are installed")
    p.add_argument("--config", help="a JSON file that replaces the components (id: repo, asset, install_dir, program, restart)")
    a = p.parse_args(argv)
    global SIGNERS
    if a.signers:
        SIGNERS = a.signers
    config = DEFAULT_CONFIG
    if a.config:
        config = json.loads(Path(a.config).read_text(encoding="utf-8"))
    folder = Folder(Path(a.dir))
    folder.path.mkdir(parents=True, exist_ok=True)
    handle(folder, config)
    return 0


if __name__ == "__main__":
    sys.exit(main())
