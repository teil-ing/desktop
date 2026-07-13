import CoreGraphics
import Foundation

// MARK: - CrossMonitorStitcher

/// Namespace for CGContext-based stitching of per-display captures into a single CGImage.
///
/// Used when a region capture spans multiple displays. Each display's portion is
/// captured separately then composited into a canvas sized to the total capture rect.
///
/// When displays have different backingScaleFactors (e.g. Retina 2x + non-Retina 1x),
/// the canvas resolution is determined by the highest-resolution display. The lower-
/// resolution portion will be upscaled, which may appear slightly softer.
enum CrossMonitorStitcher {

    // MARK: - DisplayCapture

    /// A single per-display capture, with geometry in global screen coordinates.
    struct DisplayCapture: Sendable {
        /// The captured image for this display portion.
        let image: CGImage

        /// The captured rectangle in global AppKit screen coordinates (origin bottom-left
        /// of primary display). This is the intersection of the requested rect and the
        /// display's frame.
        let displayFrame: CGRect

        /// The display's backing scale factor (from NSScreen.backingScaleFactor).
        let scale: CGFloat
    }

    // MARK: - Stitching

    /// Composites an array of per-display captures into a single CGImage.
    ///
    /// The output canvas dimensions are in backing pixels of the highest-resolution
    /// display. Each capture is positioned according to its global-coordinate frame
    /// relative to the total rect's origin.
    ///
    /// - Parameters:
    ///   - captures: Per-display captures to composite.
    ///   - totalRect: The total bounding rect of all captures in global screen coordinates.
    /// - Returns: A composited CGImage, or nil if the CGContext could not be created.
    static func stitch(_ captures: [DisplayCapture], totalRect: CGRect) -> CGImage? {
        guard !captures.isEmpty else { return nil }

        // Canvas size: use the highest backingScaleFactor so the output is the
        // sharpest possible image (non-Retina portions are upscaled to match)
        let maxScale = captures.map(\.scale).max() ?? 1.0
        let canvasW = Int(totalRect.width * maxScale)
        let canvasH = Int(totalRect.height * maxScale)

        guard canvasW > 0 && canvasH > 0 else { return nil }

        // BGRA format to match ScreenCaptureKit's kCVPixelFormatType_32BGRA output
        guard let context = CGContext(
            data: nil,
            width: canvasW,
            height: canvasH,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return nil }

        for capture in captures {
            // Convert global coordinates to canvas-local pixel coordinates.
            // CGContext origin is bottom-left (matching AppKit), so no flip needed.
            let localX = (capture.displayFrame.origin.x - totalRect.origin.x) * maxScale
            let localY = (capture.displayFrame.origin.y - totalRect.origin.y) * maxScale
            let destRect = CGRect(
                x: localX,
                y: localY,
                width: capture.displayFrame.width * maxScale,
                height: capture.displayFrame.height * maxScale
            )
            context.draw(capture.image, in: destRect)
        }

        return context.makeImage()
    }
}
