#!/usr/bin/env python3
# =============================================================================
# A.R.M.O.R. - tools/pretty_run.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""A live panel for the build and test scripts: a banner, one line per step with its time, the output of the step folded away (it appears only when the step fails), the number
of tests that ran, and a summary at the end. Standard library only, so every project can carry a copy.

    panel = Panel("ARMOR-SERVER", "build-test", "0.8.0.6")
    panel.banner()
    panel.step("Type check", ["npm", "run", "typecheck"], cwd=project)
    panel.skip("Browser tests", "no browser on this machine")
    sys.exit(panel.finish())

On a terminal the running step shows a spinner, the time and the last line of its output; in a pipe (CI, a log) every step is a plain line. NO_COLOR or PRETTY_PLAIN=1 turns the colours
and the drawing characters off. Nothing here changes what a step does: the commands, their order and their exit codes are the ones of the caller.
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Mapping, Sequence

__all__ = ["Panel", "count_tests"]

_PLAIN = bool(os.environ.get("NO_COLOR")) or os.environ.get("PRETTY_PLAIN") == "1"


def _enable_terminal() -> bool:
    """Colours and UTF-8 on the Windows console as well; False when the output is not a terminal."""
    try:
        if hasattr(sys.stdout, "reconfigure"):
            sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (OSError, ValueError):
        pass
    if not sys.stdout.isatty():
        return False
    if os.name == "nt":
        try:
            import ctypes
            kernel = ctypes.windll.kernel32  # type: ignore[attr-defined]
            handle = kernel.GetStdHandle(-11)
            mode = ctypes.c_uint32()
            if kernel.GetConsoleMode(handle, ctypes.byref(mode)):
                kernel.SetConsoleMode(handle, mode.value | 0x0004)   # ENABLE_VIRTUAL_TERMINAL_PROCESSING
        except Exception:  # noqa: BLE001 - an old console: plain lines are enough
            return False
    return True


_TTY = (_enable_terminal() or os.environ.get("PRETTY_FORCE_TTY") == "1") and not _PLAIN

