// Input and pixels for scripts/terminals.sh. Every input subcommand posts only while the given window is the front one.
import AppKit
import CoreGraphics

func fail(_ why: String) -> Never {
    FileHandle.standardError.write(("post: " + why + "\n").data(using: .utf8)!)
    exit(1)
}

func need(_ what: String) {
    switch what {
    case "input" where !CGPreflightPostEventAccess():
        fail("no Accessibility permission for the app this runs in: System Settings > Privacy & Security > Accessibility")
    case "screen" where !CGPreflightScreenCaptureAccess():
        fail("no Screen Recording permission for the app this runs in: System Settings > Privacy & Security > Screen Recording")
    default: break
    }
}

/// On-screen layer-0 windows, front to back.
func windows() -> [[String: Any]] {
    let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    return all.filter { $0[kCGWindowLayer as String] as? Int == 0 }
}

func bounds(_ w: [String: Any]) -> CGRect {
    var r = CGRect.zero
    if let b = w[kCGWindowBounds as String] { _ = CGRectMakeWithDictionaryRepresentation(b as! CFDictionary, &r) }
    return r
}

func window(_ arg: String) -> [String: Any] {
    guard let id = Int(arg), let w = windows().first(where: { $0[kCGWindowNumber as String] as? Int == id }) else {
        fail("window \(arg) is not on screen")
    }
    return w
}

func pid(_ w: [String: Any]) -> pid_t { w[kCGWindowOwnerPID as String] as? pid_t ?? 0 }

func isFront(_ arg: String) -> Bool {
    guard let w = windows().first(where: { $0[kCGWindowNumber as String] as? Int == Int(arg) }) else { return false }
    return windows().first?[kCGWindowNumber as String] as? Int == Int(arg)
        && NSWorkspace.shared.frontmostApplication?.processIdentifier == pid(w)
}

/// What is in front instead, for the note that says why nothing was posted.
func frontNow() -> String {
    let w = windows().first
    let app = NSWorkspace.shared.frontmostApplication
    let id = w?[kCGWindowNumber as String] as? Int ?? 0
    let owner = w?[kCGWindowOwnerName as String] as? String ?? "?"
    return "front is window \(id) of \(owner) pid \(w.map(pid) ?? 0), active app \(app?.localizedName ?? "?") pid \(app?.processIdentifier ?? 0)"
}

func front(_ arg: String) {
    need("input")
    if !isFront(arg) { fail("window \(arg) is not the front window, nothing posted; \(frontNow())") }
}

/// Each modifier is pressed as a key of its own, so an app that tracks modifier state sees it go down and up.
/// The left key's device bit (NX_DEVICEL*KEYMASK) goes with it: WezTerm tells left Option from right by it.
func modifiers(_ s: String) -> [(CGKeyCode, CGEventFlags)] {
    s.split(separator: ",").map { m in
        switch m {
        case "option": (58, CGEventFlags.maskAlternate.union(CGEventFlags(rawValue: 0x20)))
        case "control": (59, CGEventFlags.maskControl.union(CGEventFlags(rawValue: 0x01)))
        case "command": (55, CGEventFlags.maskCommand.union(CGEventFlags(rawValue: 0x08)))
        case "shift": (56, CGEventFlags.maskShift.union(CGEventFlags(rawValue: 0x02)))
        default: fail("unknown modifier \(m)")
        }
    }
}

/// The events that let go of what is pressed now, the last pressed last.
var undo: [CGEvent] = []

/// Posts only while the window is in front; otherwise lets go of everything pressed and stops.
func post(_ e: CGEvent, to window: String, then pause: Double) {
    if !isFront(window) {
        while let u = undo.popLast() { u.post(tap: .cghidEventTap) }
        fail("window \(window) left the front mid-input, the rest not posted; \(frontNow())")
    }
    e.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: pause)
}

func press(_ e: CGEvent, until release: CGEvent, on window: String) {
    post(e, to: window, then: 0.03)
    undo.append(release)
}

