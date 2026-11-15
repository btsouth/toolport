// tpui: the macOS acceptance job's window and accessibility probe.
//
// Built on the runner with `swiftc -O tpui.swift -o tpui`. Subcommands:
//   trust                       JSON: accessibility and screen recording grants
//   windows                     JSON: every window CGWindowList reports
//   dump <pid> [depth]          the app's accessibility tree, one element per line
//   find <pid> <text>           elements whose title, description, value or
//                               placeholder contains <text> (case-insensitive)
//   press <pid> <text> [n]      AXPress the n-th (default 1) element matching <text>
//   focus <pid> <text> [n]      focus the n-th text field matching <text>
//   keys <text>                 type <text> as keyboard events into the focused field
//   key <code> [cmd]            post one virtual key code (36 return, 48 tab, 53 esc)
//   click <x> <y>               left click at global screen coordinates
//   menubar <pid>               the app's menu bar extras (status items)
//   display                     JSON: main display's current mode and every mode
//   setmode <w> <h>             switch the main display to a w x h mode (exit 2 if none)
//   resize <pid> <w> <h>        move the main window to the top left and resize it;
//                               prints the size the window actually took
//   layout <pid>                JSON: window frame, screen frame and every control's
//                               frame, with its nearest scroll container
//
// Exit codes: 0 success, 2 not found, 3 accessibility not granted, 4 window
// list unavailable, 1 other error. Callers treat only 2 as "not there".
// Accessibility calls fail with kAXErrorAPIDisabled (-25211) when the calling
// process has no Accessibility grant; that is reported, never hidden.

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

