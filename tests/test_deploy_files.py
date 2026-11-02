# EH3D-SERVER - tests/test_deploy_files.py: the files of the deployment (systemd units, the Caddy example) say what the guide says they do.    python -I tests/test_deploy_files.py
import importlib.util
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEPLOY = ROOT / "deploy"


def settings(path: Path) -> dict:
    """The `Key=value` lines of a unit, by key (a key that repeats keeps the last)."""
    found = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#") and not line.startswith("[") and "=" in line:
            key, value = line.split("=", 1)
            found[key.strip()] = value.strip()
    return found


class ServerUnit(unittest.TestCase):
    def setUp(self):
        self.unit = settings(DEPLOY / "eh3d-server.service")

    def test_it_stops_on_sigterm_and_gives_the_server_time_to_stop_the_machines(self):
        self.assertEqual(self.unit["KillSignal"], "SIGTERM")
        self.assertGreaterEqual(int(self.unit["TimeoutStopSec"]), 10)
        self.assertEqual(self.unit["Restart"], "on-failure")

    def test_it_is_hardened_but_can_use_the_serial_ports(self):
        self.assertEqual(self.unit["UMask"], "0077")
        self.assertIn("dialout", self.unit["SupplementaryGroups"].split())
        for key in ("NoNewPrivileges", "PrivateTmp", "ProtectHome", "ProtectKernelTunables", "ProtectControlGroups", "RestrictSUIDSGID", "LockPersonality"):
            self.assertEqual(self.unit[key], "true", key)
        self.assertEqual(self.unit["ProtectSystem"], "strict")
        # the controllers are devices: they would disappear
        self.assertNotIn("PrivateDevices", self.unit)
        self.assertEqual(set(self.unit["RestrictAddressFamilies"].split()), {"AF_INET", "AF_INET6", "AF_UNIX"})

    def test_it_writes_only_where_the_guide_says(self):
        self.assertEqual(set(self.unit["ReadWritePaths"].split()), {"/var/lib/eh3d-server", "/var/lib/eh3d-updates"})


class BackupUnits(unittest.TestCase):
    def test_the_service_reads_the_data_and_writes_only_the_copies(self):
        unit = settings(DEPLOY / "eh3d-backup.service")
        self.assertEqual(unit["Type"], "oneshot")
        self.assertEqual(unit["ReadOnlyPaths"], "/var/lib/eh3d-server")
        self.assertEqual(unit["ReadWritePaths"], "/var/backups/eh3d-server")
        self.assertEqual(unit["UMask"], "0077")
        self.assertIn("eh3d-backup.py", unit["ExecStart"])

    def test_the_timer_runs_every_day_and_catches_up(self):
        timer = settings(DEPLOY / "eh3d-backup.timer")
        self.assertIn("*-*-*", timer["OnCalendar"])
        self.assertEqual(timer["Persistent"], "true")
        self.assertEqual(timer["Unit"], "eh3d-backup.service")

    def test_the_example_settings_are_the_defaults_of_the_script(self):
        spec = importlib.util.spec_from_file_location("b", DEPLOY / "eh3d-backup.py")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        got = mod.read_config(DEPLOY / "backup.cfg.example")
        default = mod.read_config(None)
        self.assertEqual(got, default)


class Caddy(unittest.TestCase):
    def test_the_proxy_points_at_the_port_the_server_listens_on_by_default(self):
        text = (DEPLOY / "Caddyfile.example").read_text(encoding="utf-8")
        self.assertIn("tls internal", text)
        self.assertRegex(text, r"reverse_proxy 127\.0\.0\.1:18200")
        config = (ROOT / "src" / "config.rs").read_text(encoding="utf-8")
        self.assertIn('"0.0.0.0:18200"', config)

    def test_the_guide_tells_the_server_to_listen_only_here(self):
        guide = (ROOT / "docs" / "DEPLOY.md").read_text(encoding="utf-8")
        self.assertRegex(guide, r"listen = 127\.0\.0\.1:18200")
        self.assertIn("login_ip_limit", guide)


if __name__ == "__main__":
    unittest.main()
