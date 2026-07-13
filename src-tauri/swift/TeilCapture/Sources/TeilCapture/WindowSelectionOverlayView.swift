import AppKit
import QuartzCore
@preconcurrency import ScreenCaptureKit

// MARK: - WindowSelectionOverlayView

/// NSView subclass that covers one display and handles hover-highlighting and click-to-select
/// for the window capture mode.
///
/// Responsibilities:
/// - Full-screen semi-transparent dimming with a bright cutout over the hovered window
/// - Camera cursor (managed by WindowSelectionCoordinator via NSCursor push/pop)
/// - Click on window → reports `.window(SCWindow)` via callback
/// - Click on desktop (no window) → reports `.desktop` via callback
/// - Escape key → reports `nil` (cancel) via callback
final class WindowSelectionOverlayView: NSView {

    // MARK: - Public Interface

    /// The screen this view covers. Set by WindowSelectionCoordinator immediately after init.
    var owningScreen: NSScreen?

    /// Called once when the user selects a window, clicks the desktop, or cancels.
    /// - `.window(SCWindow)` — user clicked on a specific window
    /// - `.desktop` — user clicked where there is no window
    /// - `nil` — user pressed Escape to cancel
    var onWindowSelected: ((WindowSelectionResult?) -> Void)?

    /// Reference to the coordinator for window hit-testing and coordinate conversion.
    /// Weak to avoid a reference cycle between coordinator (which holds the view pair) and the view.
    weak var coordinator: WindowSelectionCoordinator?

    /// Camera cursor set by WindowSelectionCoordinator, used in resetCursorRects
    /// for the key window. The coordinator also pushes this cursor globally so it
    /// appears on non-key overlay windows (other screens).
    var cameraCursor: NSCursor?

    // MARK: - Layers

    /// evenOdd fill: dims everything except the highlighted window rect (the "hole").
    private let dimmingLayer = CAShapeLayer()

    // MARK: - State

    /// The SCWindow currently under the cursor, used to avoid redundant layer updates.
    private var highlightedWindow: SCWindow?

    // MARK: - Init

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupLayers()
        setupTrackingArea()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    // MARK: - First Responder

    override var acceptsFirstResponder: Bool { true }

    /// Accept the first mouse click without requiring window activation first.
    /// Without this, the first click only activates the overlay window and is swallowed.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: - Layer Setup

    private func setupLayers() {
        wantsLayer = true
        guard let root = layer else { return }

        // Dimming layer — evenOdd fill rule creates a "hole" where the highlighted window is
        dimmingLayer.fillRule = .evenOdd
        dimmingLayer.fillColor = NSColor.black.withAlphaComponent(0.35).cgColor
        dimmingLayer.frame = root.bounds
        dimmingLayer.autoresizingMask = [.layerWidthSizable, .layerHeightSizable]
        root.addSublayer(dimmingLayer)

        // Initial state: entire view is dimmed (no highlight hole yet)
        updateDimmingPath(highlightRect: nil)
    }

    private func setupTrackingArea() {
        // .activeAlways is REQUIRED — .activeInKeyWindow would miss events since
        // the overlay may not be key during hover (research Pitfall 5).
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseMoved, .activeAlways, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
    }

    // MARK: - Layout

    override func layout() {
        super.layout()
        dimmingLayer.frame = bounds
        // Refresh highlight rect on layout changes (e.g., display resolution change)
        if let window = highlightedWindow, let coord = coordinator {
            let highlightRect = viewLocalRect(for: window, coordinator: coord)
            updateDimmingPath(highlightRect: highlightRect)
        } else {
            updateDimmingPath(highlightRect: nil)
        }
    }

    // MARK: - Dimming Path

