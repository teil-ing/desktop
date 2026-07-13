import AppKit
import QuartzCore

// MARK: - SelectionOverlayView

/// NSView subclass that covers one display and handles all drawing and mouse
/// interaction for interactive region selection.
///
/// Responsibilities:
/// - Full-screen semi-transparent dimming with a clear cutout over the selection
/// - Full-screen crosshair guide lines that track the mouse
/// - Marching-ants animated selection border
/// - Live pixel-dimension label near the selection corner
/// - Mouse tracking loop: drag to select, Escape / right-click to cancel
/// - Minimum 10×10 backing-pixel validation (smaller = cancel)
final class SelectionOverlayView: NSView {

    // MARK: - Public interface

    /// The screen this view covers. Set by OverlayCoordinator immediately after init.
    var owningScreen: NSScreen?

    /// Back-reference to the coordinator for cross-screen selection broadcasting.
    weak var coordinator: OverlayCoordinator?

    /// Called once when the user finishes or cancels. CGRect is in global screen
    /// coordinates (origin at bottom-left of primary display). nil means cancelled.
    var onSelectionComplete: ((CGRect?) -> Void)?

    // MARK: - Layer references

    private let dimmingLayer = CAShapeLayer()
    private let selectionLayer = CAShapeLayer()
    private let guideHorizontalLayer = CAShapeLayer()
    private let guideVerticalLayer = CAShapeLayer()

    // MARK: - Dimension label

    private let dimensionLabel = NSTextField(labelWithString: "")
    private let dimensionBackground = NSView()

    // MARK: - State

    private var currentMousePoint: CGPoint = .zero
    private var selectionRect: CGRect = .zero

    // MARK: - Init

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupLayers()
        setupDimensionLabel()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    // MARK: - First responder

    override var acceptsFirstResponder: Bool { true }

    /// Accept the first mouse click without requiring window activation first.
    /// Without this, the first click only activates the overlay window and is swallowed.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: - Layer setup

    private func setupLayers() {
        wantsLayer = true
        guard let root = layer else { return }

        // --- Dimming layer (evenOdd fill: dims everything outside selection) ---
        dimmingLayer.fillRule = .evenOdd
        dimmingLayer.fillColor = NSColor.black.withAlphaComponent(0.35).cgColor
        dimmingLayer.frame = root.bounds
        dimmingLayer.autoresizingMask = [.layerWidthSizable, .layerHeightSizable]
        root.addSublayer(dimmingLayer)

        // Set initial path covering entire view (no selection yet → everything dimmed)
        updateDimmingPath(selection: .zero)

        // --- Crosshair guide lines ---
        let guideColor = NSColor.white.withAlphaComponent(0.40).cgColor

        guideHorizontalLayer.strokeColor = guideColor
        guideHorizontalLayer.lineWidth = 0.5
        guideHorizontalLayer.fillColor = nil
        root.addSublayer(guideHorizontalLayer)

        guideVerticalLayer.strokeColor = guideColor
        guideVerticalLayer.lineWidth = 0.5
        guideVerticalLayer.fillColor = nil
        root.addSublayer(guideVerticalLayer)

        // --- Marching-ants selection border ---
        selectionLayer.strokeColor = NSColor.white.cgColor
        selectionLayer.fillColor = NSColor.clear.cgColor
        selectionLayer.lineWidth = 1.5
        selectionLayer.lineDashPattern = [6, 4]
        selectionLayer.isHidden = true
        root.addSublayer(selectionLayer)

        // Marching-ants animation
        let dash = CABasicAnimation(keyPath: "lineDashPhase")
        dash.fromValue = 0
        dash.toValue = -(6.0 + 4.0)   // sum of dash + gap
        dash.duration = 0.5
        dash.repeatCount = .infinity
        selectionLayer.add(dash, forKey: "marchingAnts")
    }

    private func setupDimensionLabel() {
        // Background pill
        dimensionBackground.wantsLayer = true
        dimensionBackground.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.70).cgColor
        dimensionBackground.layer?.cornerRadius = 5
        dimensionBackground.isHidden = true
        addSubview(dimensionBackground)

