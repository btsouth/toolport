#!/usr/bin/env python3
"""Tests for windows_query.py. Run: python3 scripts/macos-acceptance/test_windows_query.py"""

import io
import json
import os
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import windows_query  # noqa: E402

BASE = [
    {"owner": "Dock", "pid": 349, "name": "Dock", "layer": 20, "onscreen": True, "x": 0, "y": 0, "w": 1024, "h": 768},
    {"owner": "Window Server", "pid": 156, "name": "Menubar", "layer": 24, "onscreen": True, "x": 0, "y": 0, "w": 1024, "h": 30},
]
APP = {"owner": "Toolport", "pid": 7234, "name": "Toolport", "layer": 0, "onscreen": True, "x": 0, "y": 31, "w": 1024, "h": 674}
PROMPT = {"owner": "SecurityAgent", "pid": 900, "name": "", "layer": 1000, "onscreen": True, "x": 300, "y": 200, "w": 420, "h": 260}
GK = {"owner": "CoreServicesUIAgent", "pid": 500, "name": "", "layer": 0, "onscreen": True, "x": 382, "y": 135, "w": 260, "h": 260}


def run(rows_or_text, *args):
    text = rows_or_text if isinstance(rows_or_text, str) else json.dumps(rows_or_text)
    out, err = io.StringIO(), io.StringIO()
    old = sys.stdin
    sys.stdin = io.StringIO(text)
    try:
        with redirect_stdout(out), redirect_stderr(err):
            code = windows_query.main(["windows_query.py", *args])
    finally:
        sys.stdin = old
    return code, out.getvalue().strip()


class WindowsQueryTest(unittest.TestCase):
    def test_prompt_found_and_absent(self):
        self.assertEqual(run(BASE + [PROMPT], "prompt")[0], 0)
        self.assertEqual(run(BASE + [APP], "prompt"), (1, ""))

    def test_broken_probe_is_never_absent(self):
        for bad in ["", "not json", "{}", "[]", json.dumps([{"owner": "Toolport"}]), json.dumps([APP])]:
            self.assertEqual(run(bad, "prompt")[0], 2, bad)

    def test_gatekeeper_pid(self):
        self.assertEqual(run(BASE + [GK], "gatekeeper"), (0, "500"))
        self.assertEqual(run(BASE, "gatekeeper")[0], 1)
        self.assertEqual(run(BASE + [dict(GK, onscreen=False)], "gatekeeper")[0], 1)

    def test_app_window(self):
        code, line = run(BASE + [APP], "app-window", "7234")
        self.assertEqual(code, 0)
        self.assertEqual(line, "Toolport 1024x674 at 0,31")
        self.assertEqual(run(BASE + [APP], "app-window", "1")[0], 1)

    def test_owner(self):
        self.assertEqual(run(BASE + [APP], "owner", "Toolport"), (0, "7234"))
        self.assertEqual(run(BASE, "owner", "Toolport")[0], 1)


if __name__ == "__main__":
    unittest.main()
