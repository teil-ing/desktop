import AppKit

// MARK: - KeyableOverlayWindow

/// Borderless NSWindow subclass that can become key.
/// Standard borderless windows return false for canBecomeKey, which prevents
/// keyDown events (Escape) from reaching the content view.
private class KeyableOverlayWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}

// MARK: - OverlayCoordinator

/// Manages the lifecycle of selection overlay windows across all connected displays.
///
/// Usage:
/// ```swift
/// let coordinator = OverlayCoordinator()
/// let rect = await coordinator.beginRegionSelection()
/// // rect is in global screen coordinates, or nil if the user cancelled
/// ```
@MainActor
final class OverlayCoordinator {

    // MARK: - State

    /// Retains overlay windows for the duration of the selection to prevent ARC deallocation.
    private var overlayWindows: [(KeyableOverlayWindow, SelectionOverlayView)] = []

    // MARK: - Public API

    /// Presents full-screen crosshair-selection overlays on every connected display,
    /// waits for the user to drag a selection or cancel, then returns the result.
    ///
    /// - Returns: The selected region in global screen coordinates (origin at
    ///   bottom-left of the primary display), or `nil` if the user cancelled.
    func beginRegionSelection() async -> CGRect? {
        let windows = createOverlayWindows()
        overlayWindows = windows

        // Push crosshair cursor before showing windows
        NSCursor.crosshair.push()

        // Order all overlay windows to the front
        for (window, _) in windows {
            window.orderFrontRegardless()
        }

        // Activate the app so makeKey() actually works — without this,
        // keyboard events (Escape) are not delivered to borderless windows
        // when the app is in .accessory mode or was backgrounded.
        NSApp.activate(ignoringOtherApps: true)

        // Make the window under the current cursor the key window so it receives
        // keyboard events (Escape) immediately. Use NSMouseInRect instead of
        // CGRect.contains — contains() is exclusive on maxX/maxY boundaries.
        let mouseLocation = NSEvent.mouseLocation
        if let (keyWindow, _) = windows.first(where: { (window, _) in
            NSMouseInRect(mouseLocation, window.frame, false)
        }) {
            keyWindow.makeKey()
        }

        // Bridge the callback-based result to async/await.
        // Only the first view to call the callback wins (guards against double-resume).
        let result: CGRect? = await withCheckedContinuation { continuation in
            var resumed = false

            for (_, view) in windows {
                view.onSelectionComplete = { [weak self] rect in
                    guard let self else { return }
                    guard !resumed else { return }
                    resumed = true

                    // Tear-down must happen on the main actor (already here since
                    // mouse events arrive on the main thread and this closure is
                    // called synchronously from mouseDown).
                    self.tearDown()
                    continuation.resume(returning: rect)
                }
            }
        }

        return result
    }

    // MARK: - Cross-screen selection broadcast

    /// Updates all overlay views except the sender with the current selection.
    /// Each view receives the selection converted to its local coordinate space.
    ///
    /// - Parameters:
    ///   - globalRect: The selection in global screen coordinates, or nil to clear.
    ///   - sender: The view that originated the selection (excluded from updates).
    func broadcastSelection(globalRect: CGRect?, from sender: SelectionOverlayView) {
        for (window, view) in overlayWindows where view !== sender {
            if let globalRect {
                // Convert global rect to view-local coordinates.
                // For borderless windows: view (0,0) == window frame origin.
                let localOrigin = CGPoint(
                    x: globalRect.origin.x - window.frame.origin.x,
                    y: globalRect.origin.y - window.frame.origin.y
                )
                let localRect = CGRect(origin: localOrigin, size: globalRect.size)
                view.showCrossScreenSelection(localRect)
            } else {
                view.clearCrossScreenSelection()
            }
        }
    }

    // MARK: - Window creation

    private func createOverlayWindows() -> [(KeyableOverlayWindow, SelectionOverlayView)] {
        NSScreen.screens.map { screen in
            // Create a KeyableOverlayWindow so it can receive keyboard events (Escape).
            // Do NOT pass `screen:` — on macOS 15 the system may adjust the window
            // position to "fit" the specified screen, breaking multi-screen placement.
            // Instead, set the frame explicitly after creation.
            let window = KeyableOverlayWindow(
                contentRect: .zero,
                styleMask: [.borderless],
                backing: .buffered,
                defer: false
            )

            // Visual configuration
            window.level = .screenSaver
            window.backgroundColor = .clear
            window.isOpaque = false
            window.hasShadow = false
            window.ignoresMouseEvents = false
            window.isReleasedWhenClosed = false
            window.acceptsMouseMovedEvents = true

            // Allow overlay to appear over fullscreen spaces
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

            // Exclude this window from ScreenCaptureKit capture (macOS 13/14).
            // On macOS 15 the SCContentFilter handles exclusion on the capture side.
            window.sharingType = .none

            // Explicitly position the window on the correct screen using global coordinates
            window.setFrame(screen.frame, display: true)

            // Create the overlay view covering the whole screen
            let view = SelectionOverlayView(frame: CGRect(origin: .zero, size: screen.frame.size))
            view.owningScreen = screen
            view.coordinator = self
            window.contentView = view

            // The view needs to be first responder so it receives key events (Escape)
            window.makeFirstResponder(view)

            return (window, view)
        }
    }

    // MARK: - Tear-down

    private func tearDown() {
        NSCursor.pop()

        for (window, _) in overlayWindows {
            window.orderOut(nil)
            window.close()
        }

        overlayWindows = []
    }
}
