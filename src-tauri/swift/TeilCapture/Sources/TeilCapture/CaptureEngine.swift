import AppKit
import CoreGraphics
import Foundation
@preconcurrency import ScreenCaptureKit

// MARK: - CaptureEngineError

enum CaptureEngineError: LocalizedError {
    case noDisplayFound
    case noScreenFound
    case captureFailedNoImage
    case stitchFailed
    case windowCaptureFailedNoImage

    var errorDescription: String? {
        switch self {
        case .noDisplayFound:
            return "Could not find a suitable display for capture."
        case .noScreenFound:
            return "Could not find the current screen."
        case .captureFailedNoImage:
            return "Capture did not produce an image."
        case .stitchFailed:
            return "Could not stitch cross-monitor captures."
        case .windowCaptureFailedNoImage:
            return "Window capture did not produce an image."
        }
    }
}

// MARK: - ScreenInfo
// A Sendable snapshot of NSScreen data needed for capture configuration.

private struct ScreenInfo: Sendable {
    let frame: CGRect
    let backingScaleFactor: CGFloat
}

// MARK: - CaptureEngine

/// Main capture coordinator actor (macOS 14+, SCScreenshotManager only).
///
/// Own app bundle is always excluded from SCContentFilter so no overlay or popover
/// will ever appear in a captured image. Inside the Tauri host process Bundle.main
/// resolves to the host app bundle, so the exclusion carries over unchanged.
actor CaptureEngine {

    // MARK: - Public API

    /// Captures the display where the mouse cursor currently is.
    ///
    /// - Returns: A CaptureResult containing the full-display CGImage.
    func captureFullscreen() async throws -> CaptureResult {
        let (display, screenInfo) = try await findCurrentDisplay()

        let filter = try await buildFilter(for: display)
        let config = buildConfig(sourceRect: nil, display: display, scale: screenInfo.backingScaleFactor)

        // Wrap in nonisolated(unsafe) to transfer non-Sendable SCKit types across actor boundary.
        // These values are fully constructed before the transfer and not mutated afterward.
        nonisolated(unsafe) let f = filter
        nonisolated(unsafe) let c = config
        let image = try await SCScreenshotManager.captureImage(contentFilter: f, configuration: c)

        // Convert display.frame from CG coords (top-left origin) to AppKit coords (bottom-left origin)
        // so CaptureFeedback.showCaptureFlash positions correctly.
        let capturedRect = screenInfo.frame
        return CaptureResult(image: image, capturedRect: capturedRect)
    }

    /// Captures a region defined by a global-coordinate CGRect.
    ///
    /// If the rect spans multiple displays, each display's intersecting portion is captured
    /// separately and stitched by CrossMonitorStitcher.
    ///
    /// - Parameter rect: The region to capture, in global AppKit screen coordinates.
    /// - Returns: A CaptureResult containing the captured CGImage.
    func captureRegion(_ rect: CGRect) async throws -> CaptureResult {
        // Collect NSScreen data on MainActor — NSScreen is not Sendable
        let (primaryHeight, screenInfos): (CGFloat, [(frame: CGRect, scale: CGFloat)]) = await MainActor.run {
            let primary = NSScreen.screens.first?.frame.height ?? 0
            let infos = NSScreen.screens.map { ($0.frame, $0.backingScaleFactor) }
            return (primary, infos)
        }

        // Find which screens intersect the requested rect
        let intersecting: [(frame: CGRect, scale: CGFloat, intersection: CGRect)] = screenInfos.compactMap { info in
            let intersection = info.frame.intersection(rect)
            guard !intersection.isNull && intersection.width > 0 && intersection.height > 0 else {
                return nil
            }
            return (info.frame, info.scale, intersection)
        }

        guard !intersecting.isEmpty else {
            throw CaptureEngineError.noScreenFound
        }

        if intersecting.count == 1 {
            // Single-display path
            let entry = intersecting[0]
            let display = try await findDisplayByFrame(entry.frame)
            let filter = try await buildFilter(for: display)
            // Convert intersection from AppKit screen coords (bottom-left origin, Y-up)
            // to CG screen coords (top-left origin, Y-down) for SCStreamConfiguration.sourceRect
            let cgIntersection = appKitRectToCG(entry.intersection, primaryHeight: primaryHeight)
            let config = buildConfig(sourceRect: cgIntersection, display: display, scale: entry.scale)

            nonisolated(unsafe) let f = filter
            nonisolated(unsafe) let c = config
            let image = try await SCScreenshotManager.captureImage(contentFilter: f, configuration: c)

            return CaptureResult(image: image, capturedRect: entry.intersection)
        } else {
            // Multi-display path — capture each portion and stitch
            var displayCaptures: [CrossMonitorStitcher.DisplayCapture] = []

            for entry in intersecting {
                let display = try await findDisplayByFrame(entry.frame)
                let filter = try await buildFilter(for: display)
                let cgIntersection = appKitRectToCG(entry.intersection, primaryHeight: primaryHeight)
                let config = buildConfig(sourceRect: cgIntersection, display: display, scale: entry.scale)

                nonisolated(unsafe) let f = filter
                nonisolated(unsafe) let c = config
                let image = try await SCScreenshotManager.captureImage(contentFilter: f, configuration: c)

                displayCaptures.append(
                    CrossMonitorStitcher.DisplayCapture(
                        image: image,
                        displayFrame: entry.intersection,
                        scale: entry.scale
                    )
                )
            }

            guard let stitched = CrossMonitorStitcher.stitch(displayCaptures, totalRect: rect) else {
                throw CaptureEngineError.stitchFailed
            }

            return CaptureResult(image: stitched, capturedRect: rect)
        }
    }

    /// Captures a specific window without shadow and with transparent corners.
    ///
    /// Uses `SCContentFilter(desktopIndependentWindow:)` to capture the full window
    /// regardless of screen position (correctly handles partially off-screen windows).
    ///
    /// - Parameter scWindow: The window to capture, obtained from SCShareableContent.
    /// - Returns: A CaptureResult with a BGRA CGImage (alpha channel preserved).
    func captureWindow(_ scWindow: SCWindow) async throws -> CaptureResult {
        // Get the backing scale factor for the screen containing the window
        let scale: CGFloat = await MainActor.run {
            let windowCenter = CGPoint(
                x: scWindow.frame.midX,
                y: scWindow.frame.midY
            )
            // SCWindow.frame is in CG coordinates; NSScreen.frame is AppKit coordinates.
            // Match by x-origin and dimensions (same approach as findDisplayByFrame).
            let screen = NSScreen.screens.first(where: {
                abs($0.frame.origin.x - scWindow.frame.origin.x) < scWindow.frame.width
                    && $0.frame.contains(CGPoint(x: windowCenter.x, y: $0.frame.midY))
            }) ?? NSScreen.main
            return screen?.backingScaleFactor ?? 2.0
        }

        let filter = SCContentFilter(desktopIndependentWindow: scWindow)

        let config = SCStreamConfiguration()
        config.pixelFormat = kCVPixelFormatType_32BGRA  // Has alpha channel for transparent corners
        config.showsCursor = false
        config.capturesAudio = false

        // true = IGNORE (exclude) the shadow — counter-intuitive naming
        config.ignoreShadowsSingleWindow = true

        // false = do NOT force opaque — preserves the window's alpha channel for rounded corners
        config.shouldBeOpaque = false

        // Backing pixels: multiply by scale for Retina sharpness
        config.width = Int(scWindow.frame.width * scale)
        config.height = Int(scWindow.frame.height * scale)

        // Wrap in nonisolated(unsafe) to transfer non-Sendable SCKit types across actor boundary.
        // These values are fully constructed before the transfer and not mutated afterward.
        nonisolated(unsafe) let f = filter
        nonisolated(unsafe) let c = config
        let image = try await SCScreenshotManager.captureImage(contentFilter: f, configuration: c)

        // capturedRect: SCWindow.frame is in CG coordinates; CaptureResult documents AppKit.
        // The FFI layer converts via cgFrameToAppKit before positioning the flash.
        return CaptureResult(image: image, capturedRect: scWindow.frame)
    }

    // MARK: - Private Helpers

    /// Converts a rect from AppKit screen coordinates (bottom-left origin, Y-up)
    /// to CG screen coordinates (top-left origin, Y-down).
    ///
    /// The conversion uses the primary display height as the reference point since both
    /// coordinate systems share the same origin horizontally but are Y-flipped relative
    /// to the primary display's top/bottom edge.
    private func appKitRectToCG(_ appKitRect: CGRect, primaryHeight: CGFloat) -> CGRect {
        let cgY = primaryHeight - appKitRect.maxY
        return CGRect(x: appKitRect.origin.x, y: cgY, width: appKitRect.width, height: appKitRect.height)
    }

    /// Builds an SCContentFilter for the given display, excluding the app's own bundle.
    private func buildFilter(for display: SCDisplay) async throws -> SCContentFilter {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let excluded = content.applications.filter {
            $0.bundleIdentifier == Bundle.main.bundleIdentifier
        }
        return SCContentFilter(
            display: display,
            excludingApplications: excluded,
            exceptingWindows: []
        )
    }

    /// Builds an SCStreamConfiguration for the given capture parameters.
    ///
    /// - Parameters:
    ///   - sourceRect: The rect to capture in global screen coordinates. Nil captures the full display.
    ///   - display: The target SCDisplay.
    ///   - scale: The backing scale factor (from NSScreen.backingScaleFactor).
    private func buildConfig(sourceRect: CGRect?, display: SCDisplay, scale: CGFloat) -> SCStreamConfiguration {
        let config = SCStreamConfiguration()
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = false
        config.capturesAudio = false

        if let rect = sourceRect {
            // sourceRect must be in display-LOCAL point coordinates (subtract display origin)
            let localX = rect.origin.x - display.frame.origin.x
            let localY = rect.origin.y - display.frame.origin.y
            config.sourceRect = CGRect(x: localX, y: localY, width: rect.width, height: rect.height)
            // width/height must be in backing pixels
            config.width = Int(rect.width * scale)
            config.height = Int(rect.height * scale)
        } else {
            // Full display capture
            config.width = Int(CGFloat(display.width) * scale)
            config.height = Int(CGFloat(display.height) * scale)
        }

        return config
    }

    /// Finds the SCDisplay matching a given NSScreen frame.
    ///
    /// Matches by x-origin and dimensions rather than full origin because NSScreen uses
    /// AppKit coordinates (bottom-left, Y-up) while SCDisplay uses CG coordinates
    /// (top-left, Y-down). The x-origin is the same in both systems; the y-origin differs
    /// for vertically-offset displays.
    private func findDisplayByFrame(_ screenFrame: CGRect) async throws -> SCDisplay {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: {
            abs($0.frame.origin.x - screenFrame.origin.x) < 1.0
                && abs($0.frame.width - screenFrame.width) < 1.0
                && abs($0.frame.height - screenFrame.height) < 1.0
        }) else {
            throw CaptureEngineError.noDisplayFound
        }
        return display
    }

    /// Finds the display where the mouse cursor is currently located.
    ///
    /// NSScreen.screens and NSEvent.mouseLocation are accessed on MainActor.
    /// Only Sendable values (CGRect, CGFloat) are returned from the MainActor closure.
    ///
    /// - Returns: A tuple of (SCDisplay, ScreenInfo) for the display under the cursor.
    private func findCurrentDisplay() async throws -> (SCDisplay, ScreenInfo) {
        // Collect NSScreen data on MainActor — NSScreen is not Sendable
        // Use NSMouseInRect for correct AppKit coordinate hit-testing (CGRect.contains
        // is exclusive on maxX/maxY and fails at screen boundaries)
        let screenInfo: ScreenInfo = await MainActor.run {
            let mouse = NSEvent.mouseLocation
            let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
                ?? NSScreen.main
                ?? NSScreen.screens.first!
            return ScreenInfo(frame: screen.frame, backingScaleFactor: screen.backingScaleFactor)
        }

        let display = try await findDisplayByFrame(screenInfo.frame)
        return (display, screenInfo)
    }
}
