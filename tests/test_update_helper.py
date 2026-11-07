# EH3D-SERVER - tests/test_update_helper.py: the helper that updates the installed services from the releases of GitHub, with the network and systemd replaced by
# small stand-ins (nothing is downloaded and nothing is restarted).    python -I tests/test_update_helper.py
import hashlib
import importlib.util
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("eh3d_update", Path(__file__).resolve().parent.parent / "deploy" / "eh3d-update.py")
H = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(H)


def tarball(files: dict, folder: str = "", extra=None) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as t:
        for name, data in files.items():
            info = tarfile.TarInfo((folder + "/" if folder else "") + name)
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
        if extra:
            extra(t)
    return buf.getvalue()


class FakeGitHub:
    """The answers of GitHub for two repositories, and the files of their releases."""

    def __init__(self, tag_server="v0.0.2", tag_studio="v0.0.3"):
        arch = H.arch()
        self.files = {
            f"https://dl/eh3d-server-{tag_server.lstrip('v')}-linux-{arch}.tar.gz": tarball({"eh3d-server": b"#!/bin/sh\necho new\n"}, "eh3d-server"),
            f"https://dl/eh3d-studio-{tag_studio.lstrip('v')}.tar.gz": tarball({"index.html": b"<html>new</html>", "version.json": b'{"version":"%s"}' % tag_studio.lstrip("v").encode()}),
        }
        self.releases = {
            "EH3D-SERVER": {"tag_name": tag_server, "html_url": "https://r/s", "published_at": "2026-01-01T00:00:00Z", "assets": []},
            "EH3D-STUDIO": {"tag_name": tag_studio, "html_url": "https://r/t", "published_at": "2026-01-02T00:00:00Z", "assets": []},
        }
        for url in list(self.files):
            self.add_asset(url)
        self.requested = []

    def add_asset(self, url: str, with_hash: bool = True):
        name = url.rsplit("/", 1)[1]
        repo = "EH3D-SERVER" if "server" in name else "EH3D-STUDIO"
        self.releases[repo]["assets"].append({"name": name, "browser_download_url": url})
        if with_hash:
            hash_url = url + ".sha256"
            self.files[hash_url] = (hashlib.sha256(self.files[url]).hexdigest() + "  " + name + "\n").encode()
            self.releases[repo]["assets"].append({"name": name + ".sha256", "browser_download_url": hash_url})

    def get(self, url: str, timeout: int = 30) -> bytes:
        self.requested.append(url)
        if url.startswith("https://api.github.com/repos/JuanenRac/"):
            repo = url.split("/repos/JuanenRac/")[1].split("/")[0]
            if repo not in self.releases:
                raise OSError("404 Not Found")
            return json.dumps(self.releases[repo]).encode()
        return self.files[url]


class Restarts:
    def __init__(self):
        self.calls = []

    def __call__(self, cmd, check=False):
        self.calls.append(cmd)


def config(root: Path) -> dict:
    return {
        "server": {"repo": "EH3D-SERVER", "asset": "eh3d-server-{version}-linux-{arch}.tar.gz", "install_dir": str(root / "opt" / "eh3d-server"), "program": "eh3d-server", "restart": ["systemctl", "restart", "eh3d-server"]},
        "studio": {"repo": "EH3D-STUDIO", "asset": "eh3d-studio-{version}.tar.gz", "install_dir": str(root / "opt" / "eh3d-studio"), "program": None, "restart": None},
    }


