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
    let w = window(arg)
    return windows().first?[kCGWindowNumber as String] as? Int == Int(arg)
        && NSWorkspace.shared.frontmostApplication?.processIdentifier == pid(w)
}

func front(_ arg: String) {
    need("input")
    if !isFront(arg) { fail("window \(arg) is not the front window, nothing posted") }
}

func flags(_ s: String) -> CGEventFlags {
    var f = CGEventFlags()
    for m in s.split(separator: ",") {
        switch m {
        case "option": f.insert(.maskAlternate)
        case "control": f.insert(.maskControl)
        case "command": f.insert(.maskCommand)
        case "shift": f.insert(.maskShift)
        default: fail("unknown modifier \(m)")
        }
    }
    return f
}

let src = CGEventSource(stateID: .hidSystemState)
let a = CommandLine.arguments
switch a.count > 1 ? a[1] : "" {
case "can" where a.count == 3:
    need(a[2])
case "windows":
    for w in windows() {
        print(w[kCGWindowNumber as String] as? Int ?? 0, pid(w), w[kCGWindowOwnerName as String] as? String ?? "?")
    }
case "raise" where a.count == 3:
    _ = NSRunningApplication(processIdentifier: pid(window(a[2])))?.activate()
    Thread.sleep(forTimeInterval: 0.5)
    if !isFront(a[2]) { fail("window \(a[2]) did not come to the front") }
case "key" where (4...6).contains(a.count):
    front(a[2])
    guard let code = CGKeyCode(a[3]) else { fail("bad keycode \(a[3])") }
    for down in [true, false] {
        let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: down)!
        e.flags = flags(a.count > 4 ? a[4] : "")
        if a.count > 5 {
            let u = Array(a[5].utf16)
            e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u)
        }
        e.post(tap: .cghidEventTap)
    }
case "click" where a.count == 3:
    front(a[2])
    let r = bounds(window(a[2]))
    let p = CGPoint(x: r.midX, y: r.midY)
    for t in [CGEventType.leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: src, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!.post(tap: .cghidEventTap)
        Thread.sleep(forTimeInterval: 0.05)
    }
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
