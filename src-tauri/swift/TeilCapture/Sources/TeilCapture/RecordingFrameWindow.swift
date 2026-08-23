import AppKit

/// A non-interactive red border drawn just outside a recorded region so the user
/// can see exactly what is being captured. Excluded from the recording itself via
/// `sharingType = .none` (and, on 15+, by the own-bundle SCContentFilter exclusion).
@MainActor
final class RecordingFrameWindow {
    private var window: NSWindow?

    /// Created from nonisolated contexts (the controller); all real work is @MainActor.
    nonisolated init() {}

    /// Shows the border around `rect` (global AppKit coordinates, y-up).
    func show(around rect: CGRect) {
        hide()
        let inset: CGFloat = 3
        let frame = rect.insetBy(dx: -inset, dy: -inset)

        let win = NSWindow(
            contentRect: frame, styleMask: .borderless, backing: .buffered, defer: false)
        win.isOpaque = false
        win.backgroundColor = .clear
        win.hasShadow = false
        win.ignoresMouseEvents = true
        win.level = .screenSaver
        win.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        win.sharingType = .none

        let view = FrameView(frame: NSRect(origin: .zero, size: frame.size))
        view.borderInset = inset
        win.contentView = view
        win.orderFrontRegardless()
        window = win
    }

    func hide() {
        window?.orderOut(nil)
        window = nil
    }

    private final class FrameView: NSView {
        var borderInset: CGFloat = 3
        override var isFlipped: Bool { false }
        override func draw(_ dirtyRect: NSRect) {
            let stroke: CGFloat = 2
            let rect = bounds.insetBy(dx: borderInset - stroke / 2, dy: borderInset - stroke / 2)
            let path = NSBezierPath(roundedRect: rect, xRadius: 4, yRadius: 4)
            path.lineWidth = stroke
            NSColor(srgbRed: 1.0, green: 0.23, blue: 0.19, alpha: 0.95).setStroke()
            path.stroke()
        }
    }
}