class UpdateHelper(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "meet").mkdir()
        self.folder = H.Folder(self.root / "meet")
        self.gh = FakeGitHub()
        self.run = Restarts()

    def tearDown(self):
        self.tmp.cleanup()

    def ask(self, request: dict):
        (self.root / "meet" / "request.json").write_text(json.dumps(request), encoding="utf-8")
        return H.handle(self.folder, config(self.root), self.gh.get, self.run)

    def test_without_a_request_nothing_is_done(self):
        self.assertFalse(H.handle(self.folder, config(self.root), self.gh.get, self.run))
        self.assertEqual(self.gh.requested, [])

    def test_a_check_writes_what_the_latest_releases_are(self):
        self.assertTrue(self.ask({"action": "check"}))
        found = {c["id"]: c for c in self.folder.read("components.json")}
        self.assertEqual((found["server"]["latest"], found["studio"]["latest"]), ("0.0.2", "0.0.3"))
        self.assertEqual(found["studio"]["url"], "https://r/t")
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        # the request is taken: the server may ask again
        self.assertFalse((self.root / "meet" / "request.json").exists())

    def test_a_repository_without_a_release_is_said_and_the_rest_goes_on(self):
        del self.gh.releases["EH3D-SERVER"]
        self.ask({"action": "check"})
        found = [c["id"] for c in self.folder.read("components.json")]
        self.assertEqual(found, ["studio"])
        self.assertTrue(any("404" in line for line in self.folder.read("status.json")["log"]))

    def test_an_update_replaces_the_folder_checks_the_hash_and_keeps_the_old_one(self):
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        (studio / "index.html").write_text("old", encoding="utf-8")
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        self.assertEqual((self.root / "opt" / "eh3d-studio.previous" / "index.html").read_text(encoding="utf-8"), "old")
        self.assertEqual(self.run.calls, [])  # the pages need no restart

    def test_the_server_is_replaced_made_executable_and_restarted(self):
        self.ask({"action": "apply", "components": ["server"]})
        program = self.root / "opt" / "eh3d-server" / "eh3d-server"
        self.assertTrue(program.exists())
        self.assertEqual(self.run.calls, [["systemctl", "restart", "eh3d-server"]])
        self.assertEqual(self.folder.read("status.json")["state"], "done")

    def test_a_file_whose_hash_does_not_match_is_not_installed(self):
        for url in list(self.gh.files):
            if url.endswith(".sha256") and "studio" in url:
                self.gh.files[url] = (("0" * 64) + "  eh3d-studio-0.0.3.tar.gz\n").encode()
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        (studio / "index.html").write_text("old", encoding="utf-8")
        self.ask({"action": "apply", "components": ["studio"]})
        status = self.folder.read("status.json")
        self.assertEqual(status["state"], "error")
        self.assertIn("hash", status["message"])
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")

    def test_a_release_without_the_file_or_without_its_hash_is_refused(self):
        self.gh.releases["EH3D-STUDIO"]["assets"] = []
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertEqual(self.folder.read("status.json")["state"], "error")
        gh = FakeGitHub()
        url = next(u for u in gh.files if "studio" in u and not u.endswith(".sha256"))
        gh.releases["EH3D-STUDIO"]["assets"] = [{"name": url.rsplit("/", 1)[1], "browser_download_url": url}]
        self.gh = gh
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertIn("sha256", self.folder.read("status.json")["message"])

    def test_an_archive_with_a_name_outside_the_folder_is_refused(self):
        def evil(t):
            info = tarfile.TarInfo("../outside.txt")
            info.size = 1
            t.addfile(info, io.BytesIO(b"x"))

        url = next(u for u in self.gh.files if "studio" in u and not u.endswith(".sha256"))
        self.gh.files[url] = tarball({"index.html": b"x"}, extra=evil)
        self.gh.files[url + ".sha256"] = (hashlib.sha256(self.gh.files[url]).hexdigest() + "  " + url.rsplit("/", 1)[1] + "\n").encode()
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertEqual(self.folder.read("status.json")["state"], "error")
        self.assertFalse((self.root / "outside.txt").exists())

    def _keys(self, name: str):
        key = self.root / name
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True, capture_output=True)
        return key

    def _sign(self, key: Path, data: bytes) -> bytes:
        target = self.root / "to-sign.bin"
        target.write_bytes(data)
        (self.root / "to-sign.bin.sig").unlink(missing_ok=True)
        subprocess.run(["ssh-keygen", "-q", "-Y", "sign", "-f", str(key), "-n", "eh3d-release", str(target)], check=True, capture_output=True)
        return (self.root / "to-sign.bin.sig").read_bytes()

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_with_allowed_signers_only_a_release_signed_by_one_of_them_is_installed(self):
        owner, thief = self._keys("owner"), self._keys("thief")
        signers = self.root / "signers"
        signers.write_text("eh3d-release " + (self.root / "owner.pub").read_text(encoding="utf-8"), encoding="utf-8")
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        (studio / "index.html").write_text("old", encoding="utf-8")
        url = next(u for u in self.gh.files if "studio" in u and not u.endswith(".sha256"))
        name = url.rsplit("/", 1)[1]
        sig_url = url + ".sig"
        before = H.SIGNERS
        H.SIGNERS = str(signers)
        try:
            # no signature published: refused
            self.ask({"action": "apply", "components": ["studio"]})
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertIn(".sig", self.folder.read("status.json")["message"])
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
            # signed by somebody else: refused
            self.gh.files[sig_url] = self._sign(thief, self.gh.files[url])
            self.gh.releases["EH3D-STUDIO"]["assets"].append({"name": name + ".sig", "browser_download_url": sig_url})
            self.ask({"action": "apply", "components": ["studio"]})
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
            # signed by the owner: installed
            self.gh.files[sig_url] = self._sign(owner, self.gh.files[url])
            self.ask({"action": "apply", "components": ["studio"]})
            self.assertEqual(self.folder.read("status.json")["state"], "done", self.folder.read("status.json"))
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        finally:
            H.SIGNERS = before

    def test_without_signers_the_hash_is_enough_and_the_log_says_who_is_not_checked(self):
        before = H.SIGNERS
        H.SIGNERS = None
        try:
            studio = self.root / "opt" / "eh3d-studio"
            studio.mkdir(parents=True)
            self.ask({"action": "apply", "components": ["studio"]})
            status = self.folder.read("status.json")
            self.assertEqual(status["state"], "done")
            self.assertTrue(any("not who made it" in line for line in status["log"]))
        finally:
            H.SIGNERS = before

    def test_a_name_that_only_starts_like_the_folder_is_outside_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            stage = Path(tmp) / "stage"
            stage.mkdir()
            buf = io.BytesIO()
            with tarfile.open(fileobj=buf, mode="w:gz") as t:
                info = tarfile.TarInfo("../stage2/x.txt")
                info.size = 1
                t.addfile(info, io.BytesIO(b"x"))
            buf.seek(0)
            with tarfile.open(fileobj=buf) as t:
                with self.assertRaises(ValueError):
                    list(H.safe_members(t, stage))

    def test_a_component_this_helper_does_not_know_is_left_out_and_a_bad_request_is_an_error(self):
        self.ask({"action": "apply", "components": ["kernel"]})
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        self.ask({"action": "dance"})
        self.assertEqual(self.folder.read("status.json")["state"], "error")

    def test_the_hash_file_is_read_in_its_two_usual_forms(self):
        h = "a" * 64
        self.assertEqual(H.expected_hash(f"{h}  file.tgz\n", "file.tgz"), h)
        self.assertEqual(H.expected_hash(f"{h} *file.tgz\n", "file.tgz"), h)
        self.assertEqual(H.expected_hash(f"{h}\n", "file.tgz"), h)
        self.assertEqual(H.expected_hash(f"{h}  other.tgz\n", "file.tgz"), "")
        self.assertEqual(H.expected_hash("not a hash", "file.tgz"), "")


    def test_an_update_shows_its_steps_and_the_percent_of_the_whole(self):
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        self.ask({"action": "apply", "components": ["studio", "server"]})
        status = self.folder.read("status.json")
        ids = [st["id"] for st in status["steps"]]
        # download, verify and install of each, and the restart of the one that has it
        self.assertEqual(
            ids,
            ["studio.download", "studio.verify", "studio.install", "server.download", "server.verify", "server.install", "server.restart"],
        )
        self.assertTrue(all(st["state"] == "done" and st["percent"] == 100 for st in status["steps"]))
        self.assertEqual(status["percent"], 100)

    def test_the_step_that_was_running_when_it_failed_is_marked_failed(self):
        for url in list(self.gh.files):
            if url.endswith(".sha256") and "studio" in url:
                self.gh.files[url] = (("0" * 64) + "  eh3d-studio-0.0.3.tar.gz\n").encode()
        self.ask({"action": "apply", "components": ["studio"]})
        status = self.folder.read("status.json")
        by_id = {st["id"]: st["state"] for st in status["steps"]}
        self.assertEqual((by_id["studio.download"], by_id["studio.verify"], by_id["studio.install"]), ("done", "failed", "pending"))
        self.assertLess(status["percent"], 100)

    def test_a_check_carries_the_notes_of_the_release(self):
        self.gh.releases["EH3D-STUDIO"]["body"] = "- A thing that changed\n- Another"
        self.ask({"action": "check"})
        found = {c["id"]: c for c in self.folder.read("components.json")}
        self.assertEqual(found["studio"]["notes"], "- A thing that changed\n- Another")
        self.assertEqual(found["server"]["notes"], "")

    def test_the_jobs_are_kept_in_a_history_of_fifteen(self):
        self.ask({"action": "check"})
        self.ask({"action": "apply", "components": ["studio"]})
        history = self.folder.read("history.json")
        self.assertEqual([(h["kind"], h["label"], h["state"]) for h in history], [("check", "check", "done"), ("apply", "studio", "done")])
        for _ in range(20):
            self.ask({"action": "check"})
        self.assertEqual(len(self.folder.read("history.json")), 15)
        self.gh.releases["EH3D-STUDIO"]["assets"] = []
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertEqual(self.folder.read("history.json")[-1]["state"], "failed")

    def _two_studios(self):
        """The studio updated once: 0.0.3 installed, the old pages left behind."""
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        (studio / "index.html").write_text("old", encoding="utf-8")
        (studio / "version.json").write_text('{"version":"0.0.1"}', encoding="utf-8")
        self.ask({"action": "apply", "components": ["studio"]})
        return studio

    def test_the_helper_says_which_version_an_update_left_behind(self):
        self.ask({"action": "check"})
        self.assertEqual(self.folder.read("previous.json")["studio"], {"available": False, "version": None})
        self._two_studios()
        self.assertEqual(self.folder.read("previous.json")["studio"], {"available": True, "version": "0.0.1"})
        # the server had no marker before: it is told after the next update
        self.ask({"action": "apply", "components": ["server"]})
        self.assertEqual(self.folder.read("previous.json")["server"], {"available": False, "version": None})
        self.ask({"action": "apply", "components": ["server"]})
        self.assertEqual(self.folder.read("previous.json")["server"], {"available": True, "version": "0.0.2"})

    def test_an_undo_puts_back_the_version_left_behind_and_a_second_one_goes_forward_again(self):
        studio = self._two_studios()
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        self.ask({"action": "undo", "components": ["studio"]})
        status = self.folder.read("status.json")
        self.assertEqual(status["state"], "done", status)
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
        self.assertEqual([st["id"] for st in status["steps"]], ["studio.restore"])
        self.assertEqual(self.folder.read("previous.json")["studio"], {"available": True, "version": "0.0.3"})
        self.assertEqual(self.folder.read("history.json")[-1]["kind"], "undo")
        self.ask({"action": "undo", "components": ["studio"]})
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        self.assertFalse((self.root / "opt" / "eh3d-studio.undone").exists())

    def test_an_undo_of_the_server_restarts_it_and_one_without_a_kept_version_is_an_error(self):
        self.ask({"action": "undo", "components": ["studio"]})
        status = self.folder.read("status.json")
        self.assertEqual(status["state"], "error")
        self.assertIn("no earlier version", status["message"])
        self.ask({"action": "apply", "components": ["server"]})
        self.run.calls.clear()
        self.ask({"action": "apply", "components": ["server"]})
        self.run.calls.clear()
        self.ask({"action": "undo", "components": ["server"]})
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        self.assertEqual(self.run.calls, [["systemctl", "restart", "eh3d-server"]])
        self.assertEqual([st["id"] for st in self.folder.read("status.json")["steps"]], ["server.restore", "server.restart"])

    def test_the_helper_of_the_release_takes_the_place_of_this_one_only_when_it_parses_and_differs(self):
        tree = self.root / "tree"
        (tree / "deploy").mkdir(parents=True)
        own = self.root / "installed-helper.py"
        own.write_text("print('old')\n", encoding="utf-8")
        comp = {"helper": "deploy/eh3d-update.py"}
        # a component that does not carry the file, or that does not name it, changes nothing
        self.assertFalse(H.refresh_helper(self.folder, "server", comp, tree, own))
        self.assertFalse(H.refresh_helper(self.folder, "studio", {}, tree, own))
        # a copy that does not parse is left aside and said
        (tree / "deploy" / "eh3d-update.py").write_text("def broken(:\n", encoding="utf-8")
        self.assertFalse(H.refresh_helper(self.folder, "server", comp, tree, own))
        self.assertEqual(own.read_text(encoding="utf-8"), "print('old')\n")
        self.assertTrue(any("left aside" in line for line in self.folder.read("status.json")["log"]))
        # a good one replaces it, and the same one again is nothing to do
        (tree / "deploy" / "eh3d-update.py").write_text("print('new')\n", encoding="utf-8")
        self.assertTrue(H.refresh_helper(self.folder, "server", comp, tree, own))
        self.assertEqual(own.read_text(encoding="utf-8"), "print('new')\n")
        self.assertFalse(H.refresh_helper(self.folder, "server", comp, tree, own))
        self.assertFalse((self.root / "installed-helper.py.new").exists())

    def test_the_history_is_emptied_on_request_and_that_request_is_not_kept_in_it(self):
        self.ask({"action": "check"})
        self.ask({"action": "check"})
        self.assertEqual(len(self.folder.read("history.json")), 2)
        self.ask({"action": "clear_history"})
        self.assertEqual(self.folder.read("history.json"), [])
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        self.ask({"action": "check"})
        self.assertEqual(len(self.folder.read("history.json")), 1)

    def test_an_answer_bigger_than_the_limit_is_refused_and_one_within_it_is_read(self):
        class Answer(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

        real = H.urllib.request.urlopen
        try:
            H.urllib.request.urlopen = lambda req, timeout=30: Answer(b"x" * 11)
            self.assertEqual(H.http_get("https://dl/f", 5, 11), b"x" * 11)
            with self.assertRaises(RuntimeError):
                H.http_get("https://dl/f", 5, 10)
        finally:
            H.urllib.request.urlopen = real


if __name__ == "__main__":
    unittest.main()
