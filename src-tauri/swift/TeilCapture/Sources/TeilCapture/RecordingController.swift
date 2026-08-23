import AppKit
import CoreMedia
import Foundation
@preconcurrency import ScreenCaptureKit

/// Outcome of `RecordingController.begin`.
enum BeginResult {
    case started(width: Int, height: Int)
    case cancelled
    case busy
    case error(String)
}

/// Process-wide owner of the single in-flight recording. Resolves the user's
/// selection into a `RecordTarget`, starts a `ScreenRecorder`, and brokers the
/// control calls from the FFI layer.
final class RecordingController: @unchecked Sendable {
    static let shared = RecordingController()

    private let lock = NSLock()
    private var current: ScreenRecorder?
    private var reserved = false
    private let frameWindow = RecordingFrameWindow()
    /// Strong ref for SCStreamConfiguration.backgroundColor (an `assign` property).
    private static let letterboxColor = CGColor(gray: 0, alpha: 1)

    private init() {}

    // MARK: - Lock helpers (sync, so async methods never touch NSLock directly)

    private func tryReserve() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if current != nil || reserved { return false }
        reserved = true
        return true
    }

    private func installCurrent(_ recorder: ScreenRecorder) {
        lock.lock()
        current = recorder
        reserved = false
        lock.unlock()
    }

    private func clearIfCurrent(_ recorder: ScreenRecorder) {
        lock.lock()
        if current === recorder { current = nil }
        lock.unlock()
    }

    // MARK: - Begin

    func begin(mode: RecordMode, options: RecordOptions) async -> BeginResult {
        guard tryReserve() else { return .busy }

        // Serialize against interactive screenshot overlays (reuses the shared guard).
        guard beginSession() else {
            release()
            return .cancelled
        }

        let target: RecordTarget?
        do {
            target = try await resolveTarget(mode: mode, options: options)
        } catch {
            endSession()
            release()
            return .error(error.localizedDescription)
        }
        endSession()

        guard let target else {
            release()
            return .cancelled
        }

        let recorder = ScreenRecorder(target: target, options: options)
        do {
            try await recorder.start()
        } catch {
            release()
            return .error(error.localizedDescription)
        }

        installCurrent(recorder)

        if let frame = target.frameRectAppKit {
            await MainActor.run { self.frameWindow.show(around: frame) }
        }
        return .started(width: target.width, height: target.height)
    }

    private func release() {
        lock.lock()
        reserved = false
        lock.unlock()
    }

    private func currentRecorder() -> ScreenRecorder? {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    // MARK: - Controls

    func pause() -> Bool { currentRecorder()?.pause() ?? false }
    func resume() -> Bool { currentRecorder()?.resume() ?? false }
    func status() -> RecordStatusSnapshot { currentRecorder()?.snapshot() ?? .idle }

    func stop() async -> Result<RecordResult, RecorderError> {
        guard let recorder = currentRecorder() else { return .failure(.notRecording) }
        let result = await recorder.stop()
        await MainActor.run { self.frameWindow.hide() }
        clearIfCurrent(recorder)
        return result
    }

    func cancel() {
        guard let recorder = currentRecorder() else { return }
        recorder.cancel()
        Task { @MainActor in self.frameWindow.hide() }
        clearIfCurrent(recorder)
    }

    // MARK: - Target resolution

    private func resolveTarget(mode: RecordMode, options: RecordOptions) async throws -> RecordTarget? {
        let engine = CaptureEngine()
        switch mode {
        case .region:
            guard let rect = await selectRegion() else { return nil }
            let (primaryHeight, screens): (CGFloat, [(frame: CGRect, scale: CGFloat)]) =
                await MainActor.run {
                    let ph = NSScreen.screens.first?.frame.height ?? 0
                    return (ph, NSScreen.screens.map { (frame: $0.frame, scale: $0.backingScaleFactor) })
                }
            guard let pick = RecordingGeometry.pickRecordingScreen(for: rect, among: screens) else {
                return nil
            }
            let display = try await engine.findDisplayByFrame(pick.frame)
            let filter = try await engine.buildFilter(for: display)
            let cgInter = appKitToCG(pick.intersection, primaryHeight: primaryHeight)
            let (config, w, h) = makeDisplayConfig(
                display: display, sourceRectCG: cgInter, scale: pick.scale, options: options)
            return RecordTarget(
                filter: filter, config: config, width: w, height: h,
                windowID: nil, frameRectAppKit: pick.intersection)

        case .window:
            guard let selection = await selectWindow() else { return nil }
            switch selection {
            case .window(let scWindow):
                let (filter, config, w, h) = makeWindowConfig(scWindow: scWindow, options: options)
                return RecordTarget(
                    filter: filter, config: config, width: w, height: h,
                    windowID: scWindow.windowID, frameRectAppKit: nil)
            case .desktop:
                return try await fullscreenTarget(engine: engine, options: options)
            }

        case .fullscreen:
            return try await fullscreenTarget(engine: engine, options: options)
        }
    }

    private func fullscreenTarget(engine: CaptureEngine, options: RecordOptions) async throws -> RecordTarget {
        let (display, screenInfo) = try await engine.findCurrentDisplay()
        let filter = try await engine.buildFilter(for: display)
        let (config, w, h) = makeDisplayConfig(
            display: display, sourceRectCG: nil, scale: screenInfo.backingScaleFactor, options: options)
        return RecordTarget(
            filter: filter, config: config, width: w, height: h,
            windowID: nil, frameRectAppKit: nil)
    }

    @MainActor private func selectRegion() async -> CGRect? {
        await OverlayCoordinator().beginRegionSelection()
    }

    @MainActor private func selectWindow() async -> WindowSelectionResult? {
        await WindowSelectionCoordinator().beginWindowSelection()
    }

    // MARK: - Config builders

    private func baseConfig(options: RecordOptions) -> SCStreamConfiguration {
        let c = SCStreamConfiguration()
        c.minimumFrameInterval = CMTime(value: 1, timescale: max(1, options.fps))
        c.queueDepth = 5
        c.showsCursor = options.showCursor
        c.pixelFormat = kCVPixelFormatType_32BGRA
        c.colorSpaceName = CGColorSpace.sRGB
        c.capturesAudio = options.captureAudio
        if options.captureAudio {
            c.sampleRate = 48_000
            c.channelCount = 2
            c.excludesCurrentProcessAudio = true
        }
        return c
    }

    private func makeDisplayConfig(
        display: SCDisplay, sourceRectCG: CGRect?, scale: CGFloat, options: RecordOptions
    ) -> (SCStreamConfiguration, Int, Int) {
        let c = baseConfig(options: options)
        c.scalesToFit = true
        c.preservesAspectRatio = true
        if let src = sourceRectCG {
            let localX = src.origin.x - display.frame.origin.x
            let localY = src.origin.y - display.frame.origin.y
            c.sourceRect = CGRect(x: localX, y: localY, width: src.width, height: src.height)
            let (w, h) = RecordingGeometry.fitToH264Limit(
                Int((src.width * scale).rounded(.down)), Int((src.height * scale).rounded(.down)))
            c.width = w
            c.height = h
            return (c, w, h)
        } else {
            let (w, h) = RecordingGeometry.fitToH264Limit(
                Int(CGFloat(display.width) * scale), Int(CGFloat(display.height) * scale))
            c.width = w
            c.height = h
            return (c, w, h)
        }
    }

    private func makeWindowConfig(
        scWindow: SCWindow, options: RecordOptions
    ) -> (SCContentFilter, SCStreamConfiguration, Int, Int) {
        let filter = SCContentFilter(desktopIndependentWindow: scWindow)
        let scale = CGFloat(filter.pointPixelScale)
        let size = filter.contentRect.size
        let (w, h) = RecordingGeometry.fitToH264Limit(
            Int(size.width * scale), Int(size.height * scale))

        let c = baseConfig(options: options)
        c.width = w
        c.height = h
        c.scalesToFit = true
        c.preservesAspectRatio = true
        c.ignoreShadowsSingleWindow = true
        c.ignoreGlobalClipSingleWindow = true
        c.shouldBeOpaque = false
        c.backgroundColor = Self.letterboxColor
        if #available(macOS 14.2, *) {
            c.includeChildWindows = true
        }
        return (filter, c, w, h)
    }

    private func appKitToCG(_ r: CGRect, primaryHeight: CGFloat) -> CGRect {
        CGRect(x: r.origin.x, y: primaryHeight - r.maxY, width: r.width, height: r.height)
    }
}
