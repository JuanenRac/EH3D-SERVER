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
            f"https://dl/eh3d-server-{tag_server.lstrip('v')}-linux-{arch}.tar.gz": tarball({"eh3d-server": b"#!/bin/sh\necho new\n"}, f"eh3d-server-{tag_server.lstrip('v')}-linux-{arch}"),
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
        # the helper asks for a signature by default; the tests of the updates themselves allow the hash alone, the tests of the signature say what they want
        if request.get("action") == "apply":
            request = {"allow_unsigned": True, **request}
        (self.root / "meet" / "request.json").write_text(json.dumps(request), encoding="utf-8")
        return H.handle(self.folder, config(self.root), self.gh.get, self.run)

    def test_without_a_request_nothing_is_done(self):
        self.assertFalse(H.handle(self.folder, config(self.root), self.gh.get, self.run))
        self.assertEqual(self.gh.requested, [])

    def test_a_link_left_in_the_shared_folder_is_never_written_through(self):
        import os
        target = self.root / "root-file.txt"
        target.write_text("keep me", encoding="utf-8")
        meet = self.root / "meet"
        for name in ("status.json.tmp", "history.json.tmp", "components.json.tmp", "previous.json.tmp"):
            os.symlink(target, meet / name)
        self.assertTrue(self.ask({"action": "check"}))
        self.assertEqual(target.read_text(encoding="utf-8"), "keep me", "the file the link pointed at is untouched")
        self.assertEqual(self.folder.read("status.json")["state"], "done", "and the helper still did its work")
        # a link in the place of a file is not read either
        os.remove(meet / "history.json")
        os.symlink(target, meet / "history.json")
        self.assertIsNone(self.folder.read("history.json"))

    def test_a_request_with_any_other_word_is_refused_and_not_copied_into_the_history(self):
        self.assertTrue(self.ask({"action": "evil\ntext /etc/ld.so.preload"}))
        history = self.folder.read("history.json")
        self.assertEqual([item["kind"] for item in history], ["unknown"])
        self.assertEqual(self.folder.read("status.json")["state"], "error")

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

    def test_a_release_older_than_the_installed_version_is_refused_unless_going_back_is_allowed(self):
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True)
        (studio / "index.html").write_text("installed", encoding="utf-8")
        (studio / "version.json").write_text('{"version":"0.0.9"}', encoding="utf-8")   # the latest release says v0.0.3
        self.ask({"action": "apply", "components": ["studio"]})
        status = self.folder.read("status.json")
        self.assertEqual(status["state"], "error")
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "installed")
        self.assertFalse((self.root / "opt" / "eh3d-studio.previous").exists())
        before = H.ALLOW_DOWNGRADE
        H.ALLOW_DOWNGRADE = True
        try:
            self.ask({"action": "apply", "components": ["studio"]})
        finally:
            H.ALLOW_DOWNGRADE = before
        self.assertEqual(self.folder.read("status.json")["state"], "done")
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")

    def test_the_same_or_a_newer_version_and_an_unreadable_one_are_installed(self):
        for installed in ("0.0.3", "0.0.2", "0.0.3.0", "unknown", ""):
            studio = self.root / "opt" / "eh3d-studio"
            shutil.rmtree(studio, ignore_errors=True)
            studio.mkdir(parents=True)
            (studio / "version.json").write_text(json.dumps({"version": installed}), encoding="utf-8")
            self.ask({"action": "apply", "components": ["studio"]})
            self.assertEqual(self.folder.read("status.json")["state"], "done", installed)

    def test_the_versions_are_compared_by_their_numbers(self):
        self.assertTrue(H.is_older("0.0.3", "0.0.10"))
        self.assertTrue(H.is_older("v1.2", "1.2.1"))
        self.assertFalse(H.is_older("1.2.0", "1.2"))
        self.assertFalse(H.is_older("2.0.0", "1.9.9"))
        self.assertFalse(H.is_older("0.0.3", None))
        self.assertFalse(H.is_older("nightly", "1.0.0"))

    def test_the_option_and_the_setting_allow_going_back(self):
        before = H.ALLOW_DOWNGRADE
        try:
            cfg = self.root / "helper.json"
            for body, argv, expected in (
                (config(self.root), [], False),
                ({**config(self.root), "settings": {"allow_downgrade": True}}, [], True),
                ({**config(self.root), "settings": {"allow_downgrade": "yes"}}, [], False),
                ({**config(self.root), "settings": {}}, ["--allow-downgrade"], True),
            ):
                cfg.write_text(json.dumps(body), encoding="utf-8")
                H.ALLOW_DOWNGRADE = not expected
                self.assertEqual(H.main(["--dir", str(self.root / "meet3"), "--config", str(cfg), *argv]), 0)
                self.assertEqual(H.ALLOW_DOWNGRADE, expected, (body.get("settings"), argv))
        finally:
            H.ALLOW_DOWNGRADE = before

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
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": False})
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertIn(".sig", self.folder.read("status.json")["message"])
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
            # signed by somebody else: refused
            self.gh.files[sig_url] = self._sign(thief, self.gh.files[url])
            self.gh.releases["EH3D-STUDIO"]["assets"].append({"name": name + ".sig", "browser_download_url": sig_url})
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": False})
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
            # signed by the owner: installed
            self.gh.files[sig_url] = self._sign(owner, self.gh.files[url])
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": False})
            self.assertEqual(self.folder.read("status.json")["state"], "done", self.folder.read("status.json"))
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        finally:
            H.SIGNERS = before

    def _studio_with(self, text: str):
        studio = self.root / "opt" / "eh3d-studio"
        studio.mkdir(parents=True, exist_ok=True)
        (studio / "index.html").write_text(text, encoding="utf-8")
        return studio

    def test_without_signers_nothing_is_installed_by_default(self):
        before = (H.SIGNERS, H.ALLOW_UNSIGNED)
        H.SIGNERS, H.ALLOW_UNSIGNED = None, False
        try:
            studio = self._studio_with("old")
            for request in (
                {"action": "apply", "components": ["studio"], "allow_unsigned": False},
                {"action": "apply", "components": ["studio"], "allow_unsigned": "yes"},
            ):
                self.ask(request)
                status = self.folder.read("status.json")
                self.assertEqual(status["state"], "error", request)
                self.assertIn("eh3d-update.signers", status["message"])
                self.assertIn("nothing was installed", status["message"])
                self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
            # a request that does not say it is a request that does not allow it
            (self.root / "meet" / "request.json").write_text(json.dumps({"action": "apply", "components": ["studio"]}), encoding="utf-8")
            H.handle(self.folder, config(self.root), self.gh.get, self.run)
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
        finally:
            H.SIGNERS, H.ALLOW_UNSIGNED = before

    def test_with_the_hash_alone_allowed_a_release_without_signers_is_installed_and_the_log_says_who_is_not_checked(self):
        before = (H.SIGNERS, H.ALLOW_UNSIGNED)
        H.SIGNERS, H.ALLOW_UNSIGNED = None, False
        try:
            studio = self._studio_with("old")
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
            status = self.folder.read("status.json")
            self.assertEqual(status["state"], "done")
            self.assertTrue(any("not who made it" in line and "no file of allowed signers" in line for line in status["log"]))
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")
        finally:
            H.SIGNERS, H.ALLOW_UNSIGNED = before

    def test_the_setting_of_the_helper_allows_the_hash_alone_without_the_request_saying_so(self):
        before = (H.SIGNERS, H.ALLOW_UNSIGNED)
        H.SIGNERS, H.ALLOW_UNSIGNED = None, True
        try:
            self._studio_with("old")
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": False})
            self.assertEqual(self.folder.read("status.json")["state"], "done")
        finally:
            H.SIGNERS, H.ALLOW_UNSIGNED = before

    def test_the_settings_of_the_config_file_and_the_option_turn_the_hash_alone_on(self):
        components, settings = H.split_settings({"server": {"repo": "A"}, "settings": {"require_signature": False}})
        self.assertEqual(list(components), ["server"])
        self.assertEqual(settings, {"require_signature": False})
        self.assertEqual(H.split_settings({"server": {}})[1], {})
        self.assertEqual(H.split_settings({"server": {}, "settings": "nonsense"})[1], {})
        before = (H.SIGNERS, H.ALLOW_UNSIGNED)
        try:
            meet = self.root / "meet2"
            cfg = self.root / "helper.json"
            for body, argv, expected in (
                (config(self.root), [], False),
                ({**config(self.root), "settings": {"require_signature": False}}, [], True),
                ({**config(self.root), "settings": {"require_signature": True}}, [], False),
                ({**config(self.root), "settings": {}}, ["--allow-unsigned"], True),
            ):
                cfg.write_text(json.dumps(body), encoding="utf-8")
                H.ALLOW_UNSIGNED = not expected
                self.assertEqual(H.main(["--dir", str(meet), "--config", str(cfg), *argv]), 0)
                self.assertEqual(H.ALLOW_UNSIGNED, expected, (body.get("settings"), argv))
        finally:
            H.SIGNERS, H.ALLOW_UNSIGNED = before

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_a_signature_that_is_published_must_verify_even_when_the_hash_alone_is_allowed(self):
        owner, thief = self._keys("owner"), self._keys("thief")
        signers = self.root / "signers"
        signers.write_text("eh3d-release " + (self.root / "owner.pub").read_text(encoding="utf-8"), encoding="utf-8")
        studio = self._studio_with("old")
        url = next(u for u in self.gh.files if "studio" in u and not u.endswith(".sha256"))
        name = url.rsplit("/", 1)[1]
        before = (H.SIGNERS, H.ALLOW_UNSIGNED)
        H.SIGNERS, H.ALLOW_UNSIGNED = str(signers), False
        try:
            # signers but no signature published: the hash alone, only because it was allowed
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
            status = self.folder.read("status.json")
            self.assertEqual(status["state"], "done")
            self.assertTrue(any("the release has no signature" in line for line in status["log"]))
            (studio / "index.html").write_text("old", encoding="utf-8")
            # a signature by somebody else is refused although the hash alone is allowed
            self.gh.files[url + ".sig"] = self._sign(thief, self.gh.files[url])
            self.gh.releases["EH3D-STUDIO"]["assets"].append({"name": name + ".sig", "browser_download_url": url + ".sig"})
            self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
            self.assertEqual(self.folder.read("status.json")["state"], "error")
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
        finally:
            H.SIGNERS, H.ALLOW_UNSIGNED = before

    # ---- the record of a release

    def _signed_release(self, component: str):
        """A release later than the last one without a record, its package signed by the owner of the machine; gives the key, the address and the name of the package."""
        repo = f"EH3D-{component.upper()}"
        major, minor, patch = (int(part) for part in H.LAST_WITHOUT_RECORD[repo].split("."))
        tag = f"v{major}.{minor}.{patch + 1}"
        self.gh = FakeGitHub(tag_server=tag) if component == "server" else FakeGitHub(tag_studio=tag)
        owner = self._keys("owner")
        signers = self.root / "signers"
        signers.write_text("eh3d-release " + (self.root / "owner.pub").read_text(encoding="utf-8"), encoding="utf-8")
        before = H.SIGNERS
        H.SIGNERS = str(signers)
        self.addCleanup(setattr, H, "SIGNERS", before)
        url = next(u for u in self.gh.files if component in u and u.endswith(".tar.gz"))
        self._publish(component, url + ".sig", self._sign(owner, self.gh.files[url]))
        return owner, url, url.rsplit("/", 1)[1], tag.lstrip("v")

    def _publish(self, component: str, url: str, data: bytes):
        repo = f"EH3D-{component.upper()}"
        name = url.rsplit("/", 1)[1]
        self.gh.files[url] = data
        assets = self.gh.releases[repo]["assets"]
        assets[:] = [a for a in assets if a["name"] != name] + [{"name": name, "browser_download_url": url}]

    def _record(self, component: str, key, package_url: str, release_version: str, sign=True, **change):
        """Publishes the record of the release (and its signature), with `change` over what is right."""
        name = package_url.rsplit("/", 1)[1]
        entry = {"name": name, "arch": f"linux-{H.arch()}" if component == "server" else "any", "size": len(self.gh.files[package_url]),
                 "sha256": hashlib.sha256(self.gh.files[package_url]).hexdigest()}
        entry.update({k: v for k, v in change.items() if k in entry})
        document = {"format": "eh3d-release", "format_version": 1, "repository": f"JuanenRac/EH3D-{component.upper()}", "version": release_version, "files": [entry]}
        document.update({k: v for k, v in change.items() if k in document})
        data = change.get("text", json.dumps(document).encode())
        url = package_url.rsplit("/", 1)[0] + f"/{component}/eh3d-release.json"
        self._publish(component, url, data)
        if sign:
            self._publish(component, url + ".sig", self._sign(key, data))

    def _drop(self, component: str, name: str):
        assets = self.gh.releases[f"EH3D-{component.upper()}"]["assets"]
        assets[:] = [a for a in assets if a["name"] != name]

    def _apply_signed(self, component: str = "studio") -> dict:
        self.ask({"action": "apply", "components": [component], "allow_unsigned": False})
        return self.folder.read("status.json")

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_a_later_version_is_installed_only_with_a_signed_record_that_agrees(self):
        owner, url, name, version = self._signed_release("studio")
        thief = self._keys("thief")
        studio = self._studio_with("old")

        def refused(said: str):
            status = self._apply_signed()
            self.assertEqual(status["state"], "error", status)
            self.assertIn(said, status["message"])
            self.assertIn("nothing was installed", status["message"])
            self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")

        # the package and its signature are right, and there is no record
        refused("has no eh3d-release.json")
        self._record("studio", owner, url, version, sign=False)
        refused("eh3d-release.json.sig")
        self._record("studio", thief, url, version)
        refused("allowed signers")
        for change, said in (
            ({"repository": "JuanenRac/EH3D-SERVER"}, "is the record of"),
            ({"version": "0.0.1"}, "the version"),
            ({"sha256": "0" * 64}, "is not the file"),
            ({"name": "another.tar.gz"}, "does not name"),
            ({"arch": "linux-another"}, "not for this machine"),
            ({"format": "something"}, "not a record"),
            ({"format_version": 2}, "not a record"),
            ({"files": "none"}, "not a record"),
            ({"text": b"[]"}, "not a record"),
            ({"text": b"not json"}, "not a record"),
            ({"text": b" " * (H.MAX_RECORD + 1)}, "bigger than"),
        ):
            self._record("studio", owner, url, version, **change)
            refused(said)
        # the signature of the record stays, and the record is taken away
        self._record("studio", owner, url, version)
        self._drop("studio", "eh3d-release.json")
        refused("has no eh3d-release.json")
        self._record("studio", owner, url, version)
        status = self._apply_signed()
        self.assertEqual(status["state"], "done", status)
        self.assertTrue(any("record of the release agrees" in line for line in status["log"]))
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_the_server_takes_the_record_of_its_architecture(self):
        owner, url, name, version = self._signed_release("server")
        self.assertEqual(self._apply_signed("server")["state"], "error")
        self.assertEqual(self.run.calls, [])
        self._record("server", owner, url, version)
        self.assertEqual(self._apply_signed("server")["state"], "done", self.folder.read("status.json"))
        self.assertEqual(self.run.calls, [["systemctl", "restart", "eh3d-server"]])

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_an_earlier_release_needs_no_record_and_one_that_publishes_it_must_agree(self):
        owner = self._keys("owner")
        signers = self.root / "signers"
        signers.write_text("eh3d-release " + (self.root / "owner.pub").read_text(encoding="utf-8"), encoding="utf-8")
        before = H.SIGNERS
        H.SIGNERS = str(signers)
        self.addCleanup(setattr, H, "SIGNERS", before)
        url = next(u for u in self.gh.files if "studio" in u and u.endswith(".tar.gz"))
        self._publish("studio", url + ".sig", self._sign(owner, self.gh.files[url]))
        studio = self._studio_with("old")
        # 0.0.3 is earlier than the last version without a record, but what it publishes must agree
        self._record("studio", owner, url, "0.0.3", sha256="0" * 64)
        self.assertEqual(self._apply_signed()["state"], "error")
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
        self._drop("studio", "eh3d-release.json")
        self.assertIn("has no eh3d-release.json", self._apply_signed()["message"])
        self._drop("studio", "eh3d-release.json.sig")
        self.assertEqual(self._apply_signed()["state"], "done", self.folder.read("status.json"))

    def test_which_releases_must_have_a_record(self):
        def release(version, *names):
            return {"latest": version, "assets": {n: "u" for n in names}}

        for repo, floor in H.LAST_WITHOUT_RECORD.items():
            self.assertFalse(H.record_expected(repo, release(floor)))
            self.assertFalse(H.record_expected(repo, release("0.0.1")))
            self.assertTrue(H.record_expected(repo, release(floor + ".1")))
            self.assertTrue(H.record_expected(repo, release("1.0.0")))
            self.assertTrue(H.record_expected(repo, release("0.0.1", "eh3d-release.json")))
            self.assertTrue(H.record_expected(repo, release("0.0.1", "eh3d-release.json.sig")))
        # a component of your own, whose releases this helper does not know: only what it publishes
        self.assertFalse(H.record_expected("MY-OWN", release("9.0.0")))
        self.assertTrue(H.record_expected("MY-OWN", release("9.0.0", "eh3d-release.json")))

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen is needed to make the keys of the test")
    def test_the_hash_alone_does_not_skip_a_record_of_a_signed_release(self):
        owner, url, name, version = self._signed_release("studio")
        studio = self._studio_with("old")
        # the file is signed and the record is missing: refused although the hash alone is allowed
        self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
        self.assertIn("has no eh3d-release.json", self.folder.read("status.json")["message"])
        # a record that is published must agree, also without signatures
        self._drop("studio", name + ".sig")
        self._record("studio", owner, url, version, sign=False, sha256="0" * 64)
        self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
        self.assertEqual(self.folder.read("status.json")["state"], "error")
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "old")
        # a release with no signature at all is installed by the hash alone, as before
        self._drop("studio", "eh3d-release.json")
        self.ask({"action": "apply", "components": ["studio"], "allow_unsigned": True})
        self.assertEqual(self.folder.read("status.json")["state"], "done", self.folder.read("status.json"))
        self.assertEqual((studio / "index.html").read_text(encoding="utf-8"), "<html>new</html>")

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

    def _replace_package(self, component: str, data: bytes):
        url = next(u for u in self.gh.files if component in u and u.endswith(".tar.gz"))
        self.gh.files[url] = data
        self.gh.files[url + ".sha256"] = (hashlib.sha256(data).hexdigest() + "  " + url.rsplit("/", 1)[1] + "\n").encode()
        return url

    def test_a_package_of_another_version_published_under_a_newer_one_is_not_installed(self):
        arch = H.arch()
        for component, data in (
            ("studio", tarball({"index.html": b"old", "version.json": b'{"version":"0.0.1"}'})),
            ("studio", tarball({"index.html": b"old"})),
            ("studio", tarball({"index.html": b"old", "version.json": b"[]"})),
            ("server", tarball({"eh3d-server": b"old"}, f"eh3d-server-0.0.1-linux-{arch}")),
            ("server", tarball({"eh3d-server": b"old"})),
        ):
            target = self.root / "opt" / f"eh3d-{component}"
            shutil.rmtree(target, ignore_errors=True)
            target.mkdir(parents=True)
            (target / "kept").write_text("kept", encoding="utf-8")
            self._replace_package(component, data)
            self.ask({"action": "apply", "components": [component]})
            status = self.folder.read("status.json")
            self.assertEqual(status["state"], "error", status)
            self.assertIn("nothing was installed", status["message"])
            self.assertEqual([p.name for p in target.iterdir()], ["kept"])
        self.assertEqual(self.run.calls, [])

    def test_the_layout_of_the_published_releases_is_accepted(self):
        # the server: one folder named like the file; the pages: their files and a version.json at the top, written with "./" before each name
        stage = self.root / "stage-server"
        (stage / "eh3d-server-0.2.4-linux-aarch64").mkdir(parents=True)
        H.check_declared_version("server", stage, "eh3d-server-0.2.4-linux-aarch64.tar.gz", "0.2.4")
        with self.assertRaises(RuntimeError):
            H.check_declared_version("server", stage, "eh3d-server-0.2.5-linux-aarch64.tar.gz", "0.2.5")
        pages = self.root / "stage-studio"
        pages.mkdir()
        (pages / "version.json").write_text('{"name":"eh3d-studio","version":"0.2.0"}\n', encoding="utf-8")
        (pages / "assets").mkdir()
        H.check_declared_version("studio", pages, "eh3d-studio-0.2.0.tar.gz", "0.2.0")
        with self.assertRaises(RuntimeError):
            H.check_declared_version("studio", pages, "eh3d-studio-0.3.0.tar.gz", "0.3.0")

    def test_a_release_whose_tag_is_not_a_version_is_not_installed(self):
        self.gh.releases["EH3D-STUDIO"]["tag_name"] = "latest"
        self.ask({"action": "apply", "components": ["studio"]})
        self.assertIn("not named by a version", self.folder.read("status.json")["message"])
        self.assertFalse((self.root / "opt" / "eh3d-studio").exists())

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
