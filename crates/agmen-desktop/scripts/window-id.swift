// Prints the CGWindowID of the first on-screen, layer-0 window owned by the
// process id given as the first argument, so `screencapture -l` captures
// that one window: not another agmen-desktop the person has open, and
// nothing else on screen. Exits 1 when that process has no window yet.
import CoreGraphics

guard CommandLine.arguments.count > 1, let pid = Int(CommandLine.arguments[1]) else {
    print("usage: window-id <pid>")
    exit(2)
}
let options = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows
where (window[kCGWindowOwnerPID as String] as? Int) == pid
    && (window[kCGWindowLayer as String] as? Int) == 0
{
    if let id = window[kCGWindowNumber as String] as? Int {
        print(id)
        exit(0)
    }
}
exit(1)
