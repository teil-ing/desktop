import AppKit
import ApplicationServices
import CoreGraphics
@preconcurrency import ScreenCaptureKit

// MARK: - WindowSelectionResult

/// The outcome of a user's window selection gesture.
enum WindowSelectionResult {
    /// User clicked on a specific window.
    case window(SCWindow)
    /// User clicked on the desktop (no window under cursor) — caller should fullscreen-capture the display.
    case desktop
}

// MARK: - KeyableWindow

/// Borderless NSWindow subclass that can become key.
/// Standard borderless windows return false for canBecomeKey, which prevents
/// keyDown events (Escape) from reaching the content view.
private class KeyableWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}

// MARK: - WindowSelectionCoordinator

/// Manages the lifecycle of the window-selection overlay across all connected displays.
///
/// Usage:
/// ```swift
/// let coordinator = WindowSelectionCoordinator()
/// if let result = await coordinator.beginWindowSelection() {
///     // .window(SCWindow) or .desktop
/// }
/// // nil means the user cancelled with Escape
/// ```
@MainActor
final class WindowSelectionCoordinator {

    // MARK: - State

    /// Retains overlay windows for the duration of the selection to prevent ARC deallocation.
    private var overlayWindows: [(KeyableWindow, WindowSelectionOverlayView)] = []

    /// SCWindow list fetched once before showing the overlay (research Pitfall 6: avoids
    /// repeated async fetches on every mouseMoved, which would cause lag).
    private(set) var cachedWindows: [SCWindow] = []

    // MARK: - Public API

    /// Presents full-screen dimming overlays on every connected display, waits for the user
    /// to click a window or cancel, then returns the result.
    ///
    /// - Returns: `.window(SCWindow)` if the user clicked a window, `.desktop` if the user
    ///   clicked an area with no window, or `nil` if the user pressed Escape to cancel.
    func beginWindowSelection() async -> WindowSelectionResult? {
        // 1. Fetch window list ONCE before showing overlay (avoids per-mouseMoved async latency)
        //
        // excludingDesktopWindows: true — excludes the Finder's Desktop window which covers
        // the entire screen and would always win hit-testing if included.
        // windowLayer == 0 — normal application windows only (excludes menu bar, system overlays).
        //
        // CGWindowListCopyWindowInfo provides correct front-to-back z-ordering which
        // SCShareableContent does NOT guarantee. We sort SCWindows by their position
        // in the CGWindowList so findWindow(at:) returns the frontmost match.
        let content = try? await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
        let ownBundleID = Bundle.main.bundleIdentifier
        let scWindows = (content?.windows ?? []).filter {
            $0.isOnScreen
                && $0.windowLayer == 0
                && $0.owningApplication?.bundleIdentifier != ownBundleID
        }

        // Get front-to-back z-order from CGWindowList
        let windowList = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
        let orderedIDs = windowList.compactMap { $0[kCGWindowNumber as String] as? CGWindowID }

        // Sort SCWindows by CGWindowList z-order (front-to-back)
        let idToWindow = Dictionary(scWindows.map { ($0.windowID, $0) }, uniquingKeysWith: { first, _ in first })
        cachedWindows = orderedIDs.compactMap { idToWindow[$0] }

        // 2. Load camera cursor and push it globally so it appears on ALL screens.
        // Do NOT rely on per-window cursor rects (addCursorRect) — those only activate
        // on the key window, so the cursor would vanish when moving to other screens.
        let cursor = makeWindowSelectionCursor()
        cursor.push()

        // 3. Create overlay windows for each screen
        let windows = createOverlayWindows(cursor: cursor)
        overlayWindows = windows

        // 5. Order all overlay windows to front
        for (window, _) in windows {
            window.orderFrontRegardless()
        }

        // 6. Activate the app so makeKey() works from .accessory mode / hotkey path
        NSApp.activate(ignoringOtherApps: true)

        // Make the window under the current cursor key so it receives keyboard events (Escape).
        // Use NSMouseInRect instead of CGRect.contains — contains() is exclusive on maxX/maxY.
        let mouseLocation = NSEvent.mouseLocation
        if let (keyWindow, _) = windows.first(where: { NSMouseInRect(mouseLocation, $0.0.frame, false) }) {
            keyWindow.makeKey()
        } else if let (firstWindow, _) = windows.first {
            firstWindow.makeKey()
        }

        // 7. Bridge callback to async — first view to respond wins (guards against double-resume)
        let result: WindowSelectionResult? = await withCheckedContinuation { continuation in
            var resumed = false

            for (_, view) in windows {
                view.onWindowSelected = { [weak self] selectionResult in
                    guard let self else { return }
                    guard !resumed else { return }
                    resumed = true

                    // Tear-down must happen on the main actor (we are here since
                    // mouse events arrive on the main thread and this closure is
                    // called synchronously from mouseDown/keyDown).
                    self.tearDown()
                    continuation.resume(returning: selectionResult)
                }
            }
        }

        return result
    }

