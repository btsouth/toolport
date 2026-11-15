#!/usr/bin/env python3
"""Tests for finder_targets.py. Run: python3 scripts/macos-acceptance/test_finder_targets.py"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import finder_targets  # noqa: E402

# Real `tpui find` output from a macOS 26 runner.
SAMPLE = """AXWindow "Toolport" @40,80 440x420
AXImage "Toolport | Toolport" @111,125 48x48
AXStaticText "Toolport" @140,88 59x16
AXMenuItem "Toolport" @0,768 0x0
AXStaticText "Applications" @573,235 98x18
AXImage "Applications | Applications" @739,146 64x64
AXMenuItem "Applications" @0,768 0x0
""".splitlines()


class FinderTargetsTest(unittest.TestCase):
    def test_picks_icon_and_applications_folder(self):
        self.assertEqual(finder_targets.targets(SAMPLE), ((135, 149), (771, 178)))

    def test_missing_target(self):
        self.assertEqual(finder_targets.targets(SAMPLE[:3]), ((135, 149), None))


if __name__ == "__main__":
    unittest.main()