func releaseAll(on window: String) {
    while let u = undo.last {
        post(u, to: window, then: 0.05)
        undo.removeLast()
    }
}

func modifier(_ code: CGKeyCode, _ flags: CGEventFlags) -> CGEvent {
    let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!
    e.type = .flagsChanged
    e.flags = flags
    return e
}

// A private state: the flags set here never become the system's, or a later click would carry them.
let src = CGEventSource(stateID: .privateState)
let a = CommandLine.arguments
switch a.count > 1 ? a[1] : "" {
case "can" where a.count == 3:
    need(a[2])
case "windows":
    // Tab-separated: id, pid, owner, x,y,w,h, name. The name is empty without Screen Recording.
    for w in windows() {
        let r = bounds(w)
        let box = [r.minX, r.minY, r.width, r.height].map { String(Int($0)) }.joined(separator: ",")
        let fields = [String(w[kCGWindowNumber as String] as? Int ?? 0), String(pid(w)), w[kCGWindowOwnerName as String] as? String ?? "?", box,
            w[kCGWindowName as String] as? String ?? ""]
        print(fields.joined(separator: "\t"))
    }
case "raise" where a.count == 3:
    if isFront(a[2]) { break }
    _ = NSRunningApplication(processIdentifier: pid(window(a[2])))?.activate()
    Thread.sleep(forTimeInterval: 0.5)
    if !isFront(a[2]) { fail("window \(a[2]) did not come to the front; \(frontNow())") }
    Thread.sleep(forTimeInterval: 0.3)
case "key" where (4...6).contains(a.count):
    front(a[2])
    guard let code = CGKeyCode(a[3]) else { fail("bad keycode \(a[3])") }
    let mods = modifiers(a.count > 4 ? a[4] : "")
    var held = CGEventFlags()
    for (k, f) in mods {
        let before = held
        held.formUnion(f)
        press(modifier(k, held), until: modifier(k, before), on: a[2])
    }
    func keyEvent(_ isDown: Bool) -> CGEvent {
        let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: isDown)!
        // Without modifiers the event keeps its own flags: an arrow carries fn and numpad.
        if !mods.isEmpty { e.flags = e.flags.union(held) }
        if a.count > 5 {
            let u = Array(a[5].utf16)
            e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u)
        }
        return e
    }
    press(keyEvent(true), until: keyEvent(false), on: a[2])
    releaseAll(on: a[2])
case "click" where a.count == 3:
    front(a[2])
    let r = bounds(window(a[2]))
    let p = CGPoint(x: r.midX, y: r.midY)
    let mouse = { (t: CGEventType) in CGEvent(mouseEventSource: src, mouseType: t, mouseCursorPosition: p, mouseButton: .left)! }
    press(mouse(.leftMouseDown), until: mouse(.leftMouseUp), on: a[2])
    releaseAll(on: a[2])
case "colour" where a.count == 6:
    // Drawn into sRGB so the display profile does not shift the values the terminal was given.
    guard let s = CGImageSourceCreateWithURL(URL(fileURLWithPath: a[2]) as CFURL, nil),
        let img = CGImageSourceCreateImageAtIndex(s, 0, nil),
        let want = a[3...5].map({ Int($0) }) as? [Int]
    else { fail("cannot read \(a[2])") }
    let (w, h) = (img.width, img.height)
    var px = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    var hits = 0
    for i in stride(from: 0, to: px.count, by: 4) where (0..<3).allSatisfy({ abs(Int(px[i + $0]) - want[$0]) <= 8 }) {
        hits += 1
    }
    // Parts per 10000: a 20-cell block is under one percent of a large window.
    print(hits * 10000 / max(w * h, 1))
default:
    fail("usage: post can input|screen | windows | raise <id> | key <id> <keycode> [mods] [text] | click <id> | colour <png> <r> <g> <b>")
}
