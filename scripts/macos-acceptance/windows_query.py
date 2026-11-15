#!/usr/bin/env python3
"""Answer questions about `tpui windows` JSON (read on stdin).

usage: windows_query.py <query> [arg]
  prompt              system authorization prompts on screen (SecurityAgent etc.)
  gatekeeper          pid of Gatekeeper's first-open dialog
  app-window <pid>    the app's main window ("title WxH at X,Y")
  owner <name>        any on-screen window owned by <name>

Exit codes keep a clean "no" apart from a broken probe, so a failed probe can
never be read as "nothing there":
  0  found (details on stdout)
  1  looked, and there is none
  2  the window list is unusable (not JSON, not a list, empty, or missing the
     menu bar and Dock that every logged-in session has)
"""

import json
import sys

PROMPT_OWNERS = ("SecurityAgent", "UserNotificationCenter", "universalAccessAuthWarn", "coreautha")
GATEKEEPER_OWNERS = ("CoreServicesUIAgent",)


def load(text):
    try:
        rows = json.loads(text)
    except ValueError:
        return None
    if not isinstance(rows, list) or not rows:
        return None
    if not all(isinstance(r, dict) and "owner" in r and "pid" in r for r in rows):
        return None
    # A real logged-in session always lists the Dock and the menu bar. Without
    # them the probe is not seeing the console session.
    owners = {r.get("owner") for r in rows}
    if "Dock" not in owners and "Window Server" not in owners:
        return None
    return rows


def query(rows, name, arg=None):
    """Return (exit code, lines)."""
    if name == "prompt":
        hits = [w for w in rows if w["owner"] in PROMPT_OWNERS and w.get("onscreen") and w.get("w", 0) > 50]
        return (0 if hits else 1), ["%s %s %dx%d" % (w["owner"], w.get("name", ""), w["w"], w["h"]) for w in hits]
    if name == "gatekeeper":
        hits = [w for w in rows if w["owner"] in GATEKEEPER_OWNERS and w.get("onscreen") and w.get("layer", 0) >= 0 and w.get("w", 0) > 100]
        return (0 if hits else 1), [str(hits[0]["pid"])] if hits else []
    if name == "app-window":
        pid = int(arg)
        hits = [w for w in rows if w["pid"] == pid and w.get("layer") == 0 and w.get("onscreen") and w.get("w", 0) >= 400]
        lines = [
            "%s %dx%d at %d,%d" % (w.get("name") or "(untitled)", w["w"], w["h"], w.get("x", 0), w.get("y", 0))
            for w in hits
        ]
        return (0 if hits else 1), lines[:1]
    if name == "owner":
        hits = [w for w in rows if w["owner"] == arg and w.get("onscreen")]
        return (0 if hits else 1), [str(w["pid"]) for w in hits][:1]
    raise SystemExit(f"unknown query {name}")


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    rows = load(sys.stdin.read())
    if rows is None:
        print("window list unusable", file=sys.stderr)
        return 2
    code, lines = query(rows, argv[1], argv[2] if len(argv) > 2 else None)
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
