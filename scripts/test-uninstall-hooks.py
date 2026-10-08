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
            "stat": f'''path=""; for arg do path=$arg; done
test -d "$path" || {{ echo 'No such file or directory' >&2; exit 1; }}
while IFS=: read -r user password uid gid gecos home shell; do
  case "$path" in "$home"/.config/*) printf '%s:directory\\n' "$uid"; exit 0 ;; esac
done < "{self.passwd}"
exit 1''',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\n" + body + "\n")
            path.chmod(0o755)
        self.helper = self.root / "helper"
        self.helper.write_text((NATIVE / "disconnect-users.sh").read_text().replace("/usr/bin/toolport-gateway", str(self.gateway)).replace("PATH=/usr/sbin:/usr/bin:/sbin:/bin", f"PATH={self.bin}:/usr/bin:/bin"))
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

    def test_su_fallback_has_no_login_shell_and_handles_spaces(self):
        self.accounts(["alice"])
        spaced = self.root / "alice home"
        (spaced / ".config/Toolport").mkdir(parents=True)
        self.passwd.write_text(self.passwd.read_text().replace(str(self.root / "alice") + ":", str(spaced) + ":"))
        # Model su's shell argument forwarding, without switching a real account.
        su = self.bin / "su"
        su.write_text('#!/bin/sh\ntest "$1" = -s && test "$2" = /bin/sh && test "$3" = -c && test "$5" = -- || exit 99\ncmd=$4\nshift 6\nexec /bin/sh -c "$cmd" "$@"\n')
        su.chmod(0o755)
        self.helper.write_text(self.helper.read_text().replace("command -v runuser >/dev/null 2>&1", "false"))
        self.run_script(self.helper)
        self.assertIn("alice|unset|--disconnect-all", self.calls.read_text())

    def test_no_data_directory_does_nothing(self):
        self.accounts(["empty"])
        self.run_script(self.helper)
        self.assertFalse(self.calls.exists())

    def test_account_enumeration_failure_prints_recovery(self):
        self.accounts(["empty"])
        (self.bin / "getent").write_text("#!/bin/sh\nexit 2\n")
        self.assertIn("could not enumerate", self.run_script(self.helper).stderr)

    def test_timeouts_cover_account_lookup_and_user_switching(self):
        self.accounts(["alice"])
        (self.root / "alice/.config/Toolport").mkdir(parents=True)
        marker = self.root / "timeout-calls"
        timeout = self.bin / "timeout"
        timeout.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{marker}"\ntest "$1" = -k && test "$2" = 2 || exit 99\nshift 3\nexec "$@"\n')
        timeout.chmod(0o755)
        self.run_script(self.helper)
        calls = marker.read_text().splitlines()
        self.assertEqual(calls[0], "-k 2 5 getent passwd")
        self.assertTrue(calls[1].startswith("-k 2 5 stat -L -c %u:%F -- "))
        self.assertTrue(calls[2].startswith("-k 2 30 runuser -u alice -- env -i "))

    def test_foreign_owned_state_and_shared_system_homes_are_skipped(self):
        self.accounts(["alice", "system"])
        (self.root / "alice/.config/Toolport").mkdir(parents=True)
        self.passwd.write_text(self.passwd.read_text().replace(str(self.root / "system") + ":", str(self.root / "alice") + ":"))
        self.run_script(self.helper)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        self.assertIn("alice|unset", self.calls.read_text())
        self.calls.unlink()
        (self.bin / "stat").write_text('#!/bin/sh\necho 9999:directory\n')
        self.run_script(self.helper)
        self.assertFalse(self.calls.exists())

    def test_timeout_and_root_squash_are_logged_without_blocking_removal(self):
        self.accounts(["alice"])
        for message, status in [("Permission denied", 1), ("", 124)]:
            (self.bin / "stat").write_text(f'#!/bin/sh\necho "{message}" >&2\nexit {status}\n')
            result = self.run_script(self.helper)
            self.assertIn("root_squashed", result.stderr)
            self.assertIn(f"inspection failed ({status})", result.stderr)
            self.assertFalse(self.calls.exists())

    def test_cleanup_cannot_consume_the_next_account_from_stdin(self):
        self.accounts(["alice", "bob"])
        for name in ["alice", "bob"]:
            (self.root / f"{name}/.config/Toolport").mkdir(parents=True)
        self.gateway.write_text('#!/bin/sh\nif read -r line; then exit 99; fi\nprintf "%s\\n" "$USER" >> "' + str(self.calls) + '"\n')
        self.run_script(self.helper)
        self.assertEqual(self.calls.read_text().splitlines(), ["alice", "bob"])

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
            "deb": [("upgrade", "2.0"), ("deconfigure",), ("failed-upgrade",), ("purge",), ("remove", "in-favour", "toolport-bin"), (), ("remove",)],
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

    def test_nsis_recovers_updates_and_checks_running_app_before_cleanup(self):
        hooks = (ROOT / "src-tauri/nsis-hooks.nsh").read_text()
        preinstall = hooks.split("!macro NSIS_HOOK_PREINSTALL", 1)[1].split("!macroend", 1)[0]
        self.assertIn("/TIMEOUT=60000", preinstall)
        self.assertIn('StrCpy $1 "Toolport could not finish', preinstall)
        recovery = preinstall.split("${If} $UpdateMode = 1", 1)[1].split("${EndIf}", 1)[0]
        self.assertIn("${OrIf} $PassiveMode = 1", recovery)
        self.assertLess(recovery.index("Exec '"), recovery.index("IfSilent"))
        self.assertIn("MessageBox MB_OK", recovery)
        uninstall = hooks.split("!macro NSIS_HOOK_PREUNINSTALL", 1)[1].split("!macroend", 1)[0]
        self.assertIn('!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"', uninstall)
        self.assertLess(uninstall.index("!insertmacro CheckIfAppIsRunning"), uninstall.index("--disconnect-all"))


if __name__ == "__main__":
    unittest.main()
