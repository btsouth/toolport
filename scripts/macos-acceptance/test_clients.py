#!/usr/bin/env python3
"""Tests for clients.py. Run: python3 scripts/macos-acceptance/test_clients.py"""

import json
import os
import stat
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import clients  # noqa: E402


def statuses(rows, needle):
    return [r.split("\t")[1] for r in rows if needle in r]


class ClientsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = os.path.join(self.tmp.name, "home")
        self.manifest = os.path.join(self.tmp.name, "ev", "manifest.json")
        self.entries = os.path.join(self.tmp.name, "ev", "entries")
        os.makedirs(os.path.dirname(self.manifest))
        clients.seed(self.home, self.manifest)
        self.gw = os.path.join(self.tmp.name, "toolport-gateway")
        with open(self.gw, "w") as f:
            f.write("#!/bin/sh\n")
        os.chmod(self.gw, stat.S_IRWXU)

    def tearDown(self):
        self.tmp.cleanup()

    def path(self, cid):
        return os.path.join(self.home, clients.BY_ID[cid]["path"])

    def edit_json(self, cid, fn):
        p = self.path(cid)
        with open(p) as f:
            data = json.load(f)
        fn(data)
        with open(p, "w") as f:
            json.dump(data, f, indent=2)

    def test_seed_writes_every_client_and_markers(self):
        for c in clients.CLIENTS:
            self.assertTrue(os.path.exists(self.path(c["id"])), c["id"])
        self.assertTrue(os.path.isdir(os.path.join(self.home, ".claude")))
        rows = clients.compare(self.home, self.manifest, "restored")
        self.assertEqual(set(statuses(rows, "restored")), {"PASS"})

    def test_unconnected_clients_fail_check(self):
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertIn("FAIL", statuses(rows, "Cursor has a Toolport entry"))
        rows = clients.check(self.home, self.manifest, self.entries, expect_connected=False)
        self.assertEqual(statuses(rows, "Cursor has a Toolport entry"), ["INFO"])

    def test_standard_entry_passes_and_writes_call_entry(self):
        self.edit_json(
            "cursor",
            lambda d: d["mcpServers"].update(
                toolport={"command": self.gw, "args": [], "env": {"TOOLPORT_CLIENT_ID": "cursor"}}
            ),
        )
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertEqual(statuses(rows, "Cursor entry matches"), ["PASS"])
        self.assertEqual(statuses(rows, "Cursor keeps the user's existing server"), ["PASS"])
        with open(os.path.join(self.entries, "cursor.json")) as f:
            self.assertEqual(json.load(f)["command"], self.gw)

    def test_vscode_requires_stdio_type(self):
        entry = {"command": self.gw, "args": [], "env": {"TOOLPORT_CLIENT_ID": "vscode"}}
        self.edit_json("vscode", lambda d: d["servers"].update(toolport=dict(entry)))
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertEqual(statuses(rows, "VS Code entry matches"), ["FAIL"])
        self.edit_json("vscode", lambda d: d["servers"].update(toolport={"type": "stdio", **entry}))
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertEqual(statuses(rows, "VS Code entry matches"), ["PASS"])

    def test_wrong_client_id_relative_command_and_lost_server(self):
        def change(d):
            d["mcpServers"] = {"toolport": {"command": "toolport-gateway", "args": [], "env": {"TOOLPORT_CLIENT_ID": "x"}}}

        self.edit_json("claude-desktop", change)
        rows = clients.check(self.home, self.manifest, self.entries)
        detail = [r for r in rows if "Claude Desktop entry matches" in r][0]
        self.assertIn("\tFAIL\t", detail)
        self.assertIn("not an absolute path", detail)
        self.assertIn("TOOLPORT_CLIENT_ID", detail)
        self.assertEqual(statuses(rows, "Claude Desktop keeps the user's existing server"), ["FAIL"])

    def test_opencode_codex_zed_formats(self):
        self.edit_json(
            "opencode",
            lambda d: d["mcp"].update(
                toolport={"type": "local", "command": [self.gw], "environment": {"TOOLPORT_CLIENT_ID": "opencode"}, "enabled": True}
            ),
        )
        with open(self.path("codex"), "a") as f:
            f.write(f'\n[mcp_servers.toolport]\ncommand = "{self.gw}"\n\n[mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = "codex"\n')
        with open(self.path("zed")) as f:
            text = f.read()
        text = text.replace(
            '"context_servers": {',
            '"context_servers": {\n    // added\n    "toolport": {"command": "%s", "args": [], "env": {"TOOLPORT_CLIENT_ID": "zed"},},' % self.gw,
        )
        with open(self.path("zed"), "w") as f:
            f.write(text)
        rows = clients.check(self.home, self.manifest, self.entries)
        for name in ("OpenCode", "Codex", "Zed"):
            self.assertEqual(statuses(rows, f"{name} entry matches"), ["PASS"], name)
            self.assertEqual(statuses(rows, f"{name} keeps the user's existing server"), ["PASS"], name)

    def test_snapshot_and_unchanged(self):
        self.edit_json("cursor", lambda d: d["mcpServers"].update(toolport={"command": self.gw}))
        snap = os.path.join(self.tmp.name, "ev", "snap.json")
        clients.snapshot(self.manifest, snap)
        rows = clients.compare(self.home, snap, "untouched")
        self.assertEqual(set(statuses(rows, "untouched")), {"PASS"})
        self.edit_json("cursor", lambda d: d.update(extra=1))
        rows = clients.compare(self.home, snap, "untouched")
        self.assertEqual(statuses(rows, "Cursor untouched"), ["FAIL"])

    def test_strip_jsonc_keeps_strings(self):
        text = '{"a": "http://x//y", /* c */ "b": [1, 2,], // tail\n}'
        self.assertEqual(json.loads(clients.strip_jsonc(text)), {"a": "http://x//y", "b": [1, 2]})


if __name__ == "__main__":
    unittest.main()
