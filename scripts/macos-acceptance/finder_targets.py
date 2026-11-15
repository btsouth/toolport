#!/usr/bin/env python3
"""Pick drag coordinates from `tpui find` output for the Finder install drag.

usage: finder_targets.py <tpui-find-output>
Prints "<x>,<y> <x>,<y>": the Toolport icon in the dmg window (left, x 40-480)
and the Applications folder icon in the disk's root window (right, x >= 520).
Prints nothing when either is missing.
"""

import re
import sys


def targets(lines):
    icon = target = None
    for line in lines:
        m = re.match(r'(\S+) "(.*)" @(-?\d+),(-?\d+) (\d+)x(\d+)', line.strip())
        if not m:
            continue
        role, label = m.group(1), m.group(2)
        x, y, w, h = (int(v) for v in m.groups()[2:])
        centre = (x + w // 2, y + h // 2)
        name = label.split(" | ")[0]
        if role == "AXImage" and name.lower().startswith("toolport") and 40 <= x < 480 and icon is None:
            icon = centre
        if role == "AXImage" and name == "Applications" and x >= 520 and target is None:
            target = centre
    return icon, target


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
        icon, target = targets(f)
    if icon and target:
        print("%d,%d %d,%d" % (icon + target))