func emit(_ obj: Any) {
    let data = try! JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

func die(_ msg: String, _ code: Int32 = 1) -> Never {
    FileHandle.standardError.write("tpui: \(msg)\n".data(using: .utf8)!)
    exit(code)
}

func attr(_ el: AXUIElement, _ name: String) -> AnyObject? {
    var value: AnyObject?
    let err = AXUIElementCopyAttributeValue(el, name as CFString, &value)
    if err == .apiDisabled { die("accessibility not granted (kAXErrorAPIDisabled)", 3) }
    return err == .success ? value : nil
}

func str(_ el: AXUIElement, _ name: String) -> String {
    guard let v = attr(el, name) else { return "" }
    if let s = v as? String { return s }
    if let n = v as? NSNumber { return n.stringValue }
    return ""
}

func frame(_ el: AXUIElement) -> CGRect? {
    guard let p = attr(el, kAXPositionAttribute), let s = attr(el, kAXSizeAttribute) else { return nil }
    var point = CGPoint.zero
    var size = CGSize.zero
    AXValueGetValue(p as! AXValue, .cgPoint, &point)
    AXValueGetValue(s as! AXValue, .cgSize, &size)
    return CGRect(origin: point, size: size)
}

func children(_ el: AXUIElement) -> [AXUIElement] {
    (attr(el, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}

func label(_ el: AXUIElement) -> String {
    let parts = [
        str(el, kAXTitleAttribute), str(el, kAXDescriptionAttribute),
        str(el, kAXValueAttribute), str(el, kAXPlaceholderValueAttribute),
        str(el, kAXHelpAttribute),
    ]
    return parts.filter { !$0.isEmpty }.joined(separator: " | ")
}

func walk(_ el: AXUIElement, _ depth: Int, _ maxDepth: Int, _ visit: (AXUIElement, Int) -> Void) {
    visit(el, depth)
    if depth >= maxDepth { return }
    for c in children(el) { walk(c, depth + 1, maxDepth, visit) }
}

func walkSkipping(_ el: AXUIElement, _ depth: Int, _ maxDepth: Int, skipMenuBar: Bool, _ visit: (AXUIElement, Int) -> Void) {
    if skipMenuBar && str(el, kAXRoleAttribute) == "AXMenuBar" { return }
    visit(el, depth)
    if depth >= maxDepth { return }
    for c in children(el) { walkSkipping(c, depth + 1, maxDepth, skipMenuBar: skipMenuBar, visit) }
}

func appElement(_ pidArg: String) -> AXUIElement {
    guard let pid = pid_t(pidArg) else { die("bad pid \(pidArg)") }
    let app = AXUIElementCreateApplication(pid)
    // Ask Chromium/WebKit based apps to expose their full tree.
    AXUIElementSetAttributeValue(app, "AXEnhancedUserInterface" as CFString, kCFBooleanTrue)
    AXUIElementSetAttributeValue(app, "AXManualAccessibility" as CFString, kCFBooleanTrue)
    return app
}

// Elements whose title, description, value, placeholder or help equals <text>
// (case-insensitive); when none does, those that contain it.
func matches(_ pidArg: String, _ text: String, roles: Set<String>? = nil) -> [AXUIElement] {
    let needle = text.lowercased()
    var exact: [AXUIElement] = []
    var partial: [AXUIElement] = []
    // Menu bars are skipped unless TPUI_MENUS=1: a partial match must never
    // land on a system menu item such as "System Settings…".
    let menus = ProcessInfo.processInfo.environment["TPUI_MENUS"] == "1"
    walkSkipping(appElement(pidArg), 0, 60, skipMenuBar: !menus) { el, _ in
        let role = str(el, kAXRoleAttribute)
        if let roles = roles, !roles.contains(role) { return }
        let parts = [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute, kAXPlaceholderValueAttribute, kAXHelpAttribute]
            .map { str(el, $0).lowercased().trimmingCharacters(in: .whitespacesAndNewlines) }
        if parts.contains(needle) { exact.append(el) } else if label(el).lowercased().contains(needle) { partial.append(el) }
    }
    return exact.isEmpty ? partial : exact
}

func mainWindow(_ app: AXUIElement) -> AXUIElement? {
    if let w = attr(app, kAXMainWindowAttribute), CFGetTypeID(w) == AXUIElementGetTypeID() { return (w as! AXUIElement) }
    return (attr(app, kAXWindowsAttribute) as? [AXUIElement])?.first
}

func postKey(_ code: CGKeyCode, cmd: Bool = false) {
    let src = CGEventSource(stateID: .hidSystemState)
    let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)
    let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)
    if cmd { down?.flags = .maskCommand; up?.flags = .maskCommand }
    down?.post(tap: .cghidEventTap)
    up?.post(tap: .cghidEventTap)
}

let args = CommandLine.arguments
guard args.count >= 2 else { die("usage: tpui <command> ...") }

switch args[1] {
case "trust":
    let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: false] as CFDictionary
    emit([
        "accessibility": AXIsProcessTrustedWithOptions(opts),
        "screenRecording": CGPreflightScreenCaptureAccess(),
        "postEvents": CGPreflightPostEventAccess(),
        "listenEvents": CGPreflightListenEventAccess(),
    ])

case "windows":
    guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]], !list.isEmpty else {
        die("window list unavailable", 4)
    }
    var rows: [[String: Any]] = []
    for w in list {
        let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
        rows.append([
            "owner": w[kCGWindowOwnerName as String] as? String ?? "",
            "pid": w[kCGWindowOwnerPID as String] as? Int ?? 0,
            "name": w[kCGWindowName as String] as? String ?? "",
            "layer": w[kCGWindowLayer as String] as? Int ?? 0,
            "onscreen": w[kCGWindowIsOnscreen as String] as? Bool ?? false,
            "x": b["X"] as? Double ?? 0, "y": b["Y"] as? Double ?? 0,
            "w": b["Width"] as? Double ?? 0, "h": b["Height"] as? Double ?? 0,
        ])
    }
    emit(rows)