        // Label
        dimensionLabel.font = .monospacedDigitSystemFont(ofSize: 12, weight: .medium)
        dimensionLabel.textColor = .white
        dimensionLabel.isBezeled = false
        dimensionLabel.drawsBackground = false
        dimensionLabel.isEditable = false
        dimensionLabel.isSelectable = false
        dimensionLabel.alignment = .center
        dimensionBackground.addSubview(dimensionLabel)
    }

    // MARK: - Layout

    override func layout() {
        super.layout()
        dimmingLayer.frame = bounds
        // Refresh paths when bounds change
        updateDimmingPath(selection: selectionRect)
        updateGuideLines(at: currentMousePoint)
    }

    // MARK: - Layer update helpers

    private func updateDimmingPath(selection: CGRect) {
        let path = CGMutablePath()
        path.addRect(bounds)                           // outer rect
        if selection.width > 0 && selection.height > 0 {
            path.addRect(selection)                    // inner hole
        }
        // CALayer animations are disabled for instant dimming updates
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        dimmingLayer.path = path
        CATransaction.commit()
    }

    private func updateSelectionLayer(rect: CGRect) {
        if rect.width < 1 || rect.height < 1 {
            selectionLayer.isHidden = true
            return
        }
        let path = CGPath(rect: rect, transform: nil)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        selectionLayer.path = path
        selectionLayer.isHidden = false
        CATransaction.commit()
    }

    private func updateGuideLines(at point: CGPoint) {
        let h = CGMutablePath()
        h.move(to: CGPoint(x: 0, y: point.y))
        h.addLine(to: CGPoint(x: bounds.width, y: point.y))

        let v = CGMutablePath()
        v.move(to: CGPoint(x: point.x, y: 0))
        v.addLine(to: CGPoint(x: point.x, y: bounds.height))

        CATransaction.begin()
        CATransaction.setDisableActions(true)
        guideHorizontalLayer.path = h
        guideVerticalLayer.path = v
        CATransaction.commit()
    }

    private func updateDimensionLabel(rect: CGRect) {
        guard rect.width >= 1 && rect.height >= 1 else {
            dimensionBackground.isHidden = true
            return
        }

        let scale = window?.backingScaleFactor ?? 1.0
        let pw = Int(rect.width * scale)
        let ph = Int(rect.height * scale)
        dimensionLabel.stringValue = "\(pw) × \(ph)"
        dimensionLabel.sizeToFit()

        let padding: CGFloat = 8
        let labelSize = dimensionLabel.frame.size
        let bgSize = CGSize(width: labelSize.width + padding * 2,
                            height: labelSize.height + padding)

        // Prefer: 20pt below and to the right of bottom-right corner of selection
        // (In AppKit view coordinates, y increases upward, so "below" = lower y)
        let margin: CGFloat = 20
        var origin = CGPoint(x: rect.maxX + margin, y: rect.minY - margin - bgSize.height)

        // Clamp to stay within view bounds
        if origin.x + bgSize.width > bounds.width {
            origin.x = rect.minX - bgSize.width - margin
        }
        if origin.y < 0 {
            origin.y = rect.maxY + margin
        }
        origin.x = max(0, min(origin.x, bounds.width - bgSize.width))
        origin.y = max(0, min(origin.y, bounds.height - bgSize.height))

        dimensionBackground.frame = CGRect(origin: origin, size: bgSize)
        dimensionLabel.frame = CGRect(
            x: padding,
            y: (bgSize.height - labelSize.height) / 2,
            width: labelSize.width,
            height: labelSize.height
        )
        dimensionBackground.isHidden = false
    }

    // MARK: - Cursor rects

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .crosshair)
    }

    // MARK: - Cancel (pre-drag)

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 {  // Escape
            onSelectionComplete?(nil)
        }
    }

    override func rightMouseDown(with event: NSEvent) {
        onSelectionComplete?(nil)
    }

    // MARK: - Mouse moved (pre-drag crosshair)

    override func mouseMoved(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        currentMousePoint = point
        updateGuideLines(at: point)
    }

    // MARK: - Mouse down (tracking loop)

    override func mouseDown(with event: NSEvent) {
        let startPoint = convert(event.locationInWindow, from: nil)
        currentMousePoint = startPoint
        selectionRect = .zero
        var finalRect: CGRect? = nil

        // Event tracking loop — runs until mouseUp, Escape, or right-click
        while let next = window?.nextEvent(matching: [
            .leftMouseDragged,
            .leftMouseUp,
            .rightMouseDown,
            .keyDown
        ]) {
            switch next.type {
            case .leftMouseDragged:
                let current = convert(next.locationInWindow, from: nil)
                currentMousePoint = current
                let raw = CGRect(
                    x: min(startPoint.x, current.x),
                    y: min(startPoint.y, current.y),
                    width: abs(current.x - startPoint.x),
                    height: abs(current.y - startPoint.y)
                )
                selectionRect = raw
                updateDimmingPath(selection: raw)
                updateSelectionLayer(rect: raw)
                updateGuideLines(at: current)
                updateDimensionLabel(rect: raw)
                // Broadcast to other screens so they show their portion of the selection
                let globalRect = convertToScreenCoordinates(raw)
                coordinator?.broadcastSelection(globalRect: globalRect, from: self)

            case .leftMouseUp:
                let current = convert(next.locationInWindow, from: nil)
                let raw = CGRect(
                    x: min(startPoint.x, current.x),
                    y: min(startPoint.y, current.y),
                    width: abs(current.x - startPoint.x),
                    height: abs(current.y - startPoint.y)
                )
                let scale = window?.backingScaleFactor ?? 1.0
                if raw.width * scale >= 10 && raw.height * scale >= 10 {
                    // Convert view rect to global screen coordinates
                    finalRect = convertToScreenCoordinates(raw)
                }
                // Under threshold → treat as cancel (finalRect stays nil)

            case .rightMouseDown:
                break   // cancel

            case .keyDown:
                if next.keyCode == 53 { break }  // Escape → cancel
                continue                          // other keys: ignore

            default:
                continue
            }

            // Break out of loop for mouseUp, rightMouseDown, and Escape
            if next.type == .leftMouseUp || next.type == .rightMouseDown { break }
            if next.type == .keyDown && next.keyCode == 53 { break }
        }

        // Tear down visual state
        selectionRect = .zero
        updateDimmingPath(selection: .zero)
        selectionLayer.isHidden = true
        dimensionBackground.isHidden = true
        // Clear cross-screen selection on all other views
        coordinator?.broadcastSelection(globalRect: nil, from: self)

        onSelectionComplete?(finalRect)
    }

    // MARK: - Cross-screen selection

    /// Shows the portion of a cross-screen selection that intersects this view.
    /// Called by OverlayCoordinator when another screen's view is actively dragging.
    ///
    /// - Parameter localRect: The selection rect in this view's local coordinate space.
    ///   May extend beyond bounds — layers will clip naturally.
    func showCrossScreenSelection(_ localRect: CGRect) {
        updateDimmingPath(selection: localRect)
        updateSelectionLayer(rect: localRect)
    }

    /// Clears any cross-screen selection visual state.
    func clearCrossScreenSelection() {
        updateDimmingPath(selection: .zero)
        selectionLayer.isHidden = true
    }

    // MARK: - Coordinate conversion

    /// Converts a rect in this view's coordinate space to global screen coordinates.
    ///
    /// AppKit view coordinates have origin at bottom-left; NSScreen.frame also has
    /// origin at bottom-left of the primary display, so the conversion is:
    /// screenPoint = window.frame.origin + viewPoint.
    private func convertToScreenCoordinates(_ rect: CGRect) -> CGRect {
        guard let window else { return rect }
        let originInScreen = window.convertPoint(toScreen: rect.origin)
        // convertPoint(toScreen:) flips y already — rect size is unchanged
        return CGRect(origin: originInScreen, size: rect.size)
    }
}
