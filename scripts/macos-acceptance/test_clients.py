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
        names = ["OpenCode", "Zed"] + (["Codex"] if clients.tomllib else [])
        for name in names:
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

    def snapshot_with(self, cid, entry):
        """Connect cid with entry as the previous release wrote it, then snapshot."""
        table = clients.USER_TABLES[clients.BY_ID[cid]["format"]]
        self.edit_json(cid, lambda d: d.setdefault(table, {}).update(toolport=dict(entry)))
        snap = os.path.join(self.tmp.name, "ev", "snap.json")
        clients.snapshot(self.manifest, snap)
        return snap, table

    def test_the_2_3_7_repairs_pass_and_nothing_else_does(self):
        old = {"command": self.gw, "env": {"TOOLPORT_CLIENT_ID": "cursor"}}
        for cid, kind in [("cursor", "stdio"), ("vscode", "stdio"), ("github-copilot-cli", "local")]:
            name = clients.BY_ID[cid]["name"]
            snap, table = self.snapshot_with(cid, old)
            self.edit_json(cid, lambda d: d[table]["toolport"].update(type=kind))
            rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
            self.assertEqual(statuses(rows, f"{name} untouched"), ["PASS"], cid)
            self.assertIn("repaired", [r for r in rows if name in r][0])
            # Not when byte-exact is asked for.
            rows = clients.compare(self.home, snap, "restored")
            self.assertEqual(statuses(rows, f"{name} restored"), ["FAIL"], cid)
            # Not another type, and not the repair plus anything else.
            for change in (
                lambda d: d[table]["toolport"].update(type="sse"),
                lambda d: d[table]["toolport"].update(args=["--extra"]),
                lambda d: d[table].pop("filesystem", None) or d.update(extra=1),
            ):
                self.edit_json(cid, lambda d: d[table]["toolport"].update(type=kind))
                self.edit_json(cid, change)
                rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
                self.assertEqual(statuses(rows, f"{name} untouched"), ["FAIL"], cid)
            self.edit_json(cid, lambda d: d[table].update(toolport=dict(old)))
        # A client 2.3.7 does not add a type to.
        snap, table = self.snapshot_with("claude-code", old)
        self.edit_json("claude-code", lambda d: d[table]["toolport"].update(type="stdio"))
        rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
        self.assertEqual(statuses(rows, "Claude Code untouched"), ["FAIL"])

    def test_a_canonical_gateway_path_passes(self):
        bundle = os.path.join(self.tmp.name, "Toolport.app", "Contents")
        os.makedirs(os.path.join(bundle, "MacOS"))
        canonical = os.path.join(bundle, "Helpers", "toolport-gateway")
        old = {"command": os.path.join(bundle, "MacOS", "..", "Helpers", "toolport-gateway")}
        snap, table = self.snapshot_with("claude-code", old)
        self.edit_json("claude-code", lambda d: d[table]["toolport"].update(command=canonical))
        rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
        self.assertEqual(statuses(rows, "Claude Code untouched"), ["PASS"])
        # Another path is not a repair.
        self.edit_json("claude-code", lambda d: d[table]["toolport"].update(command=canonical + "-2"))
        rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
        self.assertEqual(statuses(rows, "Claude Code untouched"), ["FAIL"])
        if clients.tomllib:
            codex = self.path("codex")
            with open(codex, "a") as f:
                f.write(f'\n[mcp_servers.toolport]\ncommand = "{old["command"]}"\n')
            snap = os.path.join(self.tmp.name, "ev", "snap2.json")
            clients.snapshot(self.manifest, snap)
            with open(codex) as f:
                text = f.read()
            with open(codex, "w") as f:
                f.write(text.replace(old["command"], canonical))
            rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
            self.assertEqual(statuses(rows, "Codex untouched"), ["PASS"])

    def test_the_repairs_seen_on_a_real_2_3_7_upgrade_pass(self):
        # From macOS acceptance run 44026014012: Cursor got only its type and kept
        # the old path; Codex got the canonical path and its gateway settings.
        bundle = os.path.join(self.tmp.name, "Toolport.app", "Contents")
        os.makedirs(os.path.join(bundle, "MacOS"))
        canonical = os.path.join(bundle, "Helpers", "toolport-gateway")
        old_path = os.path.join(bundle, "MacOS", "..", "Helpers", "toolport-gateway")
        old = {"args": [], "command": old_path, "env": {"TOOLPORT_CLIENT_ID": "cursor"}}
        snap, table = self.snapshot_with("cursor", old)
        self.edit_json("cursor", lambda d: d[table]["toolport"].update(type="stdio"))
        rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
        self.assertEqual(statuses(rows, "Cursor untouched"), ["PASS"])
        self.edit_json("cursor", lambda d: d[table]["toolport"].update(command=canonical))
        rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
        self.assertEqual(statuses(rows, "Cursor untouched"), ["PASS"])
        if not clients.tomllib:
            return
        codex = self.path("codex")
        head = 'model = "gpt-5-codex" # "#" in a string is not one\n\n# my servers\n[mcp_servers.toolport]\n'
        env = '\n[mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = "codex"\n'

        def write(text):
            with open(codex, "w") as f:
                f.write(text)

        def codex_status(text):
            write(text)
            rows = clients.compare(self.home, snap, "untouched", allow_upgrade_repairs=True)
            return statuses(rows, "Codex untouched")

        settings = "supports_parallel_tool_calls = true\nstartup_timeout_sec = 75\ntool_timeout_sec = 600\n"
        write(f'{head}command = "{old_path}"\n{env}')
        snap = os.path.join(self.tmp.name, "ev", "snap-codex.json")
        clients.snapshot(self.manifest, snap)
        self.assertEqual(codex_status(f'{head}command = "{canonical}"\n{settings}{env}'), ["PASS"])
        # As seen on the real upgrade: Toolport's attribution id is added to the env.
        self.assertEqual(
            codex_status(f'{head}command = "{canonical}"\n{settings}{env}TOOLPORT_ATTRIBUTION_ID = "codex"\n'),
            ["PASS"],
        )
        for other in ('TOOLPORT_ATTRIBUTION_ID = "cursor"\n', 'OTHER = "codex"\n'):
            self.assertEqual(codex_status(f'{head}command = "{canonical}"\n{settings}{env}{other}'), ["FAIL"], other)
        self.assertEqual(codex_status(f'{head}command = "{old_path}"\n{settings}{env}'), ["PASS"])
        self.assertEqual(
            codex_status(f'{head}command = "{old_path}"\nsupports_parallel_tool_calls = true\n{env}'), ["PASS"]
        )
        for wrong in (
            settings.replace("600", "300"),
            settings.replace("= true", '= "true"'),
            settings + "startup_timeout_ms = 1000\n",
            settings + "enabled = false\n",
        ):
            self.assertEqual(codex_status(f'{head}command = "{canonical}"\n{wrong}{env}'), ["FAIL"], wrong)
            with open(os.path.join(self.tmp.name, "ev", "snap-codex-changed", "codex")) as f:
                self.assertIn(wrong, f.read())
        # A repair does not cover a removed or changed comment.
        repaired = f'{head}command = "{canonical}"\n{settings}{env}'
        for text in (
            repaired.replace("# my servers\n", ""),
            repaired.replace("# my servers", "# servers"),
            repaired.replace("[mcp_servers.toolport]\n", "[mcp_servers.toolport]  # added\n"),
        ):
            self.assertEqual(codex_status(text), ["FAIL"], text)
        # A setting the user already had, under its alias, is not added to.
        write(f'{head}command = "{old_path}"\nstartup_timeout_ms = 5000\n{env}')
        clients.snapshot(self.manifest, snap)
        self.assertEqual(
            codex_status(f'{head}command = "{old_path}"\nstartup_timeout_ms = 5000\nstartup_timeout_sec = 75\n{env}'),
            ["FAIL"],
        )

    def cursor_connected(self, user=None):
        def change(d):
            d["mcpServers"]["toolport"] = {"command": self.gw, "args": [], "env": {"TOOLPORT_CLIENT_ID": "cursor"}}
            if user is False:
                del d["mcpServers"]["filesystem"]
            elif user is not None:
                d["mcpServers"]["filesystem"] = user

        self.edit_json("cursor", change)

    def registry(self, server):
        p = os.path.join(self.tmp.name, "registry.json")
        with open(p, "w") as f:
            json.dump({"servers": [server]}, f)
        return p

    def keeps(self, registry=None):
        rows = clients.check(self.home, self.manifest, self.entries, registry=registry)
        return statuses(rows, "Cursor keeps the user's existing server")

    def test_changed_user_server_fails(self):
        changed = dict(clients.USER_SERVER, args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
        self.cursor_connected(user=changed)
        self.assertEqual(self.keeps(), ["FAIL"])
        self.cursor_connected(user=dict(clients.USER_SERVER, env={"X": "1"}))
        self.assertEqual(self.keeps(), ["FAIL"])

    def test_registry_copy_must_be_unchanged(self):
        self.cursor_connected(user=False)
        self.assertEqual(self.keeps(), ["FAIL"])
        same = {"id": "fs", "command": "npx", "args": list(clients.USER_SERVER["args"]), "transport": "stdio"}
        self.assertEqual(self.keeps(self.registry(same)), ["PASS"])
        other_arg = dict(same, args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
        self.assertEqual(self.keeps(self.registry(other_arg)), ["FAIL"])
        self.assertEqual(self.keeps(self.registry(dict(same, env=[{"key": "A", "value": "1"}]))), ["FAIL"])
        self.assertEqual(self.keeps(self.registry(dict(same, command="node"))), ["FAIL"])

    def test_changed_in_client_and_copied_to_registry_fails(self):
        same = {"id": "fs", "command": "npx", "args": list(clients.USER_SERVER["args"]), "transport": "stdio"}
        self.cursor_connected(user=dict(clients.USER_SERVER, args=["-y"]))
        self.assertEqual(self.keeps(self.registry(same)), ["FAIL"])

    def test_malformed_entry_is_a_fail_row_not_a_crash(self):
        self.edit_json("cursor", lambda d: d.update(mcpServers={"toolport": [], "filesystem": 3}))
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertEqual(statuses(rows, "Cursor config has the client's documented shape"), ["FAIL"])
        self.assertTrue(statuses(rows, "Windsurf") or statuses(rows, "Devin"), "later clients are still checked")

    def test_goose_wrong_location_is_named(self):
        wrong = os.path.join(self.home, "Library/Application Support/Block/goose/config.yaml")
        os.makedirs(os.path.dirname(wrong))
        with open(wrong, "w") as f:
            f.write("extensions:\n  toolport:\n    type: stdio\n")
        rows = clients.check(self.home, self.manifest, self.entries)
        detail = [r for r in rows if "Goose has a Toolport entry" in r][0]
        self.assertIn("\tFAIL\t", detail)
        self.assertIn("never reads", detail)

    def test_previous_release_problems_are_untested_not_failed(self):
        # Nothing connected: every client lacks its entry, which fails for the
        # release under test and is only noted for the previous release.
        rows = clients.check(self.home, self.manifest, self.entries)
        self.assertIn("FAIL", statuses(rows, "Goose has a Toolport entry"))
        previous = clients.as_previous_release(rows, "v2.3.5")
        self.assertEqual(statuses(previous, "Goose has a Toolport entry"), ["UNTESTED"])
        self.assertFalse([r for r in previous if "\tFAIL\t" in r])
        self.assertTrue(all("v2.3.5" in r for r in previous if "\tUNTESTED\t" in r))
        # PASS and INFO rows are kept as they are.
        kept = [r for r in rows if "\tFAIL\t" not in r]
        self.assertEqual([r for r in previous if r in kept], kept)

    def test_strip_jsonc_keeps_strings(self):
        text = '{"a": "http://x//y", /* c */ "b": [1, 2,], // tail\n}'
        self.assertEqual(json.loads(clients.strip_jsonc(text)), {"a": "http://x//y", "b": [1, 2]})


if __name__ == "__main__":
    unittest.main()