    // MARK: - Window Hit-Testing

    /// Returns the frontmost visible SCWindow whose frame contains the given CG-coordinate point.
    ///
    /// - Parameter cgPoint: The cursor position in CG coordinates (top-left origin, y increases down).
    /// - Returns: The topmost SCWindow containing the point, or nil if no window is present (desktop).
    func findWindow(at cgPoint: CGPoint) -> SCWindow? {
        // cachedWindows ordering: SCShareableContent returns front-to-back when onScreenWindowsOnly = true.
        // First match is the topmost window.
        return cachedWindows.first { $0.frame.contains(cgPoint) }
    }

    // MARK: - Coordinate Conversion

    /// Converts a point from AppKit screen coordinates to CoreGraphics coordinates.
    ///
    /// AppKit: origin at bottom-left of primary display, y increases upward.
    /// CoreGraphics / SCWindow.frame: origin at top-left of primary display, y increases downward.
    func appKitPointToCG(_ appKitPoint: CGPoint) -> CGPoint {
        guard let primaryHeight = NSScreen.screens.first?.frame.height else { return appKitPoint }
        return CGPoint(x: appKitPoint.x, y: primaryHeight - appKitPoint.y)
    }

    /// Converts an SCWindow frame from CG coordinates to AppKit screen coordinates.
    ///
    /// - Parameter cgFrame: The frame in CG coordinates (top-left origin, y-down).
    /// - Returns: The equivalent frame in AppKit coordinates (bottom-left origin, y-up).
    func cgFrameToAppKit(_ cgFrame: CGRect) -> CGRect {
        guard let primaryHeight = NSScreen.screens.first?.frame.height else { return cgFrame }
        // CG frame's top edge in CG is cgFrame.minY; its AppKit y-origin is below that edge.
        let appKitY = primaryHeight - cgFrame.maxY
        return CGRect(x: cgFrame.origin.x, y: appKitY, width: cgFrame.width, height: cgFrame.height)
    }

    // MARK: - Window Creation

    private func createOverlayWindows(cursor: NSCursor) -> [(KeyableWindow, WindowSelectionOverlayView)] {
        NSScreen.screens.map { screen in
            // KeyableWindow allows becoming key — required for keyDown (Escape) to fire.
            // Do NOT pass `screen:` — on macOS 15 the system may adjust the window
            // position to "fit" the specified screen, breaking multi-screen placement.
            let window = KeyableWindow(
                contentRect: .zero,
                styleMask: [.borderless],
                backing: .buffered,
                defer: false
            )

            // Visual configuration — mirrors OverlayCoordinator
            window.level = .screenSaver
            window.backgroundColor = .clear
            window.isOpaque = false
            window.hasShadow = false
            window.ignoresMouseEvents = false
            window.isReleasedWhenClosed = false
            window.acceptsMouseMovedEvents = true  // Required for mouseMoved events (Pitfall 5)

            // Allow overlay to appear over fullscreen spaces
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

            // Exclude this window from ScreenCaptureKit capture
            window.sharingType = .none

            // Explicitly position the window on the correct screen using global coordinates
            window.setFrame(screen.frame, display: true)

            // Create the overlay view covering the whole screen
            let view = WindowSelectionOverlayView(frame: CGRect(origin: .zero, size: screen.frame.size))
            view.owningScreen = screen
            view.coordinator = self
            view.cameraCursor = cursor  // Used in resetCursorRects for proper cursor display
            window.contentView = view

            // The view needs to be first responder to receive key events (Escape)
            window.makeFirstResponder(view)

            return (window, view)
        }
    }

