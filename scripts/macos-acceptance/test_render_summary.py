#!/usr/bin/env python3
"""Tests for render_summary.py. Run: python3 scripts/macos-acceptance/test_render_summary.py"""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import render_summary  # noqa: E402


class RenderSummaryTest(unittest.TestCase):
    def test_counts_and_verdict(self):
        rows = render_summary.parse(
            "install\tPASS\tdownload\t12M\n"
            "install\tFAIL\tdigest\texpected a, got b\n"
            "ui\tUNTESTED\tclick\tno accessibility\n"
            "ui\tINFO\tgrants\t{}\n"
        )
        md = render_summary.render(rows)
        self.assertIn("Result: **FAILED**. 1 passed, 1 failed, 1 untested, 1 notes.", md)
        self.assertIn("### Failures", md)
        self.assertIn("- install: digest. expected a, got b", md)
        self.assertIn("### Not tested on this runner", md)
        self.assertIn("| **FAIL** | digest | expected a, got b |", md)

    def test_escapes_pipes_and_truncates(self):
        rows = render_summary.parse("a\tPASS\tx|y\t" + "z" * 1000 + "\n")
        md = render_summary.render(rows)
        self.assertIn("x\\|y", md)
        self.assertIn("...", md)
        self.assertNotIn("z" * 500, md)

    def test_detail_with_tabs_and_unknown_status(self):
        rows = render_summary.parse("a\tweird\tc\td1\td2\nshort\n")
        self.assertEqual(rows[0][:3], ("a", "FAIL", "result row is well formed"))
        self.assertIn("d1\td2", rows[0][3])
        self.assertEqual(rows[1][1], "FAIL")

    def test_main_exit_codes(self):
        with tempfile.TemporaryDirectory() as d:
            ok = os.path.join(d, "ok.tsv")
            with open(ok, "w") as f:
                f.write("a\tPASS\tc\t\n")
            self.assertEqual(render_summary.main(["x", ok]), 0)
            bad = os.path.join(d, "bad.tsv")
            with open(bad, "w") as f:
                f.write("a\tFAIL\tc\t\n")
            self.assertEqual(render_summary.main(["x", bad]), 1)
            empty = os.path.join(d, "empty.tsv")
            open(empty, "w").close()
            self.assertEqual(render_summary.main(["x", empty]), 1)
            self.assertEqual(render_summary.main(["x", os.path.join(d, "missing.tsv")]), 1)
            # A failed job with no FAIL row still renders and exits as a failure.
            self.assertEqual(render_summary.main(["x", ok, "t", "failure"]), 1)
            self.assertEqual(render_summary.main(["x", ok, "t", "success"]), 0)

    def test_empty_results_render_as_failed(self):
        self.assertIn("FAILED", render_summary.render([]))


if __name__ == "__main__":
    unittest.main()