case "dump":
    guard args.count >= 3 else { die("usage: tpui dump <pid> [depth]") }
    let maxDepth = args.count >= 4 ? Int(args[3]) ?? 40 : 40
    var count = 0
    walk(appElement(args[2]), 0, maxDepth) { el, depth in
        count += 1
        let role = str(el, kAXRoleAttribute)
        let sub = str(el, kAXSubroleAttribute)
        var line = String(repeating: "  ", count: depth) + role
        if !sub.isEmpty { line += "/" + sub }
        let l = label(el)
        if !l.isEmpty { line += " \"" + String(l.prefix(160)) + "\"" }
        if let f = frame(el) { line += " @\(Int(f.minX)),\(Int(f.minY)) \(Int(f.width))x\(Int(f.height))" }
        print(line)
    }
    if count <= 1 { die("no accessibility children", 2) }

case "find":
    guard args.count >= 4 else { die("usage: tpui find <pid> <text>") }
    let found = matches(args[2], args[3])
    for el in found {
        let f = frame(el).map { "@\(Int($0.minX)),\(Int($0.minY)) \(Int($0.width))x\(Int($0.height))" } ?? ""
        print("\(str(el, kAXRoleAttribute)) \"\(label(el).prefix(160))\" \(f)")
    }
    if found.isEmpty { exit(2) }

case "press", "focus":
    guard args.count >= 4 else { die("usage: tpui \(args[1]) <pid> <text> [n]") }
    // n counts from 1; a negative n counts from the end (-1 is the last match,
    // which is the topmost dialog's control when a modal repeats a label).
    let n = args.count >= 5 ? (Int(args[4]) ?? 1) : 1
    let roles: Set<String> = args[1] == "press"
        ? ["AXButton", "AXLink", "AXMenuItem", "AXRadioButton", "AXCheckBox", "AXTab", "AXPopUpButton", "AXMenuButton", "AXCell", "AXRow", "AXStaticText", "AXGroup"]
        : ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"]
    let found = matches(args[2], args[3], roles: roles)
    // Prefer the most specific role (a button over a group that contains its text).
    let ranked = found.sorted { a, b in
        let order = ["AXButton", "AXLink", "AXRadioButton", "AXTab", "AXCheckBox", "AXMenuItem", "AXPopUpButton", "AXMenuButton", "AXTextField", "AXTextArea", "AXComboBox", "AXSearchField", "AXCell", "AXRow", "AXStaticText", "AXGroup"]
        return (order.firstIndex(of: str(a, kAXRoleAttribute)) ?? 99) < (order.firstIndex(of: str(b, kAXRoleAttribute)) ?? 99)
    }
    let index = n < 0 ? found.count + n : n - 1
    guard index >= 0, index < found.count else { die("no element matching \"\(args[3])\" (\(found.count) found)", 2) }
    // A negative index keeps document order (last = topmost); a positive one
    // prefers the most specific role.
    let el = n < 0 ? found[index] : ranked[index]
    let shown = "\(str(el, kAXRoleAttribute)) \"\(label(el).prefix(80))\""
    if args[1] == "press" {
        let err = AXUIElementPerformAction(el, kAXPressAction as CFString)
        if err != .success {
            // Fall back to a real click at the element's centre.
            guard let f = frame(el) else { die("AXPress failed (\(err.rawValue)) and no frame", 1) }
            let p = CGPoint(x: f.midX, y: f.midY)
            for t in [CGEventType.leftMouseDown, .leftMouseUp] {
                CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
            }
            print("clicked \(shown) at \(Int(p.x)),\(Int(p.y)) after AXPress error \(err.rawValue)")
        } else {
            print("pressed \(shown)")
        }
    } else {
        let err = AXUIElementSetAttributeValue(el, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        if err != .success, let f = frame(el) {
            let p = CGPoint(x: f.midX, y: f.midY)
            for t in [CGEventType.leftMouseDown, .leftMouseUp] {
                CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
            }
        }
        print("focused \(shown)")
    }

case "value", "setvalue":
    // value <pid> <field> [n]: print the n-th matching text field's value.
    // setvalue <pid> <field> <text> [n]: set it through accessibility.
    guard args.count >= 4 else { die("usage: tpui value <pid> <field> [n] | setvalue <pid> <field> <text> [n]") }
    let set = args[1] == "setvalue"
    if set && args.count < 5 { die("usage: tpui setvalue <pid> <field> <text> [n]") }
    let nArg = set ? (args.count >= 6 ? args[5] : "1") : (args.count >= 5 ? args[4] : "1")
    let n = max(1, Int(nArg) ?? 1)
    let fields = matches(args[2], args[3], roles: ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"])
    guard fields.count >= n else { die("no text field matching \"\(args[3])\"", 2) }
    let el = fields[n - 1]
    if set {
        let err = AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, args[4] as CFString)
        if err != .success { die("AXValue set failed (\(err.rawValue))", 1) }
    }
    print(str(el, kAXValueAttribute))

case "keys":
    guard args.count >= 3 else { die("usage: tpui keys <text>") }
    let src = CGEventSource(stateID: .hidSystemState)
    for scalar in args[2].utf16 {
        var c = scalar
        for down in [true, false] {
            let e = CGEvent(keyboardEventSource: src, virtualKey: 0, keyDown: down)
            e?.keyboardSetUnicodeString(stringLength: 1, unicodeString: &c)
            e?.post(tap: .cghidEventTap)
        }
        usleep(15_000)
    }

case "key":
    guard args.count >= 3, let code = CGKeyCode(args[2]) else { die("usage: tpui key <code> [cmd]") }
    postKey(code, cmd: args.count >= 4 && args[3] == "cmd")

case "click":
    guard args.count >= 4, let x = Double(args[2]), let y = Double(args[3]) else { die("usage: tpui click <x> <y>") }
    let p = CGPoint(x: x, y: y)
    for t in [CGEventType.leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
        usleep(50_000)
    }

case "drag":
    // drag <x1> <y1> <x2> <y2>: press at p1, move in steps, release at p2.
    guard args.count >= 6, let x1 = Double(args[2]), let y1 = Double(args[3]),
          let x2 = Double(args[4]), let y2 = Double(args[5]) else { die("usage: tpui drag <x1> <y1> <x2> <y2>") }
    let a = CGPoint(x: x1, y: y1), b = CGPoint(x: x2, y: y2)
    CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: a, mouseButton: .left)?.post(tap: .cghidEventTap)
    usleep(200_000)
    CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: a, mouseButton: .left)?.post(tap: .cghidEventTap)
    usleep(300_000)
    for i in 1...40 {
        let t = Double(i) / 40
        let p = CGPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t)
        CGEvent(mouseEventSource: nil, mouseType: .leftMouseDragged, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
        usleep(25_000)
    }
    usleep(400_000)
    CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: b, mouseButton: .left)?.post(tap: .cghidEventTap)

