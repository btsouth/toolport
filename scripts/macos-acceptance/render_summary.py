#!/usr/bin/env python3
"""Render the macOS acceptance results.tsv as a Markdown job summary.

usage: render_summary.py <results.tsv> [title]

Rows are "<area>\t<status>\t<check>\t<detail>" with status PASS, FAIL,
UNTESTED or INFO. Exit status is 1 when any row is FAIL, so the summary step
can also act as the job's final gate.
"""

import sys
from collections import OrderedDict

ICONS = {"PASS": "pass", "FAIL": "**FAIL**", "UNTESTED": "untested", "INFO": "info"}


def parse(text):
    rows = []
    for line in text.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        parts += [""] * (4 - len(parts))
        area, status, check, detail = parts[:3] + ["\t".join(parts[3:])]
        status = status.strip().upper()
        if status not in ICONS:
            # A malformed row is never silently a note.
            detail = f"unrecognised status {status!r} for: {check.strip()} {detail}"
            check, status = "result row is well formed", "FAIL"
        rows.append((area.strip(), status, check.strip(), detail.strip()))
    return rows


def cell(value, limit=400):
    value = value.replace("|", "\\|").replace("\n", " ")
    if len(value) > limit:
        value = value[: limit - 3] + "..."
    return value


def render(rows, title="macOS acceptance"):
    counts = {s: sum(1 for r in rows if r[1] == s) for s in ICONS}
    out = [f"## {title}", ""]
    verdict = "FAILED" if counts["FAIL"] or not rows else "passed"
    out.append(
        f"Result: **{verdict}**. {counts['PASS']} passed, {counts['FAIL']} failed, "
        f"{counts['UNTESTED']} untested, {counts['INFO']} notes."
    )
    out.append("")
    failures = [r for r in rows if r[1] == "FAIL"]
    if failures:
        out.append("### Failures")
        out.append("")
        for area, _, check, detail in failures:
            out.append(f"- {cell(area)}: {cell(check)}. {cell(detail)}")
        out.append("")
    untested = [r for r in rows if r[1] == "UNTESTED"]
    if untested:
        out.append("### Not tested on this runner")
        out.append("")
        for area, _, check, detail in untested:
            out.append(f"- {cell(area)}: {cell(check)}. {cell(detail)}")
        out.append("")
    areas = OrderedDict()
    for row in rows:
        areas.setdefault(row[0], []).append(row)
    for area, items in areas.items():
        out.append(f"### {cell(area)}")
        out.append("")
        out.append("| Result | Check | Detail |")
        out.append("| --- | --- | --- |")
        for _, status, check, detail in items:
            out.append(f"| {ICONS[status]} | {cell(check)} | {cell(detail)} |")
        out.append("")
    return "\n".join(out)


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        with open(argv[1], encoding="utf-8", errors="replace") as f:
            rows = parse(f.read())
    except FileNotFoundError:
        rows = [("summary", "FAIL", "results recorded", f"{argv[1]} was not written")]
    title = argv[2] if len(argv) > 2 else "macOS acceptance"
    # The job's own status: a step that failed without recording a FAIL row
    # (a crashed script, a timeout) must not render as a pass.
    job_status = argv[3] if len(argv) > 3 else ""
    if job_status and job_status != "success" and not any(r[1] == "FAIL" for r in rows):
        rows.append(("summary", "FAIL", "every step finished", f"job status is {job_status} but no check recorded a failure; see the step logs"))
    print(render(rows, title))
    return 1 if any(r[1] == "FAIL" for r in rows) or not rows else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
