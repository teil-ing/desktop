import CoreGraphics
import Foundation

// MARK: - CaptureResult

/// Value type wrapping a captured CGImage with metadata about the capture operation.
///
/// CGImage is a CoreFoundation type that is thread-safe, so CaptureResult
/// can be safely passed across actor and task boundaries.
struct CaptureResult: Sendable {
    /// The captured image.
    let image: CGImage

    /// The captured rectangle in global screen coordinates (AppKit coordinate space,
    /// origin at bottom-left of primary display, Y increasing upward).
    let capturedRect: CGRect
}
