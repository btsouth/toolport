#!/usr/bin/env python3
"""Find clipped or off-screen controls in `tpui layout` JSON (read on stdin).

usage: layout_check.py [required-label ...]

A control is a problem when, as accessibility geometry reports it:
  - it sticks out of the window (or of its scroll container) on the left or
    right: content cut off sideways cannot be scrolled to,
  - it is below the window bottom with no scroll container to reach it, or
  - the window itself is partly off the screen.
Controls inside a scroll container may extend below it (they scroll).
Each required label must match at least one control that is fully reachable
(the sidebar items, for example).

Exit codes: 0 no problems, 1 problems (listed on stdout), 2 unusable input.
"""

import json
import sys

SLACK = 2.0


def right(r):
    return r["x"] + r["w"]


def bottom(r):
    return r["y"] + r["h"]


def problems_of(layout, required=()):
    win, screen = layout["window"], layout["screen"]
    out = []
    if win["x"] < screen["x"] - SLACK or right(win) > right(screen) + SLACK or bottom(win) > bottom(screen) + SLACK:
        out.append("window %dx%d at %d,%d is partly off the %dx%d screen" % (win["w"], win["h"], win["x"], win["y"], screen["w"], screen["h"]))
    reachable = []
    for c in layout["controls"]:
        f = c["frame"]
        region = c.get("scroll") or win
        left_edge = max(region["x"], win["x"])
        right_edge = min(right(region), right(win))
        name = "%s %r" % (c["role"], c.get("label", ""))
        bad = []
        if f["x"] < left_edge - SLACK or right(f) > right_edge + SLACK:
            bad.append("cut off sideways (x %d..%d, visible %d..%d)" % (f["x"], right(f), left_edge, right_edge))
        if "scroll" not in c and bottom(f) > bottom(win) + SLACK:
            bad.append("below the window with nothing to scroll (bottom %d, window bottom %d)" % (bottom(f), bottom(win)))
        if f["y"] < win["y"] - SLACK and "scroll" not in c:
            bad.append("above the window")
        if bad:
            out.append("%s: %s" % (name, "; ".join(bad)))
        else:
            reachable.append(c.get("label", "").lower())
    for label in required:
        if not any(label.lower() in r for r in reachable):
            out.append("no reachable control for %r" % label)
    return out


def main(argv):
    try:
        layout = json.load(sys.stdin)
        layout["window"], layout["screen"], layout["controls"]
    except (ValueError, KeyError, TypeError) as e:
        print("unusable layout: %s" % e, file=sys.stderr)
        return 2
    if not layout["controls"]:
        print("no controls in the layout", file=sys.stderr)
        return 2
    found = problems_of(layout, argv[1:])
    for p in found:
        print(p)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
