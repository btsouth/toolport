#!/usr/bin/env python3
"""Maintainer-script contracts with synthetic accounts and no host mutations."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
NATIVE = ROOT / "packaging/linux/native"


class RemovalHooks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="toolport-removal-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.calls = self.root / "calls"
        self.gateway = self.bin / "gateway"
        self.gateway.write_text('#!/bin/sh\nprintf "%s|%s|%s|%s\\n" "$HOME" "$USER" "${TOOLPORT_DATA_DIR-unset}" "$*" >> "' + str(self.calls) + '"\n[ "$USER" != bob ]\n')
        self.gateway.chmod(0o755)
        self.passwd = self.root / "passwd"
        for name, body in {
            "getent": f'cat "{self.passwd}"',
            "runuser": 'test "$1" = -u && test "$3" = -- || exit 99\nshift 3\nexec "$@"',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\n" + body + "\n")
            path.chmod(0o755)
        self.helper = self.root / "helper"
        self.helper.write_text((NATIVE / "disconnect-users.sh").read_text().replace("/usr/bin/toolport-gateway", str(self.gateway)))
        self.helper.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", TOOLPORT_DATA_DIR="/root/never-use-this")

    def accounts(self, names):
        lines = []
        for index, name in enumerate(names):
            home = self.root / name
            home.mkdir()
            lines.append(f"{name}:x:{1000 + index}:1000:Test:{home}:/bin/false")
        lines.append(f"root:x:0:0:Root:{self.root}/root:/bin/sh")
        self.passwd.write_text("\n".join(lines) + "\n")

    def run_script(self, script, *args):
        result = subprocess.run(["sh", str(script), *args], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_multiple_users_legacy_failure_continues_and_environment_is_clean(self):
        self.accounts(["alice", "bob", "empty"])
        (self.root / "alice/.config/Toolport").mkdir(parents=True)
        (self.root / "bob/.config/Conduit").mkdir(parents=True)
        result = self.run_script(self.helper)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 2)
        self.assertTrue(all("|unset|--disconnect-all" in line for line in calls))
        self.assertIn("alice|unset", calls[0])
        self.assertIn("bob|unset", calls[1])
        self.assertIn("cleanup failed for bob", result.stderr)
        self.assertIn("Removal will continue", result.stderr)

    def test_no_data_directory_does_nothing(self):
        self.accounts(["empty"])
        self.run_script(self.helper)
        self.assertFalse(self.calls.exists())

    def test_missing_binary_prints_recovery_and_succeeds(self):
        self.accounts(["empty"])
        self.gateway.unlink()
        result = self.run_script(self.helper)
        self.assertIn("gateway binary is missing", result.stderr)
        self.assertIn("--disconnect-all", result.stderr)

    def test_deb_and_rpm_only_real_removal_calls_helper(self):
        marker = self.root / "wrapper-calls"
        self.helper.write_text(f'#!/bin/sh\necho called >> "{marker}"\nexit 9\n')
        for format, actions in {
            "deb": [("upgrade", "2.0"), ("deconfigure",), ("failed-upgrade",), ("purge",), (), ("remove",)],
            "rpm": [("1",), ("2",), (), ("0",)],
        }.items():
            script = self.root / format
            script.write_text((NATIVE / f"preremove-{format}.sh").read_text().replace("/usr/share/toolport/disconnect-users.sh", str(self.helper)))
            for action in actions[:-1]:
                self.run_script(script, *action)
                self.assertFalse(marker.exists())
            self.run_script(script, *actions[-1])
            self.assertEqual(marker.read_text(), "called\n")
            marker.unlink()
            self.helper.chmod(0o644)
            self.assertIn("helper missing", self.run_script(script, *actions[-1]).stderr)
            self.helper.chmod(0o755)

    def test_arch_pre_remove_only_and_failure_is_nonblocking(self):
        marker = self.root / "arch-calls"
        self.helper.write_text(f'#!/bin/sh\necho called >> "{marker}"\nexit 9\n')
        script = self.root / "arch"
        script.write_text((NATIVE / "toolport.install").read_text().replace("/usr/share/toolport/disconnect-users.sh", str(self.helper)) + '\npre_remove 2.0.0\n')
        self.run_script(script)
        self.assertEqual(marker.read_text(), "called\n")
        self.assertNotIn("pre_upgrade", script.read_text())


if __name__ == "__main__":
    unittest.main()
