// Prints the CGWindowID of the first on-screen, layer-0 window whose owner
// process is named by the first argument (default agmen-desktop), so
// `screencapture -l` can capture that one window and nothing else on screen.
import CoreGraphics

let owner = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "agmen-desktop"
let options = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows
where (window[kCGWindowOwnerName as String] as? String) == owner
    && (window[kCGWindowLayer as String] as? Int) == 0
{
    if let id = window[kCGWindowNumber as String] as? Int {
        print(id)
        exit(0)
    }
}
exit(1)