case "menubar":
    guard args.count >= 3 else { die("usage: tpui menubar <pid>") }
    let app = appElement(args[2])
    guard let bar = attr(app, kAXExtrasMenuBarAttribute) else { die("no extras menu bar", 2) }
    let items = children(bar as! AXUIElement)
    for el in items { print("\(str(el, kAXRoleAttribute)) \"\(label(el))\" \(frame(el).map { "@\(Int($0.minX)),\(Int($0.minY))" } ?? "")") }
    if items.isEmpty { exit(2) }

case "display":
    let d = CGMainDisplayID()
    guard let cur = CGDisplayCopyDisplayMode(d) else { die("no display mode", 4) }
    let opts = [kCGDisplayShowDuplicateLowResolutionModes as String: true] as CFDictionary
    let modes = (CGDisplayCopyAllDisplayModes(d, opts) as? [CGDisplayMode]) ?? []
    emit([
        "current": ["w": cur.width, "h": cur.height, "pixelW": cur.pixelWidth, "pixelH": cur.pixelHeight],
        "bounds": ["w": Double(CGDisplayBounds(d).width), "h": Double(CGDisplayBounds(d).height)],
        "modes": modes.map { ["w": $0.width, "h": $0.height, "pixelW": $0.pixelWidth, "pixelH": $0.pixelHeight, "usable": $0.isUsableForDesktopGUI()] },
    ])

