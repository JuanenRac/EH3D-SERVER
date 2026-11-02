# EH3D-SERVER - tests/test_backup.py: the backup of the data folder (deploy/eh3d-backup.py).    python -I tests/test_backup.py
import importlib.util
import os
import stat
import tarfile
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("eh3d_backup", Path(__file__).resolve().parent.parent / "deploy" / "eh3d-backup.py")
B = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(B)


class Backup(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.source = self.root / "data"
        (self.source / "requests").mkdir(parents=True)
        (self.source / "users.json").write_text('{"users": []}')
        (self.source / "requests" / "j1.json").write_text("{}")
        (self.source / "jobs.json.tmp").write_text("half")
        self.dest = self.root / "copies"
        self.dest.mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def test_the_copy_holds_the_data_and_leaves_out_the_files_being_written(self):
        made = B.make_backup(self.source, self.dest)
        self.assertTrue(B.NAME.match(made.name))
        with tarfile.open(made) as tar:
            names = sorted(tar.getnames())
        self.assertIn("data/users.json", names)
        self.assertIn("data/requests/j1.json", names)
        self.assertNotIn("data/jobs.json.tmp", names)
        self.assertEqual([p.name for p in self.dest.iterdir()], [made.name], "no .part is left")

    @unittest.skipIf(os.name == "nt", "file modes are those of POSIX")
    def test_the_copy_is_readable_by_its_owner_only(self):
        made = B.make_backup(self.source, self.dest)
        self.assertEqual(stat.S_IMODE(made.stat().st_mode), 0o600)

    def test_two_runs_in_the_same_second_do_not_overwrite_each_other(self):
        first = B.make_backup(self.source, self.dest, now=1_000_000)
        second = B.make_backup(self.source, self.dest, now=1_000_000)
        self.assertNotEqual(first, second)
        self.assertTrue(first.exists() and second.exists())

    def test_only_the_newest_copies_are_kept_and_other_files_are_left_alone(self):
        (self.dest / "notes.txt").write_text("mine")
        for i in range(5):
            B.make_backup(self.source, self.dest, now=1_000_000 + i * 100)
        removed = B.prune(self.dest, 3)
        self.assertEqual(len(removed), 2)
        left = sorted(p.name for p in self.dest.iterdir() if B.NAME.match(p.name))
        self.assertEqual(len(left), 3)
        self.assertEqual(removed, sorted(removed))
        self.assertTrue(all(r < left[0] for r in removed), "the oldest go")
        self.assertTrue((self.dest / "notes.txt").exists())

    def test_a_missing_folder_is_an_error_and_leaves_nothing(self):
        with self.assertRaises(FileNotFoundError):
            B.make_backup(self.root / "nowhere", self.dest)
        with self.assertRaises(FileNotFoundError):
            B.make_backup(self.source, self.root / "nowhere")
        self.assertEqual(list(self.dest.iterdir()), [])

    def test_the_settings_are_read_and_mistakes_are_errors(self):
        cfg = self.root / "backup.cfg"
        cfg.write_text("# a comment\nsource = /a\ndest = /b\nkeep = 3\n")
        got = B.read_config(cfg)
        self.assertEqual((got["source"], got["dest"], got["keep"]), (Path("/a"), Path("/b"), 3))
        self.assertEqual(B.read_config(None)["keep"], 14)
        for bad in ("keep = 0", "keep = many", "keep = 99999", "colour = red", "just words", "dest ="):
            cfg.write_text(bad + "\n")
            with self.assertRaises(ValueError, msg=bad):
                B.read_config(cfg)

    def test_main_makes_a_copy_and_prunes_and_reports_errors_with_a_status(self):
        cfg = self.root / "backup.cfg"
        cfg.write_text(f"source = {self.source}\ndest = {self.dest}\nkeep = 1\n")
        self.assertEqual(B.main(["--config", str(cfg)]), 0)
        self.assertEqual(B.main(["--config", str(cfg), "--dry-run"]), 0)
        cfg.write_text(f"source = {self.root / 'nowhere'}\ndest = {self.dest}\n")
        self.assertEqual(B.main(["--config", str(cfg)]), 1)


if __name__ == "__main__":
    unittest.main()
