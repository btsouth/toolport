#!/usr/bin/env python3
"""Tests for layout_check.py. Run: python3 scripts/macos-acceptance/test_layout_check.py"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import layout_check  # noqa: E402

SCREEN = {"x": 0, "y": 0, "w": 1024, "h": 768}
WIN = {"x": 0, "y": 30, "w": 480, "h": 360}
SCROLL = {"x": 64, "y": 60, "w": 416, "h": 330}


def ctl(label, x, y, w=80, h=24, scroll=None):
    c = {"role": "AXButton", "label": label, "frame": {"x": x, "y": y, "w": w, "h": h}}
    if scroll:
        c["scroll"] = scroll
    return c


def layout(*controls, win=WIN):
    return {"window": win, "screen": SCREEN, "controls": list(controls)}


class LayoutCheckTest(unittest.TestCase):
    def test_fitting_controls_pass(self):
        self.assertEqual(layout_check.problems_of(layout(ctl("Servers", 8, 60), ctl("Add", 100, 80, scroll=SCROLL))), [])

    def test_sideways_clip_fails(self):
        found = layout_check.problems_of(layout(ctl("Save", 440, 80, scroll=SCROLL)))
        self.assertEqual(len(found), 1)
        self.assertIn("cut off sideways", found[0])

    def test_below_fold_ok_only_in_scroll_container(self):
        self.assertEqual(layout_check.problems_of(layout(ctl("Later", 100, 900, scroll=SCROLL))), [])
        self.assertIn("below the window", layout_check.problems_of(layout(ctl("Later", 100, 900)))[0])

    def test_window_off_screen(self):
        found = layout_check.problems_of(layout(ctl("A", 900, 60), win={"x": 800, "y": 30, "w": 480, "h": 360}))
        self.assertTrue(any("off the" in p for p in found))

    def test_required_labels(self):
        lay = layout(ctl("Servers", 8, 60), ctl("Settings", 470, 60))
        found = layout_check.problems_of(lay, ["Servers", "Settings", "Sync"])
        self.assertTrue(any("'Settings'" in p and "no reachable" in p for p in found))
        self.assertTrue(any("'Sync'" in p for p in found))
        self.assertFalse(any("'Servers'" in p and "no reachable" in p for p in found))


if __name__ == "__main__":
    unittest.main()