    // MARK: - Camera Cursor

    /// Loads the system's window-selection camera cursor from the HIServices framework.
    ///
    /// Falls back to `.arrow` if the private framework path has changed (future macOS versions).
    /// Source: codejam.info/2023/07/macos-harvest-cursor-from-any-app.html
    private func makeWindowSelectionCursor() -> NSCursor {
        let base = "/System/Library/Frameworks/ApplicationServices.framework" +
                   "/Versions/A/Frameworks/HIServices.framework" +
                   "/Versions/A/Resources/cursors/screenshotwindow"

        guard
            let plistData = FileManager.default.contents(atPath: "\(base)/info.plist"),
            let plist = try? PropertyListSerialization.propertyList(
                from: plistData, options: [], format: nil
            ) as? [String: Any],
            let pdfData = try? Data(contentsOf: URL(fileURLWithPath: "\(base)/cursor.pdf")),
            let image = NSImage(data: pdfData)
        else {
            // Fallback: HIServices path changed or unavailable
            return .arrow
        }

        let hotX = (plist["hotx"] as? Int).map(CGFloat.init) ?? image.size.width / 2
        let hotY = (plist["hoty"] as? Int).map(CGFloat.init) ?? image.size.height / 2
        return NSCursor(image: image, hotSpot: NSPoint(x: hotX, y: hotY))
    }

    // MARK: - Window Raising

    /// Brings the hovered window to the front (below overlay) via the Accessibility API.
    ///
    /// Uses kAXRaiseAction which raises the window within its app's z-order without
    /// activating the app or stealing focus. Silently does nothing if Accessibility
    /// permission is not granted — the dimming highlight still works as fallback.
    func raiseWindow(_ scWindow: SCWindow) {
        guard let pid = scWindow.owningApplication?.processID else { return }

        let appElement = AXUIElementCreateApplication(pid)
        var windowListRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(appElement, kAXWindowsAttribute as CFString, &windowListRef) == .success,
              let axWindows = windowListRef as? [AXUIElement] else { return }

        let targetFrame = scWindow.frame

        for axWindow in axWindows {
            var posRef: CFTypeRef?
            var sizeRef: CFTypeRef?
            AXUIElementCopyAttributeValue(axWindow, kAXPositionAttribute as CFString, &posRef)
            AXUIElementCopyAttributeValue(axWindow, kAXSizeAttribute as CFString, &sizeRef)

            var position = CGPoint.zero
            var size = CGSize.zero
            if let pv = posRef { AXValueGetValue(pv as! AXValue, .cgPoint, &position) }
            if let sv = sizeRef { AXValueGetValue(sv as! AXValue, .cgSize, &size) }

            // Match AX window to SCWindow by frame (both use CG coordinates)
            if abs(position.x - targetFrame.origin.x) < 2
                && abs(position.y - targetFrame.origin.y) < 2
                && abs(size.width - targetFrame.width) < 2
                && abs(size.height - targetFrame.height) < 2 {
                AXUIElementPerformAction(axWindow, kAXRaiseAction as CFString)
                break
            }
        }
    }

    // MARK: - Tear-Down

    private func tearDown() {
        NSCursor.pop()

        for (window, _) in overlayWindows {
            window.orderOut(nil)
            window.close()
        }

        overlayWindows = []
        cachedWindows = []
    }
}