# ---- how a line of output says that tests ran -------------------------------------------------------------------------------------------------------------------
_COUNTERS: Sequence[tuple[re.Pattern[str], str]] = (
    (re.compile(r"^\s*ℹ pass (\d+)", re.M), "passed"), (re.compile(r"^\s*ℹ fail (\d+)", re.M), "failed"),                      # node --test
    (re.compile(r"^\s*Tests\s+(?:(\d+) failed \| )?(\d+) passed", re.M), "vitest"),                                              # vitest
    (re.compile(r"=+ (?:(\d+) failed, )?(\d+) passed"), "pytest"),                                                                # pytest
    (re.compile(r"^Ran (\d+) tests?", re.M), "ran"),                                                                              # unittest
    (re.compile(r"(\d+)% tests passed, (\d+) tests failed out of (\d+)"), "ctest"),                                               # ctest
    (re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed"), "cargo"),                                            # cargo
)


def count_tests(output: str) -> tuple[int, int]:
    """(passed, failed) as the output of a test runner says it; (0, 0) when it says nothing."""
    passed = failed = 0
    for pattern, kind in _COUNTERS:
        for match in pattern.finditer(output):
            if kind == "passed": passed += int(match.group(1))
            elif kind == "failed": failed += int(match.group(1))
            elif kind == "vitest": failed += int(match.group(1) or 0); passed += int(match.group(2))
            elif kind == "pytest": failed += int(match.group(1) or 0); passed += int(match.group(2))
            elif kind == "ran":
                total = int(match.group(1))
                bad = re.search(r"FAILED \((?:failures=(\d+))?(?:, )?(?:errors=(\d+))?", output)
                broken = int(bad.group(1) or 0) + int(bad.group(2) or 0) if bad else 0
                failed += broken; passed += total - broken
            elif kind == "ctest": failed += int(match.group(2)); passed += int(match.group(3)) - int(match.group(2))
            elif kind == "cargo": passed += int(match.group(1)); failed += int(match.group(2))
    return passed, failed


def _c(code: str, text: str) -> str:
    return text if not _TTY else f"\033[{code}m{text}\033[0m"


def _seconds(value: float) -> str:
    if value < 60:
        return f"{value:.1f} s"
    minutes, rest = divmod(int(value), 60)
    return f"{minutes} min {rest:02d} s"


class Panel:
    """The whole panel of one script: see the module text."""

    def __init__(self, project: str, mode: str, version: str = "", tagline: str = "A.R.M.O.R.") -> None:
        self.project, self.mode, self.version, self.tagline = project, mode, version, tagline
        self.started = time.monotonic()
        self.steps: list[tuple[str, str, float, str]] = []     # title, state (ok / fail / skip), seconds, note
        self.passed = self.failed = 0
        self.log_path = Path(tempfile.gettempdir()) / f"pretty_run-{re.sub(r'[^A-Za-z0-9_.-]', '_', project)}-{mode}.log"
        self._log: list[str] = []
        self._width = max(60, min(shutil.get_terminal_size((100, 24)).columns - 2, 100))

    # ---- drawing -----------------------------------------------------------------------------------------------------------------------------------------
    def _box(self, lines: Sequence[str], colour: str = "36") -> None:
        width = self._width
        top, bottom, side = ("╔" + "═" * (width - 2) + "╗", "╚" + "═" * (width - 2) + "╝", "║") if _TTY else ("+" + "=" * (width - 2) + "+", "+" + "=" * (width - 2) + "+", "|")
        print(_c(colour, top))
        for line in lines:
            plain = re.sub(r"\033\[[0-9;]*m", "", line)
            print(_c(colour, side) + line + " " * max(0, width - 2 - len(plain)) + _c(colour, side))
        print(_c(colour, bottom))

    def banner(self) -> None:
        title = f"  {_c('1;36', self.tagline)}   {_c('1', self.project)}"
        info = f"  {self.mode.upper()}" + (f"   v{self.version}" if self.version else "") + "   · Electro Hobby 3D"
        self._box(["", title, _c("2", info), ""])
        print()

    # ---- steps -------------------------------------------------------------------------------------------------------------------------------------------
    def _line(self, symbol: str, colour: str, title: str, tail: str) -> str:
        plain = re.sub(r"\[[0-9;]*m", "", tail)
        dots = max(3, self._width - 5 - len(title) - len(plain))   # the time (or the reason) lines up at the right edge
        return f" {_c(colour, symbol)} {title} {_c('2', '·' * dots)} {tail}"

    def skip(self, title: str, why: str = "") -> None:
        self.steps.append((title, "skip", 0.0, why))
        print(self._line("○", "33", title, _c("33", "skipped") + (f" {_c('2', '(' + why + ')')}" if why else "")) if _TTY else f" skip {title}" + (f"  ({why})" if why else ""))

    def fail(self, title: str, reason: str) -> None:
        """A failure that is not a command (a tool that is missing, a check that did not hold): it counts as a failed step."""
        self.steps.append((title, "fail", 0.0, reason))
        print(self._line("✘", "1;31", title, _c("1;31", "failed")) if _TTY else f" FAIL {title}")
        print("     " + _c("31", reason))
        print()

    def note(self, text: str) -> None:
        print(f"   {_c('2', text)}")

    def step(self, title: str, args: Sequence[str], cwd: Path | str | None = None, env: Mapping[str, str] | None = None, tests: bool = False) -> bool:
        """Runs a command as one step of the panel. Returns whether it succeeded; the output is kept and shown only when it fails."""
        started = time.monotonic()
        environment = {**os.environ, **(env or {}), "PYTHONUNBUFFERED": "1", "FORCE_COLOR": "0", "NO_COLOR": "1"}
        output: list[str] = []
        try:
            process = subprocess.Popen(list(args), cwd=cwd, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace", bufsize=1)
        except FileNotFoundError:
            self._finish_step(title, False, time.monotonic() - started, [f"required executable was not found: {args[0]}"])
            return False
        last = [""]

        def read() -> None:
            assert process.stdout is not None
            for raw in process.stdout:
                text = raw.rstrip("\r\n")
                output.append(text)
                if text.strip():
                    last[0] = text.strip()

        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        frames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
        tick = 0
        while process.poll() is None:
            if _TTY:
                elapsed = time.monotonic() - started
                room = self._width - 24 - len(title)
                tail = (last[0][:max(10, room)] if room > 10 else "")
                sys.stdout.write("\r\033[2K " + _c("36", frames[tick % len(frames)]) + f" {title}  {_c('2', _seconds(elapsed))}  {_c('2', tail)}")
                sys.stdout.flush()
                tick += 1
            time.sleep(0.1)
        reader.join(timeout=5)
        ok = process.returncode == 0
        self._finish_step(title, ok, time.monotonic() - started, output, count=tests or True)
        return ok

    def _finish_step(self, title: str, ok: bool, seconds: float, output: Sequence[str], count: bool = False) -> None:
        text = "\n".join(output)
        self._log.append(f"=== {title}\n{text}\n")
        passed, failed = count_tests(text) if count else (0, 0)
        self.passed += passed; self.failed += failed
        note = f"{passed} tests" if passed and ok else ""
        self.steps.append((title, "ok" if ok else "fail", seconds, note))
        if _TTY:
            sys.stdout.write("\r\033[2K")
        tail = _c("32" if ok else "1;31", _seconds(seconds)) + (f"  {_c('2', note)}" if note else "")
        print(self._line("✔" if ok else "✘", "32" if ok else "1;31", title, tail) if _TTY else f" {'ok  ' if ok else 'FAIL'} {title}  {_seconds(seconds)} {note}".rstrip())
        if not ok:
            for line in list(output)[-40:]:
                print("     " + _c("31", line))
            print()

    # ---- a whole script as a run of steps --------------------------------------------------------------------------------------------------------------------
    _MARKER = re.compile(r"^\s*\[(\d+)/(\d+)\]\s*(.+?)\s*$")
    _COMMAND = re.compile(r"^[+$] (\S.*)$")

    def wrap(self, args: Sequence[str], cwd: Path | str | None = None, env: Mapping[str, str] | None = None) -> int:
        """Runs a script as it is and draws it as steps: a line `[2/5] Running the tests...` starts a step; when the script has no such lines, a line `+ command` or `$ command` does.
        Whatever the script prints before the first step (its own banner) is left out of the panel. The exit code is the one of the script; the step that was running when it failed is the failed one."""
        import queue
        environment = {**os.environ, **(env or {}), "PYTHONUNBUFFERED": "1", "FORCE_COLOR": "0", "NO_COLOR": "1", "PRETTY_RUN": "1"}
        try:
            process = subprocess.Popen(list(args), cwd=cwd, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace", bufsize=1)
        except FileNotFoundError:
            self.fail("Start", f"required executable was not found: {args[0]}")
            return self.finish()
        lines: "queue.Queue[str | None]" = queue.Queue()

        def read() -> None:
            assert process.stdout is not None
            for raw in process.stdout:
                lines.put(raw.rstrip("\r\n"))
            lines.put(None)

        threading.Thread(target=read, daemon=True).start()
        preamble: list[str] = []                    # what the script prints before its first step
        current: list[str] | None = None            # the output of the step that is running
        title = ""
        began = 0.0
        markers = False
        frames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
        tick = 0
        last = ""
        done = False

        def close(ok: bool) -> None:
            nonlocal current, title
            if current is not None:
                self._finish_step(title, ok, time.monotonic() - began, current, count=True)
            current = None

        def begin(text: str) -> None:
            nonlocal current, title, began, last
            close(True)
            current, title, began, last = [], text[:60], time.monotonic(), ""

        while not done:
            try:
                line = lines.get(timeout=0.1)
            except queue.Empty:
                line = ""
                if _TTY and current is not None:
                    room = self._width - 24 - len(title)
                    tail = last[:max(10, room)] if room > 10 else ""
                    sys.stdout.write("\r\033[2K " + _c("36", frames[tick % len(frames)]) + f" {title}  {_c('2', _seconds(time.monotonic() - began))}  {_c('2', tail)}")
                    sys.stdout.flush()
                    tick += 1
                continue
            if line is None:
                done = True
                break
            marker = self._MARKER.match(line)
            command = None if markers else self._COMMAND.match(line)
            if marker:
                markers = True
                begin(marker.group(3))
                continue
            if command:
                begin(command.group(1))
                continue
            if current is not None:
                current.append(line)
                if line.strip():
                    last = line.strip()
            else:
                preamble.append(line)
        code = process.wait()
        if current is None and not self.steps:   # a script that says nothing about steps is one step
            current, title, began = preamble, (Path(str(args[-1])).stem or "script"), self.started
        close(code == 0)
        if code != 0 and not any(step[1] == "fail" for step in self.steps):
            self.fail("Script", f"it ended with exit code {code}")
        result = self.finish()
        return code if code else result

    # ---- the end -----------------------------------------------------------------------------------------------------------------------------------------
    def finish(self, extra: str = "") -> int:
        """The summary; the exit code that the script should return (0 when no step failed)."""
        total = time.monotonic() - self.started
        failed_steps = [s for s in self.steps if s[1] == "fail"]
        ok = not failed_steps and self.failed == 0
        try:
            self.log_path.write_text("\n".join(self._log), encoding="utf-8")
        except OSError:
            pass
        done = sum(1 for s in self.steps if s[1] == "ok")
        skipped = sum(1 for s in self.steps if s[1] == "skip")
        print()
        verdict = (_c("1;32", "  ✔  PASS") if ok else _c("1;31", "  ✘  FAIL")) if _TTY else ("  PASS" if ok else "  FAIL")
        counts = f"{done}/{len(self.steps) - skipped} steps" + (f" · {skipped} skipped" if skipped else "")
        tests = f" · {self.passed} tests passed" + (f", {self.failed} failed" if self.failed else "") if (self.passed or self.failed) else ""
        lines = ["", verdict + f"   {counts}{tests}   · {_seconds(total)}"]
        if failed_steps:
            lines.append(_c("31", "  failed: " + ", ".join(s[0] for s in failed_steps)))
        if extra:
            lines.append("  " + extra)
        lines.append(_c("2", f"  full output: {self.log_path}"))
        lines.append("")
        self._box(lines, "32" if ok else "31")
        return 0 if ok else 1


def _wrap_main(argv: Sequence[str]) -> int:
    """`pretty_run.py --wrap [--mode build] -- <script and its arguments>`: the script runs as it is and its steps are drawn as a panel."""
    import argparse
    import json
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--wrap", action="store_true")
    parser.add_argument("--mode", default="build")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    options = parser.parse_args(list(argv))
    command = [c for c in options.command if c != "--"]
    if not command:
        print("usage: pretty_run.py --wrap [--mode build] -- <script> [arguments]", file=sys.stderr)
        return 2
    script = Path(command[-1] if Path(command[-1]).exists() else command[0]).resolve()
    folder = script.parent if script.is_file() else Path.cwd()
    name, version, tagline = folder.name, "", "Electro Hobby 3D"
    for manifest in sorted(folder.glob("*.project.json")):
        try:
            data = json.loads(manifest.read_text(encoding="utf-8"))
            name, version = str(data.get("name", name)), str(data.get("version", ""))
            tagline = str(data.get("ecosystem") or tagline)   # the name of the family, as its manifest says
        except (OSError, ValueError):
            continue
        break
    panel = Panel(name, options.mode, version, tagline)
    panel.banner()
    code = panel.wrap(command, cwd=folder)
    if sys.stdin.isatty() and not os.environ.get("CI") and not os.environ.get("HYDRA_UMC_CI") and os.environ.get("PRETTY_PAUSE") != "0":
        try:
            input("  Press Enter to close...")
        except EOFError:
            pass
    return code



if __name__ == "__main__":
    if "--wrap" in sys.argv[1:]:
        raise SystemExit(_wrap_main(sys.argv[1:]))
    # a small demonstration, with no project: python tools/pretty_run.py
    demo = Panel("DEMO", "build-test", "0.0.1")
    demo.banner()
    demo.step("Say hello", [sys.executable, "-c", "print('hello'); import time; time.sleep(1.2)"])
    demo.step("Run some tests", [sys.executable, "-c", "print('Ran 12 tests in 0.1s'); print('OK')"], tests=True)
    demo.skip("Browser tests", "no browser here")
    raise SystemExit(demo.finish())
