#!/usr/bin/env python3
# EH3D-SERVER - tools/smoke_binary.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D). GPL-3.0-or-later.
"""Smoke test of the built server binary: it must start, answer its health check with its own version, and stop when told to.

    python -I tools/smoke_binary.py target/release/eh3d-server[.exe]

What it checks, in this order: `--version` prints the version of the package; the server starts on a free port of 127.0.0.1 with an empty data folder; `GET /api/v1/health`
answers `status: ok` with the same version; `GET /api/v1/setup` says a set-up is needed; and the process ends on SIGTERM (a plain termination on Windows) with a success
status. It exits 0 when all of it holds, 1 otherwise. Only the standard library is used.
"""
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(url: str):
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.loads(r.read())


def main(argv) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    binary = str(Path(argv[1]).resolve())
    problems = []

    version = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=30).stdout.strip()
    if not version.startswith("eh3d-server "):
        problems.append(f"--version printed '{version}'")
    number = version.split(" ", 1)[-1]

    with tempfile.TemporaryDirectory() as tmp:
        port = free_port()
        cfg = Path(tmp) / "server.cfg"
        cfg.write_text(f"listen = 127.0.0.1:{port}\ndata_dir = {Path(tmp) / 'data'}\n", encoding="utf-8")
        proc = subprocess.Popen([binary, "--config", str(cfg)], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            base = f"http://127.0.0.1:{port}/api/v1"
            health = None
            end = time.time() + 30
            while time.time() < end and proc.poll() is None:
                try:
                    health = get(f"{base}/health")
                    break
                except OSError:
                    time.sleep(0.2)
            if health is None:
                problems.append("the server did not answer its health check within 30 seconds")
            else:
                if health.get("status") != "ok":
                    problems.append(f"health status is {health.get('status')!r}")
                if health.get("version") != number:
                    problems.append(f"health says version {health.get('version')!r}, --version says {number!r}")
                if not isinstance(health.get("api_revision"), int):
                    problems.append("health has no api_revision")
                if get(f"{base}/setup").get("needed") is not True:
                    problems.append("a new data folder should need the set-up")
        finally:
            if proc.poll() is None:
                if os.name == "nt":
                    proc.terminate()
                else:
                    proc.send_signal(signal.SIGTERM)
                try:
                    proc.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    problems.append("the server did not stop within 20 seconds")
            else:
                problems.append(f"the server ended by itself with status {proc.returncode}")
        if os.name != "nt" and proc.returncode != 0:
            problems.append(f"the server stopped with status {proc.returncode}")
        output = proc.stdout.read() if proc.stdout else ""

    if problems:
        print("SMOKE FAIL")
        for p in problems:
            print(" -", p)
        print(output)
        return 1
    print(f"SMOKE PASS: eh3d-server {number} started, answered its health check and stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