    /// Updates the dimming layer path. The highlightRect (if provided) is the "bright" hole.
    ///
    /// evenOdd fill rule: the outer rect is filled, and the inner rect (if present) subtracts
    /// from the fill — leaving that area transparent (undimmed).
    private func updateDimmingPath(highlightRect: CGRect?) {
        let path = CGMutablePath()
        path.addRect(bounds)
        if let rect = highlightRect, rect.width > 0, rect.height > 0 {
            path.addRect(rect)  // inner hole — this area stays bright
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        dimmingLayer.path = path
        CATransaction.commit()
    }

    // MARK: - Coordinate Helpers

    /// Converts an SCWindow's CG-coordinate frame to a rect in this view's local coordinate space.
    private func viewLocalRect(for scWindow: SCWindow, coordinator: WindowSelectionCoordinator) -> CGRect {
        // Step 1: CG frame → global AppKit screen coords
        let globalAppKitRect = coordinator.cgFrameToAppKit(scWindow.frame)
        // Step 2: Global AppKit → view-local coords
        // The overlay window's frame is set to screen.frame in global AppKit coords.
        // Subtract the window origin to get view-local coordinates.
        guard let win = window else { return .zero }
        return CGRect(
            x: globalAppKitRect.origin.x - win.frame.origin.x,
            y: globalAppKitRect.origin.y - win.frame.origin.y,
            width: globalAppKitRect.width,
            height: globalAppKitRect.height
        )
    }

    // MARK: - Cursor Rects

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: cameraCursor ?? .crosshair)
    }

    // MARK: - Mouse Moved

    override func mouseMoved(with event: NSEvent) {
        // Force camera cursor on every mouse move. Cursor rects (resetCursorRects)
        // only work on the key window — this ensures the camera cursor appears on
        // ALL screens since the tracking area uses .activeAlways.
        (cameraCursor ?? .crosshair).set()

        guard let coord = coordinator else { return }

        // Convert event location to CG coordinates for SCWindow frame hit-testing
        let viewPoint = convert(event.locationInWindow, from: nil)
        let cgPoint = screenToCGPoint(viewPoint: viewPoint, coordinator: coord)

        // Find the window under the cursor
        let foundWindow = coord.findWindow(at: cgPoint)

        // Only update layer if the hovered window changed (avoids redundant CATransaction work)
        let foundID = foundWindow?.windowID
        let currentID = highlightedWindow?.windowID
        guard foundID != currentID else { return }

        highlightedWindow = foundWindow

        if let win = foundWindow {
            // Raise the window to front (below overlay) so it's fully visible through the hole
            coord.raiseWindow(win)
            let highlightRect = viewLocalRect(for: win, coordinator: coord)
            updateDimmingPath(highlightRect: highlightRect)
        } else {
            // No window under cursor (desktop) — dim everything
            updateDimmingPath(highlightRect: nil)
        }
    }

    // MARK: - Mouse Down

    override func mouseDown(with event: NSEvent) {
        guard let coord = coordinator else {
            onWindowSelected?(nil)
            return
        }

        // Convert click to CG coordinates for window hit-testing
        let viewPoint = convert(event.locationInWindow, from: nil)
        let cgPoint = screenToCGPoint(viewPoint: viewPoint, coordinator: coord)

        let foundWindow = coord.findWindow(at: cgPoint)

        if let scWindow = foundWindow {
            onWindowSelected?(.window(scWindow))
        } else {
            // No window under cursor — user clicked desktop
            onWindowSelected?(.desktop)
        }
    }

    // MARK: - Right Mouse Down (Cancel)

    override func rightMouseDown(with event: NSEvent) {
        onWindowSelected?(nil)
    }

    // MARK: - Key Down

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 {  // Escape
            onWindowSelected?(nil)
        }
    }

    // MARK: - Coordinate Conversion

    /// Converts a point from this view's local coordinate space to CG screen coordinates.
    ///
    /// Pipeline:
    ///   view local → overlay window screen point (AppKit) → CG (top-left origin, y-down)
    private func screenToCGPoint(viewPoint: CGPoint, coordinator: WindowSelectionCoordinator) -> CGPoint {
        guard let win = window else {
            // Fallback: treat as already in screen space
            return coordinator.appKitPointToCG(viewPoint)
        }
        // Convert from view-local to global AppKit screen coordinates
        let screenPoint = win.convertPoint(toScreen: viewPoint)
        // Convert from AppKit (bottom-left origin, y-up) to CG (top-left origin, y-down)
        return coordinator.appKitPointToCG(screenPoint)
    }
}
