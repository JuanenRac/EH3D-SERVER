# EH3D-SERVER - tests/test_update_helper.py: the helper that updates the installed services from the releases of GitHub, with the network and systemd replaced by
# small stand-ins (nothing is downloaded and nothing is restarted).    python -I tests/test_update_helper.py
import hashlib
import importlib.util
import io
import json
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


if __name__ == "__main__":
    unittest.main()
