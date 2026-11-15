#!/usr/bin/env bash
# Window and display sizes: every screen and Settings page at the window's
# minimum size, at about 480 px wide, and (when the runner's display offers
# one) on a larger display. Each view gets a screenshot and an accessibility
# geometry check for controls that are cut off sideways or unreachable.
#
# usage: screens.sh <label>
set -uo pipefail
LABEL="$1"
AREA="window sizes ($LABEL)"
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
source "$HERE/common.sh"
# shellcheck source=ui.sh
source "$HERE/ui.sh"

SCREENS=(Servers Clients Skills Activity Settings Sync)
PAGES=(General Tools Safety Access Connections "Help and data")

[[ -n "$(app_pid)" ]] || { tmo 60 open "$APP_PATH" >/dev/null 2>&1 & wait_for 45 app_pid; sleep 6; }
[[ -n "$(app_pid)" ]] || { fail "app is running" "could not start it"; finish_step; }
ui_activate

# layout_view <size-label> <view-name>: screenshot and geometry check.
layout_view() {
  local size="$1" view="$2" slug json rc out
  slug="$(_slug "$LABEL-$size-$view")"
  shot "size-$slug" >/dev/null
  json="$EVIDENCE/logs/layout-$slug.json"
  TPUI_TIMEOUT=60 tpui layout "$(app_pid)" >"$json" 2>"$json.err"
  rc=$?
  if [[ $rc -ne 0 ]]; then
    fail "$size: $view has no clipped or unreachable controls" "layout probe exit $rc: $(head -c 200 "$json.err")"
    return
  fi
  out="$(python3 "$HERE/layout_check.py" "${SCREENS[@]}" <"$json" 2>&1)"
  rc=$?
  case $rc in
  0) pass "$size: $view has no clipped or unreachable controls" "screenshots/size-$slug.png" ;;
  1) fail "$size: $view has no clipped or unreachable controls" "$(head -5 <<<"$out" | tr '\n' ';' | cut -c1-400) (logs/layout-$slug.json)" ;;
  *) fail "$size: $view has no clipped or unreachable controls" "layout check exit $rc: $out" ;;
  esac
}

# tour <size-label>: every main screen, then every Settings page.
tour() {
  local size="$1" s p
  for s in "${SCREENS[@]}"; do
    if ui_press "$s" 1 10; then
      sleep 1.5
      layout_view "$size" "$s"
    else
      fail "$size: $s is reachable from the sidebar" "no control labelled $s"
    fi
  done
  ui_press "Settings" 1 10 >/dev/null 2>&1
  for p in "${PAGES[@]}"; do
    if ui_press "$p" 1 10; then
      sleep 1
      layout_view "$size" "Settings > $p"
    else
      fail "$size: Settings > $p is reachable" "no control labelled $p"
    fi
  done
}

# resize_to <w> <h>: returns 0 when the window took a size within 2 px.
resize_to() {
  local got
  got="$(TPUI_TIMEOUT=20 tpui resize "$(app_pid)" "$1" "$2" 2>&1)" || { RESIZE_GOT="error: $got"; return 1; }
  RESIZE_GOT="$got"
  local gw="${got%x*}" gh="${got#*x}"
  (( gw >= $1 - 2 && gw <= $1 + 2 && gh >= $2 - 2 && gh <= $2 + 2 ))
}

# ── Narrow window ─────────────────────────────────────────────────────────
# tauri.conf.json sets a 480x360 minimum. 480x640 is the tallest narrow
# window that fits the runner's 1024x768 screen with the menu bar and Dock. Below 900 px the sidebar becomes an
# icon rail that keeps its accessible labels.
log "Minimum window size"
if TPUI_TIMEOUT=20 tpui resize "$(app_pid)" 300 200 >"$EVIDENCE/logs/resize-below-min-$LABEL.txt" 2>&1; then
  got="$(cat "$EVIDENCE/logs/resize-below-min-$LABEL.txt")"
  if [[ "${got%x*}" -ge 478 && "${got#*x}" -ge 358 ]]; then
    pass "window refuses to shrink below its 480x360 minimum" "asked 300x200, got $got"
  else
    fail "window refuses to shrink below its 480x360 minimum" "asked 300x200, got $got"
  fi
else
  fail "window can be resized" "$(cat "$EVIDENCE/logs/resize-below-min-$LABEL.txt")"
fi
for size in 480x360 480x640; do
  log "Window at $size"
  if resize_to "${size%x*}" "${size#*x}"; then
    pass "window resizes to $size" "$RESIZE_GOT"
    tour "$size"
  else
    fail "window resizes to $size" "got $RESIZE_GOT"
  fi
done

# ── Larger display ────────────────────────────────────────────────────────
log "Larger display"
TPUI_TIMEOUT=20 tpui display >"$EVIDENCE/logs/display-modes-$LABEL.json" 2>&1
current="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("%dx%d" % (d["current"]["w"], d["current"]["h"]))' "$EVIDENCE/logs/display-modes-$LABEL.json" 2>/dev/null)"
bigger="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
cw, ch = d["current"]["w"], d["current"]["h"]
best = sorted({(m["w"], m["h"]) for m in d["modes"] if m["w"] > cw and m["h"] > ch and m.get("usable")}, key=lambda m: m[0] * m[1])
print("%dx%d" % best[-1] if best else "")' "$EVIDENCE/logs/display-modes-$LABEL.json" 2>/dev/null)"
if [[ -z "$current" ]]; then
  fail "display modes readable" "$(head -c 300 "$EVIDENCE/logs/display-modes-$LABEL.json")"
elif [[ -z "$bigger" ]]; then
  untested "screens on a larger display" "the runner's virtual display is $current and offers no larger mode (logs/display-modes-$LABEL.json)"
elif TPUI_TIMEOUT=30 tpui setmode "${bigger%x*}" "${bigger#*x}" >"$EVIDENCE/logs/setmode-$LABEL.txt" 2>&1; then
  pass "display switched to a larger mode" "$current -> $bigger"
  ui_activate
  w=$(( ${bigger%x*} * 3 / 4 )); h=$(( ${bigger#*x} * 3 / 4 ))
  if resize_to "$w" "$h"; then
    pass "window resizes to ${w}x${h} on the $bigger display" "$RESIZE_GOT"
    tour "${w}x${h} on $bigger"
  else
    fail "window resizes to ${w}x${h} on the $bigger display" "got $RESIZE_GOT"
  fi
  TPUI_TIMEOUT=30 tpui setmode "${current%x*}" "${current#*x}" >>"$EVIDENCE/logs/setmode-$LABEL.txt" 2>&1 ||
    fail "display restored to $current" "$(tail -1 "$EVIDENCE/logs/setmode-$LABEL.txt")"
else
  untested "screens on a larger display" "the runner refused $bigger: $(tail -1 "$EVIDENCE/logs/setmode-$LABEL.txt")"
fi
# Leave the window at a normal size for the steps after this one.
resize_to 1000 700 >/dev/null || true
finish_step
