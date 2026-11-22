// Capture one window by its window number with ScreenCaptureKit, wherever it is: behind others, or
// moved off screen by a tiling window manager (AeroSpace parks other workspaces' windows in a
// corner), so dev scripts can take screenshots without showing the window.
// Usage: capture-window <window-number> <out.png>. Compiled on first use by the dev script's `shot`.
import AppKit
import Foundation
import ScreenCaptureKit
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 3, let id = UInt32(args[1]) else {
    FileHandle.standardError.write("usage: capture-window <window-number> <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[2])
// Connect to the window server (a command-line tool isn't connected until it asks).
_ = NSApplication.shared
let done = DispatchSemaphore(value: 0)
var code: Int32 = 0
Task {
    do {
        // All windows, off-screen ones included.
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let window = content.windows.first(where: { $0.windowID == id }) else {
            FileHandle.standardError.write("no window \(id)\n".data(using: .utf8)!)
            code = 1; done.signal(); return
        }
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let config = SCStreamConfiguration()
        let scale = CGFloat(filter.pointPixelScale)
        config.width = Int(window.frame.width * scale)
        config.height = Int(window.frame.height * scale)
        config.showsCursor = false
        config.ignoreShadowsSingleWindow = true
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        guard let dest = CGImageDestinationCreateWithURL(out as CFURL, UTType.png.identifier as CFString, 1, nil) else {
            code = 1; done.signal(); return
        }
        CGImageDestinationAddImage(dest, image, nil)
        CGImageDestinationFinalize(dest)
    } catch {
        FileHandle.standardError.write("capture failed: \(error)\n".data(using: .utf8)!)
        code = 1
    }
    done.signal()
}
done.wait()
exit(code)