case "setmode":
    guard args.count >= 4, let w = Int(args[2]), let h = Int(args[3]) else { die("usage: tpui setmode <w> <h>") }
    let d = CGMainDisplayID()
    let opts = [kCGDisplayShowDuplicateLowResolutionModes as String: true] as CFDictionary
    let modes = (CGDisplayCopyAllDisplayModes(d, opts) as? [CGDisplayMode]) ?? []
    guard let mode = modes.first(where: { $0.width == w && $0.height == h && $0.isUsableForDesktopGUI() })
        ?? modes.first(where: { $0.width == w && $0.height == h }) else { die("no \(w)x\(h) mode", 2) }
    var cfg: CGDisplayConfigRef?
    guard CGBeginDisplayConfiguration(&cfg) == .success else { die("CGBeginDisplayConfiguration failed") }
    CGConfigureDisplayWithDisplayMode(cfg, d, mode, nil)
    let err = CGCompleteDisplayConfiguration(cfg, .permanently)
    guard err == .success else { die("display configuration failed (\(err.rawValue))") }
    usleep(1_500_000)
    let b = CGDisplayBounds(d)
    print("\(Int(b.width))x\(Int(b.height))")
    if Int(b.width) != w || Int(b.height) != h { die("display is \(Int(b.width))x\(Int(b.height)) after the switch", 1) }

case "resize":
    guard args.count >= 5, let w = Double(args[3]), let h = Double(args[4]) else { die("usage: tpui resize <pid> <w> <h>") }
    let app = appElement(args[2])
    guard let el = mainWindow(app) else { die("no window", 2) }
    var origin = CGPoint(x: 0, y: 30)
    var size = CGSize(width: w, height: h)
    AXUIElementSetAttributeValue(el, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &origin)!)
    let err = AXUIElementSetAttributeValue(el, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &size)!)
    if err != .success { die("AXSize set failed (\(err.rawValue))") }
    usleep(800_000)
    guard let f = frame(el) else { die("no window frame") }
    print("\(Int(f.width))x\(Int(f.height))")

case "layout":
    guard args.count >= 3 else { die("usage: tpui layout <pid>") }
    let app = appElement(args[2])
    guard let win = mainWindow(app), let wf = frame(win) else { die("no window", 2) }
    let controls: Set<String> = ["AXButton", "AXLink", "AXRadioButton", "AXCheckBox", "AXPopUpButton", "AXMenuButton", "AXTextField", "AXTextArea", "AXComboBox", "AXSearchField", "AXSlider", "AXSwitch", "AXTab"]
    var rows: [[String: Any]] = []
    func rect(_ r: CGRect) -> [String: Double] { ["x": Double(r.minX), "y": Double(r.minY), "w": Double(r.width), "h": Double(r.height)] }
    func visit(_ el: AXUIElement, _ clip: CGRect?, _ depth: Int) {
        if depth > 60 { return }
        let role = str(el, kAXRoleAttribute)
        if role == "AXMenuBar" { return }
        var clipHere = clip
        if role == "AXScrollArea", let f = frame(el) { clipHere = clip.map { $0.intersection(f) } ?? f }
        if controls.contains(role), let f = frame(el), f.width > 0, f.height > 0 {
            var row: [String: Any] = ["role": role, "label": String(label(el).prefix(80)), "frame": rect(f)]
            if let c = clipHere { row["scroll"] = rect(c) }
            rows.append(row)
        }
        for c in children(el) { visit(c, clipHere, depth + 1) }
    }
    visit(win, nil, 0)
    let sb = CGDisplayBounds(CGMainDisplayID())
    emit(["window": rect(wf), "screen": rect(sb), "controls": rows])
    if rows.isEmpty { exit(2) }

default:
    die("unknown command \(args[1])")
}
